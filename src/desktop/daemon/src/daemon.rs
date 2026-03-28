use std::{
    fs,
    os::unix::fs::PermissionsExt,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use desktop_core::{
    automation::{Point, new_backend},
    error::AppError,
    ipc::{read_framed_json, socket_path, write_framed_json},
    protocol::{Command, RequestEnvelope, ResponseEnvelope},
};
use image::{ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};

#[cfg(target_os = "macos")]
use crate::overlay;
use crate::platform::windowing::WindowInfo;
use crate::{clipboard, permissions, platform, recording, replay, request_store, trace, vision};

#[cfg(target_os = "macos")]
const OVERLAY_WATCH_TRACK_INTERVAL_MS: u64 = 40;
#[cfg(target_os = "macos")]
const OVERLAY_SCREEN_CAPTURE_MODE_LOCK_MS: u64 = 2_000;
#[cfg(target_os = "macos")]
const PRIVACY_OVERLAY_STOP_DELAY_MS: u64 = 2_200;
#[cfg(target_os = "macos")]
static OVERLAY_WATCH_TRACK_RUNNING: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy)]
pub struct DaemonConfig {
    pub idle_timeout: Option<Duration>,
}

impl DaemonConfig {
    pub fn resident() -> Self {
        Self { idle_timeout: None }
    }

    pub fn on_demand() -> Self {
        Self {
            idle_timeout: Some(Duration::from_secs(8)),
        }
    }
}

pub fn start_background(config: DaemonConfig) -> Result<(), AppError> {
    #[cfg(target_os = "macos")]
    bootstrap_overlay_glow();
    let listener = bind_listener()?;
    thread::spawn(move || {
        if let Err(err) = accept_loop(listener, config) {
            eprintln!("daemon loop error: {err}");
        }
    });
    Ok(())
}

pub fn run_blocking(config: DaemonConfig) -> Result<(), AppError> {
    #[cfg(target_os = "macos")]
    bootstrap_overlay_glow();
    let listener = bind_listener()?;
    accept_loop(listener, config)
}

#[cfg(target_os = "macos")]
fn bootstrap_overlay_glow() {
    trace::log("overlay:bootstrap ready");
    start_overlay_watch_tracker();
    if overlay::is_active() {
        let (mode, bounds) = if let Some(bounds) = frontmost_window_bounds() {
            (overlay::WatchMode::WindowMode, Some(bounds))
        } else {
            (overlay::WatchMode::DesktopMode, None)
        };
        if let Err(err) = overlay::watch_mode_changed(mode, bounds) {
            trace::log(format!("overlay:bootstrap mode_warn {err}"));
        }
        if let Err(err) = overlay::confidence_changed(1.0) {
            trace::log(format!("overlay:bootstrap confidence_warn {err}"));
        }
    }
}

#[cfg(target_os = "macos")]
fn start_overlay_watch_tracker() {
    if OVERLAY_WATCH_TRACK_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    thread::spawn(|| {
        trace::log("overlay:watch_tracker start");
        loop {
            if overlay::is_active() {
                if !overlay::is_agent_active() && !overlay::is_watch_mode_locked() {
                    if let Some(bounds) = frontmost_window_bounds() {
                        let _ = overlay::watch_mode_changed(
                            overlay::WatchMode::WindowMode,
                            Some(bounds),
                        );
                    } else {
                        let _ = overlay::watch_mode_changed(overlay::WatchMode::DesktopMode, None);
                    }
                }
            }
            thread::sleep(Duration::from_millis(OVERLAY_WATCH_TRACK_INTERVAL_MS));
        }
    });
}

fn bind_listener() -> Result<UnixListener, AppError> {
    let path = socket_path();
    if path.exists() {
        let _ = fs::remove_file(&path);
    }

    let listener = UnixListener::bind(&path).map_err(|err| {
        AppError::backend_unavailable(format!("bind {} failed: {err}", path.display()))
    })?;
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|err| {
        AppError::backend_unavailable(format!("set socket permissions failed: {err}"))
    })?;
    listener
        .set_nonblocking(true)
        .map_err(|err| AppError::backend_unavailable(format!("set nonblocking failed: {err}")))?;
    trace::log(format!("listener:bound socket={}", path.display()));
    Ok(listener)
}

fn accept_loop(listener: UnixListener, config: DaemonConfig) -> Result<(), AppError> {
    if config.idle_timeout.is_none() {
        listener.set_nonblocking(false).map_err(|err| {
            AppError::backend_unavailable(format!("set listener blocking mode failed: {err}"))
        })?;
    }
    let mut last_activity = Instant::now();
    let active_clients = Arc::new(AtomicUsize::new(0));

    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                last_activity = Instant::now();
                if let Err(err) = stream.set_nonblocking(false) {
                    eprintln!("failed to set client stream blocking mode: {err}");
                    trace::log(format!("accept:set_blocking_failed error={err}"));
                    continue;
                }
                trace::log("accept:client_connected");
                let active_clients = Arc::clone(&active_clients);
                active_clients.fetch_add(1, Ordering::SeqCst);
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream) {
                        eprintln!("daemon client error: {err}");
                        trace::log(format!("client:error {err}"));
                    }
                    active_clients.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(timeout) = config.idle_timeout {
                    if active_clients.load(Ordering::SeqCst) == 0
                        && last_activity.elapsed() >= timeout
                    {
                        trace::log("listener:idle_timeout_exit");
                        break;
                    }
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(err) => {
                return Err(AppError::backend_unavailable(format!(
                    "accept failed: {err}"
                )));
            }
        }
    }

    let path = socket_path();
    if path.exists() {
        let _ = fs::remove_file(path);
    }
    trace::log("listener:closed");
    Ok(())
}

fn handle_client(mut stream: UnixStream) -> Result<(), AppError> {
    let request: RequestEnvelope = read_framed_json(&mut stream)?;
    let request_id = request.request_id.clone();
    let command = request.command.clone();
    let command_name = command.name().to_string();
    trace::log(format!(
        "client:request_start request_id={} command={}",
        request_id, command_name
    ));
    #[cfg(target_os = "macos")]
    let transient_overlay_started = maybe_start_privacy_overlay(&command);
    #[cfg(target_os = "macos")]
    if matches!(
        command,
        Command::ScreenCapture {
            active_window: false,
            ..
        }
    ) {
        let _ = overlay::lock_watch_mode(
            overlay::WatchMode::DesktopMode,
            None,
            Duration::from_millis(OVERLAY_SCREEN_CAPTURE_MODE_LOCK_MS),
        );
    } else if !matches!(command, Command::ScreenTokenize { .. }) {
        if let Some(bounds) = frontmost_window_bounds() {
            let _ = overlay::watch_mode_changed(overlay::WatchMode::WindowMode, Some(bounds));
        }
    }
    #[cfg(target_os = "macos")]
    let _ = overlay::agent_active_changed(true);
    let response = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| execute(command)))
    {
        Ok(Ok(result)) => {
            trace::log("client:execute_ok");
            ResponseEnvelope::success(request_id.clone(), result)
        }
        Ok(Err(err)) => {
            trace::log(format!(
                "client:execute_err code={:?} msg={}",
                err.code, err.message
            ));
            ResponseEnvelope::from_error(request_id, command_name, err)
        }
        Err(payload) => {
            let panic_message = if let Some(msg) = payload.downcast_ref::<&str>() {
                (*msg).to_string()
            } else if let Some(msg) = payload.downcast_ref::<String>() {
                msg.clone()
            } else {
                "non-string panic payload".to_string()
            };
            trace::log(format!("client:execute_panic {panic_message}"));
            let err = AppError::internal(format!(
                "daemon panic during command execution: {panic_message}"
            ));
            ResponseEnvelope::from_error(request_id, command_name, err)
        }
    };
    #[cfg(target_os = "macos")]
    let _ = overlay::agent_active_changed(false);
    #[cfg(target_os = "macos")]
    if transient_overlay_started {
        schedule_transient_overlay_stop();
    }
    if let Err(err) = recording::record_command(&request, &response) {
        eprintln!("recorder write failed: {err}");
        trace::log(format!("client:record_err {err}"));
    }
    if let Err(err) = request_store::record(&request, &response) {
        trace::log(format!("client:request_store_err {err}"));
    }
    trace::log("client:write_response_begin");
    write_framed_json(&mut stream, &response)?;
    trace::log("client:write_response_ok");
    Ok(())
}

#[cfg(target_os = "macos")]
fn maybe_start_privacy_overlay(command: &Command) -> bool {
    if !command_requires_privacy_signal(command) || overlay::is_active() {
        return false;
    }
    match overlay::start_overlay() {
        Ok(started) => {
            if started {
                let (mode, bounds) = if let Some(bounds) = frontmost_window_bounds() {
                    (overlay::WatchMode::WindowMode, Some(bounds))
                } else {
                    (overlay::WatchMode::DesktopMode, None)
                };
                if let Err(err) = overlay::watch_mode_changed(mode, bounds) {
                    trace::log(format!(
                        "overlay:privacy_auto_start mode_warn command={} err={err}",
                        command.name()
                    ));
                }
            }
            trace::log(format!(
                "overlay:privacy_auto_start command={} started={started}",
                command.name()
            ));
            started
        }
        Err(err) => {
            trace::log(format!(
                "overlay:privacy_auto_start_warn command={} err={err}",
                command.name()
            ));
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn schedule_transient_overlay_stop() {
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(PRIVACY_OVERLAY_STOP_DELAY_MS));
        if overlay::is_agent_active() || !overlay::is_active() {
            return;
        }
        match overlay::stop_overlay() {
            Ok(stopped) => trace::log(format!("overlay:privacy_auto_stop stopped={stopped}")),
            Err(err) => trace::log(format!("overlay:privacy_auto_stop_warn {err}")),
        }
    });
}

#[cfg(target_os = "macos")]
fn command_requires_privacy_signal(command: &Command) -> bool {
    matches!(
        command,
        Command::ScreenCapture { .. }
            | Command::ScreenTokenize { .. }
            | Command::ScreenFindText { .. }
            | Command::WaitText { .. }
            | Command::PointerMove { .. }
            | Command::PointerDown { .. }
            | Command::PointerUp { .. }
            | Command::PointerClick { .. }
            | Command::PointerClickText { .. }
            | Command::PointerClickId { .. }
            | Command::PointerClickToken { .. }
            | Command::PointerScroll { .. }
            | Command::PointerDrag { .. }
            | Command::UiType { .. }
            | Command::KeyHotkey { .. }
            | Command::KeyEnter
            | Command::KeyEscape
    )
}

fn execute(command: Command) -> Result<Value, AppError> {
    match command {
        Command::Ping => Ok(json!({ "message": "pong" })),
        Command::AppHide { name } => {
            trace::log(format!("app_hide:start name={name}"));
            let state = platform::apps::hide_application(&name)?;
            trace::log(format!("app_hide:ok name={name} state={state}"));
            Ok(json!({ "app": name, "state": state }))
        }
        Command::AppShow { name } => {
            trace::log(format!("app_show:start name={name}"));
            platform::apps::show_application(&name)?;
            trace::log(format!("app_show:ok name={name}"));
            Ok(json!({ "app": name, "state": "shown" }))
        }
        Command::AppIsolate { name } => {
            trace::log(format!("app_isolate:start name={name}"));
            let hidden = platform::apps::isolate_application(&name)?;
            let _ = wait_for_open_app(&name, 6_000);
            trace::log(format!("app_isolate:ok name={name} hidden={hidden}"));
            Ok(json!({ "app": name, "state": "isolated", "hidden_apps": hidden }))
        }
        Command::WindowList => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let windows = list_windows()?;
            Ok(json!({
                "windows": windows.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
            }))
        }
        Command::WindowBounds { title } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let windows = list_windows()?;
            let selected = select_window_candidate(&windows, &title)?;
            Ok(json!({
                "window": selected.as_json()
            }))
        }
        Command::WindowFocus { title } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let windows = list_windows()?;
            let selected = select_window_candidate(&windows, &title)?;
            platform::apps::focus_window(selected)?;
            Ok(json!({
                "window": selected.as_json(),
                "focused": true
            }))
        }
        Command::OpenApp {
            name,
            args,
            wait,
            timeout_ms,
        } => {
            let mut cmd = ProcessCommand::new("open");
            cmd.arg("-a").arg(&name);
            if !args.is_empty() {
                cmd.args(&args);
            }

            let output = cmd.output().map_err(|err| {
                AppError::backend_unavailable(format!("failed to invoke open: {err}"))
            })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(AppError::internal(stderr));
            }

            let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
            let script = format!(r#"tell application "{escaped}" to activate"#);
            let activate = ProcessCommand::new("osascript")
                .arg("-e")
                .arg(script)
                .output()
                .map_err(|err| {
                    AppError::backend_unavailable(format!("failed to run osascript: {err}"))
                })?;
            if !activate.status.success() {
                let stderr = String::from_utf8_lossy(&activate.stderr).trim().to_string();
                return Err(AppError::internal(stderr));
            }

            if wait {
                wait_for_open_app(&name, timeout_ms.unwrap_or(8_000))?;
            }
            Ok(json!({}))
        }
        Command::PointerMove { x, y } => {
            trace::log(format!("pointer_move:start x={x} y={y}"));
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.move_mouse(Point::new(x, y))?;
            trace::log(format!("pointer_move:ok x={x} y={y}"));
            Ok(json!({}))
        }
        Command::PointerDown { x, y } => {
            trace::log(format!("pointer_down:start x={x} y={y}"));
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = Point::new(x, y);
            backend.move_mouse(point)?;
            backend.left_down(point)?;
            trace::log(format!("pointer_down:ok x={x} y={y}"));
            Ok(json!({}))
        }
        Command::PointerUp { x, y } => {
            trace::log(format!("pointer_up:start x={x} y={y}"));
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = Point::new(x, y);
            backend.move_mouse(point)?;
            backend.left_up(point)?;
            trace::log(format!("pointer_up:ok x={x} y={y}"));
            Ok(json!({}))
        }
        Command::PointerClick { x, y, absolute } => {
            trace::log(format!(
                "pointer_click:start x={x} y={y} absolute={absolute}"
            ));
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = resolve_pointer_click_point(x, y, absolute)?;
            backend.move_mouse(point)?;
            backend.left_click(point)?;
            trace::log(format!(
                "pointer_click:ok x={} y={} absolute={absolute}",
                point.x, point.y
            ));
            Ok(json!({}))
        }
        Command::PointerScroll { dx, dy } => {
            trace::log(format!("pointer_scroll:start dx={dx} dy={dy}"));
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.scroll_wheel(dx, dy)?;
            trace::log(format!("pointer_scroll:ok dx={dx} dy={dy}"));
            Ok(json!({}))
        }
        Command::PointerDrag {
            x1,
            y1,
            x2,
            y2,
            hold_ms,
        } => {
            trace::log(format!(
                "pointer_drag:start from=({}, {}) to=({}, {}) hold_ms={}",
                x1, y1, x2, y2, hold_ms
            ));
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let start = Point::new(x1, y1);
            let end = Point::new(x2, y2);
            backend.move_mouse(start)?;
            backend.left_down(start)?;
            backend.sleep_ms(hold_ms.max(30));
            backend.left_drag(end)?;
            backend.left_up(end)?;
            trace::log(format!(
                "pointer_drag:ok from=({}, {}) to=({}, {}) hold_ms={}",
                x1, y1, x2, y2, hold_ms
            ));
            Ok(json!({}))
        }
        Command::UiType { text } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.type_text(&text)?;
            Ok(json!({}))
        }
        Command::KeyHotkey { hotkey } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.press_hotkey(&hotkey)?;
            Ok(json!({}))
        }
        Command::KeyEnter => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.press_enter()?;
            Ok(json!({}))
        }
        Command::KeyEscape => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.press_escape()?;
            Ok(json!({}))
        }
        Command::ScreenCapture {
            out_path,
            overlay,
            active_window,
            region,
        } => {
            trace::log("execute:screen_capture:start");
            permissions::ensure_screen_recording_permission()?;
            let capture_bounds = if active_window {
                let base = frontmost_window_bounds().ok_or_else(|| {
                    AppError::target_not_found(
                        "frontmost window not found; ensure a standard app window is focused",
                    )
                })?;
                Some(resolve_capture_region_bounds(base, region.as_ref())?)
            } else if region.is_some() {
                let base = main_display_bounds().ok_or_else(|| {
                    AppError::target_not_found("display bounds unavailable for screenshot --region")
                })?;
                Some(resolve_capture_region_bounds(base, region.as_ref())?)
            } else {
                None
            };
            let capture_out_path: Option<PathBuf> = out_path
                .map(Into::into)
                .or_else(|| Some(platform::capture::default_capture_path()));
            let capture = if let Some(bounds) = capture_bounds.clone() {
                platform::capture::capture_bounds(capture_out_path.clone(), bounds, None, true)?
            } else {
                platform::capture::capture_display(capture_out_path)?
            };
            let overlay_path = if overlay {
                let path = write_capture_overlay(&capture)?;
                Some(path.display().to_string())
            } else {
                None
            };
            trace::log(format!(
                "execute:screen_capture:ok snapshot_id={} event_count={}",
                capture.snapshot.snapshot_id,
                capture.event_ids.len()
            ));
            Ok(json!({
                "snapshot_id": capture.snapshot.snapshot_id,
                "timestamp": capture.snapshot.timestamp,
                "path": capture
                    .image_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                "overlay_path": overlay_path,
                "capture_scope": if active_window {
                    "active_window"
                } else if region.is_some() {
                    "region"
                } else {
                    "display"
                },
                "window_bounds": capture_bounds,
                "display": capture.snapshot.display,
                "focused_app": capture.snapshot.focused_app,
                "event_ids": capture.event_ids
            }))
        }
        Command::ScreenTokenize {
            overlay_out_path,
            window_id,
            screenshot_path,
            active_window,
            region,
        } => {
            trace::log("execute:screen_tokenize:start");
            let screenshot_mode = screenshot_path.is_some();
            let payload = if let Some(path_raw) = screenshot_path {
                if window_id.is_some() {
                    return Err(AppError::invalid_argument(
                        "--window cannot be combined with --screenshot for screen tokenize",
                    ));
                }
                if active_window {
                    return Err(AppError::invalid_argument(
                        "--active-window cannot be combined with --screenshot for screen tokenize",
                    ));
                }
                let screenshot = PathBuf::from(path_raw);
                if !screenshot.exists() {
                    return Err(AppError::invalid_argument(format!(
                        "screenshot file does not exist: {}",
                        screenshot.display()
                    )));
                }
                vision::pipeline::tokenize_screenshot(&screenshot, None, region.as_ref())?
            } else {
                permissions::ensure_screen_recording_permission()?;
                let backend = new_backend()?;
                backend.check_accessibility_permission()?;
                if active_window {
                    if window_id.is_some() {
                        return Err(AppError::invalid_argument(
                            "--active-window cannot be combined with --window for screen tokenize",
                        ));
                    }
                    let bounds = frontmost_window_bounds().ok_or_else(|| {
                        AppError::target_not_found(
                            "frontmost window bounds unavailable for --active-window",
                        )
                    })?;
                    let bounds = resolve_tokenize_region_bounds(bounds, region.as_ref())?;
                    let app = frontmost_app_name();
                    let title = app.clone().unwrap_or_else(|| "active_window".to_string());
                    let window_meta = vision::pipeline::TokenizeWindowMeta {
                        id: "frontmost:1".to_string(),
                        title,
                        app,
                        bounds,
                    };
                    vision::pipeline::tokenize_window(window_meta)?
                } else if window_id.is_none() {
                    let overlay_window_bounds = {
                        #[cfg(target_os = "macos")]
                        {
                            if overlay::is_active() {
                                overlay::tracked_window_bounds()
                            } else {
                                None
                            }
                        }
                        #[cfg(not(target_os = "macos"))]
                        {
                            None
                        }
                    };
                    if let Some(bounds) = overlay_window_bounds {
                        let bounds = resolve_tokenize_region_bounds(bounds, region.as_ref())?;
                        let window_meta = vision::pipeline::TokenizeWindowMeta {
                            id: "frontmost:1".to_string(),
                            title: "active_window".to_string(),
                            app: None,
                            bounds,
                        };
                        vision::pipeline::tokenize_window(window_meta)?
                    } else if let Some(bounds) = frontmost_window_bounds() {
                        let bounds = resolve_tokenize_region_bounds(bounds, region.as_ref())?;
                        let app = frontmost_app_name();
                        let title = app.clone().unwrap_or_else(|| "active_window".to_string());
                        let window_meta = vision::pipeline::TokenizeWindowMeta {
                            id: "frontmost:1".to_string(),
                            title,
                            app,
                            bounds,
                        };
                        vision::pipeline::tokenize_window(window_meta)?
                    } else {
                        let windows = list_windows()?;
                        let target = resolve_tokenize_window_target(&windows, None)?;
                        let bounds =
                            resolve_tokenize_region_bounds(target.bounds.clone(), region.as_ref())?;
                        let window_meta = vision::pipeline::TokenizeWindowMeta {
                            id: target.id.clone(),
                            title: target.title.clone(),
                            app: Some(target.app.clone()),
                            bounds,
                        };
                        vision::pipeline::tokenize_window(window_meta)?
                    }
                } else {
                    let windows = list_windows()?;
                    let target = resolve_tokenize_window_target(&windows, window_id.as_deref())?;
                    let bounds =
                        resolve_tokenize_region_bounds(target.bounds.clone(), region.as_ref())?;
                    let window_meta = vision::pipeline::TokenizeWindowMeta {
                        id: target.id.clone(),
                        title: target.title.clone(),
                        app: Some(target.app.clone()),
                        bounds,
                    };
                    vision::pipeline::tokenize_window(window_meta)?
                }
            };
            let mut payload = payload;
            if !screenshot_mode {
                backfill_tokenize_window_positions(&mut payload);
            }
            if let Some(path_raw) = overlay_out_path {
                let overlay_path = PathBuf::from(path_raw);
                vision::pipeline::write_tokenize_overlay(&payload, &overlay_path)?;
                trace::log(format!(
                    "execute:screen_tokenize:overlay_ok path={}",
                    overlay_path.display()
                ));
            }
            #[cfg(target_os = "macos")]
            if let Err(err) = overlay::update_from_tokenize(&payload) {
                trace::log(format!("execute:screen_tokenize:overlay_update_warn {err}"));
            }
            let element_count: usize = payload.windows.iter().map(|w| w.elements.len()).sum();
            trace::log(format!(
                "execute:screen_tokenize:ok snapshot_id={} elements={}",
                payload.snapshot_id, element_count
            ));
            Ok(serde_json::to_value(payload).map_err(|err| {
                AppError::internal(format!("failed to encode token payload: {err}"))
            })?)
        }
        Command::ScreenFindText { text, all } => {
            permissions::ensure_screen_recording_permission()?;
            find_text_targets(&text, all)
        }
        Command::OverlayStart { duration_ms } => {
            #[cfg(target_os = "macos")]
            {
                let started = overlay::start_overlay()?;
                if let Some(ms) = duration_ms {
                    let stop_after = ms.max(1);
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(stop_after));
                        if let Err(err) = overlay::stop_overlay() {
                            trace::log(format!(
                                "overlay:auto_stop err duration_ms={} error={}",
                                stop_after, err
                            ));
                        } else {
                            trace::log(format!("overlay:auto_stop ok duration_ms={stop_after}"));
                        }
                    });
                }
                return Ok(json!({
                    "overlay_running": true,
                    "started": started,
                    "duration_ms": duration_ms
                }));
            }
            #[allow(unreachable_code)]
            Err(AppError::backend_unavailable(
                "overlay is supported only on macOS",
            ))
        }
        Command::OverlayStop => {
            #[cfg(target_os = "macos")]
            {
                let stopped = overlay::stop_overlay()?;
                return Ok(json!({
                    "overlay_running": false,
                    "stopped": stopped
                }));
            }
            #[allow(unreachable_code)]
            Err(AppError::backend_unavailable(
                "overlay is supported only on macOS",
            ))
        }
        Command::WaitText {
            text,
            timeout_ms,
            interval_ms,
            disappear,
        } => wait_for_text(&text, timeout_ms, interval_ms, disappear),
        Command::PointerClickText { text } => click_text_target(&text),
        Command::PointerClickId { id } => click_element_id_target(&id),
        Command::PointerClickToken { token } => click_token_target(token),
        Command::ClipboardRead => {
            let text = clipboard::read_clipboard()?;
            Ok(json!({ "text": text }))
        }
        Command::ClipboardWrite { text } => {
            clipboard::write_clipboard(&text)?;
            Ok(json!({ "written": true }))
        }
        Command::PermissionsCheck => {
            let payload = desktop_core::protocol::PermissionsPayload {
                accessibility: desktop_core::protocol::PermissionState {
                    granted: permissions::accessibility_granted(),
                    remediation: (!permissions::accessibility_granted())
                        .then(|| permissions::accessibility_remediation().to_string()),
                },
                screen_recording: desktop_core::protocol::PermissionState {
                    granted: permissions::screen_recording_granted(),
                    remediation: (!permissions::screen_recording_granted())
                        .then(|| permissions::screen_recording_remediation().to_string()),
                },
            };
            Ok(serde_json::to_value(payload).map_err(|err| {
                AppError::internal(format!("failed to encode permissions payload: {err}"))
            })?)
        }
        Command::DebugSnapshot => vision::debug::write_debug_snapshot(),
        Command::RequestShow { request_id } => request_store::show(&request_id),
        Command::RequestScreenshot {
            request_id,
            out_path,
        } => request_store::screenshot(&request_id, out_path),
        Command::RequestResponse { request_id } => request_store::response(&request_id),
        Command::ReplayRecord { duration_ms, stop } => {
            if stop {
                recording::stop_recording()
            } else {
                recording::start_recording(duration_ms)
            }
        }
        Command::ReplayLoad { session_dir } => {
            let session_dir = replay::parse_session_dir(&session_dir)?;
            replay::load_session(&session_dir)
        }
    }
}

fn resolve_pointer_click_point(x: u32, y: u32, absolute: bool) -> Result<Point, AppError> {
    if absolute {
        return Ok(Point::new(x, y));
    }
    let bounds = click_scope_window_bounds().ok_or_else(|| {
        AppError::target_not_found(
            "frontmost window bounds unavailable for relative pointer click; use --absolute",
        )
    })?;
    let abs_x = (bounds.x + x as f64).round().max(0.0) as u32;
    let abs_y = (bounds.y + y as f64).round().max(0.0) as u32;
    Ok(Point::new(abs_x, abs_y))
}

fn resolve_tokenize_region_bounds(
    base: desktop_core::protocol::Bounds,
    region: Option<&desktop_core::protocol::Bounds>,
) -> Result<desktop_core::protocol::Bounds, AppError> {
    resolve_relative_region_bounds("tokenize", base, region)
}

fn resolve_capture_region_bounds(
    base: desktop_core::protocol::Bounds,
    region: Option<&desktop_core::protocol::Bounds>,
) -> Result<desktop_core::protocol::Bounds, AppError> {
    resolve_relative_region_bounds("screenshot", base, region)
}

fn resolve_relative_region_bounds(
    command_name: &str,
    base: desktop_core::protocol::Bounds,
    region: Option<&desktop_core::protocol::Bounds>,
) -> Result<desktop_core::protocol::Bounds, AppError> {
    let Some(region) = region else {
        return Ok(base);
    };
    if region.width <= 0.0 || region.height <= 0.0 {
        return Err(AppError::invalid_argument(format!(
            "{command_name} --region width/height must be > 0"
        )));
    }
    if region.x < 0.0 || region.y < 0.0 {
        return Err(AppError::invalid_argument(format!(
            "{command_name} --region x/y must be >= 0"
        )));
    }

    let x = base.x + region.x;
    let y = base.y + region.y;
    let right = x + region.width;
    let bottom = y + region.height;
    let base_right = base.x + base.width;
    let base_bottom = base.y + base.height;

    if x < base.x || y < base.y || right > base_right || bottom > base_bottom {
        return Err(AppError::invalid_argument(format!(
            "{command_name} --region ({:.0},{:.0},{:.0},{:.0}) exceeds target bounds ({:.0},{:.0},{:.0},{:.0})",
            region.x,
            region.y,
            region.width,
            region.height,
            base.x,
            base.y,
            base.width,
            base.height
        )));
    }

    Ok(desktop_core::protocol::Bounds {
        x,
        y,
        width: region.width,
        height: region.height,
    })
}

fn backfill_tokenize_window_positions(payload: &mut desktop_core::protocol::TokenizePayload) {
    if payload.windows.is_empty()
        || payload
            .windows
            .iter()
            .all(|window| window.os_bounds.is_some())
    {
        return;
    }
    let Some(bounds) = frontmost_window_bounds() else {
        return;
    };
    let mut filled = 0usize;
    for window in &mut payload.windows {
        if window.os_bounds.is_none() {
            window.os_bounds = Some(bounds.clone());
            filled += 1;
        }
    }
    if filled > 0 {
        trace::log(format!(
            "screen_tokenize:backfill_os_bounds filled={} bounds=({:.1},{:.1},{:.1},{:.1})",
            filled, bounds.x, bounds.y, bounds.width, bounds.height
        ));
    }
}

fn click_text_target(query: &str) -> Result<Value, AppError> {
    permissions::ensure_screen_recording_permission()?;
    let capture = vision::pipeline::capture_and_update(None)?;
    let normalized_texts = normalize_snapshot_texts_to_display(
        &capture.snapshot.texts,
        capture.image.width(),
        capture.image.height(),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
    );
    let window_bounds = click_scope_window_bounds();
    let window_filtered = window_bounds
        .as_ref()
        .map(|bounds| filter_texts_to_window_progressive(&normalized_texts, bounds))
        .unwrap_or_else(|| normalized_texts.clone());
    trace::log(format!(
        "ui_click_text:candidates snapshot_id={} query=\"{}\" texts={} window_filtered={} display={}x{} focused_app={} frontmost_window={}",
        capture.snapshot.snapshot_id,
        query,
        normalized_texts.len(),
        window_filtered.len(),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
        capture.snapshot.focused_app.as_deref().unwrap_or("<none>"),
        window_bounds
            .as_ref()
            .map(|b| format!("({:.1},{:.1},{:.1},{:.1})", b.x, b.y, b.width, b.height))
            .unwrap_or_else(|| "null".to_string())
    ));
    let target = match select_text_candidate(&window_filtered, query) {
        Ok(target) => target,
        Err(primary_err) => {
            trace::log(format!(
                "ui_click_text:ocr_primary_failed code={:?} msg={}",
                primary_err.code, primary_err.message
            ));
            match tokenize_click_text_candidate(query, window_bounds.as_ref()) {
                Ok(fallback) => {
                    trace::log("ui_click_text:fallback source=tokenize");
                    fallback
                }
                Err(fallback_err) => {
                    trace::log(format!(
                        "ui_click_text:fallback_failed code={:?} msg={}",
                        fallback_err.code, fallback_err.message
                    ));
                    if window_bounds.is_some() && window_filtered.is_empty() {
                        return Err(AppError::target_not_found(
                            "no OCR/tokenize text detected in frontmost window; cannot click target safely",
                        ));
                    }
                    return Err(primary_err);
                }
            }
        }
    };
    trace::log(format!(
        "ui_click_text:selected text=\"{}\" confidence={:.3} bounds=({}, {}, {}, {})",
        compact_for_log(&target.text),
        target.confidence,
        target.bounds.x,
        target.bounds.y,
        target.bounds.width,
        target.bounds.height
    ));
    perform_click(&target.bounds)?;

    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "text": target.text,
        "bounds": target.bounds
    }))
}

#[derive(Debug, Clone)]
struct TokenizeClickElementCandidate {
    id: String,
    text: Option<String>,
    bounds: desktop_core::protocol::Bounds,
    source: String,
}

fn click_element_id_target(id: &str) -> Result<Value, AppError> {
    permissions::ensure_screen_recording_permission()?;
    let needle = id.trim();
    if needle.is_empty() {
        return Err(AppError::invalid_argument("empty element id selector"));
    }
    let bounds = click_scope_window_bounds().ok_or_else(|| {
        AppError::target_not_found("frontmost window bounds unavailable for click --id")
    })?;
    let app = frontmost_app_name();
    let window_meta = vision::pipeline::TokenizeWindowMeta {
        id: "frontmost:1".to_string(),
        title: app.clone().unwrap_or_else(|| "active_window".to_string()),
        app,
        bounds,
    };
    let payload = vision::pipeline::tokenize_window(window_meta)?;
    let candidates = tokenize_payload_elements_for_click(&payload);
    let total_candidates = candidates.len();
    let matches: Vec<TokenizeClickElementCandidate> = candidates
        .into_iter()
        .filter(|element| element.id == needle)
        .collect();
    trace::log(format!(
        "pointer_click_id:candidates id=\"{}\" total={} matched={}",
        compact_for_log(needle),
        total_candidates,
        matches.len()
    ));
    if matches.is_empty() {
        return Err(AppError::target_not_found(format!(
            "element id \"{needle}\" was not found in frontmost window"
        )));
    }
    if matches.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple elements matched id \"{needle}\""
        )));
    }
    let target = &matches[0];
    trace::log(format!(
        "pointer_click_id:selected id=\"{}\" source={} bounds=({:.1}, {:.1}, {:.1}, {:.1}) text=\"{}\"",
        compact_for_log(&target.id),
        compact_for_log(&target.source),
        target.bounds.x,
        target.bounds.y,
        target.bounds.width,
        target.bounds.height,
        compact_for_log(target.text.as_deref().unwrap_or(""))
    ));
    perform_click(&target.bounds)?;
    Ok(json!({
        "id": target.id.clone(),
        "text": target.text.clone(),
        "bounds": target.bounds.clone(),
        "source": target.source.clone()
    }))
}

fn tokenize_click_text_candidate(
    query: &str,
    window_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Result<desktop_core::protocol::SnapshotText, AppError> {
    let bounds = window_bounds.cloned().ok_or_else(|| {
        AppError::target_not_found("frontmost window bounds unavailable for tokenize fallback")
    })?;
    let app = frontmost_app_name();
    let window_meta = vision::pipeline::TokenizeWindowMeta {
        id: "frontmost:1".to_string(),
        title: app.clone().unwrap_or_else(|| "active_window".to_string()),
        app,
        bounds,
    };
    let payload = vision::pipeline::tokenize_window(window_meta)?;
    let tokenize_texts = tokenize_payload_texts_for_click(&payload);
    if tokenize_texts.is_empty() {
        return Err(AppError::target_not_found(
            "tokenize fallback produced no text elements",
        ));
    }
    select_text_candidate(&tokenize_texts, query)
}

fn tokenize_payload_texts_for_click(
    payload: &desktop_core::protocol::TokenizePayload,
) -> Vec<desktop_core::protocol::SnapshotText> {
    let mut out = Vec::new();
    let Some(image) = payload.image.as_ref() else {
        return out;
    };
    let image_w = image.width as f64;
    let image_h = image.height as f64;
    if image_w <= 0.0 || image_h <= 0.0 {
        return out;
    }

    for window in &payload.windows {
        let Some(os_bounds) = window.os_bounds.as_ref() else {
            continue;
        };
        for element in &window.elements {
            let text = element
                .text
                .as_ref()
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(ToString::to_string);
            let Some(text) = text else { continue };
            let Some(bounds) = tokenize_element_bbox_to_display(&element.bbox, os_bounds, image)
            else {
                continue;
            };
            let confidence =
                element
                    .confidence
                    .unwrap_or(if element.source.starts_with("accessibility_ax:") {
                        0.92
                    } else {
                        0.62
                    });
            out.push(desktop_core::protocol::SnapshotText {
                text,
                bounds,
                confidence,
            });
        }
    }
    out
}

fn tokenize_payload_elements_for_click(
    payload: &desktop_core::protocol::TokenizePayload,
) -> Vec<TokenizeClickElementCandidate> {
    let mut out = Vec::new();
    let Some(image) = payload.image.as_ref() else {
        return out;
    };
    for window in &payload.windows {
        let Some(os_bounds) = window.os_bounds.as_ref() else {
            continue;
        };
        for element in &window.elements {
            let Some(bounds) = tokenize_element_bbox_to_display(&element.bbox, os_bounds, image)
            else {
                continue;
            };
            out.push(TokenizeClickElementCandidate {
                id: element.id.clone(),
                text: element
                    .text
                    .as_ref()
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                    .map(ToString::to_string),
                bounds,
                source: element.source.clone(),
            });
        }
    }
    out
}

fn tokenize_element_bbox_to_display(
    bbox: &[f64; 4],
    os_bounds: &desktop_core::protocol::Bounds,
    image: &desktop_core::protocol::TokenizeImage,
) -> Option<desktop_core::protocol::Bounds> {
    let image_w = image.width as f64;
    let image_h = image.height as f64;
    if image_w <= 0.0 || image_h <= 0.0 {
        return None;
    }
    let sx = os_bounds.width / image_w;
    let sy = os_bounds.height / image_h;
    Some(desktop_core::protocol::Bounds {
        x: os_bounds.x + bbox[0] * sx,
        y: os_bounds.y + bbox[1] * sy,
        width: bbox[2] * sx,
        height: bbox[3] * sy,
    })
}

fn find_text_targets(query: &str, all: bool) -> Result<Value, AppError> {
    let capture = vision::pipeline::capture_and_update(None)?;
    let ranked = ranked_text_candidates(&capture.snapshot.texts, query)?;
    if ranked.is_empty() {
        return Err(AppError::target_not_found(format!(
            "text target \"{query}\" was not found"
        )));
    }
    let entries: Vec<Value> = ranked
        .iter()
        .take(if all { ranked.len() } else { 1 })
        .map(|(score, candidate)| {
            json!({
                "score": score,
                "text": candidate.text,
                "confidence": candidate.confidence,
                "bounds": candidate.bounds
            })
        })
        .collect();
    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "timestamp": capture.snapshot.timestamp,
        "display": capture.snapshot.display,
        "focused_app": capture.snapshot.focused_app,
        "query": query,
        "matches": entries
    }))
}

#[allow(dead_code)]
fn screen_layout_summary() -> Result<Value, AppError> {
    let capture = vision::pipeline::capture_and_update(None)?;
    let text_envelope = bounds_from_texts(&capture.snapshot.texts);
    let panels = infer_panels_from_texts(&capture.snapshot.texts)
        .into_iter()
        .map(|(name, bounds, text_count)| {
            json!({
                "name": name,
                "bounds": bounds,
                "text_count": text_count
            })
        })
        .collect::<Vec<_>>();
    let button_like = capture
        .snapshot
        .texts
        .iter()
        .filter(|t| t.confidence >= 0.4 && t.text.len() <= 32)
        .map(|t| {
            json!({
                "text": t.text,
                "bounds": t.bounds,
                "confidence": t.confidence
            })
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "timestamp": capture.snapshot.timestamp,
        "display": capture.snapshot.display,
        "focused_app": capture.snapshot.focused_app,
        "frontmost_window": frontmost_window_bounds(),
        "text_envelope": text_envelope,
        "panels": panels,
        "button_like_texts": button_like
    }))
}

fn click_token_target(token_id: u32) -> Result<Value, AppError> {
    let token = vision::pipeline::token(token_id)?.ok_or_else(|| {
        AppError::target_not_found(format!(
            "token {token_id} not found; run `screen tokenize --json` first"
        ))
    })?;
    trace::log(format!(
        "ui_click_token:selected token={} text=\"{}\" confidence={:.3} bounds=({}, {}, {}, {})",
        token_id,
        token.text,
        token.confidence,
        token.bounds.x,
        token.bounds.y,
        token.bounds.width,
        token.bounds.height
    ));
    perform_click(&token.bounds)?;
    verify_click_postcondition(&token.text, &token.bounds, 1_200)?;
    Ok(json!({
        "token": token_id,
        "text": token.text,
        "bounds": token.bounds
    }))
}

fn wait_for_text(
    query: &str,
    timeout_ms: u64,
    interval_ms: u64,
    disappear: bool,
) -> Result<Value, AppError> {
    permissions::ensure_screen_recording_permission()?;
    let start = Instant::now();
    loop {
        let capture = vision::pipeline::capture_and_update(None)?;
        if disappear {
            let matches = ranked_text_candidates(&capture.snapshot.texts, query)?;
            if matches.is_empty() {
                return Ok(json!({
                    "snapshot_id": capture.snapshot.snapshot_id,
                    "timestamp": capture.snapshot.timestamp,
                    "disappeared": true
                }));
            }
        } else if let Ok(candidate) = select_text_candidate(&capture.snapshot.texts, query) {
            return Ok(json!({
                "snapshot_id": capture.snapshot.snapshot_id,
                "timestamp": capture.snapshot.timestamp,
                "matched_text": candidate.text,
                "bounds": candidate.bounds
            }));
        }
        if start.elapsed().as_millis() as u64 >= timeout_ms {
            let message = if disappear {
                format!("timed out waiting for text \"{query}\" to disappear")
            } else {
                format!("timed out waiting for text \"{query}\"")
            };
            return Err(
                AppError::timeout(message).with_details(json!({ "timeout_ms": timeout_ms }))
            );
        }
        thread::sleep(Duration::from_millis(interval_ms.max(30)));
    }
}

fn wait_for_open_app(app_name: &str, timeout_ms: u64) -> Result<(), AppError> {
    let needle = app_name.to_lowercase();
    let start = Instant::now();
    loop {
        if let Some(frontmost) = frontmost_app_name() {
            if frontmost.to_lowercase().contains(&needle) {
                return Ok(());
            }
        }
        if start.elapsed().as_millis() as u64 >= timeout_ms {
            return Err(AppError::timeout(format!(
                "timed out waiting for app \"{app_name}\" to become frontmost"
            )));
        }
        thread::sleep(Duration::from_millis(150));
    }
}

fn select_text_candidate(
    texts: &[desktop_core::protocol::SnapshotText],
    query: &str,
) -> Result<desktop_core::protocol::SnapshotText, AppError> {
    let mut candidates = ranked_text_candidates(texts, query)?;
    trace_ranked_candidates(query, &candidates);
    if candidates.is_empty() {
        return Err(AppError::target_not_found(format!(
            "text target \"{query}\" was not found"
        )));
    }

    let ranked = candidates
        .iter()
        .take(10)
        .enumerate()
        .map(|(idx, (score, text))| {
            let center_x = text.bounds.x + text.bounds.width / 2.0;
            let center_y = text.bounds.y + text.bounds.height / 2.0;
            format!(
                "#{} score={:.3} conf={:.3} center=({:.1},{:.1}) bounds=({:.1},{:.1},{:.1},{:.1}) text=\"{}\"",
                idx + 1,
                score,
                text.confidence,
                center_x,
                center_y,
                text.bounds.x,
                text.bounds.y,
                text.bounds.width,
                text.bounds.height,
                compact_for_log(&text.text)
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");
    trace::log(format!("select_text_candidate:ranked {ranked}"));
    if candidates.len() > 1 && (candidates[0].0 - candidates[1].0).abs() < 0.05 {
        trace::log(format!(
            "select_text_candidate:ambiguous query=\"{}\" top_delta={:.3}",
            compact_for_log(query),
            (candidates[0].0 - candidates[1].0).abs()
        ));
        return Err(AppError::ambiguous_target(format!(
            "multiple matches for text \"{query}\""
        )));
    }
    let best = candidates.remove(0).1;
    if best.confidence < 0.25 {
        trace::log(format!(
            "select_text_candidate:low_confidence query=\"{}\" conf={:.3} text=\"{}\"",
            compact_for_log(query),
            best.confidence,
            compact_for_log(&best.text)
        ));
        return Err(AppError::low_confidence(format!(
            "match confidence too low for \"{query}\""
        )));
    }
    trace::log(format!(
        "select_text_candidate:best query=\"{}\" score_conf={:.3} text=\"{}\" bounds=({:.1},{:.1},{:.1},{:.1})",
        compact_for_log(query),
        best.confidence,
        compact_for_log(&best.text),
        best.bounds.x,
        best.bounds.y,
        best.bounds.width,
        best.bounds.height
    ));
    Ok(best)
}

fn ranked_text_candidates(
    texts: &[desktop_core::protocol::SnapshotText],
    query: &str,
) -> Result<Vec<(f32, desktop_core::protocol::SnapshotText)>, AppError> {
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Err(AppError::invalid_argument("empty text selector"));
    }
    let q_confusable = normalize_confusable_text(query.trim());
    let mut candidates: Vec<(f32, desktop_core::protocol::SnapshotText)> = texts
        .iter()
        .filter_map(|t| {
            let hay = t.text.to_lowercase();
            if hay.contains(&q) {
                text_match_score(&q, &hay, t.confidence).map(|score| (score, t.clone()))
            } else {
                let hay_confusable = normalize_confusable_text(&t.text);
                if hay_confusable.contains(&q_confusable) {
                    confusable_text_match_score(&q_confusable, &hay_confusable, t.confidence)
                        .map(|score| (score, t.clone()))
                } else {
                    None
                }
            }
        })
        .collect();
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    Ok(candidates)
}

fn trace_ranked_candidates(
    query: &str,
    candidates: &[(f32, desktop_core::protocol::SnapshotText)],
) {
    trace::log(format!(
        "select_text_candidate:start query=\"{}\" matches={}",
        compact_for_log(query),
        candidates.len()
    ));
    if candidates.is_empty() {
        trace::log(format!(
            "select_text_candidate:not_found query=\"{}\"",
            compact_for_log(query)
        ));
    }
}

fn perform_click(bounds: &desktop_core::protocol::Bounds) -> Result<(), AppError> {
    let center_x = (bounds.x + bounds.width / 2.0).max(0.0).round() as u32;
    let center_y = (bounds.y + bounds.height / 2.0).max(0.0).round() as u32;
    trace::log(format!(
        "perform_click:point bounds=({}, {}, {}, {}) center=({}, {})",
        bounds.x, bounds.y, bounds.width, bounds.height, center_x, center_y
    ));
    perform_click_at(center_x, center_y)
}

fn perform_click_at(x: u32, y: u32) -> Result<(), AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let point = Point::new(x, y);
    trace::log(format!("perform_click:move start center=({}, {})", x, y));
    backend.move_mouse(point)?;
    trace::log("perform_click:move ok");
    thread::sleep(Duration::from_millis(60));
    trace::log("perform_click:left_click start");
    backend.left_click(point)?;
    trace::log("perform_click:left_click ok");
    Ok(())
}

fn verify_click_postcondition(
    query: &str,
    original_bounds: &desktop_core::protocol::Bounds,
    timeout_ms: u64,
) -> Result<(), AppError> {
    let start = Instant::now();
    while start.elapsed().as_millis() as u64 <= timeout_ms {
        let capture = vision::pipeline::capture_and_update(None)?;
        let texts = normalize_snapshot_texts_to_display(
            &capture.snapshot.texts,
            capture.image.width(),
            capture.image.height(),
            capture.snapshot.display.width,
            capture.snapshot.display.height,
        );
        let still_present = texts.iter().any(|text| {
            text_matches_query(&text.text, query)
                && iou(&inflate_bounds(original_bounds, 6.0), &text.bounds) > 0.35
        });
        if !still_present {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(120));
    }
    Err(AppError::postcondition_failed(format!(
        "postcondition failed: \"{query}\" still present at target location after click"
    )))
}

fn inflate_bounds(
    bounds: &desktop_core::protocol::Bounds,
    pad: f64,
) -> desktop_core::protocol::Bounds {
    desktop_core::protocol::Bounds {
        x: (bounds.x - pad).max(0.0),
        y: (bounds.y - pad).max(0.0),
        width: bounds.width + pad * 2.0,
        height: bounds.height + pad * 2.0,
    }
}

fn iou(a: &desktop_core::protocol::Bounds, b: &desktop_core::protocol::Bounds) -> f64 {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;

    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    if inter <= 0.0 {
        return 0.0;
    }
    let union = (a.width * a.height) + (b.width * b.height) - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

fn compact_for_log(value: &str) -> String {
    let mut normalized = value.replace(['\n', '\r', '\t'], " ");
    normalized = normalized.trim().to_string();
    if normalized.len() > 72 {
        normalized.truncate(69);
        normalized.push_str("...");
    }
    normalized
}

fn text_matches_query(candidate: &str, query: &str) -> bool {
    let q = query.trim();
    if q.is_empty() {
        return false;
    }
    if candidate.to_lowercase().contains(&q.to_lowercase()) {
        return true;
    }
    let q_confusable = normalize_confusable_text(q);
    let candidate_confusable = normalize_confusable_text(candidate);
    !q_confusable.is_empty() && candidate_confusable.contains(&q_confusable)
}

fn normalize_confusable_text(value: &str) -> String {
    value
        .chars()
        .flat_map(|ch| {
            let canonical = match ch {
                'I' | 'l' | '1' | '|' | '!' => 'l',
                _ => ch,
            };
            canonical.to_lowercase()
        })
        .collect()
}

fn text_match_score(query: &str, candidate: &str, confidence: f32) -> Option<f32> {
    if query.is_empty() || candidate.is_empty() || !candidate.contains(query) {
        return None;
    }

    let q_len = query.chars().count().max(1) as f32;
    let c_len = candidate.chars().count().max(1) as f32;
    let length_ratio = (q_len / c_len).min(1.0);
    let exact = if candidate == query { 1.0 } else { 0.0 };
    let starts = if candidate.starts_with(query) {
        1.0
    } else {
        0.0
    };
    let ends = if candidate.ends_with(query) { 1.0 } else { 0.0 };

    // Drop noisy substring matches where the query is a tiny fragment of a long line.
    if exact < 0.5 && length_ratio < 0.35 {
        return None;
    }

    Some(exact * 3.0 + starts * 0.8 + ends * 0.4 + length_ratio * 2.2 + confidence * 0.8)
}

fn confusable_text_match_score(query: &str, candidate: &str, confidence: f32) -> Option<f32> {
    let q_len = query.chars().count();
    if q_len < 4 {
        return None;
    }
    text_match_score(query, candidate, confidence).map(|score| score * 0.88)
}

#[allow(dead_code)]
fn bounds_from_texts(
    texts: &[desktop_core::protocol::SnapshotText],
) -> Option<desktop_core::protocol::Bounds> {
    if texts.is_empty() {
        return None;
    }
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = 0.0_f64;
    let mut max_y = 0.0_f64;
    for text in texts {
        min_x = min_x.min(text.bounds.x);
        min_y = min_y.min(text.bounds.y);
        max_x = max_x.max(text.bounds.x + text.bounds.width);
        max_y = max_y.max(text.bounds.y + text.bounds.height);
    }
    Some(desktop_core::protocol::Bounds {
        x: min_x.max(0.0),
        y: min_y.max(0.0),
        width: (max_x - min_x).max(0.0),
        height: (max_y - min_y).max(0.0),
    })
}

#[allow(dead_code)]
fn infer_panels_from_texts(
    texts: &[desktop_core::protocol::SnapshotText],
) -> Vec<(String, desktop_core::protocol::Bounds, usize)> {
    if texts.is_empty() {
        return Vec::new();
    }

    let mut centers: Vec<f64> = texts
        .iter()
        .map(|t| t.bounds.x + t.bounds.width / 2.0)
        .collect();
    centers.sort_by(|a, b| a.total_cmp(b));
    let mut best_gap = 0.0_f64;
    let mut split = None;
    for pair in centers.windows(2) {
        let gap = pair[1] - pair[0];
        if gap > best_gap {
            best_gap = gap;
            split = Some(pair[0] + gap / 2.0);
        }
    }

    if best_gap < 80.0 {
        return bounds_from_texts(texts)
            .map(|bounds| vec![("main".to_string(), bounds, texts.len())])
            .unwrap_or_default();
    }

    let split_x = split.unwrap_or(centers[centers.len() / 2]);
    let mut left = Vec::new();
    let mut right = Vec::new();
    for text in texts {
        let center_x = text.bounds.x + text.bounds.width / 2.0;
        if center_x < split_x {
            left.push(text.clone());
        } else {
            right.push(text.clone());
        }
    }

    let mut panels = Vec::new();
    if let Some(bounds) = bounds_from_texts(&left) {
        panels.push(("left".to_string(), bounds, left.len()));
    }
    if let Some(bounds) = bounds_from_texts(&right) {
        panels.push(("right".to_string(), bounds, right.len()));
    }
    if panels.is_empty() {
        if let Some(bounds) = bounds_from_texts(texts) {
            panels.push(("main".to_string(), bounds, texts.len()));
        }
    }
    panels
}

fn write_capture_overlay(capture: &vision::pipeline::CaptureResult) -> Result<PathBuf, AppError> {
    let mut image = capture.image.clone();
    let image_width = image.width();
    let image_height = image.height();
    if image_width == 0 || image_height == 0 {
        return Err(AppError::backend_unavailable(
            "cannot render overlay for empty capture image",
        ));
    }

    for text in &capture.snapshot.texts {
        if text.confidence < 0.45 || text.text.len() > 96 {
            continue;
        }
        draw_logical_bounds_on_image(
            &mut image,
            &text.bounds,
            capture.snapshot.display.width,
            capture.snapshot.display.height,
            Rgba([72, 196, 222, 255]),
            1,
        );
    }

    let overlay_path = capture
        .image_path
        .as_ref()
        .map(|path| overlay_path_for_capture(path))
        .unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "capture-{}.overlay.png",
                capture.snapshot.snapshot_id
            ))
        });
    image
        .save_with_format(&overlay_path, ImageFormat::Png)
        .map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to write overlay image {}: {err}",
                overlay_path.display()
            ))
        })?;
    trace::log(format!(
        "screen_capture_overlay:ok snapshot_id={} path={}",
        capture.snapshot.snapshot_id,
        overlay_path.display()
    ));
    Ok(overlay_path)
}

fn overlay_path_for_capture(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "capture".to_string());
    let mut name = format!("{stem}.overlay");
    if let Some(ext) = path.extension().and_then(|ext| ext.to_str()) {
        if !ext.is_empty() {
            name.push('.');
            name.push_str(ext);
        }
    } else {
        name.push_str(".png");
    }
    match path.parent() {
        Some(parent) => parent.join(name),
        None => PathBuf::from(name),
    }
}

fn draw_logical_bounds_on_image(
    image: &mut RgbaImage,
    bounds: &desktop_core::protocol::Bounds,
    display_width: u32,
    display_height: u32,
    color: Rgba<u8>,
    thickness: u32,
) {
    if let Some((x0, y0, x1, y1)) = logical_bounds_to_image_rect(
        bounds,
        image.width(),
        image.height(),
        display_width,
        display_height,
    ) {
        let x1 = (x1 - 1).max(x0) as u32;
        let y1 = (y1 - 1).max(y0) as u32;
        draw_rect_outline(image, x0 as u32, y0 as u32, x1, y1, color, thickness);
    }
}

fn logical_bounds_to_image_rect(
    bounds: &desktop_core::protocol::Bounds,
    image_width: u32,
    image_height: u32,
    display_width: u32,
    display_height: u32,
) -> Option<(i64, i64, i64, i64)> {
    if image_width == 0 || image_height == 0 || display_width == 0 || display_height == 0 {
        return None;
    }
    let sx = image_width as f64 / display_width as f64;
    let sy = image_height as f64 / display_height as f64;
    let x0 = (bounds.x * sx).floor() as i64;
    let y0 = (bounds.y * sy).floor() as i64;
    let x1 = ((bounds.x + bounds.width) * sx).ceil() as i64;
    let y1 = ((bounds.y + bounds.height) * sy).ceil() as i64;
    let x0 = x0.clamp(0, image_width as i64);
    let y0 = y0.clamp(0, image_height as i64);
    let x1 = x1.clamp(0, image_width as i64);
    let y1 = y1.clamp(0, image_height as i64);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0, y0, x1, y1))
}

fn draw_rect_outline(
    image: &mut RgbaImage,
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
    color: Rgba<u8>,
    thickness: u32,
) {
    if x1 < x0 || y1 < y0 {
        return;
    }
    let thickness = thickness.max(1);
    for offset in 0..thickness {
        let top = y0.saturating_add(offset).min(y1);
        let bottom = y1.saturating_sub(offset).max(y0);
        for x in x0..=x1 {
            image.put_pixel(x, top, color);
            image.put_pixel(x, bottom, color);
        }
        let left = x0.saturating_add(offset).min(x1);
        let right = x1.saturating_sub(offset).max(x0);
        for y in y0..=y1 {
            image.put_pixel(left, y, color);
            image.put_pixel(right, y, color);
        }
    }
}

#[cfg(test)]
fn logical_point_to_image_point(
    x: u32,
    y: u32,
    image_width: u32,
    image_height: u32,
    display_width: u32,
    display_height: u32,
) -> Option<(u32, u32)> {
    if image_width == 0 || image_height == 0 || display_width == 0 || display_height == 0 {
        return None;
    }
    let sx = image_width as f64 / display_width as f64;
    let sy = image_height as f64 / display_height as f64;
    let ix = ((x as f64) * sx).round() as i64;
    let iy = ((y as f64) * sy).round() as i64;
    let ix = ix.clamp(0, image_width.saturating_sub(1) as i64) as u32;
    let iy = iy.clamp(0, image_height.saturating_sub(1) as i64) as u32;
    Some((ix, iy))
}

#[cfg(test)]
fn estimate_toggle_state(
    frame_image: Option<&RgbaImage>,
    bounds: &desktop_core::protocol::Bounds,
    display_width: u32,
    display_height: u32,
) -> &'static str {
    let Some(image) = frame_image else {
        return "unknown";
    };
    let Some((x0, y0, x1, y1)) = logical_bounds_to_image_rect(
        bounds,
        image.width(),
        image.height(),
        display_width,
        display_height,
    ) else {
        return "unknown";
    };
    let mut blueish = 0usize;
    let mut total = 0usize;
    for y in y0 as u32..y1 as u32 {
        for x in x0 as u32..x1 as u32 {
            let p = image.get_pixel(x, y);
            let r = p[0] as i32;
            let g = p[1] as i32;
            let b = p[2] as i32;
            if b > r + 20 && b > g + 10 {
                blueish += 1;
            }
            total += 1;
        }
    }
    if total == 0 {
        return "unknown";
    }
    if (blueish as f64) / (total as f64) >= 0.35 {
        "on"
    } else {
        "off"
    }
}

fn normalize_snapshot_texts_to_display(
    texts: &[desktop_core::protocol::SnapshotText],
    image_width: u32,
    image_height: u32,
    display_width: u32,
    display_height: u32,
) -> Vec<desktop_core::protocol::SnapshotText> {
    if image_width == 0 || image_height == 0 || display_width == 0 || display_height == 0 {
        return texts.to_vec();
    }
    let sx = image_width as f64 / display_width as f64;
    let sy = image_height as f64 / display_height as f64;
    if (sx - 1.0).abs() < 0.0001 && (sy - 1.0).abs() < 0.0001 {
        return texts.to_vec();
    }
    texts
        .iter()
        .cloned()
        .map(|mut text| {
            text.bounds = desktop_core::protocol::Bounds {
                x: (text.bounds.x / sx).max(0.0),
                y: (text.bounds.y / sy).max(0.0),
                width: (text.bounds.width / sx).max(0.0),
                height: (text.bounds.height / sy).max(0.0),
            };
            text
        })
        .collect()
}

fn filter_texts_to_window(
    texts: &[desktop_core::protocol::SnapshotText],
    window_bounds: &desktop_core::protocol::Bounds,
) -> Vec<desktop_core::protocol::SnapshotText> {
    texts
        .iter()
        .filter(|text| {
            let cx = text.bounds.x + text.bounds.width / 2.0;
            let cy = text.bounds.y + text.bounds.height / 2.0;
            cx >= window_bounds.x
                && cx <= window_bounds.x + window_bounds.width
                && cy >= window_bounds.y
                && cy <= window_bounds.y + window_bounds.height
        })
        .cloned()
        .collect()
}

fn filter_texts_to_window_progressive(
    texts: &[desktop_core::protocol::SnapshotText],
    window_bounds: &desktop_core::protocol::Bounds,
) -> Vec<desktop_core::protocol::SnapshotText> {
    const PAD_LEVELS: [f64; 4] = [4.0, 40.0, 96.0, 180.0];
    for pad in PAD_LEVELS {
        let filtered = filter_texts_to_window(texts, &inflate_bounds(window_bounds, pad));
        if !filtered.is_empty() {
            trace::log(format!(
                "ui_click_text:window_filter pad={pad:.1} hits={}",
                filtered.len()
            ));
            return filtered;
        }
    }
    Vec::new()
}

fn click_scope_window_bounds() -> Option<desktop_core::protocol::Bounds> {
    #[cfg(target_os = "macos")]
    {
        if overlay::is_active() {
            if let Some(bounds) = overlay::tracked_window_bounds() {
                trace::log(format!(
                    "ui_click_text:window_scope source=overlay bounds=({:.1},{:.1},{:.1},{:.1})",
                    bounds.x, bounds.y, bounds.width, bounds.height
                ));
                return Some(bounds);
            }
        }
    }
    let bounds = frontmost_window_bounds();
    if let Some(b) = bounds.as_ref() {
        trace::log(format!(
            "ui_click_text:window_scope source=frontmost bounds=({:.1},{:.1},{:.1},{:.1})",
            b.x, b.y, b.width, b.height
        ));
    } else {
        trace::log("ui_click_text:window_scope source=none");
    }
    bounds
}

fn main_display_bounds() -> Option<desktop_core::protocol::Bounds> {
    platform::windowing::main_display_bounds()
}

fn frontmost_window_context() -> Option<platform::windowing::FrontmostWindowContext> {
    platform::windowing::frontmost_window_context()
}

fn frontmost_window_bounds() -> Option<desktop_core::protocol::Bounds> {
    let context = frontmost_window_context();
    let direct = context.as_ref().and_then(|ctx| ctx.bounds.clone());
    if let Some(direct) = direct.as_ref() {
        if !is_tiny_window_bounds(direct) {
            return Some(direct.clone());
        }
    }

    let app_hint = context.as_ref().and_then(|ctx| ctx.app.as_deref());
    let listed = list_frontmost_app_windows()
        .ok()
        .and_then(|windows| {
            preferred_window_for_capture(&windows, app_hint).map(|window| window.bounds.clone())
        })
        .or_else(|| {
            list_windows().ok().and_then(|windows| {
                preferred_window_for_capture(&windows, app_hint).map(|window| window.bounds.clone())
            })
        });

    match (direct, listed) {
        (Some(direct), Some(listed))
            if is_tiny_window_bounds(&direct) && !is_tiny_window_bounds(&listed) =>
        {
            trace::log(format!(
                "frontmost_window_bounds:replace_tiny_direct direct=({:.1},{:.1},{:.1},{:.1}) listed=({:.1},{:.1},{:.1},{:.1})",
                direct.x,
                direct.y,
                direct.width,
                direct.height,
                listed.x,
                listed.y,
                listed.width,
                listed.height
            ));
            Some(listed)
        }
        (Some(direct), Some(listed))
            if !is_tiny_window_bounds(&listed)
                && window_area(&listed) > window_area(&direct).saturating_mul(4) =>
        {
            trace::log(format!(
                "frontmost_window_bounds:replace_small_direct direct=({:.1},{:.1},{:.1},{:.1}) listed=({:.1},{:.1},{:.1},{:.1})",
                direct.x,
                direct.y,
                direct.width,
                direct.height,
                listed.x,
                listed.y,
                listed.width,
                listed.height
            ));
            Some(listed)
        }
        (Some(direct), _) => Some(direct),
        (None, Some(listed)) => Some(listed),
        (None, None) => None,
    }
}

fn frontmost_app_name() -> Option<String> {
    frontmost_window_context().and_then(|ctx| ctx.app)
}

fn window_area(bounds: &desktop_core::protocol::Bounds) -> u64 {
    let area = bounds.width.max(0.0) * bounds.height.max(0.0);
    area.round().max(0.0) as u64
}

fn is_tiny_window_bounds(bounds: &desktop_core::protocol::Bounds) -> bool {
    bounds.width < 120.0 || bounds.height < 90.0 || window_area(bounds) < 30_000
}

fn preferred_window_for_capture<'a>(
    windows: &'a [WindowInfo],
    app_hint: Option<&str>,
) -> Option<&'a WindowInfo> {
    let eligible = |window: &&WindowInfo| {
        window.visible && window.bounds.width > 8.0 && window.bounds.height > 8.0
    };
    let app_matches = |window: &&WindowInfo| match app_hint {
        Some(app) => window.app.eq_ignore_ascii_case(app),
        None => true,
    };

    windows
        .iter()
        .filter(|window| eligible(window) && window.frontmost && app_matches(window))
        .max_by_key(|window| window_area(&window.bounds))
        .or_else(|| {
            windows
                .iter()
                .filter(|window| eligible(window) && app_matches(window))
                .max_by_key(|window| window_area(&window.bounds))
        })
        .or_else(|| {
            windows
                .iter()
                .filter(|window| eligible(window) && window.frontmost)
                .max_by_key(|window| window_area(&window.bounds))
        })
        .or_else(|| {
            windows
                .iter()
                .filter(eligible)
                .max_by_key(|window| window_area(&window.bounds))
        })
}

fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    platform::windowing::list_windows()
}

fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    platform::windowing::list_frontmost_app_windows()
}

fn resolve_tokenize_window_target(
    windows: &[WindowInfo],
    query: Option<&str>,
) -> Result<WindowInfo, AppError> {
    if windows.is_empty() {
        return Err(AppError::target_not_found(
            "no windows available for screen tokenize",
        ));
    }

    if let Some(query) = query {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(AppError::invalid_argument(
                "window id must not be empty for screen tokenize",
            ));
        }
        if let Some(found) = windows.iter().find(|w| w.id == trimmed) {
            return Ok(found.clone());
        }
        let selected = select_window_candidate(windows, trimmed)?;
        return Ok(selected.clone());
    }

    // Filter out tiny transient windows (tooltips/popovers) that destabilize tokenization.
    windows
        .iter()
        .find(|w| w.frontmost && w.visible && w.bounds.width > 8.0 && w.bounds.height > 8.0)
        .or_else(|| {
            windows
                .iter()
                .find(|w| w.visible && w.bounds.width > 8.0 && w.bounds.height > 8.0)
        })
        .cloned()
        .ok_or_else(|| {
            AppError::target_not_found("no visible window available for screen tokenize")
        })
}

fn select_window_candidate<'a>(
    windows: &'a [WindowInfo],
    query: &str,
) -> Result<&'a WindowInfo, AppError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(AppError::invalid_argument("window title must not be empty"));
    }

    let lower = query.to_lowercase();

    let exact_title: Vec<&WindowInfo> = windows
        .iter()
        .filter(|w| w.title.eq_ignore_ascii_case(query))
        .collect();
    if exact_title.len() == 1 {
        return Ok(exact_title[0]);
    }
    if exact_title.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple windows matched title \"{query}\""
        ))
        .with_details(json!({
            "query": query,
            "candidates": exact_title.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
        })));
    }

    let exact_app: Vec<&WindowInfo> = windows
        .iter()
        .filter(|w| w.app.eq_ignore_ascii_case(query))
        .collect();
    if exact_app.len() == 1 {
        return Ok(exact_app[0]);
    }
    if exact_app.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple windows matched app \"{query}\""
        ))
        .with_details(json!({
            "query": query,
            "candidates": exact_app.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
        })));
    }

    let partial: Vec<&WindowInfo> = windows
        .iter()
        .filter(|w| {
            w.title.to_lowercase().contains(&lower) || w.app.to_lowercase().contains(&lower)
        })
        .collect();
    if partial.len() == 1 {
        return Ok(partial[0]);
    }
    if partial.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple windows partially matched \"{query}\""
        ))
        .with_details(json!({
            "query": query,
            "candidates": partial.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
        })));
    }

    Err(AppError::target_not_found(format!(
        "window \"{query}\" was not found"
    )))
}

#[cfg(test)]
mod tests {
    use super::execute;
    use desktop_core::{
        error::ErrorCode,
        protocol::{
            Bounds, RequestEnvelope, ResponseEnvelope, SnapshotText, TokenizeElement,
            TokenizeImage, TokenizePayload, TokenizeWindow,
        },
    };
    use image::{Rgba, RgbaImage};

    #[test]
    fn ping_returns_message() {
        let result = execute(desktop_core::protocol::Command::Ping).expect("ping");
        assert_eq!(result["message"], "pong");
    }

    fn test_window(pid: i64, index: u32, app: &str, title: &str) -> super::WindowInfo {
        super::WindowInfo {
            id: format!("{pid}:{index}"),
            pid,
            index,
            app: app.to_string(),
            title: title.to_string(),
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            frontmost: false,
            visible: true,
        }
    }

    #[test]
    fn select_window_prefers_exact_title() {
        let windows = vec![
            test_window(10, 1, "TextEdit", "Document 1"),
            test_window(11, 1, "Calculator", "Calculator"),
        ];
        let selected = super::select_window_candidate(&windows, "Calculator").expect("selected");
        assert_eq!(selected.app, "Calculator");
        assert_eq!(selected.title, "Calculator");
    }

    #[test]
    fn select_window_reports_ambiguous_app_matches() {
        let windows = vec![
            test_window(20, 1, "Safari", "Tab A"),
            test_window(20, 2, "Safari", "Tab B"),
        ];
        let err =
            super::select_window_candidate(&windows, "Safari").expect_err("must be ambiguous");
        assert_eq!(err.code, ErrorCode::AmbiguousTarget);
    }

    #[test]
    fn tokenize_target_prefers_explicit_window_id() {
        let mut windows = vec![
            test_window(20, 1, "Safari", "Tab A"),
            test_window(22, 2, "Calculator", "Calculator"),
        ];
        windows[0].frontmost = true;
        let selected =
            super::resolve_tokenize_window_target(&windows, Some("22:2")).expect("selected");
        assert_eq!(selected.id, "22:2");
        assert_eq!(selected.app, "Calculator");
    }

    #[test]
    fn tokenize_target_defaults_to_frontmost_visible() {
        let mut windows = vec![
            test_window(20, 1, "Safari", "Tab A"),
            test_window(22, 2, "Calculator", "Calculator"),
        ];
        windows[1].frontmost = true;
        let selected =
            super::resolve_tokenize_window_target(&windows, None).expect("selected frontmost");
        assert_eq!(selected.id, "22:2");
    }

    #[test]
    fn tokenize_region_resolves_inside_target_bounds() {
        let base = Bounds {
            x: 100.0,
            y: 200.0,
            width: 800.0,
            height: 600.0,
        };
        let region = Bounds {
            x: 50.0,
            y: 60.0,
            width: 300.0,
            height: 250.0,
        };
        let resolved = super::resolve_tokenize_region_bounds(base.clone(), Some(&region))
            .expect("region should resolve");
        assert_eq!(resolved.x, 150.0);
        assert_eq!(resolved.y, 260.0);
        assert_eq!(resolved.width, 300.0);
        assert_eq!(resolved.height, 250.0);
    }

    #[test]
    fn tokenize_region_rejects_outside_target_bounds() {
        let base = Bounds {
            x: 100.0,
            y: 200.0,
            width: 320.0,
            height: 240.0,
        };
        let region = Bounds {
            x: 200.0,
            y: 120.0,
            width: 200.0,
            height: 200.0,
        };
        let err = super::resolve_tokenize_region_bounds(base, Some(&region))
            .expect_err("region should fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn tokenize_region_without_override_uses_base_bounds() {
        let base = Bounds {
            x: 10.0,
            y: 20.0,
            width: 300.0,
            height: 200.0,
        };
        let resolved = super::resolve_tokenize_region_bounds(base.clone(), None)
            .expect("region should default to base");
        assert_eq!(resolved.x, base.x);
        assert_eq!(resolved.y, base.y);
        assert_eq!(resolved.width, base.width);
        assert_eq!(resolved.height, base.height);
    }

    #[test]
    fn screenshot_region_resolves_inside_target_bounds() {
        let base = Bounds {
            x: 100.0,
            y: 200.0,
            width: 800.0,
            height: 600.0,
        };
        let region = Bounds {
            x: 50.0,
            y: 60.0,
            width: 300.0,
            height: 250.0,
        };
        let resolved = super::resolve_capture_region_bounds(base.clone(), Some(&region))
            .expect("region should resolve");
        assert_eq!(resolved.x, 150.0);
        assert_eq!(resolved.y, 260.0);
        assert_eq!(resolved.width, 300.0);
        assert_eq!(resolved.height, 250.0);
    }

    #[test]
    fn screenshot_region_rejects_outside_target_bounds() {
        let base = Bounds {
            x: 100.0,
            y: 200.0,
            width: 320.0,
            height: 240.0,
        };
        let region = Bounds {
            x: 200.0,
            y: 120.0,
            width: 200.0,
            height: 200.0,
        };
        let err = super::resolve_capture_region_bounds(base, Some(&region))
            .expect_err("region should fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
        assert!(err.message.contains("screenshot --region"));
    }

    #[test]
    fn tokenize_payload_texts_maps_ax_element_bounds_to_display() {
        let payload = TokenizePayload {
            snapshot_id: 1,
            timestamp: "1".to_string(),
            image: Some(TokenizeImage {
                path: "<memory>".to_string(),
                width: 200,
                height: 100,
            }),
            windows: vec![TokenizeWindow {
                id: "frontmost:1".to_string(),
                title: "Calculator".to_string(),
                app: Some("Calculator".to_string()),
                bounds: Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: 200.0,
                    height: 100.0,
                },
                os_bounds: Some(Bounds {
                    x: 500.0,
                    y: 300.0,
                    width: 400.0,
                    height: 200.0,
                }),
                elements: vec![TokenizeElement {
                    id: "text_0001".to_string(),
                    kind: "".to_string(),
                    bbox: [20.0, 30.0, 50.0, 20.0],
                    has_border: None,
                    text: Some("7".to_string()),
                    confidence: None,
                    source: "accessibility_ax:AXButton".to_string(),
                }],
            }],
        };

        let texts = super::tokenize_payload_texts_for_click(&payload);
        assert_eq!(texts.len(), 1);
        let t = &texts[0];
        assert_eq!(t.text, "7");
        // x scale = 400/200 = 2.0 ; y scale = 200/100 = 2.0
        assert!((t.bounds.x - 540.0).abs() < 0.001);
        assert!((t.bounds.y - 360.0).abs() < 0.001);
        assert!((t.bounds.width - 100.0).abs() < 0.001);
        assert!((t.bounds.height - 40.0).abs() < 0.001);
        assert!((t.confidence - 0.92).abs() < 0.0001);
    }

    #[test]
    fn tokenize_payload_elements_maps_ids_and_bounds_to_display() {
        let payload = TokenizePayload {
            snapshot_id: 1,
            timestamp: "1".to_string(),
            image: Some(TokenizeImage {
                path: "<memory>".to_string(),
                width: 100,
                height: 100,
            }),
            windows: vec![TokenizeWindow {
                id: "frontmost:1".to_string(),
                title: "Calculator".to_string(),
                app: Some("Calculator".to_string()),
                bounds: Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: 100.0,
                    height: 100.0,
                },
                os_bounds: Some(Bounds {
                    x: 800.0,
                    y: 200.0,
                    width: 200.0,
                    height: 200.0,
                }),
                elements: vec![TokenizeElement {
                    id: "button_7".to_string(),
                    kind: "".to_string(),
                    bbox: [10.0, 20.0, 40.0, 30.0],
                    has_border: None,
                    text: Some("7".to_string()),
                    confidence: Some(0.9),
                    source: "accessibility_ax:AXButton".to_string(),
                }],
            }],
        };

        let elements = super::tokenize_payload_elements_for_click(&payload);
        assert_eq!(elements.len(), 1);
        let el = &elements[0];
        assert_eq!(el.id, "button_7");
        assert_eq!(el.text.as_deref(), Some("7"));
        assert_eq!(el.source, "accessibility_ax:AXButton");
        // x/y scale = 2.0
        assert!((el.bounds.x - 820.0).abs() < 0.001);
        assert!((el.bounds.y - 240.0).abs() < 0.001);
        assert!((el.bounds.width - 80.0).abs() < 0.001);
        assert!((el.bounds.height - 60.0).abs() < 0.001);
    }

    #[test]
    fn tokenize_payload_contract_omits_legacy_tokens_field() {
        let payload = TokenizePayload {
            snapshot_id: 99,
            timestamp: "99".to_string(),
            image: Some(TokenizeImage {
                path: "<memory>".to_string(),
                width: 200,
                height: 100,
            }),
            windows: vec![TokenizeWindow {
                id: "frontmost:1".to_string(),
                title: "Notes".to_string(),
                app: Some("Notes".to_string()),
                bounds: Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: 200.0,
                    height: 100.0,
                },
                os_bounds: None,
                elements: vec![TokenizeElement {
                    id: "text_note".to_string(),
                    kind: "text".to_string(),
                    bbox: [10.0, 10.0, 100.0, 30.0],
                    has_border: None,
                    text: Some("Hello".to_string()),
                    confidence: Some(0.99),
                    source: "vision_ocr".to_string(),
                }],
            }],
        };
        let value = serde_json::to_value(&payload).expect("serialize payload");
        assert!(value.get("tokens").is_none(), "tokens field must be absent");
    }

    #[test]
    fn error_roundtrip_shape() {
        let req = RequestEnvelope::new(
            "r1".to_string(),
            desktop_core::protocol::Command::ReplayLoad {
                session_dir: "/tmp/missing".to_string(),
            },
        );
        let response = match execute(req.command.clone()) {
            Ok(v) => ResponseEnvelope::success(req.request_id, v),
            Err(err) => ResponseEnvelope::from_error(req.request_id, req.command.name(), err),
        };
        let bytes = serde_json::to_vec(&response).expect("encode");
        let decoded: ResponseEnvelope = serde_json::from_slice(&bytes).expect("decode");
        match decoded {
            ResponseEnvelope::Error(err) => assert_eq!(err.error.code, ErrorCode::InvalidArgument),
            ResponseEnvelope::Success(_) => panic!("expected error response"),
        }
    }

    #[test]
    fn on_demand_config_has_idle_timeout() {
        let cfg = super::DaemonConfig::on_demand();
        assert_eq!(cfg.idle_timeout.map(|d| d.as_secs()), Some(8));
    }

    #[test]
    fn select_text_candidate_returns_not_found() {
        let result = super::select_text_candidate(&[], "Send");
        assert_eq!(
            result.expect_err("should fail").code,
            ErrorCode::TargetNotFound
        );
    }

    #[test]
    fn select_text_candidate_returns_ambiguous() {
        let texts = vec![
            SnapshotText {
                text: "Send".to_string(),
                bounds: Bounds {
                    x: 10.0,
                    y: 10.0,
                    width: 40.0,
                    height: 16.0,
                },
                confidence: 0.8,
            },
            SnapshotText {
                text: "Send".to_string(),
                bounds: Bounds {
                    x: 90.0,
                    y: 10.0,
                    width: 40.0,
                    height: 16.0,
                },
                confidence: 0.79,
            },
        ];
        let result = super::select_text_candidate(&texts, "Send");
        assert_eq!(
            result.expect_err("should fail").code,
            ErrorCode::AmbiguousTarget
        );
    }

    #[test]
    fn ranked_text_candidates_filters_long_noise_lines() {
        let texts = vec![
            SnapshotText {
                text: r#"./dist/desktopctl pointer click --text "New Document""#.to_string(),
                bounds: Bounds {
                    x: 250.0,
                    y: 40.0,
                    width: 650.0,
                    height: 16.0,
                },
                confidence: 0.6,
            },
            SnapshotText {
                text: "New Document".to_string(),
                bounds: Bounds {
                    x: 500.0,
                    y: 560.0,
                    width: 96.0,
                    height: 18.0,
                },
                confidence: 0.5,
            },
        ];
        let ranked =
            super::ranked_text_candidates(&texts, "New Document").expect("ranked candidates");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].1.text, "New Document");
    }

    #[test]
    fn ranked_text_candidates_match_confusable_characters() {
        let texts = vec![SnapshotText {
            text: "DesktopCtI".to_string(),
            bounds: Bounds {
                x: 500.0,
                y: 560.0,
                width: 96.0,
                height: 18.0,
            },
            confidence: 0.86,
        }];
        let ranked = super::ranked_text_candidates(&texts, "DesktopCtl")
            .expect("ranked confusable candidates");
        assert_eq!(ranked.len(), 1);
        assert_eq!(ranked[0].1.text, "DesktopCtI");
    }

    #[test]
    fn select_text_candidate_prefers_exact_over_confusable_match() {
        let texts = vec![
            SnapshotText {
                text: "DesktopCtI".to_string(),
                bounds: Bounds {
                    x: 500.0,
                    y: 560.0,
                    width: 96.0,
                    height: 18.0,
                },
                confidence: 0.9,
            },
            SnapshotText {
                text: "DesktopCtl".to_string(),
                bounds: Bounds {
                    x: 500.0,
                    y: 590.0,
                    width: 96.0,
                    height: 18.0,
                },
                confidence: 0.9,
            },
        ];
        let selected =
            super::select_text_candidate(&texts, "DesktopCtl").expect("select exact match");
        assert_eq!(selected.text, "DesktopCtl");
    }

    #[test]
    fn infer_panels_splits_left_and_right_clusters() {
        let texts = vec![
            SnapshotText {
                text: "Accessibility".to_string(),
                bounds: Bounds {
                    x: 340.0,
                    y: 350.0,
                    width: 90.0,
                    height: 14.0,
                },
                confidence: 0.9,
            },
            SnapshotText {
                text: "Screen & System Audio Recording".to_string(),
                bounds: Bounds {
                    x: 570.0,
                    y: 100.0,
                    width: 260.0,
                    height: 20.0,
                },
                confidence: 0.9,
            },
        ];
        let panels = super::infer_panels_from_texts(&texts);
        assert!(panels.len() >= 2);
    }

    #[test]
    fn logical_bounds_map_to_retina_image_rect() {
        let rect = super::logical_bounds_to_image_rect(
            &Bounds {
                x: 100.0,
                y: 50.0,
                width: 24.0,
                height: 10.0,
            },
            2940,
            1912,
            1470,
            956,
        )
        .expect("mapped rect");
        assert_eq!(rect, (200, 100, 248, 120));
    }

    #[test]
    fn logical_point_maps_to_retina_pixels() {
        let mapped = super::logical_point_to_image_point(355, 268, 2940, 1912, 1470, 956)
            .expect("point should map");
        assert_eq!(mapped, (710, 536));
    }

    #[test]
    fn overlay_path_uses_overlay_suffix() {
        let path = std::path::Path::new("/tmp/capture.png");
        let overlay = super::overlay_path_for_capture(path);
        assert_eq!(
            overlay,
            std::path::PathBuf::from("/tmp/capture.overlay.png")
        );
    }

    #[test]
    fn toggle_state_sampling_respects_retina_mapping() {
        let mut img = RgbaImage::from_pixel(2940, 1912, Rgba([160, 160, 160, 255]));
        for y in 100..120 {
            for x in 200..248 {
                img.put_pixel(x, y, Rgba([40, 110, 220, 255]));
            }
        }
        let state = super::estimate_toggle_state(
            Some(&img),
            &Bounds {
                x: 100.0,
                y: 50.0,
                width: 24.0,
                height: 10.0,
            },
            1470,
            956,
        );
        assert_eq!(state, "on");
    }
}

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
    protocol::{
        Command, RequestEnvelope, ResponseEnvelope, SnapshotDisplay, SnapshotPayload, now_millis,
    },
};
use image::{ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};

#[cfg(target_os = "macos")]
use crate::overlay;
use crate::{clipboard, permissions, recording, replay, trace, vision};

mod settings_flow;

use self::settings_flow::*;

#[cfg(target_os = "macos")]
const OVERLAY_WATCH_TRACK_INTERVAL_MS: u64 = 120;
#[cfg(target_os = "macos")]
const OVERLAY_SCREEN_CAPTURE_MODE_LOCK_MS: u64 = 2_000;
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
    match overlay::start_overlay() {
        Ok(started) => trace::log(format!("overlay:bootstrap start started={started}")),
        Err(err) => trace::log(format!("overlay:bootstrap start_warn {err}")),
    }
    start_overlay_watch_tracker();
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
    if let Err(err) = recording::record_command(&request, &response) {
        eprintln!("recorder write failed: {err}");
        trace::log(format!("client:record_err {err}"));
    }
    trace::log("client:write_response_begin");
    write_framed_json(&mut stream, &response)?;
    trace::log("client:write_response_ok");
    Ok(())
}

fn execute(command: Command) -> Result<Value, AppError> {
    match command {
        Command::Ping => Ok(json!({ "message": "pong" })),
        Command::AppHide { name } => {
            trace::log(format!("app_hide:start name={name}"));
            let state = hide_application(&name)?;
            trace::log(format!("app_hide:ok name={name} state={state}"));
            Ok(json!({ "app": name, "state": state }))
        }
        Command::AppShow { name } => {
            trace::log(format!("app_show:start name={name}"));
            show_application(&name)?;
            trace::log(format!("app_show:ok name={name}"));
            Ok(json!({ "app": name, "state": "shown" }))
        }
        Command::AppIsolate { name } => {
            trace::log(format!("app_isolate:start name={name}"));
            let hidden = isolate_application(&name)?;
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
            focus_window_candidate(selected)?;
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
        Command::OpenSpotlight => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.press_hotkey("cmd+space")?;
            Ok(json!({}))
        }
        Command::OpenLaunchpad => {
            let output = ProcessCommand::new("open")
                .args(["-a", "Launchpad"])
                .output()
                .map_err(|err| {
                    AppError::backend_unavailable(format!("failed to invoke open: {err}"))
                })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(AppError::internal(stderr));
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
        Command::PointerClick { x, y } => {
            trace::log(format!("pointer_click:start x={x} y={y}"));
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = Point::new(x, y);
            backend.move_mouse(point)?;
            backend.left_click(point)?;
            trace::log(format!("pointer_click:ok x={x} y={y}"));
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
        Command::Wait { ms } => {
            let backend = new_backend()?;
            backend.sleep_ms(ms);
            Ok(json!({}))
        }
        Command::ScreenCapture {
            out_path,
            overlay,
            active_window,
        } => {
            trace::log("execute:screen_capture:start");
            permissions::ensure_screen_recording_permission()?;
            let window_bounds = if active_window {
                Some(frontmost_window_bounds().ok_or_else(|| {
                    AppError::target_not_found(
                        "frontmost window not found; ensure a standard app window is focused",
                    )
                })?)
            } else {
                None
            };
            let capture_out_path: Option<PathBuf> = out_path
                .map(Into::into)
                .or_else(|| Some(vision::capture::default_capture_path()));
            let capture = if let Some(bounds) = window_bounds.clone() {
                vision::pipeline::capture_and_update_active_window(
                    capture_out_path.clone(),
                    bounds,
                    None,
                    true,
                )?
            } else {
                vision::pipeline::capture_and_update(capture_out_path)?
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
                "capture_scope": if active_window { "active_window" } else { "display" },
                "window_bounds": window_bounds,
                "display": capture.snapshot.display,
                "focused_app": capture.snapshot.focused_app,
                "event_ids": capture.event_ids
            }))
        }
        Command::ScreenSnapshot { screenshot_path } => {
            trace::log("execute:screen_snapshot:start");
            if let Some(path) = screenshot_path {
                let path = PathBuf::from(path);
                if !path.exists() {
                    return Err(AppError::invalid_argument(format!(
                        "screenshot file does not exist: {}",
                        path.display()
                    )));
                }
                let image = image::open(&path).map_err(|err| {
                    AppError::invalid_argument(format!(
                        "failed to open screenshot {}: {err}",
                        path.display()
                    ))
                })?;
                let width = image.width();
                let height = image.height();
                let texts = vision::ocr::recognize_text_from_image(&path, width, height)?;
                let snapshot = SnapshotPayload {
                    snapshot_id: now_millis() as u64,
                    timestamp: now_millis().to_string(),
                    display: SnapshotDisplay {
                        id: 1,
                        width,
                        height,
                        scale: 1.0,
                    },
                    focused_app: None,
                    texts,
                };
                trace::log(format!(
                    "execute:screen_snapshot:from_screenshot path={} snapshot_id={} texts={}",
                    path.display(),
                    snapshot.snapshot_id,
                    snapshot.texts.len()
                ));
                Ok(serde_json::to_value(snapshot).map_err(|err| {
                    AppError::internal(format!("failed to encode snapshot: {err}"))
                })?)
            } else {
                if let Some(snapshot) = vision::pipeline::latest_snapshot()? {
                    trace::log(format!(
                        "execute:screen_snapshot:cache_hit snapshot_id={}",
                        snapshot.snapshot_id
                    ));
                    Ok(serde_json::to_value(snapshot).map_err(|err| {
                        AppError::internal(format!("failed to encode snapshot: {err}"))
                    })?)
                } else {
                    trace::log("execute:screen_snapshot:cache_miss");
                    Err(AppError::target_not_found(
                        "no snapshot available; run `desktopctl screen capture` first",
                    ))
                }
            }
        }
        Command::ScreenTokenize {
            overlay_out_path,
            window_id,
            screenshot_path,
        } => {
            trace::log("execute:screen_tokenize:start");
            let payload = if let Some(path_raw) = screenshot_path {
                if window_id.is_some() {
                    return Err(AppError::invalid_argument(
                        "--window cannot be combined with --screenshot for screen tokenize",
                    ));
                }
                let screenshot = PathBuf::from(path_raw);
                if !screenshot.exists() {
                    return Err(AppError::invalid_argument(format!(
                        "screenshot file does not exist: {}",
                        screenshot.display()
                    )));
                }
                vision::pipeline::tokenize_screenshot(&screenshot, None)?
            } else {
                permissions::ensure_screen_recording_permission()?;
                let backend = new_backend()?;
                backend.check_accessibility_permission()?;
                if window_id.is_none() {
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
                        let window_meta = vision::pipeline::TokenizeWindowMeta {
                            id: "frontmost:1".to_string(),
                            title: "active_window".to_string(),
                            app: None,
                            bounds,
                        };
                        vision::pipeline::tokenize_window(window_meta)?
                    } else if let Some(bounds) = frontmost_window_bounds() {
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
                        let window_meta = vision::pipeline::TokenizeWindowMeta {
                            id: target.id.clone(),
                            title: target.title.clone(),
                            app: Some(target.app.clone()),
                            bounds: target.bounds.clone(),
                        };
                        vision::pipeline::tokenize_window(window_meta)?
                    }
                } else {
                    let windows = list_windows()?;
                    let target = resolve_tokenize_window_target(&windows, window_id.as_deref())?;
                    let window_meta = vision::pipeline::TokenizeWindowMeta {
                        id: target.id.clone(),
                        title: target.title.clone(),
                        app: Some(target.app.clone()),
                        bounds: target.bounds.clone(),
                    };
                    vision::pipeline::tokenize_window(window_meta)?
                }
            };
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
            trace::log(format!(
                "execute:screen_tokenize:ok snapshot_id={} tokens={}",
                payload.snapshot_id,
                payload.tokens.len()
            ));
            Ok(serde_json::to_value(payload).map_err(|err| {
                AppError::internal(format!("failed to encode token payload: {err}"))
            })?)
        }
        Command::ScreenFindText { text, all } => {
            permissions::ensure_screen_recording_permission()?;
            find_text_targets(&text, all)
        }
        Command::ScreenLayout => {
            permissions::ensure_screen_recording_permission()?;
            screen_layout_summary()
        }
        Command::ScreenSettingsMap => {
            permissions::ensure_screen_recording_permission()?;
            screen_settings_map()
        }
        Command::OverlayStart => {
            #[cfg(target_os = "macos")]
            {
                let started = overlay::start_overlay()?;
                return Ok(json!({
                    "overlay_running": true,
                    "started": started
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
        } => wait_for_text(&text, timeout_ms, interval_ms),
        Command::UiClickText { text, timeout_ms } => click_text_target(&text, timeout_ms),
        Command::UiClickTextOffset {
            text,
            dx,
            dy,
            timeout_ms,
        } => click_text_offset_target(&text, dx, dy, timeout_ms),
        Command::UiClickSettingsAdd => click_settings_control("add", None, 800),
        Command::UiClickSettingsRemove => click_settings_control("remove", None, 800),
        Command::UiClickSettingsToggle { text, timeout_ms } => {
            click_settings_control("toggle", Some(&text), timeout_ms)
        }
        Command::UiSettingsEnsureEnabled { text, timeout_ms } => {
            settings_ensure_enabled(&text, timeout_ms)
        }
        Command::UiSettingsUnlock {
            password,
            timeout_ms,
        } => settings_unlock(&password, timeout_ms),
        Command::UiClickToken { token } => click_token_target(token),
        Command::ClipboardRead => {
            let text = clipboard::read_clipboard()?;
            Ok(json!({ "text": text }))
        }
        Command::ClipboardWrite { text } => {
            clipboard::write_clipboard(&text)?;
            Ok(json!({ "written": true }))
        }
        Command::UiRead => ui_read_with_clipboard_restore(),
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
        Command::ReplayLoad { session_dir } => {
            let session_dir = replay::parse_session_dir(&session_dir)?;
            replay::load_session(&session_dir)
        }
    }
}

fn click_text_target(query: &str, timeout_ms: u64) -> Result<Value, AppError> {
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
    if window_bounds.is_some() && window_filtered.is_empty() {
        return Err(AppError::target_not_found(
            "no OCR text detected in frontmost window; cannot click target safely",
        ));
    }
    let target = select_text_candidate(&window_filtered, query)?;
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

    verify_click_postcondition(query, &target.bounds, timeout_ms.min(2_000).max(300))?;
    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "text": target.text,
        "bounds": target.bounds
    }))
}

fn click_text_offset_target(
    query: &str,
    dx: i32,
    dy: i32,
    timeout_ms: u64,
) -> Result<Value, AppError> {
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
    if window_bounds.is_some() && window_filtered.is_empty() {
        return Err(AppError::target_not_found(
            "no OCR text detected in frontmost window; cannot click target safely",
        ));
    }
    let target = select_text_candidate(&window_filtered, query)?;
    let base_x = (target.bounds.x + target.bounds.width / 2.0).round() as i64;
    let base_y = (target.bounds.y + target.bounds.height / 2.0).round() as i64;
    let click_x = (base_x + dx as i64).max(0) as u32;
    let click_y = (base_y + dy as i64).max(0) as u32;
    trace::log(format!(
        "ui_click_text_offset:selected query=\"{}\" target=\"{}\" base=({}, {}) offset=({}, {}) click=({}, {})",
        compact_for_log(query),
        compact_for_log(&target.text),
        base_x,
        base_y,
        dx,
        dy,
        click_x,
        click_y
    ));
    perform_click_at(click_x, click_y)?;
    // Offset-based clicks are often for unlabeled controls (+/-/toggles), so we don't enforce a strict text-disappears postcondition.
    thread::sleep(Duration::from_millis(timeout_ms.min(400).max(40)));
    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "anchor_text": target.text,
        "anchor_bounds": target.bounds,
        "offset": { "dx": dx, "dy": dy },
        "click_point": { "x": click_x, "y": click_y }
    }))
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

fn wait_for_text(query: &str, timeout_ms: u64, interval_ms: u64) -> Result<Value, AppError> {
    permissions::ensure_screen_recording_permission()?;
    let start = Instant::now();
    loop {
        let capture = vision::pipeline::capture_and_update(None)?;
        if let Ok(candidate) = select_text_candidate(&capture.snapshot.texts, query) {
            return Ok(json!({
                "snapshot_id": capture.snapshot.snapshot_id,
                "timestamp": capture.snapshot.timestamp,
                "matched_text": candidate.text,
                "bounds": candidate.bounds
            }));
        }
        if start.elapsed().as_millis() as u64 >= timeout_ms {
            return Err(
                AppError::timeout(format!("timed out waiting for text \"{query}\""))
                    .with_details(json!({ "timeout_ms": timeout_ms })),
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

    let image_regions = vision::regions::detect_settings_regions(&image);
    let regions_display = scale_regions_to_display(
        &image_regions,
        Some(image_width),
        Some(image_height),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
    );
    let heading = find_settings_heading(
        &capture.snapshot.texts,
        regions_display.content_bounds.as_ref(),
    )
    .or_else(|| find_settings_heading(&capture.snapshot.texts, None));
    let instruction = find_settings_instruction(&capture.snapshot.texts, heading.as_ref());
    let section_break = find_settings_section_break(
        &capture.snapshot.texts,
        heading.as_ref(),
        instruction.as_ref(),
    );
    let no_items = first_matching_text(&capture.snapshot.texts, "no items");
    let inferred_window_bounds = infer_window_bounds_from_content(
        regions_display.content_bounds.as_ref(),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
    )
    .or_else(|| regions_display.window_bounds.clone());
    if let Some(bounds) = inferred_window_bounds.as_ref() {
        draw_logical_bounds_on_image(
            &mut image,
            bounds,
            capture.snapshot.display.width,
            capture.snapshot.display.height,
            Rgba([245, 179, 34, 255]),
            2,
        );
    }
    if let Some(bounds) = regions_display.content_bounds.as_ref() {
        draw_logical_bounds_on_image(
            &mut image,
            bounds,
            capture.snapshot.display.width,
            capture.snapshot.display.height,
            Rgba([45, 199, 124, 255]),
            2,
        );
    }

    if let Some(heading) = heading.as_ref() {
        draw_logical_bounds_on_image(
            &mut image,
            &heading.bounds,
            capture.snapshot.display.width,
            capture.snapshot.display.height,
            Rgba([255, 211, 42, 255]),
            2,
        );
    }
    if let Some(no_items) = no_items.as_ref() {
        draw_logical_bounds_on_image(
            &mut image,
            &no_items.bounds,
            capture.snapshot.display.width,
            capture.snapshot.display.height,
            Rgba([94, 145, 252, 255]),
            2,
        );
    }
    if let Some(instruction) = instruction.as_ref() {
        draw_logical_bounds_on_image(
            &mut image,
            &instruction.bounds,
            capture.snapshot.display.width,
            capture.snapshot.display.height,
            Rgba([255, 161, 76, 255]),
            2,
        );
    }
    if let Some(section_break) = section_break.as_ref() {
        draw_logical_bounds_on_image(
            &mut image,
            &section_break.bounds,
            capture.snapshot.display.width,
            capture.snapshot.display.height,
            Rgba([255, 106, 106, 255]),
            2,
        );
    }

    let list_bounds = infer_list_bounds_from_anchors(
        heading.as_ref(),
        no_items.as_ref(),
        regions_display.content_bounds.as_ref(),
    );
    if let Some(bounds) = list_bounds.as_ref() {
        draw_logical_bounds_on_image(
            &mut image,
            bounds,
            capture.snapshot.display.width,
            capture.snapshot.display.height,
            Rgba([225, 132, 255, 255]),
            2,
        );
    }

    if let Some(controls) = infer_settings_controls_for_settings_pane(
        &capture.snapshot.texts,
        heading.as_ref(),
        no_items.as_ref(),
        instruction.as_ref(),
        section_break.as_ref(),
        list_bounds.as_ref(),
        regions_display.content_bounds.as_ref(),
        14.0,
    ) {
        if let Some(add_bounds) = controls
            .get("add_button_bounds")
            .and_then(bounds_tuple_from_value)
        {
            draw_logical_bounds_on_image(
                &mut image,
                &desktop_core::protocol::Bounds {
                    x: add_bounds.0,
                    y: add_bounds.1,
                    width: add_bounds.2,
                    height: add_bounds.3,
                },
                capture.snapshot.display.width,
                capture.snapshot.display.height,
                Rgba([90, 255, 90, 255]),
                2,
            );
        }
        if let Some(remove_bounds) = controls
            .get("remove_button_bounds")
            .and_then(bounds_tuple_from_value)
        {
            draw_logical_bounds_on_image(
                &mut image,
                &desktop_core::protocol::Bounds {
                    x: remove_bounds.0,
                    y: remove_bounds.1,
                    width: remove_bounds.2,
                    height: remove_bounds.3,
                },
                capture.snapshot.display.width,
                capture.snapshot.display.height,
                Rgba([255, 96, 96, 255]),
                2,
            );
        }
        if let Some(footer) = controls
            .get("footer_bounds")
            .and_then(bounds_tuple_from_value)
        {
            draw_logical_bounds_on_image(
                &mut image,
                &desktop_core::protocol::Bounds {
                    x: footer.0,
                    y: footer.1,
                    width: footer.2,
                    height: footer.3,
                },
                capture.snapshot.display.width,
                capture.snapshot.display.height,
                Rgba([255, 111, 97, 255]),
                2,
            );
        }
        if let Some((x, y)) = controls.get("add_click").and_then(point_from_value) {
            draw_logical_point_on_image(
                &mut image,
                x,
                y,
                capture.snapshot.display.width,
                capture.snapshot.display.height,
                Rgba([90, 255, 90, 255]),
            );
        }
        if let Some((x, y)) = controls.get("remove_click").and_then(point_from_value) {
            draw_logical_point_on_image(
                &mut image,
                x,
                y,
                capture.snapshot.display.width,
                capture.snapshot.display.height,
                Rgba([255, 96, 96, 255]),
            );
        }
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

fn draw_logical_point_on_image(
    image: &mut RgbaImage,
    x: u32,
    y: u32,
    display_width: u32,
    display_height: u32,
    color: Rgba<u8>,
) {
    let Some((ix, iy)) = logical_point_to_image_point(
        x,
        y,
        image.width(),
        image.height(),
        display_width,
        display_height,
    ) else {
        return;
    };
    let radius = 6_i32;
    for dx in -radius..=radius {
        let px = ix as i32 + dx;
        if px >= 0 && px < image.width() as i32 {
            image.put_pixel(px as u32, iy, color);
        }
    }
    for dy in -radius..=radius {
        let py = iy as i32 + dy;
        if py >= 0 && py < image.height() as i32 {
            image.put_pixel(ix, py as u32, color);
        }
    }
}

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

#[derive(Debug, Clone)]
struct FrontmostWindowContext {
    app: Option<String>,
    bounds: Option<desktop_core::protocol::Bounds>,
}

fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    let script = r#"tell application "System Events"
	set frontProc to first application process whose frontmost is true
	set appName to name of frontProc
	if (count of windows of frontProc) is 0 then
	    return appName
	end if
	set winPos to position of front window of frontProc
	set winSize to size of front window of frontProc
	return appName & tab & (item 1 of winPos as string) & tab & (item 2 of winPos as string) & tab & (item 1 of winSize as string) & tab & (item 2 of winSize as string)
	end tell"#;
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let parts: Vec<&str> = raw.split('\t').map(str::trim).collect();
    let app = parts
        .first()
        .map(|v| v.to_string())
        .filter(|v| !v.is_empty());
    let bounds = if parts.len() >= 5 {
        let parsed: Vec<f64> = parts[1..5]
            .iter()
            .filter_map(|v| v.parse::<f64>().ok())
            .collect();
        if parsed.len() == 4 {
            Some(desktop_core::protocol::Bounds {
                x: parsed[0].max(0.0),
                y: parsed[1].max(0.0),
                width: parsed[2].max(0.0),
                height: parsed[3].max(0.0),
            })
        } else {
            None
        }
    } else {
        None
    };
    Some(FrontmostWindowContext { app, bounds })
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

#[derive(Debug, Clone)]
struct WindowInfo {
    id: String,
    pid: i64,
    index: u32,
    app: String,
    title: String,
    bounds: desktop_core::protocol::Bounds,
    frontmost: bool,
    visible: bool,
}

impl WindowInfo {
    fn as_json(&self) -> Value {
        json!({
            "id": self.id,
            "pid": self.pid,
            "index": self.index,
            "app": self.app,
            "title": self.title,
            "bounds": self.bounds,
            "frontmost": self.frontmost,
            "visible": self.visible
        })
    }
}

fn parse_applescript_bool(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("true")
}

fn parse_window_line(line: &str) -> Option<WindowInfo> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() != 10 {
        return None;
    }

    let pid = fields[0].trim().parse::<i64>().ok()?;
    let index = fields[1].trim().parse::<u32>().ok()?;
    let app = fields[2].trim().to_string();
    let title = fields[3].trim().to_string();
    let x = fields[4].trim().parse::<f64>().ok()?;
    let y = fields[5].trim().parse::<f64>().ok()?;
    let width = fields[6].trim().parse::<f64>().ok()?;
    let height = fields[7].trim().parse::<f64>().ok()?;
    let frontmost = parse_applescript_bool(fields[8]);
    let visible = parse_applescript_bool(fields[9]);

    Some(WindowInfo {
        id: format!("{pid}:{index}"),
        pid,
        index,
        app,
        title,
        bounds: desktop_core::protocol::Bounds {
            x: x.max(0.0),
            y: y.max(0.0),
            width: width.max(0.0),
            height: height.max(0.0),
        },
        frontmost,
        visible,
    })
}

fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    let script = r#"tell application "System Events"
set resultRows to {}
repeat with p in (application processes whose background only is false)
    set pname to (name of p) as text
    set pfront to (frontmost of p) as string
    set pvisible to (visible of p) as string
    set ppid to unix id of p
    set widx to 0
    repeat with w in (windows of p)
        set widx to widx + 1
        try
            set wname to (name of w) as text
        on error
            set wname to ""
        end try
        try
            set winPos to position of w
            set winSize to size of w
            set wx to item 1 of winPos
            set wy to item 2 of winPos
            set ww to item 1 of winSize
            set wh to item 2 of winSize
            set end of resultRows to (ppid as string) & tab & (widx as string) & tab & pname & tab & wname & tab & (wx as string) & tab & (wy as string) & tab & (ww as string) & tab & (wh as string) & tab & pfront & tab & pvisible
        end try
    end repeat
end repeat
set AppleScript's text item delimiters to linefeed
set outputText to resultRows as text
set AppleScript's text item delimiters to ""
return outputText
end tell"#;

    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::backend_unavailable(format!(
            "failed to enumerate windows: {stderr}"
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let mut windows: Vec<WindowInfo> = raw.lines().filter_map(parse_window_line).collect();
    windows.sort_by(|a, b| {
        b.frontmost
            .cmp(&a.frontmost)
            .then_with(|| a.app.to_lowercase().cmp(&b.app.to_lowercase()))
            .then_with(|| a.index.cmp(&b.index))
    });
    Ok(windows)
}

fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    let script = r#"tell application "System Events"
set resultRows to {}
set frontProc to first application process whose frontmost is true
set pname to (name of frontProc) as text
set pvisible to (visible of frontProc) as string
set ppid to unix id of frontProc
set widx to 0
repeat with w in (windows of frontProc)
    set widx to widx + 1
    try
        set wname to (name of w) as text
    on error
        set wname to ""
    end try
    try
        set winPos to position of w
        set winSize to size of w
        set wx to item 1 of winPos
        set wy to item 2 of winPos
        set ww to item 1 of winSize
        set wh to item 2 of winSize
        set end of resultRows to (ppid as string) & tab & (widx as string) & tab & pname & tab & wname & tab & (wx as string) & tab & (wy as string) & tab & (ww as string) & tab & (wh as string) & tab & "true" & tab & pvisible
    end try
end repeat
set AppleScript's text item delimiters to linefeed
set outputText to resultRows as text
set AppleScript's text item delimiters to ""
return outputText
end tell"#;

    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::backend_unavailable(format!(
            "failed to enumerate frontmost app windows: {stderr}"
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let mut windows: Vec<WindowInfo> = raw.lines().filter_map(parse_window_line).collect();
    windows.sort_by_key(|window| std::cmp::Reverse(window_area(&window.bounds)));
    Ok(windows)
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

fn focus_window_candidate(window: &WindowInfo) -> Result<(), AppError> {
    let escaped_app = window.app.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
set targetPid to {pid}
set targetIndex to {index}
repeat with p in (application processes whose background only is false)
    if (unix id of p) is targetPid then
        set frontmost of p to true
        set idx to 0
        repeat with w in (windows of p)
            set idx to idx + 1
            if idx is targetIndex then
                try
                    perform action "AXRaise" of w
                end try
                exit repeat
            end if
        end repeat
        return "ok"
    end if
end repeat
return ""
end tell
tell application "{escaped_app}" to activate"#,
        pid = window.pid,
        index = window.index
    );
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::internal(format!(
            "failed to focus window \"{}\": {stderr}",
            window.title
        )));
    }
    Ok(())
}

fn ui_read_with_clipboard_restore() -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;

    let clipboard_before = clipboard::read_clipboard().ok();
    backend.press_hotkey("cmd+a")?;
    thread::sleep(Duration::from_millis(70));
    backend.press_hotkey("cmd+c")?;
    thread::sleep(Duration::from_millis(120));
    let captured = clipboard::read_clipboard()?;

    let mut restore_ok = true;
    if let Some(previous) = clipboard_before {
        if clipboard::write_clipboard(&previous).is_err() {
            restore_ok = false;
        }
    }

    Ok(json!({
        "text": captured,
        "clipboard_restored": restore_ok
    }))
}

fn hide_application(name: &str) -> Result<&'static str, AppError> {
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
if exists process "{escaped}" then
    set visible of process "{escaped}" to false
    return "hidden"
else
    return "not_running"
end if
end tell"#
    );
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::internal(format!(
            "failed to hide application \"{name}\": {stderr}"
        )));
    }

    let state = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if state == "hidden" {
        Ok("hidden")
    } else {
        Ok("not_running")
    }
}

fn show_application(name: &str) -> Result<(), AppError> {
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
if exists process "{escaped}" then
    set visible of process "{escaped}" to true
end if
end tell
tell application "{escaped}" to activate"#
    );
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::internal(format!(
            "failed to show application \"{name}\": {stderr}"
        )));
    }
    Ok(())
}

fn isolate_application(name: &str) -> Result<u32, AppError> {
    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
set targetName to "{escaped}"
set hiddenCount to 0
repeat with p in (application processes whose background only is false)
    set pname to (name of p) as text
    if pname is not targetName then
        try
            if visible of p then
                set visible of p to false
                set hiddenCount to hiddenCount + 1
            end if
        end try
    end if
end repeat
return hiddenCount as string
end tell
tell application "{escaped}" to activate"#
    );
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::internal(format!(
            "failed to isolate application \"{name}\": {stderr}"
        )));
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(value.parse::<u32>().unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use desktop_core::{
        error::ErrorCode,
        protocol::{Bounds, RequestEnvelope, ResponseEnvelope, SnapshotText},
    };
    use image::{Rgba, RgbaImage};
    use serde_json::json;

    use super::execute;

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
                text: r#"./dist/desktopctl ui click --text "New Document" --timeout 2000"#
                    .to_string(),
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
    fn infer_settings_rows_filters_noise() {
        let heading = SnapshotText {
            text: "Screen & System Audio Recording".to_string(),
            bounds: Bounds {
                x: 560.0,
                y: 100.0,
                width: 260.0,
                height: 20.0,
            },
            confidence: 0.9,
        };
        let texts = vec![
            heading.clone(),
            SnapshotText {
                text: "DesktopCtl".to_string(),
                bounds: Bounds {
                    x: 550.0,
                    y: 210.0,
                    width: 90.0,
                    height: 14.0,
                },
                confidence: 0.9,
            },
            SnapshotText {
                text: "Allow the applications below to record".to_string(),
                bounds: Bounds {
                    x: 540.0,
                    y: 140.0,
                    width: 290.0,
                    height: 12.0,
                },
                confidence: 0.8,
            },
        ];
        let rows = super::infer_settings_rows(&texts, Some(&heading));
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].text, "DesktopCtl");
    }

    #[test]
    fn sidebar_like_rows_are_rejected() {
        let rows = Bounds {
            x: 56.0,
            y: 242.0,
            width: 168.0,
            height: 372.0,
        };
        let heading = SnapshotText {
            text: "Accessibility".to_string(),
            bounds: Bounds {
                x: 302.0,
                y: 132.0,
                width: 108.0,
                height: 20.0,
            },
            confidence: 0.9,
        };
        let no_items = SnapshotText {
            text: "No Items".to_string(),
            bounds: Bounds {
                x: 410.0,
                y: 194.0,
                width: 58.0,
                height: 15.0,
            },
            confidence: 0.9,
        };
        assert!(super::is_sidebar_like_rows(
            Some(&rows),
            Some(&heading),
            Some(&no_items)
        ));
    }

    #[test]
    fn table_rows_are_not_rejected() {
        let rows = Bounds {
            x: 286.0,
            y: 188.0,
            width: 340.0,
            height: 46.0,
        };
        let heading = SnapshotText {
            text: "Accessibility".to_string(),
            bounds: Bounds {
                x: 302.0,
                y: 132.0,
                width: 108.0,
                height: 20.0,
            },
            confidence: 0.9,
        };
        let no_items = SnapshotText {
            text: "No Items".to_string(),
            bounds: Bounds {
                x: 410.0,
                y: 194.0,
                width: 58.0,
                height: 15.0,
            },
            confidence: 0.9,
        };
        assert!(!super::is_sidebar_like_rows(
            Some(&rows),
            Some(&heading),
            Some(&no_items)
        ));
    }

    #[test]
    fn scales_region_bounds_from_retina_image_to_logical_display() {
        let regions = crate::vision::regions::SettingsRegions {
            window_bounds: Some(Bounds {
                x: 0.0,
                y: 300.0,
                width: 2000.0,
                height: 1200.0,
            }),
            sidebar_bounds: None,
            content_bounds: Some(Bounds {
                x: 300.0,
                y: 420.0,
                width: 1600.0,
                height: 950.0,
            }),
            table_bounds: Some(Bounds {
                x: 500.0,
                y: 640.0,
                width: 1200.0,
                height: 88.0,
            }),
        };

        let scaled = super::scale_regions_to_display(&regions, Some(2940), Some(1912), 1470, 956);
        let table = scaled.table_bounds.expect("table bounds");
        assert!((table.x - 250.0).abs() < 0.001);
        assert!((table.y - 320.0).abs() < 0.001);
        assert!((table.width - 600.0).abs() < 0.001);
        assert!((table.height - 44.0).abs() < 0.001);
    }

    #[test]
    fn keeps_region_bounds_when_image_and_display_match() {
        let regions = crate::vision::regions::SettingsRegions {
            window_bounds: None,
            sidebar_bounds: None,
            content_bounds: None,
            table_bounds: Some(Bounds {
                x: 240.0,
                y: 220.0,
                width: 700.0,
                height: 44.0,
            }),
        };
        let scaled = super::scale_regions_to_display(&regions, Some(1470), Some(956), 1470, 956);
        let table = scaled.table_bounds.expect("table");
        assert!((table.x - 240.0).abs() < 0.001);
        assert!((table.y - 220.0).abs() < 0.001);
        assert!((table.width - 700.0).abs() < 0.001);
        assert!((table.height - 44.0).abs() < 0.001);
    }

    #[test]
    fn infers_window_bounds_from_content_geometry() {
        let window = super::infer_window_bounds_from_content(
            Some(&Bounds {
                x: 246.0,
                y: 176.0,
                width: 382.0,
                height: 438.0,
            }),
            1470,
            956,
        )
        .expect("window bounds");
        assert!((window.x - 96.0).abs() < 0.5);
        assert!((window.y - 138.77).abs() < 0.5);
        assert!((window.width - 532.0).abs() < 0.5);
        assert!((window.height - 475.23).abs() < 0.5);
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

    #[test]
    fn derives_settings_add_click_from_no_items_anchor() {
        let payload = json!({
            "no_items": {
                "bounds": {
                    "x": 508.5,
                    "y": 241.05,
                    "width": 57.72,
                    "height": 12.97
                }
            }
        });
        let (x, y) = super::settings_click_from_no_items_anchor("add", &payload)
            .expect("add click from anchor");
        assert_eq!((x, y), (355, 268));
    }

    #[test]
    fn derives_settings_remove_click_from_no_items_anchor() {
        let payload = json!({
            "no_items": {
                "bounds": {
                    "x": 508.5,
                    "y": 241.05,
                    "width": 57.72,
                    "height": 12.97
                }
            }
        });
        let (x, y) = super::settings_click_from_no_items_anchor("remove", &payload)
            .expect("remove click from anchor");
        assert_eq!((x, y), (373, 268));
    }

    #[test]
    fn derives_settings_add_click_from_instruction_x_anchor() {
        let payload = json!({
            "instruction": {
                "bounds": {
                    "x": 302.0,
                    "y": 166.0,
                    "width": 268.0,
                    "height": 14.0
                }
            },
            "no_items": {
                "bounds": {
                    "x": 508.5,
                    "y": 241.05,
                    "width": 97.72,
                    "height": 12.97
                }
            }
        });
        let (x, y) = super::settings_click_from_no_items_anchor("add", &payload)
            .expect("add click from instruction anchor");
        assert_eq!((x, y), (306, 268));
    }

    #[test]
    fn derives_settings_add_click_from_instruction_anchor_payload() {
        let payload = json!({
            "heading": {
                "bounds": { "x": 1021.3, "y": 202.7, "width": 94.0, "height": 19.2 }
            },
            "instruction": {
                "bounds": { "x": 965.8, "y": 247.5, "width": 329.0, "height": 15.0 }
            },
            "list_bounds": { "x": 997.3, "y": 229.9, "width": 338.0, "height": 76.0 },
            "regions": {
                "content": { "x": 720.0, "y": 184.0, "width": 715.0, "height": 625.0 }
            },
            "rows": [{
                "bounds": { "x": 1004.0, "y": 274.0, "width": 132.0, "height": 14.0 }
            }]
        });
        let (x, y) = super::settings_click_from_instruction_anchor("add", &payload)
            .expect("add click from instruction payload");
        assert_eq!((x, y), (970, 330));
    }

    #[test]
    fn derives_settings_remove_click_from_instruction_anchor_payload() {
        let payload = json!({
            "heading": {
                "bounds": { "x": 1021.3, "y": 202.7, "width": 94.0, "height": 19.2 }
            },
            "instruction": {
                "bounds": { "x": 965.8, "y": 247.5, "width": 329.0, "height": 15.0 }
            },
            "list_bounds": { "x": 997.3, "y": 229.9, "width": 338.0, "height": 76.0 },
            "regions": {
                "content": { "x": 720.0, "y": 184.0, "width": 715.0, "height": 625.0 }
            },
            "rows": [{
                "bounds": { "x": 1004.0, "y": 274.0, "width": 132.0, "height": 14.0 }
            }]
        });
        let (x, y) = super::settings_click_from_instruction_anchor("remove", &payload)
            .expect("remove click from instruction payload");
        assert_eq!((x, y), (988, 330));
    }

    #[test]
    fn finds_settings_instruction_under_heading() {
        let heading = SnapshotText {
            text: "Accessibility".to_string(),
            bounds: Bounds {
                x: 300.0,
                y: 134.0,
                width: 112.0,
                height: 19.0,
            },
            confidence: 0.9,
        };
        let texts = vec![
            heading.clone(),
            SnapshotText {
                text: "Allow the applications below to control your computer.".to_string(),
                bounds: Bounds {
                    x: 255.0,
                    y: 166.0,
                    width: 268.0,
                    height: 14.0,
                },
                confidence: 0.9,
            },
        ];
        let instruction = super::find_settings_instruction(&texts, Some(&heading))
            .expect("instruction should be found");
        assert!(instruction.text.to_lowercase().contains("allow"));
        assert!(instruction.bounds.y > heading.bounds.y);
    }

    #[test]
    fn infers_controls_from_ocr_symbols_pair() {
        let heading = SnapshotText {
            text: "Accessibility".to_string(),
            bounds: Bounds {
                x: 299.0,
                y: 136.0,
                width: 112.0,
                height: 18.0,
            },
            confidence: 0.9,
        };
        let no_items = SnapshotText {
            text: "No Items".to_string(),
            bounds: Bounds {
                x: 409.0,
                y: 198.0,
                width: 58.0,
                height: 14.0,
            },
            confidence: 0.9,
        };
        let plus = SnapshotText {
            text: "+".to_string(),
            bounds: Bounds {
                x: 252.0,
                y: 214.0,
                width: 8.0,
                height: 8.0,
            },
            confidence: 0.9,
        };
        let minus = SnapshotText {
            text: "-".to_string(),
            bounds: Bounds {
                x: 270.0,
                y: 214.0,
                width: 8.0,
                height: 8.0,
            },
            confidence: 0.9,
        };
        let controls = super::infer_settings_controls_for_settings_pane(
            &[heading.clone(), no_items.clone(), plus, minus],
            Some(&heading),
            Some(&no_items),
            None,
            None,
            None,
            Some(&Bounds {
                x: 234.0,
                y: 150.0,
                width: 398.0,
                height: 470.0,
            }),
            14.0,
        )
        .expect("controls should be inferred");
        assert_eq!(controls["source"], "ocr_symbols");
        let (x, y) = super::point_from_value(&controls["add_click"]).expect("add click");
        assert_eq!((x, y), (256, 218));
    }

    #[test]
    fn infers_controls_from_compound_ocr_symbol_token_with_instruction_anchor() {
        let heading = SnapshotText {
            text: "Accessibility".to_string(),
            bounds: Bounds {
                x: 1198.0,
                y: 134.0,
                width: 112.0,
                height: 18.0,
            },
            confidence: 0.9,
        };
        let instruction = SnapshotText {
            text: "Allow the applications below to control your computer.".to_string(),
            bounds: Bounds {
                x: 1204.0,
                y: 164.0,
                width: 352.0,
                height: 15.0,
            },
            confidence: 0.8,
        };
        let no_items = SnapshotText {
            text: "No Items".to_string(),
            bounds: Bounds {
                x: 1403.0,
                y: 242.0,
                width: 64.0,
                height: 14.0,
            },
            confidence: 0.8,
        };
        let combined = SnapshotText {
            text: "+ -".to_string(),
            bounds: Bounds {
                x: 1247.79,
                y: 240.0,
                width: 76.92,
                height: 30.11,
            },
            confidence: 0.5,
        };
        let controls = super::infer_settings_controls_for_settings_pane(
            &[
                heading.clone(),
                instruction.clone(),
                no_items.clone(),
                combined,
            ],
            Some(&heading),
            Some(&no_items),
            Some(&instruction),
            None,
            None,
            Some(&Bounds {
                x: 1180.0,
                y: 124.0,
                width: 500.0,
                height: 760.0,
            }),
            14.0,
        )
        .expect("controls should be inferred");
        assert_eq!(controls["source"], "ocr_symbols");
        let (x, _y) = super::point_from_value(&controls["add_click"]).expect("add click");
        assert_eq!(x, 1208);
    }

    #[test]
    fn list_bounds_controls_include_footer() {
        let heading = SnapshotText {
            text: "Accessibility".to_string(),
            bounds: Bounds {
                x: 300.0,
                y: 134.0,
                width: 112.0,
                height: 18.0,
            },
            confidence: 0.9,
        };
        let controls = super::infer_settings_controls_from_list_bounds(
            Some(&Bounds {
                x: 246.0,
                y: 188.0,
                width: 360.0,
                height: 42.0,
            }),
            Some(&heading),
            14.0,
        )
        .expect("controls");
        assert!(controls["footer_bounds"].is_object());
    }
}

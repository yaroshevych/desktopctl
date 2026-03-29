use std::{
    collections::HashSet,
    fs,
    os::unix::fs::PermissionsExt,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
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
        Command, ObserveOptions, ObserveUntil, PointerButton, RequestEnvelope, ResponseEnvelope,
    },
};
use image::{ImageFormat, Rgba, RgbaImage};
use serde_json::{Value, json};

mod commands;
mod guards;

#[cfg(target_os = "macos")]
use crate::overlay;
use crate::{
    permissions, platform, recording, request_store, trace, vision, window_refs, window_target,
};

#[cfg(target_os = "macos")]
const OVERLAY_WATCH_TRACK_INTERVAL_MS: u64 = 40;
#[cfg(target_os = "macos")]
const OVERLAY_SCREEN_CAPTURE_MODE_LOCK_MS: u64 = 2_000;
#[cfg(target_os = "macos")]
const PRIVACY_OVERLAY_STOP_DELAY_MS: u64 = 2_200;
#[cfg(target_os = "macos")]
static OVERLAY_WATCH_TRACK_RUNNING: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static PRIVACY_OVERLAY_ACTIVE: AtomicBool = AtomicBool::new(false);
const OBSERVE_SAMPLE_INTERVAL_MS: u64 = 40;
const OBSERVE_QUIET_FRAMES: u32 = 2;
const OBSERVE_DIFF_THRESHOLD: u8 = 8;
const OBSERVE_THUMB_WIDTH: u32 = 96;
const OBSERVE_THUMB_HEIGHT: u32 = 54;
const OBSERVE_REGION_PAD_PX: f64 = 14.0;
const OBSERVE_MIN_THUMB_COMPONENT_AREA: u32 = 2;
const OBSERVE_OCR_PAD_PX: f64 = 40.0;

#[derive(Debug, Clone, Default)]
struct RequestContext {
    #[cfg(target_os = "macos")]
    frontmost: Option<window_target::FrontmostSnapshot>,
}

#[derive(Debug, Clone, Default)]
struct ObserveStartState {
    active_window_id: Option<String>,
    focused_element_id: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct ObserveEndState {
    focus_changed: bool,
    focused_element_id: Option<String>,
    active_window_changed: bool,
    active_window_id: Option<String>,
    active_window_bounds: Option<desktop_core::protocol::Bounds>,
}

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
        let (mode, bounds) = if let Some(bounds) = window_target::frontmost_window_bounds() {
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
                    if let Some(bounds) = window_target::frontmost_window_bounds() {
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
    let request_context = RequestContext {
        frontmost: if command_requires_privacy_signal(&command) {
            Some(window_target::resolve_frontmost_snapshot())
        } else {
            None
        },
    };
    #[cfg(not(target_os = "macos"))]
    let request_context = RequestContext::default();
    #[cfg(target_os = "macos")]
    let transient_overlay_started = maybe_start_privacy_overlay(&command, &request_context);
    #[cfg(target_os = "macos")]
    let overlay_token_updates_enabled =
        !transient_overlay_started && !PRIVACY_OVERLAY_ACTIVE.load(Ordering::SeqCst);
    #[cfg(not(target_os = "macos"))]
    let overlay_token_updates_enabled = true;
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
        if let Some(bounds) = request_frontmost_bounds(&request_context) {
            let _ = overlay::watch_mode_changed(overlay::WatchMode::WindowMode, Some(bounds));
        }
    }
    #[cfg(target_os = "macos")]
    let _ = overlay::agent_active_changed(true);
    let response = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        execute_with_context(command, overlay_token_updates_enabled, &request_context)
    })) {
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
fn maybe_start_privacy_overlay(command: &Command, context: &RequestContext) -> bool {
    if !command_requires_privacy_signal(command) || overlay::is_active() {
        return false;
    }
    match overlay::start_overlay() {
        Ok(started) => {
            if started {
                PRIVACY_OVERLAY_ACTIVE.store(true, Ordering::SeqCst);
                let (mode, bounds) = if let Some(bounds) = request_frontmost_bounds(context) {
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
fn request_frontmost_bounds(context: &RequestContext) -> Option<desktop_core::protocol::Bounds> {
    context
        .frontmost
        .as_ref()
        .and_then(|snapshot| snapshot.bounds.clone())
        .or_else(window_target::frontmost_window_bounds)
}

#[cfg(not(target_os = "macos"))]
fn request_frontmost_bounds(_context: &RequestContext) -> Option<desktop_core::protocol::Bounds> {
    window_target::frontmost_window_bounds()
}

fn request_frontmost_app(context: &RequestContext) -> Option<String> {
    #[cfg(target_os = "macos")]
    if let Some(app) = context
        .frontmost
        .as_ref()
        .and_then(|snapshot| snapshot.app.clone())
    {
        return Some(app);
    }
    window_target::frontmost_app_name()
}

#[cfg(target_os = "macos")]
fn schedule_transient_overlay_stop() {
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(PRIVACY_OVERLAY_STOP_DELAY_MS));
        if overlay::is_agent_active() || !overlay::is_active() {
            return;
        }
        match overlay::stop_overlay() {
            Ok(stopped) => {
                if stopped {
                    PRIVACY_OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
                }
                trace::log(format!("overlay:privacy_auto_stop stopped={stopped}"));
            }
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
            | Command::PointerScroll { .. }
            | Command::PointerDrag { .. }
            | Command::UiType { .. }
            | Command::KeyHotkey { .. }
            | Command::KeyEnter { .. }
            | Command::KeyEscape { .. }
    )
}

#[cfg(test)]
fn execute(command: Command) -> Result<Value, AppError> {
    execute_with_context(command, true, &RequestContext::default())
}

fn execute_with_context(
    command: Command,
    overlay_token_updates_enabled: bool,
    request_context: &RequestContext,
) -> Result<Value, AppError> {
    match command {
        Command::Ping => Ok(json!({ "message": "pong" })),
        Command::AppHide { name } => commands::app::hide(name),
        Command::AppShow { name } => commands::app::show(name),
        Command::AppIsolate { name } => commands::app::isolate(name),
        Command::WindowList => commands::window::list(),
        Command::WindowBounds { title } => commands::window::bounds(title),
        Command::WindowFocus { title } => commands::window::focus(title),
        Command::OpenApp {
            name,
            args,
            wait,
            timeout_ms,
        } => commands::app::open(name, args, wait, timeout_ms),
        Command::PointerMove {
            x,
            y,
            absolute,
            active_window,
            active_window_id,
        } => commands::input::pointer_move(
            x,
            y,
            absolute,
            active_window,
            active_window_id,
            request_context,
        ),
        Command::PointerDown {
            x,
            y,
            button,
            active_window,
            active_window_id,
        } => commands::input::pointer_down(x, y, button, active_window, active_window_id),
        Command::PointerUp {
            x,
            y,
            button,
            active_window,
            active_window_id,
        } => commands::input::pointer_up(x, y, button, active_window, active_window_id),
        Command::PointerClick {
            x,
            y,
            absolute,
            button,
            observe,
            active_window,
            active_window_id,
        } => commands::input::pointer_click(
            x,
            y,
            absolute,
            button,
            observe,
            active_window,
            active_window_id,
            request_context,
        ),
        Command::PointerScroll {
            id,
            dx,
            dy,
            observe,
            active_window,
            active_window_id,
        } => commands::input::pointer_scroll(
            id,
            dx,
            dy,
            observe,
            active_window,
            active_window_id,
            request_context,
        ),
        Command::PointerDrag {
            x1,
            y1,
            x2,
            y2,
            hold_ms,
            active_window,
            active_window_id,
        } => {
            commands::input::pointer_drag(x1, y1, x2, y2, hold_ms, active_window, active_window_id)
        }
        Command::UiType {
            text,
            observe,
            active_window,
            active_window_id,
        } => commands::input::key_type(text, observe, active_window, active_window_id),
        Command::KeyHotkey {
            hotkey,
            observe,
            active_window,
            active_window_id,
        } => commands::input::key_hotkey(hotkey, observe, active_window, active_window_id),
        Command::KeyEnter {
            observe,
            active_window,
            active_window_id,
        } => commands::input::key_enter(observe, active_window, active_window_id),
        Command::KeyEscape {
            observe,
            active_window,
            active_window_id,
        } => commands::input::key_escape(observe, active_window, active_window_id),
        Command::ScreenCapture {
            out_path,
            overlay,
            active_window,
            active_window_id,
            region,
        } => {
            commands::screen::screenshot(out_path, overlay, active_window, active_window_id, region)
        }
        Command::ScreenTokenize {
            overlay_out_path,
            window_query,
            screenshot_path,
            active_window,
            active_window_id,
            region,
        } => commands::screen::tokenize(
            overlay_out_path,
            window_query,
            screenshot_path,
            active_window,
            active_window_id,
            region,
            overlay_token_updates_enabled,
        ),
        Command::ScreenFindText { text, all } => commands::screen::find_text(text, all),
        Command::OverlayStart { duration_ms } => commands::overlay::start(duration_ms),
        Command::OverlayStop => commands::overlay::stop(),
        Command::WaitText {
            text,
            timeout_ms,
            interval_ms,
            disappear,
        } => commands::screen::wait_text(text, timeout_ms, interval_ms, disappear),
        Command::PointerClickText {
            text,
            button,
            active_window,
            active_window_id,
            observe,
        } => commands::pointer::click_text(
            text,
            button,
            active_window,
            active_window_id,
            observe,
            request_context,
        ),
        Command::PointerClickId {
            id,
            button,
            active_window,
            active_window_id,
            observe,
        } => commands::pointer::click_id(
            id,
            button,
            active_window,
            active_window_id,
            observe,
            request_context,
        ),
        Command::ClipboardRead => commands::misc::clipboard_read(),
        Command::ClipboardWrite { text } => commands::misc::clipboard_write(text),
        Command::PermissionsCheck => commands::misc::permissions_check(),
        Command::DebugSnapshot => commands::misc::debug_snapshot(),
        Command::RequestShow { request_id } => commands::misc::request_show(request_id),
        Command::RequestList { limit } => commands::misc::request_list(limit),
        Command::RequestScreenshot {
            request_id,
            out_path,
        } => commands::misc::request_screenshot(request_id, out_path),
        Command::RequestResponse { request_id } => commands::misc::request_response(request_id),
        Command::RequestSearch {
            text,
            limit,
            command,
        } => commands::misc::request_search(text, limit, command),
        Command::ReplayRecord { duration_ms, stop } => {
            commands::misc::replay_record(duration_ms, stop)
        }
        Command::ReplayLoad { session_dir } => commands::misc::replay_load(session_dir),
    }
}

fn resolve_pointer_click_point(
    x: u32,
    y: u32,
    absolute: bool,
    active_window: bool,
    active_window_id: Option<&str>,
    request_context: &RequestContext,
) -> Result<Point, AppError> {
    if absolute {
        return Ok(Point::new(x, y));
    }
    let bounds = if active_window {
        let target = if let Some(reference) = active_window_id {
            assert_active_window_id_matches(reference)?
        } else {
            resolve_active_window_target()?
        };
        target.bounds
    } else {
        click_scope_window_bounds(request_context).ok_or_else(|| {
            AppError::target_not_found(
                "frontmost window bounds unavailable for relative pointer action; use --absolute",
            )
        })?
    };
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

pub(super) fn enrich_window_refs(windows: &mut [platform::windowing::WindowInfo]) {
    for window in windows.iter_mut() {
        if window.window_ref.is_none() {
            window.window_ref = Some(window_refs::issue_for_window(window));
        }
    }
    let id_to_ref: std::collections::HashMap<String, String> = windows
        .iter()
        .filter_map(|window| window.window_ref.clone().map(|r| (window.id.clone(), r)))
        .collect();
    for window in windows.iter_mut() {
        if let Some(parent_internal) = window.parent_id.clone() {
            if let Some(parent_ref) = id_to_ref.get(&parent_internal) {
                window.parent_id = Some(parent_ref.clone());
            }
        }
    }
}

fn resolve_active_window_target() -> Result<platform::windowing::WindowInfo, AppError> {
    let snapshot_raw = window_target::resolve_frontmost_snapshot_raw();
    let snapshot = if snapshot_raw.bounds.is_some() {
        snapshot_raw
    } else {
        window_target::resolve_frontmost_snapshot()
    };
    let app_hint = snapshot.app.as_deref();
    let target_bounds = snapshot.bounds.as_ref();

    let mut windows = window_target::list_windows_basic()?;
    enrich_window_refs(&mut windows);

    let selected = windows
        .iter()
        .filter(|window| window.visible && window.bounds.width > 8.0 && window.bounds.height > 8.0)
        .filter(|window| {
            app_hint
                .map(|app| window.app.eq_ignore_ascii_case(app))
                .unwrap_or(true)
        })
        .max_by(|a, b| {
            let sa = active_window_candidate_score(a, target_bounds);
            let sb = active_window_candidate_score(b, target_bounds);
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        })
        .cloned();

    selected.ok_or_else(|| {
        AppError::target_not_found(
            "frontmost window not found; ensure a standard app window is focused",
        )
    })
}

fn active_window_candidate_score(
    window: &platform::windowing::WindowInfo,
    target_bounds: Option<&desktop_core::protocol::Bounds>,
) -> f64 {
    let area = (window.bounds.width.max(0.0) * window.bounds.height.max(0.0)).max(0.0);
    let overlap_bonus = target_bounds
        .map(|target| {
            // Prioritize geometric overlap with raw frontmost bounds. Avoid
            // rewarding large container windows, which hides modal dialogs.
            iou(&window.bounds, target) * 10.0
        })
        .unwrap_or(0.0);
    let frontmost_bonus = if window.frontmost { 0.5 } else { 0.0 };
    overlap_bonus + frontmost_bonus + area.sqrt() * 0.01
}

fn assert_active_window_id_matches(
    reference: &str,
) -> Result<platform::windowing::WindowInfo, AppError> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_argument(
            "active window id must not be empty",
        ));
    }
    let active = resolve_active_window_target()?;
    let active_ref = active
        .window_ref
        .clone()
        .ok_or_else(|| AppError::target_not_found("active window id is unavailable"))?;
    if active_ref != trimmed {
        return Err(AppError::target_not_found(format!(
            "active window does not match requested id \"{trimmed}\""
        )));
    }
    Ok(active)
}

fn bind_active_window_reference(
    active_window: bool,
    active_window_id: Option<&str>,
) -> Result<Option<String>, AppError> {
    if active_window_id.is_some() && !active_window {
        return Err(AppError::invalid_argument(
            "active window id requires --active-window",
        ));
    }
    if !active_window {
        return Ok(None);
    }
    let target = if let Some(reference) = active_window_id {
        assert_active_window_id_matches(reference)?
    } else {
        resolve_active_window_target()?
    };
    let bound = target
        .window_ref
        .ok_or_else(|| AppError::target_not_found("active window id is unavailable"))?;
    Ok(Some(bound))
}

fn resolve_observe_scope_bounds(
    active_window: bool,
    active_window_id: Option<&str>,
) -> Result<Option<desktop_core::protocol::Bounds>, AppError> {
    if active_window_id.is_some() && !active_window {
        return Err(AppError::invalid_argument(
            "active window id requires --active-window",
        ));
    }
    if !active_window {
        return Ok(None);
    }
    let target = if let Some(reference) = active_window_id {
        assert_active_window_id_matches(reference)?
    } else {
        resolve_active_window_target()?
    };
    Ok(Some(target.bounds))
}

fn attach_window_ref_to_payload(payload: &mut desktop_core::protocol::TokenizePayload) {
    let Some(first) = payload.windows.first_mut() else {
        return;
    };
    if first.window_ref.is_some() {
        return;
    }
    let Some(target_bounds) = first
        .os_bounds
        .as_ref()
        .cloned()
        .or_else(|| Some(first.bounds.clone()))
    else {
        return;
    };

    let mut windows = match window_target::list_frontmost_app_windows() {
        Ok(items) => items,
        Err(_) => return,
    };
    enrich_window_refs(&mut windows);
    let app_hint = first.app.as_deref();
    let title_hint = first.title.as_str();
    let selected = windows
        .iter()
        .filter(|window| window.bounds.width > 8.0 && window.bounds.height > 8.0 && window.visible)
        .max_by(|a, b| {
            let sa = window_match_score(a, &target_bounds, app_hint, title_hint);
            let sb = window_match_score(b, &target_bounds, app_hint, title_hint);
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        });
    if let Some(window) = selected {
        first.window_ref = window.window_ref.clone();
    }
}

fn window_match_score(
    window: &platform::windowing::WindowInfo,
    target_bounds: &desktop_core::protocol::Bounds,
    app_hint: Option<&str>,
    title_hint: &str,
) -> f64 {
    let overlap = iou(&window.bounds, target_bounds);
    let area_delta = ((window.bounds.width - target_bounds.width).abs()
        + (window.bounds.height - target_bounds.height).abs())
    .min(2000.0);
    let mut score = overlap * 10.0 - area_delta * 0.002;
    if let Some(app) = app_hint {
        if window.app.eq_ignore_ascii_case(app) {
            score += 1.0;
        }
    }
    if !title_hint.trim().is_empty() && window.title.eq_ignore_ascii_case(title_hint) {
        score += 0.4;
    }
    if window.frontmost {
        score += 0.25;
    }
    score
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
    let Some(bounds) = window_target::frontmost_window_bounds() else {
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

fn remap_tokenize_window_id_field(value: &mut Value) {
    let Some(windows) = value.get_mut("windows").and_then(Value::as_array_mut) else {
        return;
    };
    for window in windows {
        let Some(object) = window.as_object_mut() else {
            continue;
        };
        match object.remove("window_ref") {
            Some(Value::String(ref_id)) if !ref_id.trim().is_empty() => {
                object.insert("id".to_string(), Value::String(ref_id));
            }
            _ => {
                object.remove("id");
            }
        }
    }
}

fn append_tokenize_text_dump(value: &mut Value) {
    let Some(windows) = value.get("windows").and_then(Value::as_array) else {
        return;
    };
    let mut chunks: Vec<String> = Vec::new();
    for window in windows {
        let Some(window_obj) = window.as_object() else {
            continue;
        };
        let title = window_obj
            .get("title")
            .and_then(Value::as_str)
            .unwrap_or("window");
        let app = window_obj.get("app").and_then(Value::as_str).unwrap_or("");
        let Some(elements) = window_obj.get("elements").and_then(Value::as_array) else {
            continue;
        };
        let dump = build_window_text_dump(elements);
        if dump.trim().is_empty() {
            continue;
        }
        let mut header_lines = vec![format!("window title: {title}")];
        if !app.trim().is_empty() {
            header_lines.push(format!("window app: {app}"));
        }
        header_lines.push(dump);
        chunks.push(header_lines.join("\n"));
    }
    if chunks.is_empty() {
        return;
    }
    let text_dump = chunks.join("\n\n");
    if let Some(obj) = value.as_object_mut() {
        obj.insert("text_dump".to_string(), Value::String(text_dump));
    }
}

#[derive(Clone)]
struct TextDumpEntry {
    text: String,
    x: f64,
    y: f64,
    width: f64,
}

struct TextDumpColumn {
    entries: Vec<TextDumpEntry>,
    min_x: f64,
    max_x: f64,
    likely_scrollable: bool,
}

fn build_window_text_dump(elements: &[Value]) -> String {
    let mut entries: Vec<TextDumpEntry> = Vec::new();
    for el in elements {
        let Some(obj) = el.as_object() else { continue };
        let Some(text_raw) = obj.get("text").and_then(Value::as_str) else {
            continue;
        };
        let Some(bbox) = obj.get("bbox").and_then(Value::as_array) else {
            continue;
        };
        if bbox.len() != 4 {
            continue;
        }
        let x = bbox[0].as_f64().unwrap_or(0.0);
        let y = bbox[1].as_f64().unwrap_or(0.0);
        let w = bbox[2].as_f64().unwrap_or(0.0);
        let cleaned = compact_text_dump_text(text_raw);
        if cleaned.is_empty() || w <= 2.0 {
            continue;
        }
        entries.push(TextDumpEntry {
            text: cleaned,
            x,
            y,
            width: w,
        });
    }
    if entries.is_empty() {
        return String::new();
    }

    entries.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
    let mut columns: Vec<TextDumpColumn> = Vec::new();
    let column_split = 140.0;
    for entry in entries {
        if let Some(last_col) = columns.last_mut() {
            let last_x = last_col.entries.last().map(|e| e.x).unwrap_or(entry.x);
            if (entry.x - last_x).abs() <= column_split {
                last_col.min_x = last_col.min_x.min(entry.x);
                last_col.max_x = last_col.max_x.max(entry.x + entry.width);
                last_col.entries.push(entry);
                continue;
            }
        }
        columns.push(TextDumpColumn {
            min_x: entry.x,
            max_x: entry.x + entry.width,
            entries: vec![entry],
            likely_scrollable: false,
        });
    }

    for column in &mut columns {
        column.likely_scrollable = elements.iter().any(|el| {
            let Some(obj) = el.as_object() else {
                return false;
            };
            if !element_scrollability_hint(obj) {
                return false;
            }
            let Some(bbox) = obj.get("bbox").and_then(Value::as_array) else {
                return false;
            };
            if bbox.len() != 4 {
                return false;
            }
            let x = bbox[0].as_f64().unwrap_or(0.0);
            let w = bbox[2].as_f64().unwrap_or(0.0).max(0.0);
            let left = x;
            let right = x + w;
            right >= (column.min_x - 12.0) && left <= (column.max_x + 12.0)
        });
    }

    let mut out_lines: Vec<String> = Vec::new();
    for (idx, mut col) in columns.into_iter().enumerate() {
        col.entries
            .sort_by(|a, b| a.y.total_cmp(&b.y).then_with(|| a.x.total_cmp(&b.x)));
        let heading = column_heading(idx, col.likely_scrollable);
        out_lines.push(heading);
        out_lines.push("---".to_string());
        let mut seen = HashSet::<String>::new();
        let mut emitted = 0usize;
        for entry in col.entries {
            let norm = entry.text.to_ascii_lowercase();
            if !seen.insert(norm) {
                continue;
            }
            out_lines.push(entry.text);
            emitted += 1;
            if emitted >= 40 {
                break;
            }
        }
        out_lines.push(String::new());
    }
    out_lines.join("\n").trim().to_string()
}

fn column_heading(idx: usize, scrollable: bool) -> String {
    let base = match idx {
        0 => "left column".to_string(),
        1 => "right column".to_string(),
        _ => format!("column {}", idx + 1),
    };
    if scrollable {
        format!("{base} (scrollable)")
    } else {
        base
    }
}

fn element_scrollability_hint(obj: &serde_json::Map<String, Value>) -> bool {
    if obj
        .get("scrollable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    let source = obj
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default();
    source.contains("AXScrollArea")
        || source.contains("AXOutline")
        || source.contains("AXTable")
        || source.contains("AXList")
        || source.contains("AXTextArea")
        || source.contains("AXSplitGroup")
}

fn compact_text_dump_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len().min(160));
    let mut last_was_space = false;
    for ch in input.chars() {
        let mapped = if ch.is_whitespace() { ' ' } else { ch };
        if mapped == ' ' {
            if last_was_space {
                continue;
            }
            last_was_space = true;
            out.push(' ');
            continue;
        }
        last_was_space = false;
        out.push(mapped);
        if out.len() >= 160 {
            out.push('…');
            break;
        }
    }
    out.trim().to_string()
}

fn click_text_target(
    query: &str,
    button: PointerButton,
    active_window: bool,
    active_window_id: Option<&str>,
    request_context: &RequestContext,
) -> Result<Value, AppError> {
    if active_window_id.is_some() && !active_window {
        return Err(AppError::invalid_argument(
            "active window id requires --active-window",
        ));
    }
    if active_window {
        if let Some(reference) = active_window_id {
            assert_active_window_id_matches(reference)?;
        }
        if let Some(result) = try_click_text_active_window_ax(query, button)? {
            return Ok(result);
        }
        let bounds = click_scope_window_bounds(request_context).ok_or_else(|| {
            AppError::target_not_found("frontmost window bounds unavailable for click --text")
        })?;
        let app = request_frontmost_app(request_context);
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
                "no tokenize text detected in frontmost window; cannot click target safely",
            ));
        }
        let target = select_text_candidate(&tokenize_texts, query)?;
        trace::log("ui_click_text:active_window source=tokenize");
        trace::log(format!(
            "ui_click_text:selected text=\"{}\" confidence={:.3} bounds=({}, {}, {}, {})",
            compact_for_log(&target.text),
            target.confidence,
            target.bounds.x,
            target.bounds.y,
            target.bounds.width,
            target.bounds.height
        ));
        let click_point = perform_click(&target.bounds, button)?;
        return Ok(json!({
            "snapshot_id": payload.snapshot_id,
            "text": target.text,
            "bounds": target.bounds,
            "x": click_point.x,
            "y": click_point.y
        }));
    }

    permissions::ensure_screen_recording_permission()?;
    let capture = vision::pipeline::capture_and_update(None)?;
    let normalized_texts = normalize_snapshot_texts_to_display(
        &capture.snapshot.texts,
        capture.image.width(),
        capture.image.height(),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
    );
    let window_bounds = click_scope_window_bounds(request_context);
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
            match tokenize_click_text_candidate(query, window_bounds.as_ref(), request_context) {
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
    let click_point = perform_click(&target.bounds, button)?;

    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "text": target.text,
        "bounds": target.bounds,
        "x": click_point.x,
        "y": click_point.y
    }))
}

fn try_click_text_active_window_ax(
    query: &str,
    button: PointerButton,
) -> Result<Option<Value>, AppError> {
    let ax_elements = match platform::ax::collect_frontmost_window_elements() {
        Ok(items) => items,
        Err(err) => {
            trace::log(format!("ui_click_text:active_window_ax_warn {err}"));
            return Ok(None);
        }
    };
    if ax_elements.is_empty() {
        return Ok(None);
    }

    let texts: Vec<desktop_core::protocol::SnapshotText> = ax_elements
        .iter()
        .filter_map(|ax| {
            let text = ax.text.as_ref()?.trim();
            if text.is_empty() {
                return None;
            }
            Some(desktop_core::protocol::SnapshotText {
                text: text.to_string(),
                bounds: ax.bounds.clone(),
                confidence: 0.92,
            })
        })
        .collect();
    if texts.is_empty() {
        return Ok(None);
    }

    match select_text_candidate(&texts, query) {
        Ok(target) => {
            trace::log("ui_click_text:active_window source=ax");
            trace::log(format!(
                "ui_click_text:selected text=\"{}\" confidence={:.3} bounds=({}, {}, {}, {})",
                compact_for_log(&target.text),
                target.confidence,
                target.bounds.x,
                target.bounds.y,
                target.bounds.width,
                target.bounds.height
            ));
            let click_point = perform_click(&target.bounds, button)?;
            Ok(Some(json!({
                "snapshot_id": 0,
                "text": target.text,
                "bounds": target.bounds,
                "x": click_point.x,
                "y": click_point.y
            })))
        }
        Err(err) if matches!(err.code, desktop_core::error::ErrorCode::TargetNotFound) => Ok(None),
        Err(err) if matches!(err.code, desktop_core::error::ErrorCode::AmbiguousTarget) => Ok(None),
        Err(err) => Err(err),
    }
}

#[derive(Debug, Clone)]
struct TokenizeClickElementCandidate {
    id: String,
    text: Option<String>,
    bounds: desktop_core::protocol::Bounds,
    source: String,
}

fn click_element_id_target(
    id: &str,
    button: PointerButton,
    active_window: bool,
    active_window_id: Option<&str>,
    request_context: &RequestContext,
) -> Result<Value, AppError> {
    if !active_window {
        return Err(AppError::invalid_argument(
            "pointer click --id requires --active-window",
        ));
    }
    if let Some(reference) = active_window_id {
        assert_active_window_id_matches(reference)?;
    }
    permissions::ensure_screen_recording_permission()?;
    let needle = id.trim();
    if needle.is_empty() {
        return Err(AppError::invalid_argument("empty element id selector"));
    }
    if is_ax_element_id(needle) {
        if let Some(result) = try_click_ax_element_id_target(needle, button)? {
            return Ok(result);
        }
    }
    let bounds = click_scope_window_bounds(request_context).ok_or_else(|| {
        AppError::target_not_found("frontmost window bounds unavailable for click --id")
    })?;
    let app = request_frontmost_app(request_context);
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
    let click_point = perform_click(&target.bounds, button)?;
    Ok(json!({
        "id": target.id.clone(),
        "text": target.text.clone(),
        "bounds": target.bounds.clone(),
        "x": click_point.x,
        "y": click_point.y,
        "source": target.source.clone()
    }))
}

fn is_ax_element_id(id: &str) -> bool {
    id.starts_with("axid_") || id.starts_with("axp_")
}

fn center_point(bounds: &desktop_core::protocol::Bounds) -> Point {
    let x = (bounds.x + bounds.width * 0.5).round().max(0.0) as u32;
    let y = (bounds.y + bounds.height * 0.5).round().max(0.0) as u32;
    Point::new(x, y)
}

fn resolve_element_id_target(
    id: &str,
    active_window: bool,
    active_window_id: Option<&str>,
    request_context: &RequestContext,
) -> Result<TokenizeClickElementCandidate, AppError> {
    if let Some(reference) = active_window_id {
        assert_active_window_id_matches(reference)?;
    }
    let needle = id.trim();
    if needle.is_empty() {
        return Err(AppError::invalid_argument("empty element id selector"));
    }

    if let Some(target) = resolve_ax_element_id_target(needle)? {
        return Ok(target);
    }

    let bounds = if active_window {
        let target = if let Some(reference) = active_window_id {
            assert_active_window_id_matches(reference)?
        } else {
            resolve_active_window_target()?
        };
        target.bounds
    } else {
        click_scope_window_bounds(request_context).ok_or_else(|| {
            AppError::target_not_found("frontmost window bounds unavailable for element id lookup")
        })?
    };
    let app = request_frontmost_app(request_context);
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
        "element_id_lookup:candidates id=\"{}\" total={} matched={}",
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
    Ok(matches[0].clone())
}

fn resolve_ax_element_id_target(
    needle: &str,
) -> Result<Option<TokenizeClickElementCandidate>, AppError> {
    let ax_elements = match platform::ax::collect_frontmost_window_elements() {
        Ok(items) => items,
        Err(err) => {
            trace::log(format!("element_id_lookup:ax_warn {err}"));
            return Ok(None);
        }
    };
    if ax_elements.is_empty() {
        return Ok(None);
    }

    let mut elements: Vec<desktop_core::protocol::TokenizeElement> = ax_elements
        .iter()
        .map(|ax| {
            vision::element_normalizer::ElementBuilder::new()
                .id(vision::ax_merge::primary_id_for_ax(ax))
                .kind("")
                .bbox(ax.bounds.clone())
                .has_border(None)
                .text(ax.text.clone())
                .confidence(None)
                .checked(ax.checked)
                .source(format!("accessibility_ax:{}", ax.role))
                .build()
        })
        .collect();
    vision::element_normalizer::finalize_elements(&mut elements);
    let candidates: Vec<TokenizeClickElementCandidate> = elements
        .into_iter()
        .map(|element| TokenizeClickElementCandidate {
            id: element.id,
            text: element.text,
            bounds: desktop_core::protocol::Bounds {
                x: element.bbox[0],
                y: element.bbox[1],
                width: element.bbox[2],
                height: element.bbox[3],
            },
            source: element.source,
        })
        .collect();
    let total_candidates = candidates.len();
    let matches: Vec<TokenizeClickElementCandidate> = candidates
        .into_iter()
        .filter(|element| element.id == needle)
        .collect();
    trace::log(format!(
        "element_id_lookup:ax_candidates id=\"{}\" total={} matched={}",
        compact_for_log(needle),
        total_candidates,
        matches.len()
    ));
    if matches.is_empty() {
        return Ok(None);
    }
    if matches.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple AX elements matched id \"{needle}\""
        )));
    }
    Ok(Some(matches[0].clone()))
}

fn try_click_ax_element_id_target(
    needle: &str,
    button: PointerButton,
) -> Result<Option<Value>, AppError> {
    let ax_elements = match platform::ax::collect_frontmost_window_elements() {
        Ok(items) => items,
        Err(err) => {
            trace::log(format!("pointer_click_id:ax_direct_warn {err}"));
            return Ok(None);
        }
    };
    if ax_elements.is_empty() {
        return Ok(None);
    }

    let mut elements: Vec<desktop_core::protocol::TokenizeElement> = ax_elements
        .iter()
        .map(|ax| {
            vision::element_normalizer::ElementBuilder::new()
                .id(vision::ax_merge::primary_id_for_ax(ax))
                .kind("")
                .bbox(ax.bounds.clone())
                .has_border(None)
                .text(ax.text.clone())
                .confidence(None)
                .checked(ax.checked)
                .source(format!("accessibility_ax:{}", ax.role))
                .build()
        })
        .collect();
    vision::element_normalizer::finalize_elements(&mut elements);
    let candidates: Vec<TokenizeClickElementCandidate> = elements
        .into_iter()
        .map(|element| TokenizeClickElementCandidate {
            id: element.id,
            text: element.text,
            bounds: desktop_core::protocol::Bounds {
                x: element.bbox[0],
                y: element.bbox[1],
                width: element.bbox[2],
                height: element.bbox[3],
            },
            source: element.source,
        })
        .collect();
    let total_candidates = candidates.len();
    let matches: Vec<TokenizeClickElementCandidate> = candidates
        .into_iter()
        .filter(|element| element.id == needle)
        .collect();
    trace::log(format!(
        "pointer_click_id:ax_direct_candidates id=\"{}\" total={} matched={}",
        compact_for_log(needle),
        total_candidates,
        matches.len()
    ));
    if matches.is_empty() {
        return Ok(None);
    }
    if matches.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple AX elements matched id \"{needle}\""
        )));
    }

    let target = &matches[0];
    trace::log(format!(
        "pointer_click_id:ax_direct_selected id=\"{}\" source={} bounds=({:.1}, {:.1}, {:.1}, {:.1}) text=\"{}\"",
        compact_for_log(&target.id),
        compact_for_log(&target.source),
        target.bounds.x,
        target.bounds.y,
        target.bounds.width,
        target.bounds.height,
        compact_for_log(target.text.as_deref().unwrap_or(""))
    ));
    let click_point = perform_click(&target.bounds, button)?;
    Ok(Some(json!({
        "id": target.id.clone(),
        "text": target.text.clone(),
        "bounds": target.bounds.clone(),
        "x": click_point.x,
        "y": click_point.y,
        "source": target.source.clone()
    })))
}

fn tokenize_click_text_candidate(
    query: &str,
    window_bounds: Option<&desktop_core::protocol::Bounds>,
    request_context: &RequestContext,
) -> Result<desktop_core::protocol::SnapshotText, AppError> {
    let bounds = window_bounds.cloned().ok_or_else(|| {
        AppError::target_not_found("frontmost window bounds unavailable for tokenize fallback")
    })?;
    let app = request_frontmost_app(request_context);
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
    _image: &desktop_core::protocol::TokenizeImage,
) -> Option<desktop_core::protocol::Bounds> {
    if bbox[2] <= 0.0 || bbox[3] <= 0.0 {
        return None;
    }
    Some(desktop_core::protocol::Bounds {
        x: os_bounds.x + bbox[0],
        y: os_bounds.y + bbox[1],
        width: bbox[2],
        height: bbox[3],
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
        "frontmost_window": window_target::frontmost_window_bounds(),
        "text_envelope": text_envelope,
        "panels": panels,
        "button_like_texts": button_like
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

pub(super) fn wait_for_open_app(app_name: &str, timeout_ms: u64) -> Result<(), AppError> {
    let needle = app_name.to_lowercase();
    let start = Instant::now();
    loop {
        if let Some(frontmost) = window_target::frontmost_app_name() {
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

fn perform_click(
    bounds: &desktop_core::protocol::Bounds,
    button: PointerButton,
) -> Result<Point, AppError> {
    let center_x = (bounds.x + bounds.width / 2.0).max(0.0).round() as u32;
    let center_y = (bounds.y + bounds.height / 2.0).max(0.0).round() as u32;
    trace::log(format!(
        "perform_click:point bounds=({}, {}, {}, {}) center=({}, {})",
        bounds.x, bounds.y, bounds.width, bounds.height, center_x, center_y
    ));
    perform_click_at(center_x, center_y, button)
}

fn perform_click_at(x: u32, y: u32, button: PointerButton) -> Result<Point, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let point = Point::new(x, y);
    trace::log(format!("perform_click:move start center=({}, {})", x, y));
    backend.move_mouse(point)?;
    trace::log("perform_click:move ok");
    thread::sleep(Duration::from_millis(60));
    match button {
        PointerButton::Left => {
            trace::log("perform_click:left_click start");
            backend.left_click(point)?;
            trace::log("perform_click:left_click ok");
        }
        PointerButton::Right => {
            trace::log("perform_click:right_click start");
            backend.right_click(point)?;
            trace::log("perform_click:right_click ok");
        }
    }
    Ok(point)
}

fn append_observe_payload(result: &mut Value, observe: Option<Value>) {
    let Some(observe) = observe else {
        return;
    };
    if let Some(object) = result.as_object_mut() {
        object.insert("observe".to_string(), observe);
    }
}

fn capture_observe_start_state(options: &ObserveOptions) -> ObserveStartState {
    if !options.enabled {
        return ObserveStartState::default();
    }
    let active_window_id = resolve_active_window_target()
        .ok()
        .and_then(|window| window.window_ref.clone());
    let focused_element_id = focused_element_id_from_ax();
    ObserveStartState {
        active_window_id,
        focused_element_id,
    }
}

fn focused_element_id_from_ax() -> Option<String> {
    let ax = platform::ax::focused_frontmost_element().ok()??;
    vision::ax_merge::primary_id_for_ax(&ax).or_else(|| {
        let role = ax.role.trim().to_ascii_lowercase();
        if role.is_empty() {
            None
        } else {
            Some(format!("ax_{role}"))
        }
    })
}

fn observe_transition_state(start_state: &ObserveStartState) -> ObserveEndState {
    let active_window = resolve_active_window_target().ok();
    let active_window_id = active_window
        .as_ref()
        .and_then(|window| window.window_ref.clone());
    let focused_element_id = focused_element_id_from_ax();
    let active_window_changed = active_window_id != start_state.active_window_id;
    let focus_changed = focused_element_id != start_state.focused_element_id;
    ObserveEndState {
        focus_changed,
        focused_element_id,
        active_window_changed,
        active_window_id,
        active_window_bounds: active_window.map(|window| window.bounds),
    }
}

fn observe_after_action(
    options: &ObserveOptions,
    start_state: &ObserveStartState,
    observe_scope: Option<&desktop_core::protocol::Bounds>,
) -> Result<Option<Value>, AppError> {
    if !options.enabled {
        return Ok(None);
    }
    let start = Instant::now();
    let prev = vision::capture::capture_screen_png(None)?;
    let mut prev_thumb =
        vision::diff::thumbnail_from_rgba(&prev.image, OBSERVE_THUMB_WIDTH, OBSERVE_THUMB_HEIGHT);
    let mut last_capture = prev;
    let start_capture = last_capture.clone();
    let mut changed_any = false;
    let mut last_change_at: Option<Instant> = None;
    let mut quiet_frames = 0u32;
    let mut changed_regions: Vec<desktop_core::protocol::Bounds> = Vec::new();
    let effective_timeout_ms = options.timeout_ms.max(options.settle_ms).max(20);
    let timeout = Duration::from_millis(effective_timeout_ms);
    let mut sample_count = 0u64;
    let mut diff_ms_total = 0u64;

    loop {
        if start.elapsed() >= timeout {
            let (tokens, ax_available, ax_count) =
                observe_tokens_for_regions(&last_capture, &changed_regions);
            let raw_tokens = tokens;
            let end_state = observe_transition_state(start_state);
            let regions = normalize_observe_regions(
                &changed_regions,
                end_state.active_window_bounds.as_ref(),
            );
            let start_tokens = observe_tokens_for_regions(&start_capture, &changed_regions).0;
            let tokens_delta = normalize_observe_tokens_delta(
                diff_observe_tokens(&start_tokens, &raw_tokens),
                end_state.active_window_bounds.as_ref(),
            );
            let observe_text_dump = build_observe_tokens_delta_text_dump(&tokens_delta);
            let settle_ms = start.elapsed().as_millis() as u64;
            trace::log(format!(
                "observe:settle outcome=timeout settle_ms={} samples={} diff_ms_total={} regions={}",
                settle_ms,
                sample_count,
                diff_ms_total,
                changed_regions.len()
            ));
            return Ok(Some(json!({
                "changed": changed_any,
                "regions": regions,
                "tokens_delta": tokens_delta,
                "text_dump": observe_text_dump,
                "focus_changed": end_state.focus_changed,
                "focused_element_id": end_state.focused_element_id,
                "active_window_changed": end_state.active_window_changed,
                "active_window_id": end_state.active_window_id,
                "ax": {
                    "available": ax_available,
                    "count": ax_count
                },
                "stability": "timeout",
                "elapsed_ms": settle_ms,
                "settle_ms": settle_ms
            })));
        }
        thread::sleep(Duration::from_millis(OBSERVE_SAMPLE_INTERVAL_MS));
        let curr = vision::capture::capture_screen_png(None)?;
        let curr_thumb = vision::diff::thumbnail_from_rgba(
            &curr.image,
            OBSERVE_THUMB_WIDTH,
            OBSERVE_THUMB_HEIGHT,
        );
        sample_count += 1;
        let diff_started = Instant::now();
        let frame_regions =
            vision::diff::diff_regions(&prev_thumb, &curr_thumb, OBSERVE_DIFF_THRESHOLD);
        diff_ms_total += diff_started.elapsed().as_millis() as u64;
        let significant_regions: Vec<_> = frame_regions
            .into_iter()
            .filter(|region| {
                region.width.saturating_mul(region.height).max(1)
                    >= OBSERVE_MIN_THUMB_COMPONENT_AREA
            })
            .collect();
        if !significant_regions.is_empty() {
            changed_any = true;
            last_change_at = Some(Instant::now());
            quiet_frames = 0;
            for changed_region in significant_regions {
                let upscaled = vision::diff::upscale_region(
                    changed_region,
                    curr.frame.width,
                    curr.frame.height,
                    curr_thumb.width,
                    curr_thumb.height,
                );
                let padded = pad_bounds(upscaled, OBSERVE_REGION_PAD_PX);
                if let Some(clipped) = clip_to_scope(&padded, observe_scope) {
                    merge_region_into_list(&mut changed_regions, clipped);
                }
            }
            if options.until == ObserveUntil::FirstChange {
                let (tokens, ax_available, ax_count) =
                    observe_tokens_for_regions(&curr, &changed_regions);
                let raw_tokens = tokens;
                let end_state = observe_transition_state(start_state);
                let regions = normalize_observe_regions(
                    &changed_regions,
                    end_state.active_window_bounds.as_ref(),
                );
                let start_tokens = observe_tokens_for_regions(&start_capture, &changed_regions).0;
                let tokens_delta = normalize_observe_tokens_delta(
                    diff_observe_tokens(&start_tokens, &raw_tokens),
                    end_state.active_window_bounds.as_ref(),
                );
                let observe_text_dump = build_observe_tokens_delta_text_dump(&tokens_delta);
                let settle_ms = start.elapsed().as_millis() as u64;
                trace::log(format!(
                    "observe:settle outcome=first_change settle_ms={} samples={} diff_ms_total={} regions={}",
                    settle_ms,
                    sample_count,
                    diff_ms_total,
                    changed_regions.len()
                ));
                return Ok(Some(json!({
                    "changed": true,
                    "regions": regions,
                    "tokens_delta": tokens_delta,
                    "text_dump": observe_text_dump,
                    "focus_changed": end_state.focus_changed,
                    "focused_element_id": end_state.focused_element_id,
                    "active_window_changed": end_state.active_window_changed,
                    "active_window_id": end_state.active_window_id,
                    "ax": {
                        "available": ax_available,
                        "count": ax_count
                    },
                    "stability": "settled",
                    "elapsed_ms": settle_ms,
                    "settle_ms": settle_ms
                })));
            }
        } else {
            quiet_frames += 1;
            if changed_any {
                if quiet_frames >= OBSERVE_QUIET_FRAMES {
                    if let Some(last_change) = last_change_at {
                        if last_change.elapsed() < Duration::from_millis(options.settle_ms) {
                            last_capture = curr;
                            prev_thumb = curr_thumb;
                            continue;
                        }
                    }
                    let (tokens, ax_available, ax_count) =
                        observe_tokens_for_regions(&curr, &changed_regions);
                    let raw_tokens = tokens;
                    let end_state = observe_transition_state(start_state);
                    let regions = normalize_observe_regions(
                        &changed_regions,
                        end_state.active_window_bounds.as_ref(),
                    );
                    let start_tokens =
                        observe_tokens_for_regions(&start_capture, &changed_regions).0;
                    let tokens_delta = normalize_observe_tokens_delta(
                        diff_observe_tokens(&start_tokens, &raw_tokens),
                        end_state.active_window_bounds.as_ref(),
                    );
                    let observe_text_dump = build_observe_tokens_delta_text_dump(&tokens_delta);
                    let settle_ms = start.elapsed().as_millis() as u64;
                    trace::log(format!(
                        "observe:settle outcome=settled settle_ms={} samples={} diff_ms_total={} regions={}",
                        settle_ms,
                        sample_count,
                        diff_ms_total,
                        changed_regions.len()
                    ));
                    return Ok(Some(json!({
                        "changed": true,
                        "regions": regions,
                        "tokens_delta": tokens_delta,
                        "text_dump": observe_text_dump,
                        "focus_changed": end_state.focus_changed,
                        "focused_element_id": end_state.focused_element_id,
                        "active_window_changed": end_state.active_window_changed,
                        "active_window_id": end_state.active_window_id,
                        "ax": {
                            "available": ax_available,
                            "count": ax_count
                        },
                        "stability": "settled",
                        "elapsed_ms": settle_ms,
                        "settle_ms": settle_ms
                    })));
                }
            } else if options.until == ObserveUntil::Stable && quiet_frames >= OBSERVE_QUIET_FRAMES
            {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                if elapsed_ms < options.settle_ms {
                    last_capture = curr;
                    prev_thumb = curr_thumb;
                    continue;
                }
                let end_state = observe_transition_state(start_state);
                let tokens_delta = json!({
                    "added": [],
                    "removed": [],
                    "changed": []
                });
                let observe_text_dump = build_observe_tokens_delta_text_dump(&tokens_delta);
                trace::log(format!(
                    "observe:settle outcome=no_change settle_ms={} samples={} diff_ms_total={} regions={}",
                    elapsed_ms,
                    sample_count,
                    diff_ms_total,
                    changed_regions.len()
                ));
                return Ok(Some(json!({
                    "changed": false,
                    "regions": [],
                    "tokens_delta": tokens_delta,
                    "text_dump": observe_text_dump,
                    "focus_changed": end_state.focus_changed,
                    "focused_element_id": end_state.focused_element_id,
                    "active_window_changed": end_state.active_window_changed,
                    "active_window_id": end_state.active_window_id,
                    "ax": {
                        "available": false,
                        "count": 0
                    },
                    "stability": "no_change",
                    "elapsed_ms": elapsed_ms,
                    "settle_ms": elapsed_ms
                })));
            }
        }
        last_capture = curr;
        prev_thumb = curr_thumb;
    }
}

fn observe_tokens_for_regions(
    capture: &vision::types::CapturedImage,
    regions: &[desktop_core::protocol::Bounds],
) -> (Vec<Value>, bool, usize) {
    let observe_started = Instant::now();
    let mut tokens: Vec<Value> = Vec::new();
    let (ax_available, ax_elements) = match platform::ax::collect_frontmost_window_elements() {
        Ok(items) => (true, items),
        Err(_) => (false, Vec::new()),
    };

    let mut ocr_regions = Vec::new();
    let mut ocr_tokens = 0usize;
    let ocr_started = Instant::now();
    if !regions.is_empty() {
        for (idx, core_region) in regions.iter().enumerate() {
            let dynamic_pad = observe_adaptive_ocr_pad(core_region, &ax_elements);
            let (padded, applied_pad) = expand_bounds_with_pad_clamped(
                core_region,
                dynamic_pad,
                capture.frame.width as f64,
                capture.frame.height as f64,
            );
            if let Some((x0, y0, x1, y1)) = logical_bounds_to_image_rect(
                &padded,
                capture.image.width(),
                capture.image.height(),
                capture.frame.width,
                capture.frame.height,
            ) {
                let crop_w = (x1 - x0).max(0) as u32;
                let crop_h = (y1 - y0).max(0) as u32;
                if crop_w <= 1 || crop_h <= 1 {
                    continue;
                }
                let crop =
                    image::imageops::crop_imm(&capture.image, x0 as u32, y0 as u32, crop_w, crop_h)
                        .to_image();
                dump_observe_region_screenshot(
                    &crop,
                    capture.frame.snapshot_id,
                    idx,
                    core_region,
                    &padded,
                );
                if let Ok(texts) = vision::ocr::recognize_text(&crop) {
                    let sx = padded.width.max(1.0) / crop_w.max(1) as f64;
                    let sy = padded.height.max(1.0) / crop_h.max(1) as f64;
                    let mut emitted = 0usize;
                    for text in texts {
                        if text.text.trim().is_empty() {
                            continue;
                        }
                        let logical_bounds = desktop_core::protocol::Bounds {
                            x: padded.x + text.bounds.x * sx,
                            y: padded.y + text.bounds.y * sy,
                            width: text.bounds.width * sx,
                            height: text.bounds.height * sy,
                        };
                        // Keep only OCR boxes that overlap the core changed region.
                        if iou(core_region, &logical_bounds) <= 0.01 {
                            continue;
                        }
                        tokens.push(json!({
                            "source": "vision_ocr",
                            "text": text.text,
                            "confidence": text.confidence,
                            "bbox": [logical_bounds.x, logical_bounds.y, logical_bounds.width, logical_bounds.height]
                        }));
                        emitted += 1;
                    }
                    ocr_tokens += emitted;
                    ocr_regions.push(format!(
                        "#{idx}:core=({:.0},{:.0},{:.0},{:.0}) pad_req={:.1} pad_applied=(l:{:.1},r:{:.1},t:{:.1},b:{:.1}) pad=({:.0},{:.0},{:.0},{:.0}) crop={}x{} emitted={}",
                        core_region.x,
                        core_region.y,
                        core_region.width,
                        core_region.height,
                        dynamic_pad,
                        applied_pad.left,
                        applied_pad.right,
                        applied_pad.top,
                        applied_pad.bottom,
                        padded.x,
                        padded.y,
                        padded.width,
                        padded.height,
                        crop_w,
                        crop_h,
                        emitted
                    ));
                }
            }
        }
    }
    let ocr_elapsed = ocr_started.elapsed().as_millis() as u64;
    trace::log(format!(
        "observe:ocr elapsed_ms={} regions={} tokens={} details={}",
        ocr_elapsed,
        regions.len(),
        ocr_tokens,
        ocr_regions.join(" | ")
    ));

    let ax_started = Instant::now();
    let mut ax_count = 0usize;
    for ax in ax_elements {
        if !regions.is_empty() && !regions.iter().any(|region| iou(region, &ax.bounds) > 0.01) {
            continue;
        }
        let id = vision::ax_merge::primary_id_for_ax(&ax)
            .unwrap_or_else(|| format!("ax_{}", ax.role.to_ascii_lowercase()));
        tokens.push(json!({
            "id": id,
            "source": format!("accessibility_ax:{}", ax.role),
            "text": ax.text,
            "checked": ax.checked,
            "bbox": [ax.bounds.x, ax.bounds.y, ax.bounds.width, ax.bounds.height]
        }));
        ax_count += 1;
    }
    let ax_elapsed = ax_started.elapsed().as_millis() as u64;
    let total_elapsed = observe_started.elapsed().as_millis() as u64;
    trace::log(format!(
        "observe:tokens elapsed_ms={} ocr_ms={} ax_ms={} total_tokens={} ax_count={}",
        total_elapsed,
        ocr_elapsed,
        ax_elapsed,
        tokens.len(),
        ax_count
    ));

    (tokens, ax_available, ax_count)
}

#[derive(Debug, Clone, Copy)]
struct AppliedPadding {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

fn expand_bounds_with_pad_clamped(
    core: &desktop_core::protocol::Bounds,
    pad: f64,
    frame_width: f64,
    frame_height: f64,
) -> (desktop_core::protocol::Bounds, AppliedPadding) {
    let core_x1 = core.x.max(0.0);
    let core_y1 = core.y.max(0.0);
    let core_x2 = (core.x + core.width).max(core_x1);
    let core_y2 = (core.y + core.height).max(core_y1);

    let x1 = (core_x1 - pad).max(0.0).min(frame_width);
    let y1 = (core_y1 - pad).max(0.0).min(frame_height);
    let x2 = (core_x2 + pad).min(frame_width).max(0.0);
    let y2 = (core_y2 + pad).min(frame_height).max(0.0);

    let applied = AppliedPadding {
        left: (core_x1 - x1).max(0.0),
        right: (x2 - core_x2).max(0.0),
        top: (core_y1 - y1).max(0.0),
        bottom: (y2 - core_y2).max(0.0),
    };
    (
        desktop_core::protocol::Bounds {
            x: x1,
            y: y1,
            width: (x2 - x1).max(0.0),
            height: (y2 - y1).max(0.0),
        },
        applied,
    )
}

fn dump_observe_region_screenshot(
    image: &RgbaImage,
    snapshot_id: u64,
    region_idx: usize,
    core_region: &desktop_core::protocol::Bounds,
    padded_region: &desktop_core::protocol::Bounds,
) {
    let dir = PathBuf::from("/tmp/desktopctl-observe-crops");
    if let Err(err) = fs::create_dir_all(&dir) {
        trace::log(format!("observe:dump mkdir_failed err={err}"));
        return;
    }
    let file_name = format!(
        "snap{}_r{}_core_{}_{}_{}_{}_pad_{}_{}_{}_{}.png",
        snapshot_id,
        region_idx,
        round_nonnegative_i64(core_region.x),
        round_nonnegative_i64(core_region.y),
        round_nonnegative_i64(core_region.width),
        round_nonnegative_i64(core_region.height),
        round_nonnegative_i64(padded_region.x),
        round_nonnegative_i64(padded_region.y),
        round_nonnegative_i64(padded_region.width),
        round_nonnegative_i64(padded_region.height)
    );
    let out_path = dir.join(file_name);
    if let Err(err) = image.save_with_format(&out_path, ImageFormat::Png) {
        trace::log(format!(
            "observe:dump write_failed path={} err={err}",
            out_path.display()
        ));
    }
}

fn observe_adaptive_ocr_pad(
    core_region: &desktop_core::protocol::Bounds,
    ax_elements: &[platform::ax::AxElement],
) -> f64 {
    let mut dims: Vec<f64> = ax_elements
        .iter()
        .filter(|ax| {
            iou(core_region, &ax.bounds) > 0.01
                || iou(&inflate_bounds(core_region, 100.0), &ax.bounds) > 0.01
        })
        .filter(|ax| {
            matches!(
                ax.role.as_str(),
                "AXTextField"
                    | "AXTextArea"
                    | "AXButton"
                    | "AXCheckBox"
                    | "AXRadioButton"
                    | "AXPopUpButton"
            )
        })
        .map(|ax| ax.bounds.width.min(ax.bounds.height))
        .filter(|dim| *dim >= 8.0 && *dim <= 240.0)
        .collect();
    if dims.is_empty() {
        return OBSERVE_OCR_PAD_PX;
    }
    dims.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min_dim = dims[0];
    (min_dim * 1.5).clamp(16.0, 96.0)
}

fn clip_to_scope(
    bounds: &desktop_core::protocol::Bounds,
    scope: Option<&desktop_core::protocol::Bounds>,
) -> Option<desktop_core::protocol::Bounds> {
    let Some(scope) = scope else {
        return Some(bounds.clone());
    };
    let x1 = bounds.x.max(scope.x);
    let y1 = bounds.y.max(scope.y);
    let x2 = (bounds.x + bounds.width).min(scope.x + scope.width);
    let y2 = (bounds.y + bounds.height).min(scope.y + scope.height);
    let w = x2 - x1;
    let h = y2 - y1;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some(desktop_core::protocol::Bounds {
        x: x1,
        y: y1,
        width: w,
        height: h,
    })
}

fn merge_region_into_list(
    regions: &mut Vec<desktop_core::protocol::Bounds>,
    incoming: desktop_core::protocol::Bounds,
) {
    for region in regions.iter_mut() {
        if iou(region, &incoming) > 0.0 {
            let merged = merge_bounds(Some(&region.clone()), &incoming);
            *region = merged;
            return;
        }
    }
    regions.push(incoming);
}

fn pad_bounds(bounds: desktop_core::protocol::Bounds, pad: f64) -> desktop_core::protocol::Bounds {
    desktop_core::protocol::Bounds {
        x: (bounds.x - pad).max(0.0),
        y: (bounds.y - pad).max(0.0),
        width: bounds.width + pad * 2.0,
        height: bounds.height + pad * 2.0,
    }
}

fn diff_observe_tokens(before: &[Value], after: &[Value]) -> Value {
    use std::collections::{HashMap, HashSet};
    let mut before_map: HashMap<String, &Value> = HashMap::new();
    let mut after_map: HashMap<String, &Value> = HashMap::new();
    for token in before {
        before_map.insert(observe_token_key(token), token);
    }
    for token in after {
        after_map.insert(observe_token_key(token), token);
    }

    let mut added: Vec<Value> = Vec::new();
    let mut removed: Vec<Value> = Vec::new();
    let mut changed: Vec<Value> = Vec::new();
    let before_keys: HashSet<String> = before_map.keys().cloned().collect();
    let after_keys: HashSet<String> = after_map.keys().cloned().collect();

    for key in after_keys.difference(&before_keys) {
        if let Some(token) = after_map.get(key) {
            added.push((*token).clone());
        }
    }
    for key in before_keys.difference(&after_keys) {
        if let Some(token) = before_map.get(key) {
            removed.push((*token).clone());
        }
    }
    for key in before_keys.intersection(&after_keys) {
        let Some(before_token) = before_map.get(key) else {
            continue;
        };
        let Some(after_token) = after_map.get(key) else {
            continue;
        };
        if !observe_token_semantic_equal(before_token, after_token) {
            changed.push(json!({
                "before": (*before_token).clone(),
                "after": (*after_token).clone()
            }));
        }
    }

    json!({
        "added": added,
        "removed": removed,
        "changed": changed
    })
}

fn normalize_observe_regions(
    regions: &[desktop_core::protocol::Bounds],
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Vec<Value> {
    regions
        .iter()
        .map(|bounds| relative_bounds_json(bounds, origin))
        .collect()
}

fn normalize_observe_tokens_delta(
    mut delta: Value,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Value {
    for key in ["added", "removed"] {
        if let Some(items) = delta.get_mut(key).and_then(Value::as_array_mut) {
            for token in items {
                rewrite_token_bbox_relative(token, origin);
            }
        }
    }
    if let Some(items) = delta.get_mut("changed").and_then(Value::as_array_mut) {
        for entry in items {
            if let Some(before) = entry.get_mut("before") {
                rewrite_token_bbox_relative(before, origin);
            }
            if let Some(after) = entry.get_mut("after") {
                rewrite_token_bbox_relative(after, origin);
            }
        }
    }
    delta
}

fn build_observe_tokens_delta_text_dump(tokens_delta: &Value) -> String {
    let mut sections = Vec::new();
    sections.push(format_observe_token_dump_section(
        "added",
        tokens_delta.get("added").and_then(Value::as_array),
    ));
    sections.push(format_observe_token_dump_section(
        "removed",
        tokens_delta.get("removed").and_then(Value::as_array),
    ));
    let (changed_before, changed_after) = observe_changed_token_sides(tokens_delta);
    sections.push(format_observe_token_dump_section(
        "changed_before",
        Some(&changed_before),
    ));
    sections.push(format_observe_token_dump_section(
        "changed_after",
        Some(&changed_after),
    ));
    sections.join("\n\n")
}

fn format_observe_token_dump_section(label: &str, items: Option<&Vec<Value>>) -> String {
    let mut lines = vec![format!("{label}:"), "---".to_string()];
    if let Some(values) = items {
        let dump = build_window_text_dump(values);
        if !dump.trim().is_empty() {
            lines.push(dump);
        }
    }
    if lines.len() == 2 {
        lines.push("(none)".to_string());
    }
    lines.join("\n")
}

fn observe_changed_token_sides(tokens_delta: &Value) -> (Vec<Value>, Vec<Value>) {
    let mut before = Vec::new();
    let mut after = Vec::new();
    let Some(changed) = tokens_delta.get("changed").and_then(Value::as_array) else {
        return (before, after);
    };
    for entry in changed {
        if let Some(item) = entry.get("before") {
            before.push(item.clone());
        }
        if let Some(item) = entry.get("after") {
            after.push(item.clone());
        }
    }
    (before, after)
}

fn rewrite_token_bbox_relative(token: &mut Value, origin: Option<&desktop_core::protocol::Bounds>) {
    let Some(bbox) = token.get("bbox").and_then(Value::as_array) else {
        return;
    };
    if bbox.len() != 4 {
        return;
    }
    let x = bbox[0].as_f64().unwrap_or(0.0);
    let y = bbox[1].as_f64().unwrap_or(0.0);
    let w = bbox[2].as_f64().unwrap_or(0.0);
    let h = bbox[3].as_f64().unwrap_or(0.0);
    let rel = relative_bounds(
        &desktop_core::protocol::Bounds {
            x,
            y,
            width: w,
            height: h,
        },
        origin,
    );
    if let Some(obj) = token.as_object_mut() {
        obj.insert(
            "bbox".to_string(),
            json!([
                round_nonnegative_i64(rel.x),
                round_nonnegative_i64(rel.y),
                round_nonnegative_i64(rel.width),
                round_nonnegative_i64(rel.height)
            ]),
        );
    }
}

fn relative_bounds_json(
    bounds: &desktop_core::protocol::Bounds,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Value {
    let rel = relative_bounds(bounds, origin);
    json!({
        "x": round_nonnegative_i64(rel.x),
        "y": round_nonnegative_i64(rel.y),
        "width": round_nonnegative_i64(rel.width),
        "height": round_nonnegative_i64(rel.height)
    })
}

fn relative_bounds(
    bounds: &desktop_core::protocol::Bounds,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> desktop_core::protocol::Bounds {
    let mut out = bounds.clone();
    if let Some(window) = origin {
        out.x -= window.x;
        out.y -= window.y;
    }
    out.x = out.x.max(0.0);
    out.y = out.y.max(0.0);
    out.width = out.width.max(0.0);
    out.height = out.height.max(0.0);
    out
}

fn round_nonnegative_i64(value: f64) -> i64 {
    value.round().max(0.0) as i64
}

fn observe_token_key(token: &Value) -> String {
    if let Some(id) = token.get("id").and_then(Value::as_str) {
        if !id.trim().is_empty() {
            return format!("id:{id}");
        }
    }
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let text = token.get("text").and_then(Value::as_str).unwrap_or("");
    let bbox_key = quantized_bbox_key(token.get("bbox").and_then(Value::as_array));
    format!("fallback:{source}:{text}:{bbox_key}")
}

fn quantized_bbox_key(bbox: Option<&Vec<Value>>) -> String {
    let Some(bbox) = bbox else {
        return "[]".to_string();
    };
    if bbox.len() != 4 {
        return "[]".to_string();
    }
    let q = |v: Option<f64>| -> i64 {
        let n = v.unwrap_or(0.0);
        // Tolerate small OCR jitter by quantizing to 8px grid.
        (n / 8.0).round() as i64
    };
    let x = q(bbox[0].as_f64());
    let y = q(bbox[1].as_f64());
    let w = q(bbox[2].as_f64());
    let h = q(bbox[3].as_f64());
    format!("{x},{y},{w},{h}")
}

fn observe_token_semantic_equal(a: &Value, b: &Value) -> bool {
    let a_text = a.get("text").cloned().unwrap_or(Value::Null);
    let b_text = b.get("text").cloned().unwrap_or(Value::Null);
    let a_bbox = a.get("bbox").cloned().unwrap_or_else(|| json!([]));
    let b_bbox = b.get("bbox").cloned().unwrap_or_else(|| json!([]));
    let a_source = a.get("source").cloned().unwrap_or(Value::Null);
    let b_source = b.get("source").cloned().unwrap_or(Value::Null);
    let a_checked = a.get("checked").cloned().unwrap_or(Value::Null);
    let b_checked = b.get("checked").cloned().unwrap_or(Value::Null);
    a_text == b_text && a_bbox == b_bbox && a_source == b_source && a_checked == b_checked
}

fn merge_bounds(
    existing: Option<&desktop_core::protocol::Bounds>,
    incoming: &desktop_core::protocol::Bounds,
) -> desktop_core::protocol::Bounds {
    let Some(existing) = existing else {
        return incoming.clone();
    };
    let x1 = existing.x.min(incoming.x);
    let y1 = existing.y.min(incoming.y);
    let x2 = (existing.x + existing.width).max(incoming.x + incoming.width);
    let y2 = (existing.y + existing.height).max(incoming.y + incoming.height);
    desktop_core::protocol::Bounds {
        x: x1,
        y: y1,
        width: (x2 - x1).max(1.0),
        height: (y2 - y1).max(1.0),
    }
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

fn click_scope_window_bounds(
    request_context: &RequestContext,
) -> Option<desktop_core::protocol::Bounds> {
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
    let bounds = request_frontmost_bounds(request_context);
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
                window_ref: None,
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
                    text_truncated: None,
                    confidence: None,
                    scrollable: None,
                    checked: None,
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
                window_ref: None,
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
                    text_truncated: None,
                    confidence: Some(0.9),
                    scrollable: None,
                    checked: None,
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
                window_ref: None,
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
                    text_truncated: None,
                    confidence: Some(0.99),
                    scrollable: None,
                    checked: None,
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

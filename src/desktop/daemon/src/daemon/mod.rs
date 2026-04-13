#[cfg(windows)]
use std::io::Read;
#[cfg(windows)]
use std::net::{TcpListener, TcpStream};
#[cfg(unix)]
use std::os::unix::{
    fs::PermissionsExt,
    net::{UnixListener, UnixStream},
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    sync::{
        Arc, Mutex, MutexGuard, OnceLock,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

#[cfg(unix)]
use desktop_core::ipc::socket_path;
#[cfg(windows)]
use desktop_core::ipc::{socket_addr, windows_ipc_token_path};
use desktop_core::{
    automation::{Point, new_backend},
    error::AppError,
    ipc::{read_framed_json, write_framed_json},
    protocol::{
        Command, ObserveOptions, ObserveUntil, PointerButton, RequestEnvelope, ResponseEnvelope,
    },
};
use serde_json::{Value, json};

mod click_ops;
mod commands;
mod geometry;
mod guards;
mod observe_pipeline;
mod platform_runtime;
mod query_ops;
mod recording;
mod replay;
mod screen_image;
mod text_match;
mod window_context;
mod window_refs;
mod window_target;

#[allow(unused_imports)]
use click_ops::{
    center_point, click_element_id_target, click_scope_window_bounds, click_text_target,
    resolve_element_id_target, tokenize_payload_elements_for_click,
    tokenize_payload_texts_for_click,
};
use geometry::{bounds_intersect, inflate_bounds, iou, merge_bounds};
use observe_pipeline::{
    append_observe_payload, capture_observe_start_state, observe_after_action,
    observe_seed_tokens_from_tokenize_payload,
};
#[cfg(test)]
use query_ops::infer_panels_from_texts;
use query_ops::{find_text_targets, wait_for_text};
#[cfg(test)]
use screen_image::overlay_path_for_capture;
#[cfg(test)]
use screen_image::{estimate_toggle_state, logical_point_to_image_point};
use screen_image::{logical_bounds_to_image_rect, write_capture_overlay};
use text_match::{compact_for_log, ranked_text_candidates, select_text_candidate};
use window_context::{
    append_tokenize_new_window_hint, assert_active_window_id_matches, attach_window_ref_to_payload,
    backfill_tokenize_window_positions, bind_active_window_reference,
    collect_tokenize_new_window_hint_snapshot,
    collect_tokenize_new_window_hint_snapshot_from_windows, enrich_window_refs,
    remap_tokenize_window_id_field, resolve_active_window_target, resolve_observe_scope_bounds,
};

#[cfg(target_os = "macos")]
use crate::overlay;
use crate::platform::permissions;
use crate::{app_policy, platform, request_store, trace, vision};

const MAX_CONCURRENT_CLIENTS: usize = 16;
const COMMAND_QUEUE_TIMEOUT: Duration = Duration::from_secs(5);
const COMMAND_QUEUE_POLL_INTERVAL: Duration = Duration::from_millis(10);
#[cfg(target_os = "macos")]
static PRIVACY_OVERLAY_ACTIVE: AtomicBool = AtomicBool::new(false);

static COMMAND_EXECUTION_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct CommandExecutionGuard<'a> {
    _guard: MutexGuard<'a, ()>,
}

fn command_execution_lock() -> &'static Mutex<()> {
    COMMAND_EXECUTION_LOCK.get_or_init(|| Mutex::new(()))
}

fn acquire_command_execution_slot() -> Result<CommandExecutionGuard<'static>, AppError> {
    let lock = command_execution_lock();
    let queued_at = Instant::now();
    loop {
        match lock.try_lock() {
            Ok(guard) => return Ok(CommandExecutionGuard { _guard: guard }),
            Err(std::sync::TryLockError::Poisoned(_)) => {
                return Err(AppError::internal("command execution lock poisoned"));
            }
            Err(std::sync::TryLockError::WouldBlock) => {
                if queued_at.elapsed() >= COMMAND_QUEUE_TIMEOUT {
                    return Err(AppError::timeout(format!(
                        "command queue timeout after {} ms; another command is still running",
                        COMMAND_QUEUE_TIMEOUT.as_millis()
                    )));
                }
                thread::sleep(COMMAND_QUEUE_POLL_INTERVAL);
            }
        }
    }
}
const OBSERVE_SAMPLE_INTERVAL_MS: u64 = 40;
const OBSERVE_QUIET_FRAMES: u32 = 2;
const OBSERVE_DIFF_THRESHOLD: u8 = 8;
const OBSERVE_THUMB_WIDTH: u32 = 96;
const OBSERVE_THUMB_HEIGHT: u32 = 54;
const OBSERVE_REGION_PAD_PX: f64 = 14.0;
const OBSERVE_MIN_THUMB_COMPONENT_AREA: u32 = 4;
const OBSERVE_FINAL_REGION_PAD_PX: f64 = 4.0;
const OBSERVE_FINAL_MIN_COMPONENT_AREA: u32 = 96;
const OBSERVE_OCR_PAD_PX: f64 = 40.0;
const OBSERVE_FINAL_MERGE_GAP_PX: f64 = 48.0;
const OBSERVE_HIGH_CHANGE_RATIO: f64 = 0.18;
const OBSERVE_COARSE_MERGE_GAP_PX: f64 = 120.0;
const OBSERVE_COARSE_REGION_PAD_PX: f64 = 20.0;
const OBSERVE_COARSE_MAX_REGIONS: usize = 4;
const OBSERVE_MAX_FINAL_REGIONS: usize = 8;
const OBSERVE_MAX_OCR_REGIONS: usize = 24;
static GUI_OPS_DISABLED: AtomicBool = AtomicBool::new(false);
static GUI_OPS_STATE_HOOK: OnceLock<fn(bool)> = OnceLock::new();
static TOKENIZE_WINDOW_HINT_STATE: OnceLock<Mutex<HashMap<String, HashSet<String>>>> =
    OnceLock::new();
#[cfg(windows)]
static WINDOWS_IPC_AUTH_TOKEN: OnceLock<String> = OnceLock::new();
#[cfg(unix)]
type IpcListener = UnixListener;
#[cfg(unix)]
type IpcStream = UnixStream;
#[cfg(windows)]
type IpcListener = TcpListener;
#[cfg(windows)]
type IpcStream = TcpStream;

type TokenizeHintWindow = (String, String, bool, Option<String>);

#[derive(Debug, Clone)]
pub(crate) struct TokenizeHintSnapshot {
    context: Option<String>,
    state_key: String,
    current_windows: Vec<TokenizeHintWindow>,
}

#[derive(Debug, Clone, Default)]
struct RequestContext {
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
    let outcome = app_policy::reload_current_from_disk();
    set_gui_ops_disabled(outcome.config.agent_access_disabled);
    platform_runtime::bootstrap_overlay_glow();
    let listener = bind_listener()?;
    thread::spawn(move || {
        if let Err(err) = accept_loop(listener, config) {
            eprintln!("daemon loop error: {err}");
        }
    });
    Ok(())
}

pub fn run_blocking(config: DaemonConfig) -> Result<(), AppError> {
    let outcome = app_policy::reload_current_from_disk();
    set_gui_ops_disabled(outcome.config.agent_access_disabled);
    platform_runtime::bootstrap_overlay_glow();
    let listener = bind_listener()?;
    accept_loop(listener, config)
}

pub fn gui_ops_disabled() -> bool {
    GUI_OPS_DISABLED.load(Ordering::SeqCst)
}

pub fn register_gui_ops_state_hook(hook: fn(bool)) {
    let _ = GUI_OPS_STATE_HOOK.set(hook);
}

pub fn set_gui_ops_disabled(disabled: bool) -> bool {
    let previous = GUI_OPS_DISABLED.swap(disabled, Ordering::SeqCst);
    if previous != disabled {
        if let Err(err) = app_policy::set_agent_access_disabled(disabled) {
            eprintln!("app policy: failed to persist agent_access_disabled={disabled}: {err}");
        }
        if let Some(hook) = GUI_OPS_STATE_HOOK.get() {
            hook(disabled);
        }
    }
    previous
}

fn bind_listener() -> Result<IpcListener, AppError> {
    #[cfg(unix)]
    {
        let path = socket_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| {
                AppError::backend_unavailable(format!(
                    "create socket directory {} failed: {err}",
                    parent.display()
                ))
            })?;
            fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|err| {
                AppError::backend_unavailable(format!(
                    "set socket directory permissions failed: {err}"
                ))
            })?;
        }
        if path.exists() {
            let _ = fs::remove_file(&path);
        }

        let listener = UnixListener::bind(&path).map_err(|err| {
            AppError::backend_unavailable(format!("bind {} failed: {err}", path.display()))
        })?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).map_err(|err| {
            AppError::backend_unavailable(format!("set socket permissions failed: {err}"))
        })?;
        listener.set_nonblocking(true).map_err(|err| {
            AppError::backend_unavailable(format!("set nonblocking failed: {err}"))
        })?;
        trace::log(format!("listener:bound socket={}", path.display()));
        return Ok(listener);
    }

    #[cfg(windows)]
    {
        let _ = ensure_windows_ipc_auth_token()?;
        let addr = socket_addr();
        let listener = TcpListener::bind(&addr)
            .map_err(|err| AppError::backend_unavailable(format!("bind {addr} failed: {err}")))?;
        listener.set_nonblocking(true).map_err(|err| {
            AppError::backend_unavailable(format!("set nonblocking failed: {err}"))
        })?;
        trace::log(format!("listener:bound addr={addr}"));
        return Ok(listener);
    }

    #[allow(unreachable_code)]
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

fn accept_loop(listener: IpcListener, config: DaemonConfig) -> Result<(), AppError> {
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
                let active = active_clients.load(Ordering::SeqCst);
                if active >= MAX_CONCURRENT_CLIENTS {
                    trace::log(format!(
                        "accept:client_rejected too_many_active_clients active={} max={}",
                        active, MAX_CONCURRENT_CLIENTS
                    ));
                    continue;
                }
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

    #[cfg(unix)]
    {
        let path = socket_path();
        if path.exists() {
            let _ = fs::remove_file(path);
        }
    }
    trace::log("listener:closed");
    Ok(())
}

fn handle_client(mut stream: IpcStream) -> Result<(), AppError> {
    #[cfg(windows)]
    validate_windows_ipc_auth(&mut stream)?;
    let request: RequestEnvelope = read_framed_json(&mut stream)?;
    let request_started = Instant::now();
    let request_id = request.request_id.clone();
    let command = request.command.clone();
    let command_name = command.name().to_string();
    trace::log(format!(
        "client:request_start request_id={} command={}",
        request_id, command_name
    ));
    let request_context = RequestContext {
        frontmost: if platform_runtime::command_requires_frontmost_snapshot(&command) {
            Some(window_target::resolve_frontmost_snapshot())
        } else {
            None
        },
    };
    let response = if let Err(err) = enforce_gui_ops_enabled(&command) {
        trace::log(format!(
            "client:gui_disabled_block request_id={} command={} msg={}",
            request_id, command_name, err.message
        ));
        ResponseEnvelope::from_error(request_id.clone(), command_name.clone(), err)
    } else if let Err(err) = enforce_frontmost_app_policy(&command, &request_context) {
        trace::log(format!(
            "client:policy_block request_id={} command={} msg={}",
            request_id, command_name, err.message
        ));
        ResponseEnvelope::from_error(request_id.clone(), command_name.clone(), err)
    } else {
        let runtime_state = platform_runtime::begin_command(&command, &request_context);
        let response = match acquire_command_execution_slot() {
            Ok(_slot) => match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                execute_with_context(
                    command,
                    runtime_state.overlay_token_updates_enabled,
                    &request_context,
                )
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
                    ResponseEnvelope::from_error(request_id.clone(), command_name.clone(), err)
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
                    ResponseEnvelope::from_error(request_id.clone(), command_name.clone(), err)
                }
            },
            Err(err) => {
                trace::log(format!(
                    "client:queue_timeout request_id={} command={} msg={}",
                    request_id, command_name, err.message
                ));
                ResponseEnvelope::from_error(request_id.clone(), command_name.clone(), err)
            }
        };
        platform_runtime::end_command(runtime_state);
        response
    };
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
    trace::log(format!(
        "client:request_timing request_id={} command={} total_ms={}",
        request_id,
        command_name,
        request_started.elapsed().as_millis()
    ));
    Ok(())
}

#[cfg(windows)]
fn ensure_windows_ipc_auth_token() -> Result<&'static str, AppError> {
    if let Some(token) = WINDOWS_IPC_AUTH_TOKEN.get() {
        return Ok(token.as_str());
    }

    let token = uuid::Uuid::new_v4().to_string();
    let path = windows_ipc_token_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to create IPC token directory {}: {err}",
                parent.display()
            ))
        })?;
    }
    fs::write(&path, format!("{token}\n")).map_err(|err| {
        AppError::backend_unavailable(format!(
            "failed to persist IPC token to {}: {err}",
            path.display()
        ))
    })?;

    let _ = WINDOWS_IPC_AUTH_TOKEN.set(token);
    WINDOWS_IPC_AUTH_TOKEN
        .get()
        .map(|token| token.as_str())
        .ok_or_else(|| AppError::backend_unavailable("failed to initialize Windows IPC auth token"))
}

#[cfg(windows)]
fn validate_windows_ipc_auth(stream: &mut IpcStream) -> Result<(), AppError> {
    let expected = ensure_windows_ipc_auth_token()?;
    let mut buf = Vec::with_capacity(64);
    loop {
        if buf.len() >= 4096 {
            return Err(AppError::permission_denied(
                "IPC authentication header exceeds maximum length",
            ));
        }
        let mut byte = [0_u8; 1];
        stream.read_exact(&mut byte).map_err(|err| {
            AppError::permission_denied(format!("failed to read IPC authentication header: {err}"))
        })?;
        if byte[0] == b'\n' {
            break;
        }
        if byte[0] != b'\r' {
            buf.push(byte[0]);
        }
    }

    let line = String::from_utf8(buf)
        .map_err(|_| AppError::permission_denied("IPC authentication header is not valid UTF-8"))?;
    let token = line
        .strip_prefix("AUTH ")
        .map(str::trim)
        .ok_or_else(|| AppError::permission_denied("missing IPC authentication header"))?;
    if token != expected {
        return Err(AppError::permission_denied(
            "invalid IPC authentication token",
        ));
    }
    Ok(())
}

fn request_frontmost_bounds(context: &RequestContext) -> Option<desktop_core::protocol::Bounds> {
    platform_runtime::request_frontmost_bounds(context)
}

fn request_frontmost_app(context: &RequestContext) -> Option<String> {
    platform_runtime::request_frontmost_app(context)
}

fn enforce_frontmost_app_policy(
    command: &Command,
    context: &RequestContext,
) -> Result<(), AppError> {
    if !app_policy::command_requires_policy(command) {
        return Ok(());
    }

    let cfg = app_policy::current();
    if app_policy::command_is_full_screen_capture(command) && !cfg.allow_full_screen_capture {
        return Err(
            AppError::permission_denied("full-screen capture is disabled by current policy")
                .with_details(json!({
                    "policy_mode": cfg.policy_mode,
                    "apps": cfg.apps,
                    "allow_full_screen_capture": cfg.allow_full_screen_capture,
                    "remediation": "open DesktopCtl menu -> App Access Policy and enable Allow full-screen capture"
                })),
        );
    }

    let Some(frontmost_app) = request_frontmost_app(context) else {
        if matches!(cfg.policy_mode, app_policy::PolicyMode::AllowAll) {
            return Ok(());
        }
        return Err(
            AppError::permission_denied(
                "frontmost app could not be resolved under restrictive app policy",
            )
            .with_details(json!({
                "frontmost_app": null,
                "policy_mode": cfg.policy_mode,
                "apps": cfg.apps,
                "remediation": "open DesktopCtl menu -> App Access Policy, switch to Allow all, or focus a resolvable app window"
            })),
        );
    };
    if app_policy::is_app_allowed(&cfg, &frontmost_app) {
        return Ok(());
    }

    Err(AppError::permission_denied(format!(
        "frontmost app \"{frontmost_app}\" is blocked by current policy"
    ))
    .with_details(json!({
        "frontmost_app": frontmost_app,
        "policy_mode": cfg.policy_mode,
        "apps": cfg.apps,
        "remediation": "open DesktopCtl menu -> App Access Policy"
    })))
}

fn enforce_gui_ops_enabled(command: &Command) -> Result<(), AppError> {
    if !GUI_OPS_DISABLED.load(Ordering::SeqCst) {
        return Ok(());
    }
    if command_is_allowed_when_gui_disabled(command) {
        return Ok(());
    }

    Err(AppError::permission_denied(
        "GUI operations are disabled in daemon (desktopctl disable was previously issued)",
    ))
}

fn command_is_allowed_when_gui_disabled(command: &Command) -> bool {
    matches!(
        command,
        Command::Ping
            | Command::DisableGui
            | Command::PermissionsCheck
            | Command::RequestShow { .. }
            | Command::RequestList { .. }
            | Command::RequestScreenshot { .. }
            | Command::RequestResponse { .. }
            | Command::RequestSearch { .. }
            | Command::ReplayRecord { .. }
            | Command::ReplayLoad { .. }
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
        Command::DisableGui => {
            set_gui_ops_disabled(true);
            Ok(json!({}))
        }
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
        super::GUI_OPS_DISABLED.store(false, std::sync::atomic::Ordering::SeqCst);
        let result = execute(desktop_core::protocol::Command::Ping).expect("ping");
        assert_eq!(result["message"], "pong");
    }

    #[test]
    fn disable_gui_sets_daemon_gate() {
        super::GUI_OPS_DISABLED.store(false, std::sync::atomic::Ordering::SeqCst);
        let result = execute(desktop_core::protocol::Command::DisableGui).expect("disable");
        assert_eq!(result, serde_json::json!({}));
        assert!(super::GUI_OPS_DISABLED.load(std::sync::atomic::Ordering::SeqCst));
        super::GUI_OPS_DISABLED.store(false, std::sync::atomic::Ordering::SeqCst);
    }

    #[test]
    fn gui_commands_blocked_when_disabled() {
        super::GUI_OPS_DISABLED.store(true, std::sync::atomic::Ordering::SeqCst);
        let err = super::enforce_gui_ops_enabled(&desktop_core::protocol::Command::ScreenCapture {
            out_path: None,
            overlay: false,
            active_window: false,
            active_window_id: None,
            region: None,
        })
        .expect_err("screen capture should be blocked");
        assert_eq!(err.code, ErrorCode::PermissionDenied);

        let ping = super::enforce_gui_ops_enabled(&desktop_core::protocol::Command::Ping);
        assert!(ping.is_ok(), "ping must remain allowed");

        let clipboard_err =
            super::enforce_gui_ops_enabled(&desktop_core::protocol::Command::ClipboardRead)
                .expect_err("clipboard should be blocked");
        assert_eq!(clipboard_err.code, ErrorCode::PermissionDenied);

        super::GUI_OPS_DISABLED.store(false, std::sync::atomic::Ordering::SeqCst);
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

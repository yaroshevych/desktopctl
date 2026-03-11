use std::{
    fs,
    os::unix::fs::PermissionsExt,
    os::unix::net::{UnixListener, UnixStream},
    process::Command as ProcessCommand,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
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
use serde_json::{Value, json};

use crate::{clipboard, permissions, recording, replay, vision};

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
    let listener = bind_listener()?;
    thread::spawn(move || {
        if let Err(err) = accept_loop(listener, config) {
            eprintln!("daemon loop error: {err}");
        }
    });
    Ok(())
}

pub fn run_blocking(config: DaemonConfig) -> Result<(), AppError> {
    let listener = bind_listener()?;
    accept_loop(listener, config)
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
    Ok(listener)
}

fn accept_loop(listener: UnixListener, config: DaemonConfig) -> Result<(), AppError> {
    let mut last_activity = Instant::now();
    let active_clients = Arc::new(AtomicUsize::new(0));

    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                last_activity = Instant::now();
                let active_clients = Arc::clone(&active_clients);
                active_clients.fetch_add(1, Ordering::SeqCst);
                thread::spawn(move || {
                    if let Err(err) = handle_client(stream) {
                        eprintln!("daemon client error: {err}");
                    }
                    active_clients.fetch_sub(1, Ordering::SeqCst);
                });
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                if let Some(timeout) = config.idle_timeout {
                    if active_clients.load(Ordering::SeqCst) == 0
                        && last_activity.elapsed() >= timeout
                    {
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
    Ok(())
}

fn handle_client(mut stream: UnixStream) -> Result<(), AppError> {
    let request: RequestEnvelope = read_framed_json(&mut stream)?;
    let request_id = request.request_id.clone();
    let command = request.command.clone();
    let command_name = command.name().to_string();
    let response = match execute(command) {
        Ok(result) => ResponseEnvelope::success(request_id.clone(), result),
        Err(err) => ResponseEnvelope::from_error(request_id, command_name, err),
    };
    if let Err(err) = recording::record_command(&request, &response) {
        eprintln!("recorder write failed: {err}");
    }
    write_framed_json(&mut stream, &response)?;
    Ok(())
}

fn execute(command: Command) -> Result<Value, AppError> {
    match command {
        Command::Ping => Ok(json!({ "message": "pong" })),
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
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            backend.move_mouse(Point::new(x, y))?;
            Ok(json!({}))
        }
        Command::PointerDown { x, y } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = Point::new(x, y);
            backend.move_mouse(point)?;
            backend.left_down(point)?;
            Ok(json!({}))
        }
        Command::PointerUp { x, y } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = Point::new(x, y);
            backend.move_mouse(point)?;
            backend.left_up(point)?;
            Ok(json!({}))
        }
        Command::PointerClick { x, y } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let point = Point::new(x, y);
            backend.move_mouse(point)?;
            backend.left_click(point)?;
            Ok(json!({}))
        }
        Command::PointerDrag {
            x1,
            y1,
            x2,
            y2,
            hold_ms,
        } => {
            let backend = new_backend()?;
            backend.check_accessibility_permission()?;
            let start = Point::new(x1, y1);
            let end = Point::new(x2, y2);
            backend.move_mouse(start)?;
            backend.left_down(start)?;
            backend.sleep_ms(hold_ms.max(30));
            backend.left_drag(end)?;
            backend.left_up(end)?;
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
        Command::ScreenCapture { out_path } => {
            permissions::ensure_screen_recording_permission()?;
            let capture = vision::pipeline::capture_and_update(out_path.map(Into::into))?;
            Ok(json!({
                "snapshot_id": capture.snapshot.snapshot_id,
                "timestamp": capture.snapshot.timestamp,
                "path": capture.image_path,
                "display": capture.snapshot.display,
                "focused_app": capture.snapshot.focused_app,
                "event_ids": capture.event_ids
            }))
        }
        Command::ScreenSnapshot => {
            if let Some(snapshot) = vision::pipeline::latest_snapshot()? {
                Ok(serde_json::to_value(snapshot).map_err(|err| {
                    AppError::internal(format!("failed to encode snapshot: {err}"))
                })?)
            } else {
                permissions::ensure_screen_recording_permission()?;
                let capture = vision::pipeline::capture_and_update(None)?;
                Ok(serde_json::to_value(capture.snapshot).map_err(|err| {
                    AppError::internal(format!("failed to encode snapshot: {err}"))
                })?)
            }
        }
        Command::ScreenTokenize => {
            permissions::ensure_screen_recording_permission()?;
            let payload = vision::pipeline::tokenize()?;
            Ok(serde_json::to_value(payload).map_err(|err| {
                AppError::internal(format!("failed to encode token payload: {err}"))
            })?)
        }
        Command::WaitText {
            text,
            timeout_ms,
            interval_ms,
        } => wait_for_text(&text, timeout_ms, interval_ms),
        Command::UiClickText { text, timeout_ms } => click_text_target(&text, timeout_ms),
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
    let target = select_text_candidate(&capture.snapshot.texts, query)?;
    perform_click(&target.bounds)?;

    verify_click_postcondition(query, &target.bounds, timeout_ms.min(2_000).max(300))?;
    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "text": target.text,
        "bounds": target.bounds
    }))
}

fn click_token_target(token_id: u32) -> Result<Value, AppError> {
    let token = vision::pipeline::token(token_id)?
        .ok_or_else(|| AppError::target_not_found(format!("token {token_id} not found; run `screen tokenize --json` first")))?;
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
    let q = query.trim().to_lowercase();
    if q.is_empty() {
        return Err(AppError::invalid_argument("empty text selector"));
    }

    let mut candidates: Vec<(f32, desktop_core::protocol::SnapshotText)> = texts
        .iter()
        .filter_map(|t| {
            let hay = t.text.to_lowercase();
            if hay.contains(&q) {
                let exact = if hay == q { 1.0 } else { 0.0 };
                Some((exact + t.confidence, t.clone()))
            } else {
                None
            }
        })
        .collect();

    if candidates.is_empty() {
        return Err(AppError::target_not_found(format!(
            "text target \"{query}\" was not found"
        )));
    }

    candidates.sort_by(|a, b| b.0.total_cmp(&a.0));
    if candidates.len() > 1 && (candidates[0].0 - candidates[1].0).abs() < 0.05 {
        return Err(AppError::ambiguous_target(format!(
            "multiple matches for text \"{query}\""
        )));
    }
    let best = candidates.remove(0).1;
    if best.confidence < 0.25 {
        return Err(AppError::low_confidence(format!(
            "match confidence too low for \"{query}\""
        )));
    }
    Ok(best)
}

fn perform_click(bounds: &desktop_core::protocol::Bounds) -> Result<(), AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let center_x = (bounds.x + bounds.width / 2.0).max(0.0).round() as u32;
    let center_y = (bounds.y + bounds.height / 2.0).max(0.0).round() as u32;
    let point = Point::new(center_x, center_y);
    backend.move_mouse(point)?;
    thread::sleep(Duration::from_millis(60));
    backend.left_click(point)?;
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
        let still_present = capture.snapshot.texts.iter().any(|text| {
            text.text.to_lowercase().contains(&query.to_lowercase())
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

fn inflate_bounds(bounds: &desktop_core::protocol::Bounds, pad: f64) -> desktop_core::protocol::Bounds {
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

fn frontmost_app_name() -> Option<String> {
    let script =
        r#"tell application "System Events" to get name of first process whose frontmost is true"#;
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
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

#[cfg(test)]
mod tests {
    use desktop_core::{
        error::ErrorCode,
        protocol::{RequestEnvelope, ResponseEnvelope},
    };

    use super::execute;

    #[test]
    fn ping_returns_message() {
        let result = execute(desktop_core::protocol::Command::Ping).expect("ping");
        assert_eq!(result["message"], "pong");
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
}

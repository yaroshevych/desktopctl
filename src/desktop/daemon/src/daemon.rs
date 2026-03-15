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
use image::RgbaImage;
use serde_json::{Value, json};

use crate::{clipboard, permissions, recording, replay, trace, vision};

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
    trace::log(format!("listener:bound socket={}", path.display()));
    Ok(listener)
}

fn accept_loop(listener: UnixListener, config: DaemonConfig) -> Result<(), AppError> {
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
        Command::ScreenCapture { out_path } => {
            trace::log("execute:screen_capture:start");
            permissions::ensure_screen_recording_permission()?;
            let capture = vision::pipeline::capture_and_update(out_path.map(Into::into))?;
            trace::log(format!(
                "execute:screen_capture:ok snapshot_id={} event_count={}",
                capture.snapshot.snapshot_id,
                capture.event_ids.len()
            ));
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
            trace::log("execute:screen_snapshot:start");
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
        Command::ScreenTokenize => {
            trace::log("execute:screen_tokenize:start");
            permissions::ensure_screen_recording_permission()?;
            let payload = vision::pipeline::tokenize()?;
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
    trace::log(format!(
        "ui_click_text:candidates snapshot_id={} query=\"{}\" texts={} display={}x{} focused_app={}",
        capture.snapshot.snapshot_id,
        query,
        capture.snapshot.texts.len(),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
        capture.snapshot.focused_app.as_deref().unwrap_or("<none>")
    ));
    let target = select_text_candidate(&capture.snapshot.texts, query)?;
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
    let target = select_text_candidate(&capture.snapshot.texts, query)?;
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

fn screen_settings_map() -> Result<Value, AppError> {
    let capture = vision::pipeline::capture_and_update(None)?;
    let frame_image = load_rgba_image(&capture.image_path);
    let heading = find_settings_heading(&capture.snapshot.texts);
    let rows = infer_settings_rows(&capture.snapshot.texts, heading.as_ref());
    let list_bounds = bounds_from_texts(&rows).map(|b| desktop_core::protocol::Bounds {
        x: (b.x - 18.0).max(0.0),
        y: (b.y - 6.0).max(0.0),
        width: b.width + 56.0,
        height: b.height + 12.0,
    });

    let row_height = median_row_height(&rows).unwrap_or(14.0);
    let controls = list_bounds.as_ref().map(|list| {
        let control_y = list.y + list.height + row_height.max(10.0);
        let plus = bounds_from_center(list.x + 12.0, control_y, 14.0, 14.0);
        let minus = bounds_from_center(list.x + 30.0, control_y, 14.0, 14.0);
        json!({
            "add_button_bounds": plus,
            "remove_button_bounds": minus,
            "add_click": center_point(&plus),
            "remove_click": center_point(&minus)
        })
    });

    let row_entries = rows
        .iter()
        .map(|row| {
            let toggle = list_bounds.as_ref().map(|list| {
                bounds_from_center(
                    list.x + list.width - 18.0,
                    row.bounds.y + row.bounds.height / 2.0,
                    28.0,
                    16.0,
                )
            });
            let toggle_state = toggle
                .as_ref()
                .map(|bounds| estimate_toggle_state(frame_image.as_ref(), bounds))
                .unwrap_or_else(|| "unknown".to_string());
            json!({
                "text": row.text,
                "bounds": row.bounds,
                "confidence": row.confidence,
                "toggle_bounds": toggle,
                "toggle_click": toggle.as_ref().map(center_point),
                "toggle_state": toggle_state
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "timestamp": capture.snapshot.timestamp,
        "display": capture.snapshot.display,
        "focused_app": capture.snapshot.focused_app,
        "heading": heading,
        "list_bounds": list_bounds,
        "rows": row_entries,
        "controls": controls
    }))
}

fn click_settings_control(
    control: &str,
    row_text: Option<&str>,
    timeout_ms: u64,
) -> Result<Value, AppError> {
    permissions::ensure_screen_recording_permission()?;
    let payload = screen_settings_map()?;
    let (x, y, details) = match control {
        "add" => {
            let click = payload["controls"]["add_click"].clone();
            let (x, y) = point_from_value(&click).ok_or_else(|| {
                AppError::target_not_found("settings add (+) button was not found")
            })?;
            (x, y, json!({ "control": "add", "click": click }))
        }
        "remove" => {
            let click = payload["controls"]["remove_click"].clone();
            let (x, y) = point_from_value(&click).ok_or_else(|| {
                AppError::target_not_found("settings remove (-) button was not found")
            })?;
            (x, y, json!({ "control": "remove", "click": click }))
        }
        "toggle" => {
            let needle = row_text
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::invalid_argument("settings toggle requires row text"))?;
            let rows = payload["rows"]
                .as_array()
                .ok_or_else(|| AppError::internal("invalid settings rows payload"))?;
            let matched = rows.iter().find(|row| {
                row["text"]
                    .as_str()
                    .map(|text| text.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            });
            let row = matched.ok_or_else(|| {
                AppError::target_not_found(format!(
                    "settings row \"{}\" was not found",
                    row_text.unwrap_or_default()
                ))
            })?;
            let click = row["toggle_click"].clone();
            let (x, y) = point_from_value(&click).ok_or_else(|| {
                AppError::target_not_found(format!(
                    "toggle target for row \"{}\" was not found",
                    row_text.unwrap_or_default()
                ))
            })?;
            (
                x,
                y,
                json!({
                    "control": "toggle",
                    "row_text": row["text"],
                    "click": click,
                    "toggle_bounds": row["toggle_bounds"]
                }),
            )
        }
        _ => {
            return Err(AppError::invalid_argument(format!(
                "unsupported settings control: {control}"
            )));
        }
    };

    trace::log(format!(
        "ui_click_settings_control control={} point=({}, {})",
        control, x, y
    ));
    perform_click_at(x, y)?;
    thread::sleep(Duration::from_millis(timeout_ms.min(500).max(40)));
    Ok(json!({
        "snapshot_id": payload["snapshot_id"],
        "timestamp": payload["timestamp"],
        "result": details
    }))
}

fn settings_ensure_enabled(row_text: &str, timeout_ms: u64) -> Result<Value, AppError> {
    let before = screen_settings_map()?;
    let before_row = find_settings_row(&before, row_text)?;
    let before_state = before_row["toggle_state"].as_str().unwrap_or("unknown");
    if before_state == "on" {
        return Ok(json!({
            "row_text": before_row["text"],
            "state_before": before_state,
            "state_after": before_state,
            "changed": false
        }));
    }

    let _ = click_settings_control("toggle", Some(row_text), timeout_ms)?;
    let after = screen_settings_map()?;
    let after_row = find_settings_row(&after, row_text)?;
    let after_state = after_row["toggle_state"].as_str().unwrap_or("unknown");
    if after_state == "on" {
        return Ok(json!({
            "row_text": after_row["text"],
            "state_before": before_state,
            "state_after": after_state,
            "changed": true
        }));
    }

    if unlock_prompt_visible()? {
        return Err(AppError::permission_denied(
            "settings requires authentication; run `desktopctl ui settings unlock --password <password>` and retry",
        ));
    }

    Err(AppError::postcondition_failed(format!(
        "failed to enable settings row \"{row_text}\" (state remained \"{after_state}\")"
    )))
}

fn settings_unlock(password: &str, timeout_ms: u64) -> Result<Value, AppError> {
    let candidates = ["Use Password...", "Use Password", "Unlock"];
    for label in candidates {
        let _ = click_text_once(label, 400);
    }

    let start = Instant::now();
    let mut field_clicked = false;
    while start.elapsed().as_millis() as u64 <= timeout_ms.max(600) {
        let capture = vision::pipeline::capture_and_update(None)?;
        if let Ok(password_label) = select_text_candidate(&capture.snapshot.texts, "Password") {
            let x = (password_label.bounds.x + password_label.bounds.width + 120.0)
                .max(0.0)
                .round() as u32;
            let y = (password_label.bounds.y + password_label.bounds.height / 2.0)
                .max(0.0)
                .round() as u32;
            perform_click_at(x, y)?;
            field_clicked = true;
            break;
        }
        thread::sleep(Duration::from_millis(120));
    }
    if !field_clicked {
        return Err(AppError::target_not_found(
            "password field was not found in settings unlock prompt",
        ));
    }

    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    backend.type_text(password)?;
    backend.press_enter()?;
    thread::sleep(Duration::from_millis(260));

    Ok(json!({
        "unlocked": true
    }))
}

fn click_text_once(query: &str, timeout_ms: u64) -> Result<Value, AppError> {
    permissions::ensure_screen_recording_permission()?;
    let start = Instant::now();
    while start.elapsed().as_millis() as u64 <= timeout_ms.max(250) {
        let capture = vision::pipeline::capture_and_update(None)?;
        if let Ok(target) = select_text_candidate(&capture.snapshot.texts, query) {
            perform_click(&target.bounds)?;
            return Ok(json!({
                "snapshot_id": capture.snapshot.snapshot_id,
                "text": target.text,
                "bounds": target.bounds
            }));
        }
        thread::sleep(Duration::from_millis(80));
    }
    Err(AppError::target_not_found(format!(
        "text target \"{query}\" was not found"
    )))
}

fn find_settings_row<'a>(payload: &'a Value, needle: &str) -> Result<&'a Value, AppError> {
    let rows = payload["rows"]
        .as_array()
        .ok_or_else(|| AppError::internal("invalid settings rows payload"))?;
    let needle = needle.trim().to_lowercase();
    rows.iter()
        .find(|row| {
            row["text"]
                .as_str()
                .map(|text| text.to_lowercase().contains(&needle))
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            AppError::target_not_found(format!("settings row \"{needle}\" was not found"))
        })
}

fn unlock_prompt_visible() -> Result<bool, AppError> {
    let capture = vision::pipeline::capture_and_update(None)?;
    let found = capture.snapshot.texts.iter().any(|text| {
        let lower = text.text.to_lowercase();
        lower.contains("password")
            || lower.contains("unlock")
            || lower.contains("use password")
            || lower.contains("touch id")
    });
    Ok(found)
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
    let mut candidates: Vec<(f32, desktop_core::protocol::SnapshotText)> = texts
        .iter()
        .filter_map(|t| {
            let hay = t.text.to_lowercase();
            if hay.contains(&q) {
                text_match_score(&q, &hay, t.confidence).map(|score| (score, t.clone()))
            } else {
                None
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

fn find_settings_heading(
    texts: &[desktop_core::protocol::SnapshotText],
) -> Option<desktop_core::protocol::SnapshotText> {
    let keys = [
        "screen & system audio recording",
        "screen recording",
        "accessibility",
    ];
    texts
        .iter()
        .filter_map(|text| {
            let lower = text.text.to_lowercase();
            let matched = keys.iter().any(|key| lower.contains(key));
            matched.then_some(text.clone())
        })
        .max_by(|a, b| a.confidence.total_cmp(&b.confidence))
}

fn infer_settings_rows(
    texts: &[desktop_core::protocol::SnapshotText],
    heading: Option<&desktop_core::protocol::SnapshotText>,
) -> Vec<desktop_core::protocol::SnapshotText> {
    let mut rows: Vec<desktop_core::protocol::SnapshotText> = texts
        .iter()
        .filter_map(|text| {
            if !is_probable_settings_row_label(&text.text, text.confidence) {
                return None;
            }
            if let Some(heading) = heading {
                let min_y = heading.bounds.y + heading.bounds.height + 8.0;
                let max_y = heading.bounds.y + 360.0;
                if text.bounds.y < min_y || text.bounds.y > max_y {
                    return None;
                }
                if text.bounds.x + text.bounds.width < heading.bounds.x - 120.0 {
                    return None;
                }
            }
            Some(text.clone())
        })
        .collect();
    rows.sort_by(|a, b| a.bounds.y.total_cmp(&b.bounds.y));
    rows
}

fn is_probable_settings_row_label(text: &str, confidence: f32) -> bool {
    let value = text.trim();
    if confidence < 0.35 || value.is_empty() || value.len() > 48 {
        return false;
    }
    if !value.chars().any(|c| c.is_alphanumeric()) {
        return false;
    }
    let lower = value.to_lowercase();
    let blocked = [
        "allow the applications",
        "system audio recording only",
        "no items",
        "privacy",
    ];
    !blocked.iter().any(|word| lower.contains(word))
}

fn median_row_height(rows: &[desktop_core::protocol::SnapshotText]) -> Option<f64> {
    if rows.is_empty() {
        return None;
    }
    let mut heights = rows
        .iter()
        .map(|row| row.bounds.height)
        .filter(|h| *h > 0.0)
        .collect::<Vec<_>>();
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(|a, b| a.total_cmp(b));
    Some(heights[heights.len() / 2])
}

fn bounds_from_center(x: f64, y: f64, width: f64, height: f64) -> desktop_core::protocol::Bounds {
    desktop_core::protocol::Bounds {
        x: (x - width / 2.0).max(0.0),
        y: (y - height / 2.0).max(0.0),
        width,
        height,
    }
}

fn center_point(bounds: &desktop_core::protocol::Bounds) -> serde_json::Value {
    json!({
        "x": (bounds.x + bounds.width / 2.0).round().max(0.0) as u32,
        "y": (bounds.y + bounds.height / 2.0).round().max(0.0) as u32
    })
}

fn point_from_value(value: &serde_json::Value) -> Option<(u32, u32)> {
    Some((
        value.get("x")?.as_u64()? as u32,
        value.get("y")?.as_u64()? as u32,
    ))
}

fn load_rgba_image(path: &std::path::Path) -> Option<RgbaImage> {
    image::open(path).ok().map(|img| img.to_rgba8())
}

fn estimate_toggle_state(
    image: Option<&RgbaImage>,
    bounds: &desktop_core::protocol::Bounds,
) -> String {
    let image = match image {
        Some(img) => img,
        None => return "unknown".to_string(),
    };
    let width = image.width() as i32;
    let height = image.height() as i32;
    if width <= 0 || height <= 0 {
        return "unknown".to_string();
    }

    let x0 = bounds.x.floor().max(0.0) as i32;
    let y0 = bounds.y.floor().max(0.0) as i32;
    let x1 = (bounds.x + bounds.width).ceil().max(0.0) as i32;
    let y1 = (bounds.y + bounds.height).ceil().max(0.0) as i32;
    let x0 = x0.clamp(0, width - 1);
    let y0 = y0.clamp(0, height - 1);
    let x1 = x1.clamp(x0 + 1, width);
    let y1 = y1.clamp(y0 + 1, height);

    let mut r_sum = 0f64;
    let mut g_sum = 0f64;
    let mut b_sum = 0f64;
    let mut count = 0f64;
    for y in y0..y1 {
        for x in x0..x1 {
            let px = image.get_pixel(x as u32, y as u32).0;
            r_sum += px[0] as f64;
            g_sum += px[1] as f64;
            b_sum += px[2] as f64;
            count += 1.0;
        }
    }
    if count <= 0.0 {
        return "unknown".to_string();
    }
    let r = r_sum / count;
    let g = g_sum / count;
    let b = b_sum / count;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;

    if chroma < 12.0 {
        "off".to_string()
    } else if b > g + 8.0 && b > r + 18.0 && b > 90.0 {
        "on".to_string()
    } else {
        "unknown".to_string()
    }
}

fn frontmost_window_bounds() -> Option<desktop_core::protocol::Bounds> {
    let script = r#"tell application "System Events"
set frontProc to first application process whose frontmost is true
if (count of windows of frontProc) is 0 then
    return ""
end if
set winPos to position of front window of frontProc
set winSize to size of front window of frontProc
return (item 1 of winPos as string) & "," & (item 2 of winPos as string) & "," & (item 1 of winSize as string) & "," & (item 2 of winSize as string)
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
    let parts: Vec<f64> = raw
        .split(',')
        .filter_map(|v| v.trim().parse::<f64>().ok())
        .collect();
    if parts.len() != 4 {
        return None;
    }
    Some(desktop_core::protocol::Bounds {
        x: parts[0].max(0.0),
        y: parts[1].max(0.0),
        width: parts[2].max(0.0),
        height: parts[3].max(0.0),
    })
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

#[cfg(test)]
mod tests {
    use desktop_core::{
        error::ErrorCode,
        protocol::{Bounds, RequestEnvelope, ResponseEnvelope, SnapshotText},
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
}

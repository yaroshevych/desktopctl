use std::{
    fs,
    os::unix::fs::PermissionsExt,
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
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
use image::{ImageFormat, Rgba, RgbaImage};
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
        Command::AppIsolate { name } => {
            trace::log(format!("app_isolate:start name={name}"));
            let hidden = isolate_application(&name)?;
            let _ = wait_for_open_app(&name, 6_000);
            trace::log(format!("app_isolate:ok name={name} hidden={hidden}"));
            Ok(json!({ "app": name, "state": "isolated", "hidden_apps": hidden }))
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
        Command::ScreenCapture { out_path, overlay } => {
            trace::log("execute:screen_capture:start");
            permissions::ensure_screen_recording_permission()?;
            let capture = vision::pipeline::capture_and_update(out_path.map(Into::into))?;
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
                "path": capture.image_path,
                "overlay_path": overlay_path,
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
    let detected_regions_raw = frame_image
        .as_ref()
        .map(vision::regions::detect_settings_regions)
        .unwrap_or_default();
    let detected_regions = scale_regions_to_display(
        &detected_regions_raw,
        frame_image.as_ref().map(|img| img.width()),
        frame_image.as_ref().map(|img| img.height()),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
    );
    let inferred_window_bounds = infer_window_bounds_from_content(
        detected_regions.content_bounds.as_ref(),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
    )
    .or_else(|| detected_regions.window_bounds.clone());
    let heading = find_settings_heading(
        &capture.snapshot.texts,
        detected_regions.content_bounds.as_ref(),
    )
    .or_else(|| find_settings_heading(&capture.snapshot.texts, None));
    let instruction = find_settings_instruction(&capture.snapshot.texts, heading.as_ref());
    let rows = infer_settings_rows(&capture.snapshot.texts, heading.as_ref());
    let row_height = median_row_height(&rows).unwrap_or(14.0);
    let no_items = first_matching_text(&capture.snapshot.texts, "no items");
    let mut rows_bounds = bounds_from_texts(&rows).map(|b| desktop_core::protocol::Bounds {
        x: (b.x - 18.0).max(0.0),
        y: (b.y - 6.0).max(0.0),
        width: b.width + 56.0,
        height: b.height + 12.0,
    });
    if let (Some(list), Some(content)) = (&rows_bounds, &detected_regions.content_bounds) {
        let list_center_x = list.x + list.width / 2.0;
        let list_center_y = list.y + list.height / 2.0;
        let content_x2 = content.x + content.width;
        let content_y2 = content.y + content.height;
        let center_inside = list_center_x >= content.x
            && list_center_x <= content_x2
            && list_center_y >= content.y
            && list_center_y <= content_y2;
        if !center_inside || iou(list, content) < 0.06 {
            rows_bounds = None;
        }
    }
    if is_sidebar_like_rows(rows_bounds.as_ref(), heading.as_ref(), no_items.as_ref()) {
        rows_bounds = None;
    }

    let mut list_bounds = rows_bounds;
    if list_bounds.is_none() {
        list_bounds = infer_list_bounds_from_anchors(
            heading.as_ref(),
            no_items.as_ref(),
            detected_regions.content_bounds.as_ref(),
        );
    }
    if list_bounds.is_none() {
        list_bounds = detected_regions.table_bounds.clone();
    }

    let controls = infer_settings_controls_for_settings_pane(
        &capture.snapshot.texts,
        heading.as_ref(),
        no_items.as_ref(),
        instruction.as_ref(),
        list_bounds.as_ref(),
        detected_regions.content_bounds.as_ref(),
        row_height,
    );
    trace::log(format!(
        "screen_settings_map snapshot_id={} image={}x{} display={}x{} heading={} instruction={} no_items={} rows={} list={} regions.window={} regions.content={} regions.table={} controls={}",
        capture.snapshot.snapshot_id,
        frame_image
            .as_ref()
            .map(|img| img.width().to_string())
            .unwrap_or_else(|| "0".to_string()),
        frame_image
            .as_ref()
            .map(|img| img.height().to_string())
            .unwrap_or_else(|| "0".to_string()),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
        fmt_bounds_opt(heading.as_ref().map(|h| &h.bounds)),
        fmt_bounds_opt(instruction.as_ref().map(|t| &t.bounds)),
        fmt_bounds_opt(no_items.as_ref().map(|t| &t.bounds)),
        rows.len(),
        fmt_bounds_opt(list_bounds.as_ref()),
        fmt_bounds_opt(inferred_window_bounds.as_ref()),
        fmt_bounds_opt(detected_regions.content_bounds.as_ref()),
        fmt_bounds_opt(detected_regions.table_bounds.as_ref()),
        controls
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string())
    ));

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
                .map(|bounds| {
                    estimate_toggle_state(
                        frame_image.as_ref(),
                        bounds,
                        capture.snapshot.display.width,
                        capture.snapshot.display.height,
                    )
                })
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
        "instruction": instruction,
        "no_items": no_items,
        "list_bounds": list_bounds,
        "regions": {
            "settings_window": inferred_window_bounds,
            "sidebar": detected_regions.sidebar_bounds,
            "content": detected_regions.content_bounds,
            "table": detected_regions.table_bounds
        },
        "rows": row_entries,
        "controls": controls
    }))
}

fn is_sidebar_like_rows(
    rows_bounds: Option<&desktop_core::protocol::Bounds>,
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
) -> bool {
    let Some(rows) = rows_bounds else {
        return false;
    };
    if rows.height >= 240.0 {
        return true;
    }
    if let Some(no_items) = no_items {
        if rows.y > no_items.bounds.y + 56.0 {
            return true;
        }
        if rows.x + rows.width < no_items.bounds.x + 20.0 {
            return true;
        }
    }
    if let Some(heading) = heading {
        if rows.y > heading.bounds.y + 190.0 {
            return true;
        }
        if rows.x + rows.width < heading.bounds.x - 8.0 {
            return true;
        }
    }
    false
}

fn fmt_bounds_opt(bounds: Option<&desktop_core::protocol::Bounds>) -> String {
    match bounds {
        Some(b) => format!("({:.1},{:.1},{:.1},{:.1})", b.x, b.y, b.width, b.height),
        None => "null".to_string(),
    }
}

fn scale_regions_to_display(
    regions: &vision::regions::SettingsRegions,
    image_width: Option<u32>,
    image_height: Option<u32>,
    display_width: u32,
    display_height: u32,
) -> vision::regions::SettingsRegions {
    let Some(img_w) = image_width else {
        return regions.clone();
    };
    let Some(img_h) = image_height else {
        return regions.clone();
    };
    if img_w == 0 || img_h == 0 || display_width == 0 || display_height == 0 {
        return regions.clone();
    }
    let sx = display_width as f64 / img_w as f64;
    let sy = display_height as f64 / img_h as f64;
    if (sx - 1.0).abs() < 0.0001 && (sy - 1.0).abs() < 0.0001 {
        return regions.clone();
    }

    let scale = |b: &desktop_core::protocol::Bounds| desktop_core::protocol::Bounds {
        x: (b.x * sx).max(0.0),
        y: (b.y * sy).max(0.0),
        width: (b.width * sx).max(0.0),
        height: (b.height * sy).max(0.0),
    };

    vision::regions::SettingsRegions {
        window_bounds: regions.window_bounds.as_ref().map(scale),
        sidebar_bounds: regions.sidebar_bounds.as_ref().map(scale),
        content_bounds: regions.content_bounds.as_ref().map(scale),
        table_bounds: regions.table_bounds.as_ref().map(scale),
    }
}

fn infer_window_bounds_from_content(
    content: Option<&desktop_core::protocol::Bounds>,
    display_width: u32,
    display_height: u32,
) -> Option<desktop_core::protocol::Bounds> {
    let content = content?;
    if content.width <= 0.0 || content.height <= 0.0 {
        return None;
    }
    let sidebar_w = (content.width * 0.31).clamp(150.0, 340.0);
    let title_h = (content.height * 0.085).clamp(30.0, 56.0);
    let x0 = (content.x - sidebar_w).max(0.0);
    let y0 = (content.y - title_h).max(0.0);
    let x1 = (content.x + content.width).min(display_width as f64);
    let y1 = (content.y + content.height).min(display_height as f64);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(desktop_core::protocol::Bounds {
        x: x0,
        y: y0,
        width: (x1 - x0).max(0.0),
        height: (y1 - y0).max(0.0),
    })
}

fn infer_settings_controls_for_settings_pane(
    texts: &[desktop_core::protocol::SnapshotText],
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
    instruction: Option<&desktop_core::protocol::SnapshotText>,
    list_bounds: Option<&desktop_core::protocol::Bounds>,
    content_bounds: Option<&desktop_core::protocol::Bounds>,
    row_height: f64,
) -> Option<Value> {
    if let Some(controls) =
        infer_settings_controls_from_ocr_symbols(texts, heading, no_items, content_bounds)
    {
        return Some(controls);
    }
    if let Some(controls) = infer_settings_controls_from_anchor(heading, no_items) {
        return Some(controls);
    }
    if let Some(controls) =
        infer_settings_controls_from_list_bounds(list_bounds, heading, row_height)
    {
        return Some(controls);
    }
    infer_settings_controls_from_instruction_anchor(instruction, heading, content_bounds)
}

fn infer_settings_controls_from_ocr_symbols(
    texts: &[desktop_core::protocol::SnapshotText],
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
    content_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Option<Value> {
    let heading = heading?;
    let y_min = heading.bounds.y + heading.bounds.height + 8.0;
    let y_max = heading.bounds.y + 360.0;
    let mut x_min = heading.bounds.x - 100.0;
    let mut x_max = heading.bounds.x + 320.0;
    if let Some(content) = content_bounds {
        x_min = x_min.max(content.x - 8.0);
        x_max = x_max.min(content.x + content.width * 0.55);
    }
    if let Some(no_items) = no_items {
        // In this pane, +/- controls sit to the left of "No Items".
        x_max = x_max.min(no_items.bounds.x - 8.0);
    }
    if x_max <= x_min {
        return None;
    }

    let mut pluses = Vec::new();
    let mut minuses = Vec::new();
    for text in texts {
        let Some(symbol) = normalize_control_symbol(&text.text) else {
            continue;
        };
        let cx = text.bounds.x + text.bounds.width / 2.0;
        let cy = text.bounds.y + text.bounds.height / 2.0;
        if cx < x_min || cx > x_max || cy < y_min || cy > y_max {
            continue;
        }
        if let Some(content) = content_bounds {
            let cx2 = content.x + content.width;
            let cy2 = content.y + content.height;
            if cx < content.x || cx > cx2 || cy < content.y || cy > cy2 {
                continue;
            }
        }
        match symbol {
            '+' => pluses.push(text.clone()),
            '-' => minuses.push(text.clone()),
            _ => {}
        }
    }

    let mut best: Option<(
        desktop_core::protocol::SnapshotText,
        desktop_core::protocol::SnapshotText,
        f64,
    )> = None;
    for plus in &pluses {
        let plus_cx = plus.bounds.x + plus.bounds.width / 2.0;
        let plus_cy = plus.bounds.y + plus.bounds.height / 2.0;
        for minus in &minuses {
            let minus_cx = minus.bounds.x + minus.bounds.width / 2.0;
            let minus_cy = minus.bounds.y + minus.bounds.height / 2.0;
            let dx = minus_cx - plus_cx;
            let dy = (minus_cy - plus_cy).abs();
            if !(8.0..=36.0).contains(&dx) || dy > 12.0 {
                continue;
            }
            let score = (dx - 18.0).abs() + dy * 2.0;
            if best.as_ref().map(|(_, _, s)| score < *s).unwrap_or(true) {
                best = Some((plus.clone(), minus.clone(), score));
            }
        }
    }
    let (plus, minus, _) = best?;
    let plus_center_x = plus.bounds.x + plus.bounds.width / 2.0;
    let plus_center_y = plus.bounds.y + plus.bounds.height / 2.0;
    let minus_center_x = minus.bounds.x + minus.bounds.width / 2.0;
    let footer = desktop_core::protocol::Bounds {
        x: (plus_center_x - 18.0).max(0.0),
        y: (plus_center_y - 12.0).max(0.0),
        width: (minus_center_x - plus_center_x + 36.0).max(36.0),
        height: 24.0,
    };
    Some(json!({
        "source": "ocr_symbols",
        "add_button_bounds": plus.bounds,
        "remove_button_bounds": minus.bounds,
        "add_click": center_point(&plus.bounds),
        "remove_click": center_point(&minus.bounds),
        "footer_bounds": footer
    }))
}

fn normalize_control_symbol(text: &str) -> Option<char> {
    let value = text.trim();
    if value == "+" {
        return Some('+');
    }
    if matches!(value, "-" | "−" | "–" | "—") {
        return Some('-');
    }
    None
}

fn infer_settings_controls_from_anchor(
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
) -> Option<Value> {
    let no_items = no_items?;
    if let Some(heading) = heading {
        // Keep anchor + heading in the same pane band.
        if no_items.bounds.x + no_items.bounds.width < heading.bounds.x + 20.0 {
            return None;
        }
        if no_items.bounds.y < heading.bounds.y + heading.bounds.height + 12.0 {
            return None;
        }
        if no_items.bounds.y > heading.bounds.y + 260.0 {
            return None;
        }
    }
    let center_x = no_items.bounds.x + no_items.bounds.width / 2.0;
    let center_y = no_items.bounds.y + no_items.bounds.height / 2.0;
    let add_x = center_x - 182.0;
    let remove_x = center_x - 164.0;
    let controls_y = center_y + 20.0;
    let plus = bounds_from_center(add_x, controls_y, 14.0, 14.0);
    let minus = bounds_from_center(remove_x, controls_y, 14.0, 14.0);
    let footer = desktop_core::protocol::Bounds {
        x: (add_x - 14.0).max(0.0),
        y: (controls_y - 12.0).max(0.0),
        width: 48.0,
        height: 24.0,
    };
    Some(json!({
        "source": "no_items_anchor",
        "add_button_bounds": plus,
        "remove_button_bounds": minus,
        "add_click": center_point(&plus),
        "remove_click": center_point(&minus),
        "footer_bounds": footer
    }))
}

fn infer_settings_controls_from_instruction_anchor(
    instruction: Option<&desktop_core::protocol::SnapshotText>,
    heading: Option<&desktop_core::protocol::SnapshotText>,
    content_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Option<Value> {
    let instruction = instruction?;
    if let Some(heading) = heading {
        if instruction.bounds.y < heading.bounds.y + heading.bounds.height + 4.0 {
            return None;
        }
        if instruction.bounds.y > heading.bounds.y + 200.0 {
            return None;
        }
    }
    let mut add_x = instruction.bounds.x + 4.0;
    let mut remove_x = add_x + 18.0;
    let controls_y = instruction.bounds.y + instruction.bounds.height + 44.0;
    if let Some(content) = content_bounds {
        add_x = add_x.max(content.x + 8.0);
        remove_x = remove_x.max(add_x + 16.0);
    }
    let plus = bounds_from_center(add_x, controls_y, 14.0, 14.0);
    let minus = bounds_from_center(remove_x, controls_y, 14.0, 14.0);
    let footer = desktop_core::protocol::Bounds {
        x: (plus.x - 8.0).max(0.0),
        y: (plus.y - 6.0).max(0.0),
        width: 52.0,
        height: 24.0,
    };
    Some(json!({
        "source": "instruction_anchor",
        "add_button_bounds": plus,
        "remove_button_bounds": minus,
        "add_click": center_point(&plus),
        "remove_click": center_point(&minus),
        "footer_bounds": footer
    }))
}

fn settings_click_from_no_items_anchor(control: &str, payload: &Value) -> Option<(u32, u32)> {
    let (nx, ny, nw, nh) = bounds_tuple_from_value(&payload["no_items"]["bounds"])?;
    if let Some((hx, hy, hw, hh)) = payload
        .get("heading")
        .and_then(|v| v.get("bounds"))
        .and_then(bounds_tuple_from_value)
    {
        if nx + nw < hx + 20.0 {
            return None;
        }
        if ny < hy + hh + 12.0 || ny > hy + 260.0 {
            return None;
        }
        if hx + hw < nx - 340.0 {
            return None;
        }
    }
    let center_x = nx + nw / 2.0;
    let center_y = ny + nh / 2.0;
    let dx = match control {
        "add" => -182.0,
        "remove" => -164.0,
        _ => return None,
    };
    let x = (center_x + dx).round().max(0.0) as u32;
    let y = (center_y + 20.0).round().max(0.0) as u32;
    Some((x, y))
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
            if let Some((x, y)) = settings_click_from_no_items_anchor("add", &payload) {
                (
                    x,
                    y,
                    json!({
                        "control": "add",
                        "click": { "x": x, "y": y },
                        "derived_from": "no_items_anchor"
                    }),
                )
            } else {
                let click = payload["controls"]["add_click"].clone();
                let (x, y) = point_from_value(&click).ok_or_else(|| {
                    AppError::target_not_found("settings add (+) button was not found")
                })?;
                (x, y, json!({ "control": "add", "click": click }))
            }
        }
        "remove" => {
            if let Some((x, y)) = settings_click_from_no_items_anchor("remove", &payload) {
                (
                    x,
                    y,
                    json!({
                        "control": "remove",
                        "click": { "x": x, "y": y },
                        "derived_from": "no_items_anchor"
                    }),
                )
            } else {
                let click = payload["controls"]["remove_click"].clone();
                let (x, y) = point_from_value(&click).ok_or_else(|| {
                    AppError::target_not_found("settings remove (-) button was not found")
                })?;
                (x, y, json!({ "control": "remove", "click": click }))
            }
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

    let display_w = payload["display"]["width"]
        .as_u64()
        .map(|v| v as u32)
        .unwrap_or(u32::MAX);
    let display_h = payload["display"]["height"]
        .as_u64()
        .map(|v| v as u32)
        .unwrap_or(u32::MAX);
    if x >= display_w || y >= display_h {
        return Err(AppError::target_not_found(format!(
            "settings {control} click target ({x},{y}) is outside display bounds {display_w}x{display_h}"
        )));
    }
    if matches!(control, "add" | "remove") {
        if let Some((nx, ny, _nw, _nh)) = bounds_tuple_from_value(&payload["no_items"]["bounds"]) {
            let xf = x as f64;
            let yf = y as f64;
            let x_ok = xf >= (nx - 520.0) && xf <= (nx - 20.0);
            let y_ok = yf >= (ny - 90.0) && yf <= (ny + 90.0);
            if !x_ok || !y_ok {
                return Err(AppError::target_not_found(format!(
                    "rejected unsafe settings {control} click ({x},{y}); anchor mismatch with No Items at ({:.1},{:.1})",
                    nx, ny
                )));
            }
        }
        if payload.get("heading").map(|v| v.is_null()).unwrap_or(true)
            && payload["regions"]
                .get("settings_window")
                .map(|v| v.is_null())
                .unwrap_or(true)
        {
            return Err(AppError::target_not_found(
                "settings pane is not detected in the current frame",
            ));
        }
        if let Some((wx, wy, ww, wh)) = payload["regions"]
            .get("settings_window")
            .and_then(bounds_tuple_from_value)
        {
            let inside_window = (x as f64) >= wx
                && (x as f64) <= wx + ww
                && (y as f64) >= wy
                && (y as f64) <= wy + wh;
            if !inside_window {
                return Err(AppError::target_not_found(format!(
                    "settings {control} click target ({x},{y}) is outside detected settings window"
                )));
            }
        }
        if let Some((tx, ty, tw, th)) = payload["controls"]
            .get("footer_bounds")
            .and_then(bounds_tuple_from_value)
        {
            let margin = 26.0;
            let inside_table_band = (x as f64) >= tx - margin
                && (x as f64) <= tx + tw + margin
                && (y as f64) >= ty - margin
                && (y as f64) <= ty + th + margin;
            if !inside_table_band {
                return Err(AppError::target_not_found(format!(
                    "settings {control} click target ({x},{y}) is outside inferred controls band"
                )));
            }
        }
        if let Some((hx, hy, _hw, hh)) = payload
            .get("heading")
            .and_then(|h| h.get("bounds"))
            .and_then(bounds_tuple_from_value)
        {
            let y_f = y as f64;
            let x_f = x as f64;
            let y_min = hy - hh.max(14.0);
            let y_max = hy + 320.0;
            let x_min = hx - 90.0;
            if y_f < y_min || y_f > y_max || x_f < x_min {
                return Err(AppError::target_not_found(format!(
                    "settings {control} click target ({x},{y}) is outside expected pane near heading"
                )));
            }
        }
    }

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
    content_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Option<desktop_core::protocol::SnapshotText> {
    let keys = [
        "screen & system audio recording",
        "screen recording",
        "accessibility",
    ];
    let mut candidates = texts
        .iter()
        .filter_map(|text| {
            let lower = text.text.to_lowercase();
            let matched = keys.iter().any(|key| lower.contains(key));
            if !matched {
                return None;
            }
            if let Some(content) = content_bounds {
                let center_x = text.bounds.x + text.bounds.width / 2.0;
                let center_y = text.bounds.y + text.bounds.height / 2.0;
                let x2 = content.x + content.width;
                let y2 = content.y + content.height;
                let inside = center_x >= content.x
                    && center_x <= x2
                    && center_y >= content.y
                    && center_y <= y2;
                if !inside || text.bounds.y > content.y + content.height * 0.45 {
                    return None;
                }
            }
            Some(text.clone())
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        a.bounds
            .y
            .total_cmp(&b.bounds.y)
            .then_with(|| b.bounds.x.total_cmp(&a.bounds.x))
            .then_with(|| b.confidence.total_cmp(&a.confidence))
            .then_with(|| b.bounds.width.total_cmp(&a.bounds.width))
    });
    candidates.into_iter().next()
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

fn first_matching_text(
    texts: &[desktop_core::protocol::SnapshotText],
    needle: &str,
) -> Option<desktop_core::protocol::SnapshotText> {
    let needle = needle.trim().to_lowercase();
    texts
        .iter()
        .find(|t| t.text.to_lowercase().contains(&needle))
        .cloned()
}

fn find_settings_instruction(
    texts: &[desktop_core::protocol::SnapshotText],
    heading: Option<&desktop_core::protocol::SnapshotText>,
) -> Option<desktop_core::protocol::SnapshotText> {
    let mut candidates: Vec<_> = texts
        .iter()
        .filter(|text| {
            let lower = text.text.to_lowercase();
            lower.contains("allow the applications below")
                || lower.contains("allow applications below")
                || lower.contains("system audio recording only")
        })
        .cloned()
        .collect();
    if let Some(heading) = heading {
        candidates.retain(|text| {
            text.bounds.y >= heading.bounds.y + heading.bounds.height
                && text.bounds.y <= heading.bounds.y + 180.0
                && text.bounds.x + text.bounds.width >= heading.bounds.x - 20.0
        });
    }
    candidates.sort_by(|a, b| {
        a.bounds
            .y
            .total_cmp(&b.bounds.y)
            .then_with(|| a.bounds.x.total_cmp(&b.bounds.x))
    });
    candidates.into_iter().next()
}

fn infer_list_bounds_from_anchors(
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
    content_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Option<desktop_core::protocol::Bounds> {
    let (mut x0, mut y0, mut x1, mut y1) = match (heading, no_items) {
        (Some(h), Some(no)) => (
            (h.bounds.x - 24.0).max(0.0),
            (h.bounds.y + h.bounds.height + 8.0).max(0.0),
            no.bounds.x + no.bounds.width + 170.0,
            no.bounds.y + no.bounds.height + 24.0,
        ),
        (Some(h), None) => (
            (h.bounds.x - 24.0).max(0.0),
            (h.bounds.y + h.bounds.height + 8.0).max(0.0),
            h.bounds.x + h.bounds.width + 220.0,
            h.bounds.y + h.bounds.height + 84.0,
        ),
        (None, Some(no)) => (
            (no.bounds.x - 170.0).max(0.0),
            (no.bounds.y - 22.0).max(0.0),
            no.bounds.x + no.bounds.width + 170.0,
            no.bounds.y + no.bounds.height + 24.0,
        ),
        (None, None) => return None,
    };

    if let Some(content) = content_bounds {
        let cx2 = content.x + content.width;
        let cy2 = content.y + content.height;
        x0 = x0.max(content.x + 4.0);
        y0 = y0.max(content.y + 20.0);
        x1 = x1.min(cx2 - 6.0);
        y1 = y1.min(cy2 - 4.0);
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let width = x1 - x0;
    let height = y1 - y0;
    if width < 120.0 || height < 36.0 {
        return None;
    }

    Some(desktop_core::protocol::Bounds {
        x: x0,
        y: y0,
        width,
        height,
    })
}

fn infer_settings_controls_from_list_bounds(
    list_bounds: Option<&desktop_core::protocol::Bounds>,
    heading: Option<&desktop_core::protocol::SnapshotText>,
    row_height: f64,
) -> Option<Value> {
    let list = list_bounds?;
    if let Some(heading) = heading {
        if list.x + list.width < heading.bounds.x - 32.0 {
            return None;
        }
        if list.y > heading.bounds.y + 320.0 {
            return None;
        }
    }
    let control_y = list.y + list.height + row_height.max(10.0).min(24.0);
    let plus = bounds_from_center(list.x + 12.0, control_y, 14.0, 14.0);
    let minus = bounds_from_center(list.x + 30.0, control_y, 14.0, 14.0);
    let footer = desktop_core::protocol::Bounds {
        x: (plus.x - 8.0).max(0.0),
        y: (plus.y - 6.0).max(0.0),
        width: 52.0,
        height: 24.0,
    };
    Some(json!({
        "source": "list_bounds",
        "add_button_bounds": plus,
        "remove_button_bounds": minus,
        "add_click": center_point(&plus),
        "remove_click": center_point(&minus),
        "footer_bounds": footer
    }))
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

fn bounds_tuple_from_value(value: &serde_json::Value) -> Option<(f64, f64, f64, f64)> {
    Some((
        value.get("x")?.as_f64()?,
        value.get("y")?.as_f64()?,
        value.get("width")?.as_f64()?,
        value.get("height")?.as_f64()?,
    ))
}

fn load_rgba_image(path: &std::path::Path) -> Option<RgbaImage> {
    image::open(path).ok().map(|img| img.to_rgba8())
}

fn estimate_toggle_state(
    image: Option<&RgbaImage>,
    bounds: &desktop_core::protocol::Bounds,
    display_width: u32,
    display_height: u32,
) -> String {
    let image = match image {
        Some(img) => img,
        None => return "unknown".to_string(),
    };
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 || display_width == 0 || display_height == 0 {
        return "unknown".to_string();
    }
    let (x0, y0, x1, y1) =
        match logical_bounds_to_image_rect(bounds, width, height, display_width, display_height) {
            Some(rect) => rect,
            None => return "unknown".to_string(),
        };
    let width = width as i32;
    let height = height as i32;
    if width <= 0 || height <= 0 {
        return "unknown".to_string();
    }

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

fn logical_bounds_to_image_rect(
    bounds: &desktop_core::protocol::Bounds,
    image_width: u32,
    image_height: u32,
    display_width: u32,
    display_height: u32,
) -> Option<(i32, i32, i32, i32)> {
    if image_width == 0 || image_height == 0 || display_width == 0 || display_height == 0 {
        return None;
    }
    let sx = image_width as f64 / display_width as f64;
    let sy = image_height as f64 / display_height as f64;
    let x0 = (bounds.x * sx).floor().max(0.0) as i32;
    let y0 = (bounds.y * sy).floor().max(0.0) as i32;
    let x1 = ((bounds.x + bounds.width) * sx).ceil().max(0.0) as i32;
    let y1 = ((bounds.y + bounds.height) * sy).ceil().max(0.0) as i32;
    let max_x = image_width as i32;
    let max_y = image_height as i32;
    if max_x <= 0 || max_y <= 0 {
        return None;
    }
    let x0 = x0.clamp(0, max_x - 1);
    let y0 = y0.clamp(0, max_y - 1);
    let x1 = x1.clamp(x0 + 1, max_x);
    let y1 = y1.clamp(y0 + 1, max_y);
    Some((x0, y0, x1, y1))
}

fn write_capture_overlay(capture: &vision::pipeline::CaptureResult) -> Result<PathBuf, AppError> {
    let mut image = load_rgba_image(&capture.image_path).ok_or_else(|| {
        AppError::backend_unavailable(format!(
            "failed to load capture image for overlay: {}",
            capture.image_path.display()
        ))
    })?;
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

    let overlay_path = overlay_path_for_capture(&capture.image_path);
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

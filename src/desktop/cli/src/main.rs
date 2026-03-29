mod output;
mod parse;
mod transport;
mod usage;

use desktop_core::{
    error::AppError,
    protocol::{Command, RequestEnvelope, ResponseEnvelope},
};

pub(crate) use parse::parse_command;
#[cfg(test)]
pub(crate) use transport::send_request_with_hooks;
use transport::{map_error_code, next_request_id, send_request_with_autostart, trace_log};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let request_id = next_request_id();
    match run(&args, &request_id) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            let payload = serde_json::json!({
                "ok": false,
                "request_id": request_id,
                "error": {
                    "code": err.code,
                    "message": err.message,
                    "retryable": err.retryable,
                    "command": err.command,
                    "debug_ref": err.debug_ref,
                }
            });
            println!(
                "{}",
                serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string())
            );
            std::process::exit(map_error_code(&err.code));
        }
    }
}

fn run(args: &[String], request_id: &str) -> Result<i32, AppError> {
    let command = parse_command(args)?;
    let passthrough_stored_response = matches!(command, Command::RequestResponse { .. });
    let request = RequestEnvelope::new(request_id.to_string(), command);
    trace_log(format!(
        "run:request_start request_id={} command={}",
        request.request_id,
        request.command.name()
    ));
    let response = send_request_with_autostart(&request)?;

    let rendered =
        output::render_response(&request.command, &response, passthrough_stored_response);
    println!(
        "{}",
        serde_json::to_string_pretty(&rendered).unwrap_or_else(|_| "{}".to_string())
    );
    let code = match response {
        ResponseEnvelope::Success(_) => 0,
        ResponseEnvelope::Error(err) => map_error_code(&err.error.code),
    };
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::{parse_command, send_request_with_hooks};
    use desktop_core::{
        error::{AppError, ErrorCode},
        protocol::{Command, PointerButton, RequestEnvelope, ResponseEnvelope},
    };
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    #[test]
    fn auto_start_invoked_when_daemon_missing() {
        let request = RequestEnvelope::new("r1".to_string(), Command::Ping);
        let attempts = Arc::new(AtomicUsize::new(0));
        let launched = Arc::new(AtomicUsize::new(0));
        let attempts_clone = Arc::clone(&attempts);
        let launched_clone = Arc::clone(&launched);

        let result = send_request_with_hooks(
            &request,
            move |_| {
                let n = attempts_clone.fetch_add(1, Ordering::SeqCst);
                if n == 0 {
                    Err(AppError::daemon_not_running("missing socket"))
                } else {
                    Ok(ResponseEnvelope::success_message("r1", "pong"))
                }
            },
            move || {
                launched_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect("request should succeed after launch");

        assert_eq!(launched.load(Ordering::SeqCst), 1);
        match result {
            ResponseEnvelope::Success(ok) => assert_eq!(ok.result["message"], "pong"),
            ResponseEnvelope::Error(_) => panic!("expected success response"),
        }
    }

    #[test]
    fn auto_start_not_invoked_for_invalid_argument() {
        let request = RequestEnvelope::new("r2".to_string(), Command::Ping);
        let launched = Arc::new(AtomicUsize::new(0));
        let launched_clone = Arc::clone(&launched);

        let err = send_request_with_hooks(
            &request,
            |_| Err(AppError::new(ErrorCode::InvalidArgument, "bad request")),
            move || {
                launched_clone.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect_err("invalid argument should be returned directly");

        assert_eq!(launched.load(Ordering::SeqCst), 0);
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn parses_screen_find_text() {
        let args = vec![
            "screen".to_string(),
            "find".to_string(),
            "--text".to_string(),
            "DesktopCtl".to_string(),
            "--all".to_string(),
        ];
        let command = parse_command(&args).expect("screen find should parse");
        match command {
            Command::ScreenFindText { text, all } => {
                assert_eq!(text, "DesktopCtl");
                assert!(all);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_wait_text() {
        let args = vec![
            "screen".to_string(),
            "wait".to_string(),
            "--text".to_string(),
            "Ready".to_string(),
            "--timeout".to_string(),
            "3000".to_string(),
            "--interval".to_string(),
            "120".to_string(),
        ];
        let command = parse_command(&args).expect("screen wait should parse");
        match command {
            Command::WaitText {
                text,
                timeout_ms,
                interval_ms,
                disappear,
            } => {
                assert_eq!(text, "Ready");
                assert_eq!(timeout_ms, 3000);
                assert_eq!(interval_ms, 120);
                assert!(!disappear);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_wait_disappear() {
        let args = vec![
            "screen".to_string(),
            "wait".to_string(),
            "--text".to_string(),
            "Loading".to_string(),
            "--disappear".to_string(),
        ];
        let command = parse_command(&args).expect("screen wait --disappear should parse");
        match command {
            Command::WaitText {
                text, disappear, ..
            } => {
                assert_eq!(text, "Loading");
                assert!(disappear);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_replay_record_default_duration() {
        let command = parse_command(&["replay", "record"].map(str::to_string))
            .expect("replay record should parse");
        match command {
            Command::ReplayRecord { duration_ms, stop } => {
                assert_eq!(duration_ms, 3_000);
                assert!(!stop);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_replay_record_stop() {
        let command = parse_command(&["replay", "record", "--stop"].map(str::to_string))
            .expect("replay record --stop should parse");
        match command {
            Command::ReplayRecord { stop, .. } => assert!(stop),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_app_open_with_wait() {
        let args = vec![
            "app".to_string(),
            "open".to_string(),
            "Calculator".to_string(),
            "--wait".to_string(),
            "--timeout".to_string(),
            "1500".to_string(),
        ];
        let command = parse_command(&args).expect("app open should parse");
        match command {
            Command::OpenApp {
                name,
                wait,
                timeout_ms,
                ..
            } => {
                assert_eq!(name, "Calculator");
                assert!(wait);
                assert_eq!(timeout_ms, Some(1500));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn rejects_replay_record_duration_over_30m() {
        let err = parse_command(&["replay", "record", "--duration", "1800001"].map(str::to_string))
            .expect_err("duration over max should fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn rejects_top_level_wait_command() {
        let args = vec![
            "wait".to_string(),
            "--text".to_string(),
            "Ready".to_string(),
        ];
        let err = parse_command(&args).expect_err("top-level wait should be invalid");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn rejects_top_level_open_command() {
        let args = vec!["open".to_string(), "Calculator".to_string()];
        let err = parse_command(&args).expect_err("top-level open should be invalid");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn rejects_top_level_ping_command() {
        let args = vec!["ping".to_string()];
        let err = parse_command(&args).expect_err("top-level ping should be invalid");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn rejects_top_level_type_command() {
        let args = vec!["type".to_string(), "hello".to_string()];
        let err = parse_command(&args).expect_err("top-level type should be invalid");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn rejects_top_level_key_command() {
        let args = vec!["key".to_string(), "press".to_string(), "enter".to_string()];
        let err = parse_command(&args).expect_err("top-level key should be invalid");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn parses_screen_screenshot_with_overlay() {
        let args = vec![
            "screen".to_string(),
            "screenshot".to_string(),
            "--out".to_string(),
            "/tmp/cap.png".to_string(),
            "--overlay".to_string(),
            "--active-window".to_string(),
        ];
        let command = parse_command(&args).expect("screen screenshot should parse");
        match command {
            Command::ScreenCapture {
                out_path,
                overlay,
                active_window,
                active_window_id,
                region,
            } => {
                assert_eq!(out_path.as_deref(), Some("/tmp/cap.png"));
                assert!(overlay);
                assert!(active_window);
                assert!(active_window_id.is_none());
                assert!(region.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_screenshot_with_active_window_id() {
        let args = vec![
            "screen".to_string(),
            "screenshot".to_string(),
            "--active-window".to_string(),
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
        ];
        let command = parse_command(&args).expect("screen screenshot with active-window id");
        match command {
            Command::ScreenCapture {
                active_window,
                active_window_id,
                ..
            } => {
                assert!(active_window);
                assert_eq!(
                    active_window_id.as_deref(),
                    Some("550e8400-e29b-41d4-a716-446655440000")
                );
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_screenshot_with_region() {
        let args = vec![
            "screen".to_string(),
            "screenshot".to_string(),
            "--region".to_string(),
            "0".to_string(),
            "80".to_string(),
            "640".to_string(),
            "720".to_string(),
        ];
        let command = parse_command(&args).expect("screen screenshot with region should parse");
        match command {
            Command::ScreenCapture { region, .. } => {
                let region = region.expect("region should be present");
                assert_eq!(region.x, 0.0);
                assert_eq!(region.y, 80.0);
                assert_eq!(region.width, 640.0);
                assert_eq!(region.height, 720.0);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn rejects_screen_screenshot_with_zero_region_size() {
        let args = vec![
            "screen".to_string(),
            "screenshot".to_string(),
            "--region".to_string(),
            "10".to_string(),
            "20".to_string(),
            "0".to_string(),
            "100".to_string(),
        ];
        let err = parse_command(&args).expect_err("zero width region must fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn parses_screen_tokenize_with_overlay() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--overlay".to_string(),
            "/tmp/tokens.overlay.png".to_string(),
        ];
        let command = parse_command(&args).expect("screen tokenize should parse");
        match command {
            Command::ScreenTokenize {
                overlay_out_path,
                window_query,
                screenshot_path,
                active_window,
                active_window_id,
                region,
            } => {
                assert_eq!(overlay_out_path.as_deref(), Some("/tmp/tokens.overlay.png"));
                assert!(window_query.is_none());
                assert!(screenshot_path.is_none());
                assert!(!active_window);
                assert!(active_window_id.is_none());
                assert!(region.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_tokenize_with_window() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--window-query".to_string(),
            "777:3".to_string(),
        ];
        let command = parse_command(&args).expect("screen tokenize should parse");
        match command {
            Command::ScreenTokenize {
                overlay_out_path,
                window_query,
                screenshot_path,
                active_window,
                active_window_id,
                region,
            } => {
                assert!(overlay_out_path.is_none());
                assert_eq!(window_query.as_deref(), Some("777:3"));
                assert!(screenshot_path.is_none());
                assert!(!active_window);
                assert!(active_window_id.is_none());
                assert!(region.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_tokenize_with_active_window_id() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--active-window".to_string(),
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
        ];
        let command =
            parse_command(&args).expect("screen tokenize --active-window <id> should parse");
        match command {
            Command::ScreenTokenize {
                active_window,
                active_window_id,
                window_query,
                screenshot_path,
                region,
                overlay_out_path,
            } => {
                assert!(active_window);
                assert_eq!(
                    active_window_id.as_deref(),
                    Some("550e8400-e29b-41d4-a716-446655440000")
                );
                assert!(window_query.is_none());
                assert!(screenshot_path.is_none());
                assert!(region.is_none());
                assert!(overlay_out_path.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_tokenize_with_active_window() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--active-window".to_string(),
        ];
        let command = parse_command(&args).expect("screen tokenize should parse");
        match command {
            Command::ScreenTokenize {
                overlay_out_path,
                window_query,
                screenshot_path,
                active_window,
                active_window_id,
                region,
            } => {
                assert!(overlay_out_path.is_none());
                assert!(window_query.is_none());
                assert!(screenshot_path.is_none());
                assert!(active_window);
                assert!(active_window_id.is_none());
                assert!(region.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_tokenize_with_region() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--active-window".to_string(),
            "--region".to_string(),
            "10".to_string(),
            "20".to_string(),
            "300".to_string(),
            "400".to_string(),
        ];
        let command = parse_command(&args).expect("screen tokenize with region should parse");
        match command {
            Command::ScreenTokenize { region, .. } => {
                let region = region.expect("region should be present");
                assert_eq!(region.x, 10.0);
                assert_eq!(region.y, 20.0);
                assert_eq!(region.width, 300.0);
                assert_eq!(region.height, 400.0);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn rejects_screen_tokenize_with_zero_region_size() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--region".to_string(),
            "10".to_string(),
            "20".to_string(),
            "300".to_string(),
            "0".to_string(),
        ];
        let err = parse_command(&args).expect_err("zero height region must fail");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn rejects_screen_tokenize_window_with_screenshot() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--window-query".to_string(),
            "123:1".to_string(),
            "--screenshot".to_string(),
            "/tmp/sample.png".to_string(),
        ];
        let err = parse_command(&args).expect_err("must reject incompatible flags");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn rejects_screen_tokenize_active_window_with_window() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--active-window".to_string(),
            "--window-query".to_string(),
            "123:1".to_string(),
        ];
        let err = parse_command(&args).expect_err("must reject incompatible flags");
        assert_eq!(err.code, ErrorCode::InvalidArgument);
    }

    #[test]
    fn parses_debug_overlay_start_stop() {
        let start = parse_command(&["debug", "overlay", "start"].map(str::to_string))
            .expect("debug overlay start should parse");
        assert!(matches!(start, Command::OverlayStart { duration_ms: None }));

        let stop = parse_command(&["debug", "overlay", "stop"].map(str::to_string))
            .expect("debug overlay stop should parse");
        assert!(matches!(stop, Command::OverlayStop));
    }

    #[test]
    fn parses_debug_overlay_start_with_duration() {
        let start =
            parse_command(&["debug", "overlay", "start", "--duration", "1500"].map(str::to_string))
                .expect("debug overlay start with duration should parse");
        match start {
            Command::OverlayStart { duration_ms } => assert_eq!(duration_ms, Some(1500)),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_debug_ping() {
        let command =
            parse_command(&["debug", "ping"].map(str::to_string)).expect("debug ping should parse");
        assert!(matches!(command, Command::Ping));
    }

    #[test]
    fn parses_request_commands() {
        let list = parse_command(&["request", "list", "--limit", "5"].map(str::to_string))
            .expect("request list should parse");
        match list {
            Command::RequestList { limit } => assert_eq!(limit, Some(5)),
            other => panic!("unexpected command: {other:?}"),
        }

        let show = parse_command(&["request", "show", "req-1"].map(str::to_string))
            .expect("request show should parse");
        match show {
            Command::RequestShow { request_id } => assert_eq!(request_id, "req-1"),
            other => panic!("unexpected command: {other:?}"),
        }

        let screenshot = parse_command(
            &["request", "screenshot", "req-2", "--out", "/tmp/req.png"].map(str::to_string),
        )
        .expect("request screenshot should parse");
        match screenshot {
            Command::RequestScreenshot {
                request_id,
                out_path,
            } => {
                assert_eq!(request_id, "req-2");
                assert_eq!(out_path.as_deref(), Some("/tmp/req.png"));
            }
            other => panic!("unexpected command: {other:?}"),
        }

        let response = parse_command(&["request", "response", "req-3"].map(str::to_string))
            .expect("request response should parse");
        match response {
            Command::RequestResponse { request_id } => assert_eq!(request_id, "req-3"),
            other => panic!("unexpected command: {other:?}"),
        }

        let search = parse_command(
            &[
                "request",
                "search",
                "Add task",
                "--limit",
                "10",
                "--command",
                "screen_tokenize",
            ]
            .map(str::to_string),
        )
        .expect("request search should parse");
        match search {
            Command::RequestSearch {
                text,
                limit,
                command,
            } => {
                assert_eq!(text, "Add task");
                assert_eq!(limit, Some(10));
                assert_eq!(command.as_deref(), Some("screen_tokenize"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_click_coordinates_relative_by_default() {
        let command = parse_command(&["pointer", "click", "42", "7"].map(str::to_string))
            .expect("pointer click should parse");
        match command {
            Command::PointerClick { x, y, absolute, .. } => {
                assert_eq!(x, 42);
                assert_eq!(y, 7);
                assert!(!absolute);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_click_coordinates_absolute() {
        let command =
            parse_command(&["pointer", "click", "--absolute", "420", "169"].map(str::to_string))
                .expect("pointer click --absolute should parse");
        match command {
            Command::PointerClick { x, y, absolute, .. } => {
                assert_eq!(x, 420);
                assert_eq!(y, 169);
                assert!(absolute);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_click_text() {
        let args = vec![
            "pointer".to_string(),
            "click".to_string(),
            "--text".to_string(),
            "DesktopCtl".to_string(),
            "--active-window".to_string(),
        ];
        let command = parse_command(&args).expect("pointer click --text should parse");
        match command {
            Command::PointerClickText {
                text,
                active_window,
                active_window_id,
                ..
            } => {
                assert_eq!(text, "DesktopCtl");
                assert!(active_window);
                assert!(active_window_id.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_click_id() {
        let args = vec![
            "pointer".to_string(),
            "click".to_string(),
            "--id".to_string(),
            "button_0018".to_string(),
            "--active-window".to_string(),
        ];
        let command = parse_command(&args).expect("pointer click --id should parse");
        match command {
            Command::PointerClickId {
                id,
                active_window,
                active_window_id,
                ..
            } => {
                assert_eq!(id, "button_0018");
                assert!(active_window);
                assert!(active_window_id.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn rejects_pointer_click_id_without_active_window() {
        let args = vec![
            "pointer".to_string(),
            "click".to_string(),
            "--id".to_string(),
            "button_0018".to_string(),
        ];
        let err =
            parse_command(&args).expect_err("pointer click --id must require --active-window");
        assert!(err.message.contains("requires --active-window"));
    }

    #[test]
    fn parses_pointer_click_id_with_active_window_id() {
        let args = vec![
            "pointer".to_string(),
            "click".to_string(),
            "--id".to_string(),
            "axid_nine".to_string(),
            "--active-window".to_string(),
            "550e8400-e29b-41d4-a716-446655440000".to_string(),
        ];
        let command =
            parse_command(&args).expect("pointer click --id with active window id should parse");
        match command {
            Command::PointerClickId {
                id,
                active_window,
                active_window_id,
                ..
            } => {
                assert_eq!(id, "axid_nine");
                assert!(active_window);
                assert_eq!(
                    active_window_id.as_deref(),
                    Some("550e8400-e29b-41d4-a716-446655440000")
                );
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_click_with_right_button() {
        let command = parse_command(
            &[
                "pointer",
                "click",
                "--button",
                "right",
                "10",
                "20",
                "--active-window",
            ]
            .map(str::to_string),
        )
        .expect("pointer click --button right should parse");
        match command {
            Command::PointerClick {
                x,
                y,
                button,
                active_window,
                ..
            } => {
                assert_eq!(x, 10);
                assert_eq!(y, 20);
                assert!(matches!(button, PointerButton::Right));
                assert!(active_window);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_click_id_with_right_button() {
        let command = parse_command(
            &[
                "pointer",
                "click",
                "--id",
                "button_7",
                "--active-window",
                "--button",
                "right",
            ]
            .map(str::to_string),
        )
        .expect("pointer click --id --button right should parse");
        match command {
            Command::PointerClickId { button, .. } => {
                assert!(matches!(button, PointerButton::Right));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_keyboard_type_and_press() {
        let typed = parse_command(&["keyboard", "type", "hello"].map(str::to_string))
            .expect("keyboard type should parse");
        match typed {
            Command::UiType { text, .. } => assert_eq!(text, "hello"),
            other => panic!("unexpected command: {other:?}"),
        }

        let esc = parse_command(&["keyboard", "press", "escape"].map(str::to_string))
            .expect("keyboard press escape should parse");
        match esc {
            Command::KeyEscape { .. } => {}
            other => panic!("unexpected command: {other:?}"),
        }

        let pressed = parse_command(&["keyboard", "press", "cmd+shift+p"].map(str::to_string))
            .expect("keyboard press should parse");
        match pressed {
            Command::KeyHotkey { hotkey, .. } => assert_eq!(hotkey, "cmd+shift+p"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_app_isolate() {
        let args = vec!["app".to_string(), "isolate".to_string(), "UTM".to_string()];
        let command = parse_command(&args).expect("app isolate should parse");
        match command {
            Command::AppIsolate { name } => assert_eq!(name, "UTM"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_window_list() {
        let command = parse_command(&["window", "list"].map(str::to_string))
            .expect("window list should parse");
        assert!(matches!(command, Command::WindowList));
    }

    #[test]
    fn parses_window_bounds_with_title() {
        let command =
            parse_command(&["window", "bounds", "--title", "Calculator"].map(str::to_string))
                .expect("window bounds should parse");
        match command {
            Command::WindowBounds { title } => assert_eq!(title, "Calculator"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_window_focus_with_title() {
        let command =
            parse_command(&["window", "focus", "--title", "Reminders"].map(str::to_string))
                .expect("window focus should parse");
        match command {
            Command::WindowFocus { title } => assert_eq!(title, "Reminders"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_window_bounds_with_id() {
        let command = parse_command(&["window", "bounds", "--id", "35a5c9"].map(str::to_string))
            .expect("window bounds by id should parse");
        match command {
            Command::WindowBounds { title } => assert_eq!(title, "35a5c9"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_window_focus_with_id() {
        let command = parse_command(&["window", "focus", "--id", "35a5c9"].map(str::to_string))
            .expect("window focus by id should parse");
        match command {
            Command::WindowFocus { title } => assert_eq!(title, "35a5c9"),
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_scroll() {
        let command = parse_command(&["pointer", "scroll", "0", "-320"].map(str::to_string))
            .expect("pointer scroll should parse");
        match command {
            Command::PointerScroll { id, dx, dy, .. } => {
                assert!(id.is_none());
                assert_eq!(dx, 0);
                assert_eq!(dy, -320);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_scroll_by_id() {
        let command = parse_command(
            &[
                "pointer",
                "scroll",
                "--id",
                "axid_ns_7",
                "0",
                "420",
                "--active-window",
                "abc123",
            ]
            .map(str::to_string),
        )
        .expect("pointer scroll --id should parse");
        match command {
            Command::PointerScroll {
                id,
                dx,
                dy,
                active_window,
                active_window_id,
                ..
            } => {
                assert_eq!(id.as_deref(), Some("axid_ns_7"));
                assert_eq!(dx, 0);
                assert_eq!(dy, 420);
                assert!(active_window);
                assert_eq!(active_window_id.as_deref(), Some("abc123"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_move_with_active_window_id() {
        let command = parse_command(
            &["pointer", "move", "10", "20", "--active-window", "abc123"].map(str::to_string),
        )
        .expect("pointer move with active-window id should parse");
        match command {
            Command::PointerMove {
                x,
                y,
                absolute,
                active_window,
                active_window_id,
            } => {
                assert_eq!(x, 10);
                assert_eq!(y, 20);
                assert!(!absolute);
                assert!(active_window);
                assert_eq!(active_window_id.as_deref(), Some("abc123"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_pointer_move_absolute() {
        let command =
            parse_command(&["pointer", "move", "--absolute", "400", "500"].map(str::to_string))
                .expect("pointer move --absolute should parse");
        match command {
            Command::PointerMove {
                x,
                y,
                absolute,
                active_window,
                active_window_id,
            } => {
                assert_eq!(x, 400);
                assert_eq!(y, 500);
                assert!(absolute);
                assert!(!active_window);
                assert!(active_window_id.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }
}

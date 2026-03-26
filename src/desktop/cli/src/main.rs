use desktop_core::{
    error::{AppError, ErrorCode},
    ipc,
    protocol::{Command, RequestEnvelope, ResponseEnvelope},
};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    let (json_output, args) = strip_global_json_flag(raw_args);
    match run(&args, json_output) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            if json_output {
                let payload = serde_json::json!({
                    "ok": false,
                    "request_id": serde_json::Value::Null,
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
            } else {
                eprintln!("error: {err}");
            }
            std::process::exit(map_error_code(&err.code));
        }
    }
}

fn run(args: &[String], json_output: bool) -> Result<i32, AppError> {
    let command = parse_command(args)?;
    let request = RequestEnvelope::new(next_request_id(), command);
    trace_log(format!(
        "run:request_start request_id={} command={}",
        request.request_id,
        request.command.name()
    ));
    let response = send_request_with_autostart(&request)?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&response).unwrap_or_else(|_| "{}".to_string())
        );
        let code = match response {
            ResponseEnvelope::Success(_) => 0,
            ResponseEnvelope::Error(err) => map_error_code(&err.error.code),
        };
        return Ok(code);
    }

    match response {
        ResponseEnvelope::Success(success) => {
            if let Some(message) = success.result.get("message").and_then(|v| v.as_str()) {
                println!("{message}");
            } else if success.result != serde_json::json!({}) {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&success.result)
                        .unwrap_or_else(|_| "{}".to_string())
                );
            }
            Ok(0)
        }
        ResponseEnvelope::Error(err) => Err(AppError::new(err.error.code, err.error.message)
            .with_retryable(err.error.retryable)
            .with_command(err.error.command)
            .with_debug_ref(err.error.debug_ref)),
    }
}

fn parse_command(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }

    match args[0].as_str() {
        "app" => parse_app(&args[1..]),
        "window" => parse_window(&args[1..]),
        "screen" => parse_screen(&args[1..]),
        "clipboard" => parse_clipboard(&args[1..]),
        "debug" => parse_debug(&args[1..]),
        "request" => parse_request(&args[1..]),
        "replay" => parse_replay(&args[1..]),
        "pointer" => parse_pointer(&args[1..]),
        "keyboard" => parse_keyboard(&args[1..]),
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_app(args: &[String]) -> Result<Command, AppError> {
    if args.len() < 2 {
        return Err(AppError::invalid_argument(
            "usage: desktopctl app open <application> [--wait] [--timeout <ms>] [-- <open-args...>] | desktopctl app hide <application> | desktopctl app show <application> | desktopctl app isolate <application>",
        ));
    }

    let action = args[0].as_str();
    if action == "open" {
        return parse_open(&args[1..]);
    }

    let name = args[1..].join(" ").trim().to_string();
    if name.is_empty() {
        return Err(AppError::invalid_argument(
            "missing application name: desktopctl app hide <application>",
        ));
    }

    match action {
        "hide" => Ok(Command::AppHide { name }),
        "show" => Ok(Command::AppShow { name }),
        "isolate" => Ok(Command::AppIsolate { name }),
        _ => Err(AppError::invalid_argument(
            "usage: desktopctl app open <application> [--wait] [--timeout <ms>] [-- <open-args...>] | desktopctl app hide <application> | desktopctl app show <application> | desktopctl app isolate <application>",
        )),
    }
}

fn parse_window(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(
            "usage: desktopctl window list [--json] | desktopctl window bounds --title <text> [--json] | desktopctl window focus --title <text>",
        ));
    }

    match args[0].as_str() {
        "list" => {
            if args.len() > 1 && args[1] != "--json" {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window list [--json]",
                ));
            }
            Ok(Command::WindowList)
        }
        "bounds" => {
            if args.len() < 3 || args[1] != "--title" {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window bounds --title <text> [--json]",
                ));
            }
            let title = args[2].clone();
            if title.trim().is_empty() {
                return Err(AppError::invalid_argument(
                    "missing title: desktopctl window bounds --title <text>",
                ));
            }
            if args.len() > 3 && args[3] != "--json" {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window bounds --title <text> [--json]",
                ));
            }
            Ok(Command::WindowBounds { title })
        }
        "focus" => {
            if args.len() != 3 || args[1] != "--title" {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window focus --title <text>",
                ));
            }
            let title = args[2].clone();
            if title.trim().is_empty() {
                return Err(AppError::invalid_argument(
                    "missing title: desktopctl window focus --title <text>",
                ));
            }
            Ok(Command::WindowFocus { title })
        }
        _ => Err(AppError::invalid_argument(
            "usage: desktopctl window list [--json] | desktopctl window bounds --title <text> [--json] | desktopctl window focus --title <text>",
        )),
    }
}

fn parse_debug(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }
    match args[0].as_str() {
        "snapshot" if args.len() == 1 => Ok(Command::DebugSnapshot),
        "ping" if args.len() == 1 => Ok(Command::Ping),
        "permissions" if args.len() == 1 => Ok(Command::PermissionsCheck),
        "overlay" => {
            if args.len() < 2 {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl debug overlay start [--duration <ms>] | desktopctl debug overlay stop",
                ));
            }
            match args[1].as_str() {
                "start" => {
                    let mut duration_ms: Option<u64> = None;
                    let mut i = 2;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--duration" => {
                                duration_ms = Some(parse_u64(args.get(i + 1), "duration_ms")?);
                                i += 2;
                            }
                            flag => {
                                return Err(AppError::invalid_argument(format!(
                                    "unknown flag for debug overlay start: {flag}"
                                )));
                            }
                        }
                    }
                    Ok(Command::OverlayStart { duration_ms })
                }
                "stop" if args.len() == 2 => Ok(Command::OverlayStop),
                _ => Err(AppError::invalid_argument(
                    "usage: desktopctl debug overlay start [--duration <ms>] | desktopctl debug overlay stop",
                )),
            }
        }
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_replay(args: &[String]) -> Result<Command, AppError> {
    const MAX_REPLAY_DURATION_MS: u64 = 30 * 60 * 1000;
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }
    match args[0].as_str() {
        "load" => {
            if args.len() != 2 {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl replay load <session_dir>",
                ));
            }
            Ok(Command::ReplayLoad {
                session_dir: args[1].clone(),
            })
        }
        "record" => {
            let mut duration_ms = 3_000_u64;
            let mut stop = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--duration" => {
                        duration_ms = parse_u64(args.get(i + 1), "duration_ms")?;
                        i += 2;
                    }
                    "--stop" => {
                        stop = true;
                        i += 1;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for replay record: {flag}"
                        )));
                    }
                }
            }
            if stop && args.len() > 2 {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl replay record [--duration <ms>] | desktopctl replay record --stop",
                ));
            }
            if !stop && duration_ms > MAX_REPLAY_DURATION_MS {
                return Err(AppError::invalid_argument(format!(
                    "duration_ms exceeds max of {MAX_REPLAY_DURATION_MS}"
                )));
            }
            Ok(Command::ReplayRecord { duration_ms, stop })
        }
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_request(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }
    match args[0].as_str() {
        "show" => {
            let request_id = args.get(1).cloned().ok_or_else(|| {
                AppError::invalid_argument("usage: desktopctl request show <request_id>")
            })?;
            Ok(Command::RequestShow { request_id })
        }
        "screenshot" => {
            let request_id = args.get(1).cloned().ok_or_else(|| {
                AppError::invalid_argument(
                    "usage: desktopctl request screenshot <request_id> [--out <path>]",
                )
            })?;
            let mut out_path: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--out" => {
                        out_path = Some(
                            args.get(i + 1)
                                .cloned()
                                .ok_or_else(|| AppError::invalid_argument("missing out_path"))?,
                        );
                        i += 2;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for request screenshot: {flag}"
                        )));
                    }
                }
            }
            Ok(Command::RequestScreenshot {
                request_id,
                out_path,
            })
        }
        "response" => {
            let request_id = args.get(1).cloned().ok_or_else(|| {
                AppError::invalid_argument("usage: desktopctl request response <request_id>")
            })?;
            Ok(Command::RequestResponse { request_id })
        }
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_clipboard(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }
    match args[0].as_str() {
        "read" => Ok(Command::ClipboardRead),
        "write" => {
            let text = args.get(1).cloned().ok_or_else(|| {
                AppError::invalid_argument("usage: desktopctl clipboard write <text>")
            })?;
            Ok(Command::ClipboardWrite { text })
        }
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_screen(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }

    match args[0].as_str() {
        "screenshot" => {
            let mut out_path: Option<String> = None;
            let mut overlay = false;
            let mut active_window = false;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--out" => {
                        let path = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --out: desktopctl screen screenshot --out <path>",
                            )
                        })?;
                        out_path = Some(path.clone());
                        i += 2;
                    }
                    "--overlay" => {
                        overlay = true;
                        i += 1;
                    }
                    "--active-window" => {
                        active_window = true;
                        i += 1;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for screen screenshot: {flag}"
                        )));
                    }
                }
            }
            Ok(Command::ScreenCapture {
                out_path,
                overlay,
                active_window,
            })
        }
        "tokenize" => {
            let mut overlay_out_path: Option<String> = None;
            let mut window_id: Option<String> = None;
            let mut screenshot_path: Option<String> = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--json" => {
                        i += 1;
                    }
                    "--overlay" => {
                        let path = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --overlay: desktopctl screen tokenize [--json] [--overlay <path>]",
                            )
                        })?;
                        overlay_out_path = Some(path.clone());
                        i += 2;
                    }
                    "--window" => {
                        let id = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --window: desktopctl screen tokenize [--json] [--overlay <path>] [--window <id>] [--screenshot <path>]",
                            )
                        })?;
                        if id.trim().is_empty() {
                            return Err(AppError::invalid_argument("window id must not be empty"));
                        }
                        window_id = Some(id.clone());
                        i += 2;
                    }
                    "--screenshot" => {
                        let path = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --screenshot: desktopctl screen tokenize [--json] [--overlay <path>] [--window <id>] [--screenshot <path>]",
                            )
                        })?;
                        screenshot_path = Some(path.clone());
                        i += 2;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for screen tokenize: {flag}"
                        )));
                    }
                }
            }
            if window_id.is_some() && screenshot_path.is_some() {
                return Err(AppError::invalid_argument(
                    "--window cannot be combined with --screenshot for screen tokenize",
                ));
            }
            Ok(Command::ScreenTokenize {
                overlay_out_path,
                window_id,
                screenshot_path,
            })
        }
        "find" => {
            if args.len() < 3 || args[1] != "--text" {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl screen find --text <text> [--all] [--json]",
                ));
            }
            let text = args[2].clone();
            let mut all = false;
            let mut i = 3;
            while i < args.len() {
                match args[i].as_str() {
                    "--all" => {
                        all = true;
                        i += 1;
                    }
                    "--json" => {
                        i += 1;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for screen find: {flag}"
                        )));
                    }
                }
            }
            Ok(Command::ScreenFindText { text, all })
        }
        "wait" => parse_screen_wait(&args[1..]),
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_screen_wait(args: &[String]) -> Result<Command, AppError> {
    if args.first().map(String::as_str) != Some("--text") {
        return Err(AppError::invalid_argument(
            "usage: desktopctl screen wait --text <text> [--timeout <ms>] [--interval <ms>] [--disappear]",
        ));
    }

    let text = args
        .get(1)
        .cloned()
        .ok_or_else(|| AppError::invalid_argument("usage: desktopctl screen wait --text <text>"))?;
    let mut timeout_ms = 8_000_u64;
    let mut interval_ms = 200_u64;
    let mut disappear = false;
    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout" => {
                timeout_ms = parse_u64(args.get(i + 1), "timeout_ms")?;
                i += 2;
            }
            "--interval" => {
                interval_ms = parse_u64(args.get(i + 1), "interval_ms")?;
                i += 2;
            }
            "--disappear" => {
                disappear = true;
                i += 1;
            }
            flag => {
                return Err(AppError::invalid_argument(format!(
                    "unknown flag for screen wait: {flag}"
                )));
            }
        }
    }
    Ok(Command::WaitText {
        text,
        timeout_ms,
        interval_ms,
        disappear,
    })
}

fn parse_open(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }
    let mut wait = false;
    let mut timeout_ms: Option<u64> = None;
    let mut app_name_parts: Vec<String> = Vec::new();
    let mut trailing: Vec<String> = Vec::new();
    let mut passthrough = false;
    let mut i = 0;
    while i < args.len() {
        let token = &args[i];
        if token == "--" {
            passthrough = true;
            i += 1;
            continue;
        }

        if passthrough {
            trailing.push(token.clone());
            i += 1;
            continue;
        }

        match token.as_str() {
            "--wait" => {
                wait = true;
                i += 1;
            }
            "--timeout" => {
                timeout_ms = Some(parse_u64(args.get(i + 1), "timeout_ms")?);
                i += 2;
            }
            _ => {
                app_name_parts.push(token.clone());
                i += 1;
            }
        }
    }

    if app_name_parts.is_empty() {
        return Err(AppError::invalid_argument(
            "missing app name: desktopctl app open <application> [-- <open-args...>]",
        ));
    }

    Ok(Command::OpenApp {
        name: app_name_parts.join(" "),
        args: trailing,
        wait,
        timeout_ms,
    })
}

fn parse_pointer(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }

    match args[0].as_str() {
        "move" => {
            let x = parse_u32(args.get(1), "x")?;
            let y = parse_u32(args.get(2), "y")?;
            Ok(Command::PointerMove { x, y })
        }
        "down" => {
            let x = parse_u32(args.get(1), "x")?;
            let y = parse_u32(args.get(2), "y")?;
            Ok(Command::PointerDown { x, y })
        }
        "up" => {
            let x = parse_u32(args.get(1), "x")?;
            let y = parse_u32(args.get(2), "y")?;
            Ok(Command::PointerUp { x, y })
        }
        "click" => {
            if args.len() >= 2 && args[1] == "--text" {
                let text = args.get(2).cloned().ok_or_else(|| {
                    AppError::invalid_argument("usage: desktopctl pointer click --text <text>")
                })?;
                if args.len() > 3 {
                    return Err(AppError::invalid_argument(format!(
                        "unknown flag for pointer click --text: {}",
                        args[3]
                    )));
                }
                Ok(Command::PointerClickText { text })
            } else if args.len() >= 2 && args[1] == "--token" {
                let token = parse_u32(args.get(2), "token")?;
                Ok(Command::PointerClickToken { token })
            } else {
                let x = parse_u32(args.get(1), "x")?;
                let y = parse_u32(args.get(2), "y")?;
                Ok(Command::PointerClick { x, y })
            }
        }
        "drag" => {
            let x1 = parse_u32(args.get(1), "x1")?;
            let y1 = parse_u32(args.get(2), "y1")?;
            let x2 = parse_u32(args.get(3), "x2")?;
            let y2 = parse_u32(args.get(4), "y2")?;
            let hold_ms = args
                .get(5)
                .map(|v| parse_u64(Some(v), "hold_ms"))
                .transpose()?
                .unwrap_or(60);
            Ok(Command::PointerDrag {
                x1,
                y1,
                x2,
                y2,
                hold_ms,
            })
        }
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_keyboard(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }

    match args[0].as_str() {
        "type" => {
            let text = args.get(1).cloned().ok_or_else(|| {
                AppError::invalid_argument("missing text: desktopctl keyboard type \"text\"")
            })?;
            Ok(Command::UiType { text })
        }
        "press" => {
            let key = args.get(1).cloned().ok_or_else(|| {
                AppError::invalid_argument("missing key: desktopctl keyboard press enter")
            })?;
            if key.eq_ignore_ascii_case("enter") || key.eq_ignore_ascii_case("return") {
                Ok(Command::KeyEnter)
            } else {
                Ok(Command::KeyHotkey { hotkey: key })
            }
        }
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_u32(value: Option<&String>, field: &str) -> Result<u32, AppError> {
    let raw = value.ok_or_else(|| AppError::invalid_argument(format!("missing {field}")))?;
    raw.parse::<u32>()
        .map_err(|_| AppError::invalid_argument(format!("invalid {field}: {raw}")))
}

fn parse_u64(value: Option<&String>, field: &str) -> Result<u64, AppError> {
    let raw = value.ok_or_else(|| AppError::invalid_argument(format!("missing {field}")))?;
    raw.parse::<u64>()
        .map_err(|_| AppError::invalid_argument(format!("invalid {field}: {raw}")))
}

fn usage() -> &'static str {
    "usage:
  desktopctl app open <application> [--wait] [--timeout <ms>] [-- <open-args...>]
  desktopctl app hide <application>
  desktopctl app show <application>
  desktopctl app isolate <application>
  desktopctl window list [--json]
  desktopctl window bounds --title <text> [--json]
  desktopctl window focus --title <text>
  desktopctl screen screenshot [--out <path>] [--overlay] [--active-window]
  desktopctl screen tokenize [--json] [--overlay <path>] [--window <id>] [--screenshot <path>]
  desktopctl screen find --text <text> [--all] [--json]
  desktopctl screen wait --text <text> [--timeout <ms>] [--interval <ms>] [--disappear]
  desktopctl clipboard read
  desktopctl clipboard write <text>
  desktopctl debug permissions
  desktopctl debug ping
  desktopctl debug overlay start [--duration <ms>]
  desktopctl debug overlay stop
  desktopctl debug snapshot
  desktopctl request show <request_id>
  desktopctl request screenshot <request_id> [--out <path>]
  desktopctl request response <request_id>
  desktopctl replay record [--duration <ms>]
  desktopctl replay record --stop
  desktopctl replay load <session_dir>
  desktopctl pointer move <x> <y>
  desktopctl pointer down <x> <y>
  desktopctl pointer up <x> <y>
  desktopctl pointer click <x> <y>
  desktopctl pointer click --text <text>
  desktopctl pointer click --token <n>
  desktopctl pointer drag <x1> <y1> <x2> <y2> [hold_ms]
  desktopctl keyboard type \"text\"
  desktopctl keyboard press <key-or-hotkey>"
}

fn send_request_with_autostart(request: &RequestEnvelope) -> Result<ResponseEnvelope, AppError> {
    send_request_with_hooks(request, ipc::send_request, launch_daemon)
}

fn send_request_with_hooks<FSend, FLaunch>(
    request: &RequestEnvelope,
    mut send: FSend,
    mut launch: FLaunch,
) -> Result<ResponseEnvelope, AppError>
where
    FSend: FnMut(&RequestEnvelope) -> Result<ResponseEnvelope, AppError>,
    FLaunch: FnMut() -> Result<(), AppError>,
{
    trace_log(format!(
        "send:attempt_initial request_id={} command={}",
        request.request_id,
        request.command.name()
    ));
    match send(request) {
        Ok(response) => {
            trace_log("send:initial_ok");
            Ok(response)
        }
        Err(err) if err.code == ErrorCode::DaemonNotRunning => {
            trace_log("send:daemon_not_running_autostart");
            launch()?;
            retry_request(request, &mut send)
        }
        Err(err) => {
            trace_log(format!(
                "send:initial_err code={:?} msg={}",
                err.code, err.message
            ));
            Err(err)
        }
    }
}

fn strip_global_json_flag(mut args: Vec<String>) -> (bool, Vec<String>) {
    if let Some(pos) = args.iter().position(|arg| arg == "--json") {
        args.remove(pos);
        (true, args)
    } else {
        (false, args)
    }
}

fn retry_request<FSend>(
    request: &RequestEnvelope,
    send: &mut FSend,
) -> Result<ResponseEnvelope, AppError>
where
    FSend: FnMut(&RequestEnvelope) -> Result<ResponseEnvelope, AppError>,
{
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_error: Option<AppError> = None;
    let mut attempt = 0_u32;
    while Instant::now() < deadline {
        attempt += 1;
        match send(request) {
            Ok(response) => {
                trace_log(format!("retry:ok attempt={attempt}"));
                return Ok(response);
            }
            Err(err)
                if err.code == ErrorCode::DaemonNotRunning
                    || err.code == ErrorCode::BackendUnavailable =>
            {
                trace_log(format!(
                    "retry:transient attempt={} code={:?} msg={}",
                    attempt, err.code, err.message
                ));
                last_error = Some(err);
                thread::sleep(Duration::from_millis(150));
            }
            Err(err) => {
                trace_log(format!(
                    "retry:non_transient attempt={} code={:?} msg={}",
                    attempt, err.code, err.message
                ));
                return Err(err);
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        AppError::daemon_not_running("daemon did not become ready after auto-start")
    }))
}

fn launch_daemon() -> Result<(), AppError> {
    if let Some(app_path) = discover_daemon_app_path() {
        let autostart_mode =
            std::env::var("DESKTOPCTL_AUTOSTART_MODE").unwrap_or_else(|_| "resident".to_string());
        trace_log(format!(
            "launch:app_path={} mode={}",
            app_path.display(),
            autostart_mode
        ));
        let mut open_cmd = ProcessCommand::new("open");
        open_cmd.arg("-g").arg(app_path);
        if autostart_mode.eq_ignore_ascii_case("on-demand") {
            open_cmd.arg("--args").arg("--on-demand");
        }

        let status = open_cmd.status().map_err(|err| {
            AppError::backend_unavailable(format!("failed to launch app bundle: {err}"))
        })?;
        if status.success() {
            trace_log("launch:app_ok");
            return Ok(());
        }
        trace_log(format!("launch:app_failed status={status}"));
    }

    if let Some(daemon_bin) = discover_daemon_binary_path() {
        trace_log(format!("launch:daemon_bin={}", daemon_bin.display()));
        ProcessCommand::new(daemon_bin)
            .arg("--on-demand")
            .spawn()
            .map_err(|err| {
                AppError::backend_unavailable(format!("failed to launch daemon binary: {err}"))
            })?;
        trace_log("launch:daemon_bin_ok");
        return Ok(());
    }

    trace_log("launch:no_binary_or_app");
    Err(AppError::daemon_not_running(
        "unable to auto-start daemon; run `just build` and retry",
    ))
}

fn discover_daemon_app_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DESKTOPCTL_APP_PATH") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    if let Ok(cwd) = std::env::current_dir() {
        let candidate = cwd.join("dist/DesktopCtl.app");
        if candidate.exists() {
            return Some(candidate);
        }
    }

    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let sibling = exe_dir.join("DesktopCtl.app");
    if sibling.exists() {
        return Some(sibling);
    }

    let mut cursor: Option<&Path> = Some(exe_dir);
    while let Some(dir) = cursor {
        let candidate = dir.join("dist/DesktopCtl.app");
        if candidate.exists() {
            return Some(candidate);
        }
        cursor = dir.parent();
    }

    None
}

fn discover_daemon_binary_path() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DESKTOPCTL_DAEMON_BIN") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;
    let sibling = exe_dir.join("desktopctld");
    if sibling.exists() {
        return Some(sibling);
    }
    None
}

fn next_request_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

fn trace_log(message: impl AsRef<str>) {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    let tid = format!("{:?}", std::thread::current().id());
    let line = format!("{ts} pid={pid} tid={tid} {}\n", message.as_ref());
    let path = std::env::var("DESKTOPCTL_CLI_TRACE_PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "/tmp/desktopctl.cli.trace.log".to_string());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

fn map_error_code(code: &ErrorCode) -> i32 {
    match code {
        ErrorCode::PermissionDenied => 2,
        ErrorCode::Timeout => 3,
        ErrorCode::TargetNotFound => 4,
        ErrorCode::InvalidArgument => 5,
        ErrorCode::DaemonNotRunning | ErrorCode::BackendUnavailable => 6,
        ErrorCode::LowConfidence => 7,
        ErrorCode::AmbiguousTarget => 8,
        ErrorCode::PostconditionFailed => 9,
        ErrorCode::Internal => 10,
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_command, send_request_with_hooks};
    use desktop_core::{
        error::{AppError, ErrorCode},
        protocol::{Command, RequestEnvelope, ResponseEnvelope},
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
            "--json".to_string(),
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
            } => {
                assert_eq!(out_path.as_deref(), Some("/tmp/cap.png"));
                assert!(overlay);
                assert!(active_window);
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_tokenize_with_overlay() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--json".to_string(),
            "--overlay".to_string(),
            "/tmp/tokens.overlay.png".to_string(),
        ];
        let command = parse_command(&args).expect("screen tokenize should parse");
        match command {
            Command::ScreenTokenize {
                overlay_out_path,
                window_id,
                screenshot_path,
            } => {
                assert_eq!(overlay_out_path.as_deref(), Some("/tmp/tokens.overlay.png"));
                assert!(window_id.is_none());
                assert!(screenshot_path.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_screen_tokenize_with_window() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--window".to_string(),
            "777:3".to_string(),
        ];
        let command = parse_command(&args).expect("screen tokenize should parse");
        match command {
            Command::ScreenTokenize {
                overlay_out_path,
                window_id,
                screenshot_path,
            } => {
                assert!(overlay_out_path.is_none());
                assert_eq!(window_id.as_deref(), Some("777:3"));
                assert!(screenshot_path.is_none());
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn rejects_screen_tokenize_window_with_screenshot() {
        let args = vec![
            "screen".to_string(),
            "tokenize".to_string(),
            "--window".to_string(),
            "123:1".to_string(),
            "--screenshot".to_string(),
            "/tmp/sample.png".to_string(),
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
    }

    #[test]
    fn parses_pointer_click_text() {
        let args = vec![
            "pointer".to_string(),
            "click".to_string(),
            "--text".to_string(),
            "DesktopCtl".to_string(),
        ];
        let command = parse_command(&args).expect("pointer click --text should parse");
        match command {
            Command::PointerClickText { text } => {
                assert_eq!(text, "DesktopCtl");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn parses_keyboard_type_and_press() {
        let typed = parse_command(&["keyboard", "type", "hello"].map(str::to_string))
            .expect("keyboard type should parse");
        match typed {
            Command::UiType { text } => assert_eq!(text, "hello"),
            other => panic!("unexpected command: {other:?}"),
        }

        let pressed = parse_command(&["keyboard", "press", "cmd+shift+p"].map(str::to_string))
            .expect("keyboard press should parse");
        match pressed {
            Command::KeyHotkey { hotkey } => assert_eq!(hotkey, "cmd+shift+p"),
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
        let command = parse_command(&["window", "list", "--json"].map(str::to_string))
            .expect("window list should parse");
        assert!(matches!(command, Command::WindowList));
    }

    #[test]
    fn parses_window_bounds_with_title() {
        let command = parse_command(
            &["window", "bounds", "--title", "Calculator", "--json"].map(str::to_string),
        )
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
    fn parses_pointer_click_token() {
        let token = parse_command(&["pointer", "click", "--token", "42"].map(str::to_string))
            .expect("pointer click --token should parse");
        match token {
            Command::PointerClickToken { token } => assert_eq!(token, 42),
            other => panic!("unexpected command: {other:?}"),
        }
    }
}

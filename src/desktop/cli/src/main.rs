use desktop_core::{
    error::{AppError, ErrorCode},
    ipc,
    protocol::{Command, RequestEnvelope, ResponseEnvelope},
};
use std::{
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    thread,
    time::{Duration, Instant},
};

fn main() {
    match run() {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("error: {err}");
            std::process::exit(map_error_code(&err.code));
        }
    }
}

fn run() -> Result<i32, AppError> {
    let args: Vec<String> = std::env::args().collect();
    let command = parse_command(&args[1..])?;
    let request = RequestEnvelope::new(next_request_id(), command);
    let response = send_request_with_autostart(&request)?;

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
        "ping" => Ok(Command::Ping),
        "open" => parse_open(&args[1..]),
        "screen" => parse_screen(&args[1..]),
        "ui" => parse_ui(&args[1..]),
        "permissions" => parse_permissions(&args[1..]),
        "clipboard" => parse_clipboard(&args[1..]),
        "debug" => parse_debug(&args[1..]),
        "replay" => parse_replay(&args[1..]),
        "pointer" => parse_pointer(&args[1..]),
        "type" => parse_type(&args[1..]),
        "key" => parse_key(&args[1..]),
        "wait" => parse_wait(&args[1..]),
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_permissions(args: &[String]) -> Result<Command, AppError> {
    if args.len() == 1 && args[0] == "check" {
        return Ok(Command::PermissionsCheck);
    }
    Err(AppError::invalid_argument(usage()))
}

fn parse_debug(args: &[String]) -> Result<Command, AppError> {
    if args.len() == 1 && args[0] == "snapshot" {
        return Ok(Command::DebugSnapshot);
    }
    Err(AppError::invalid_argument(usage()))
}

fn parse_replay(args: &[String]) -> Result<Command, AppError> {
    if args.len() == 2 && args[0] == "load" {
        return Ok(Command::ReplayLoad {
            session_dir: args[1].clone(),
        });
    }
    Err(AppError::invalid_argument(usage()))
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

fn parse_wait(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }

    if args[0] == "--text" {
        let text = args
            .get(1)
            .cloned()
            .ok_or_else(|| AppError::invalid_argument("usage: desktopctl wait --text <text>"))?;
        let mut timeout_ms = 8_000_u64;
        let mut interval_ms = 200_u64;
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
                flag => {
                    return Err(AppError::invalid_argument(format!(
                        "unknown flag for wait --text: {flag}"
                    )));
                }
            }
        }
        return Ok(Command::WaitText {
            text,
            timeout_ms,
            interval_ms,
        });
    }

    let ms = parse_u64(args.first(), "ms")?;
    Ok(Command::Wait { ms })
}

fn parse_screen(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }

    match args[0].as_str() {
        "capture" => {
            let mut out_path: Option<String> = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--out" => {
                        let path = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --out: desktopctl screen capture --out <path>",
                            )
                        })?;
                        out_path = Some(path.clone());
                        i += 2;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for screen capture: {flag}"
                        )));
                    }
                }
            }
            Ok(Command::ScreenCapture { out_path })
        }
        "snapshot" => {
            if args.get(1).is_some() && args.get(1).map(String::as_str) != Some("--json") {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl screen snapshot [--json]",
                ));
            }
            Ok(Command::ScreenSnapshot)
        }
        "tokenize" => {
            if args.get(1).is_some() && args.get(1).map(String::as_str) != Some("--json") {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl screen tokenize [--json]",
                ));
            }
            Ok(Command::ScreenTokenize)
        }
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_ui(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }

    match args[0].as_str() {
        "click" => {
            if args.len() < 3 {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl ui click --text <text> [--timeout <ms>] | --token <n>",
                ));
            }
            match args[1].as_str() {
                "--text" => {
                    let text = args[2].clone();
                    let mut timeout_ms = 2_000_u64;
                    let mut i = 3;
                    while i < args.len() {
                        match args[i].as_str() {
                            "--timeout" => {
                                timeout_ms = parse_u64(args.get(i + 1), "timeout_ms")?;
                                i += 2;
                            }
                            flag => {
                                return Err(AppError::invalid_argument(format!(
                                    "unknown flag for ui click --text: {flag}"
                                )));
                            }
                        }
                    }
                    Ok(Command::UiClickText { text, timeout_ms })
                }
                "--token" => {
                    let token = parse_u32(args.get(2), "token")?;
                    Ok(Command::UiClickToken { token })
                }
                _ => Err(AppError::invalid_argument(
                    "usage: desktopctl ui click --text <text> [--timeout <ms>] | --token <n>",
                )),
            }
        }
        "read" => Ok(Command::UiRead),
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_open(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }

    if args.len() == 1 {
        match args[0].as_str() {
            "spotlight" => return Ok(Command::OpenSpotlight),
            "launchpad" => return Ok(Command::OpenLaunchpad),
            _ => {}
        }
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
            "missing app name: desktopctl open <application> [-- <open-args...>]",
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
            let x = parse_u32(args.get(1), "x")?;
            let y = parse_u32(args.get(2), "y")?;
            Ok(Command::PointerClick { x, y })
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

fn parse_type(args: &[String]) -> Result<Command, AppError> {
    let text = args
        .first()
        .cloned()
        .ok_or_else(|| AppError::invalid_argument("missing text: desktopctl type \"text\""))?;
    Ok(Command::UiType { text })
}

fn parse_key(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(usage()));
    }

    match args[0].as_str() {
        "press" => {
            let key = args.get(1).cloned().ok_or_else(|| {
                AppError::invalid_argument("missing key: desktopctl key press enter")
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
  desktopctl ping
  desktopctl open <application> [--wait] [--timeout <ms>] [-- <open-args...>]
  desktopctl open spotlight
  desktopctl open launchpad
  desktopctl screen capture [--out <path>]
  desktopctl screen snapshot [--json]
  desktopctl screen tokenize [--json]
  desktopctl ui click --text <text> [--timeout <ms>]
  desktopctl ui click --token <n>
  desktopctl ui read
  desktopctl permissions check
  desktopctl clipboard read
  desktopctl clipboard write <text>
  desktopctl debug snapshot
  desktopctl replay load <session_dir>
  desktopctl pointer move <x> <y>
  desktopctl pointer down <x> <y>
  desktopctl pointer up <x> <y>
  desktopctl pointer click <x> <y>
  desktopctl pointer drag <x1> <y1> <x2> <y2> [hold_ms]
  desktopctl type \"text\"
  desktopctl key press <key-or-hotkey>
  desktopctl wait <ms>
  desktopctl wait --text <text> [--timeout <ms>] [--interval <ms>]"
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
    match send(request) {
        Ok(response) => Ok(response),
        Err(err) if err.code == ErrorCode::DaemonNotRunning => {
            launch()?;
            retry_request(request, &mut send)
        }
        Err(err) => Err(err),
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
    while Instant::now() < deadline {
        match send(request) {
            Ok(response) => return Ok(response),
            Err(err)
                if err.code == ErrorCode::DaemonNotRunning
                    || err.code == ErrorCode::BackendUnavailable =>
            {
                last_error = Some(err);
                thread::sleep(Duration::from_millis(150));
            }
            Err(err) => return Err(err),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        AppError::daemon_not_running("daemon did not become ready after auto-start")
    }))
}

fn launch_daemon() -> Result<(), AppError> {
    if let Some(app_path) = discover_daemon_app_path() {
        let autostart_mode = std::env::var("DESKTOPCTL_AUTOSTART_MODE")
            .unwrap_or_else(|_| "resident".to_string());
        let mut open_cmd = ProcessCommand::new("open");
        open_cmd.arg("-g").arg(app_path);
        if autostart_mode.eq_ignore_ascii_case("on-demand") {
            open_cmd.arg("--args").arg("--on-demand");
        }

        let status = open_cmd.status().map_err(|err| {
            AppError::backend_unavailable(format!("failed to launch app bundle: {err}"))
        })?;
        if status.success() {
            return Ok(());
        }
    }

    if let Some(daemon_bin) = discover_daemon_binary_path() {
        ProcessCommand::new(daemon_bin)
            .arg("--on-demand")
            .spawn()
            .map_err(|err| {
                AppError::backend_unavailable(format!("failed to launch daemon binary: {err}"))
            })?;
        return Ok(());
    }

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
    use std::time::{SystemTime, UNIX_EPOCH};
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    format!("req-{ts}")
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
    use super::send_request_with_hooks;
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
}

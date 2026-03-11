use desktop_core::{
    error::{AppError, ErrorCode},
    ipc,
    protocol::{Command, RequestEnvelope, ResponseEnvelope},
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
    let response = ipc::send_request(&request)?;

    match response {
        ResponseEnvelope::Success(success) => {
            if let Some(message) = success.result.get("message").and_then(|v| v.as_str()) {
                println!("{message}");
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
        "pointer" => parse_pointer(&args[1..]),
        "type" => parse_type(&args[1..]),
        "key" => parse_key(&args[1..]),
        "wait" => {
            let ms = parse_u64(args.get(1), "ms")?;
            Ok(Command::Wait { ms })
        }
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

    let sep = args.iter().position(|x| x == "--");
    let (name_parts, trailing): (&[String], &[String]) = match sep {
        Some(idx) => (&args[..idx], &args[idx + 1..]),
        None => (args, &[]),
    };

    if name_parts.is_empty() {
        return Err(AppError::invalid_argument(
            "missing app name: desktopctl open <application> [-- <open-args...>]",
        ));
    }

    Ok(Command::OpenApp {
        name: name_parts.join(" "),
        args: trailing.to_vec(),
        wait: false,
        timeout_ms: None,
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
  desktopctl open <application> [-- <open-args...>]
  desktopctl open spotlight
  desktopctl open launchpad
  desktopctl pointer move <x> <y>
  desktopctl pointer down <x> <y>
  desktopctl pointer up <x> <y>
  desktopctl pointer click <x> <y>
  desktopctl pointer drag <x1> <y1> <x2> <y2> [hold_ms]
  desktopctl type \"text\"
  desktopctl key press <key-or-hotkey>
  desktopctl wait <ms>"
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

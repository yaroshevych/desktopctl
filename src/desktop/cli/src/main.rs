use desktop_core::{
    error::AppError,
    ipc,
    protocol::{Command, Request},
};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), AppError> {
    let args: Vec<String> = std::env::args().collect();
    let command = parse_command(&args[1..])?;
    let response = ipc::send_request(&Request { command })?;

    if response.ok {
        if let Some(message) = response.message {
            println!("{message}");
        }
        Ok(())
    } else {
        Err(AppError::Ipc(
            response
                .message
                .unwrap_or_else(|| "unknown daemon error".to_string()),
        ))
    }
}

fn parse_command(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::Cli(usage().to_string()));
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
        _ => Err(AppError::Cli(usage().to_string())),
    }
}

fn parse_open(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::Cli(usage().to_string()));
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
        return Err(AppError::Cli(
            "missing app name: desktopctl open <application> [-- <open-args...>]".to_string(),
        ));
    }

    Ok(Command::OpenApp {
        name: name_parts.join(" "),
        args: trailing.to_vec(),
    })
}

fn parse_pointer(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::Cli(usage().to_string()));
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
        _ => Err(AppError::Cli(usage().to_string())),
    }
}

fn parse_type(args: &[String]) -> Result<Command, AppError> {
    let text = args
        .first()
        .cloned()
        .ok_or_else(|| AppError::Cli("missing text: desktopctl type \"text\"".to_string()))?;
    Ok(Command::UiType { text })
}

fn parse_key(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::Cli(usage().to_string()));
    }

    match args[0].as_str() {
        "press" => {
            let key = args.get(1).cloned().ok_or_else(|| {
                AppError::Cli("missing key: desktopctl key press enter".to_string())
            })?;
            if key.eq_ignore_ascii_case("enter") || key.eq_ignore_ascii_case("return") {
                Ok(Command::KeyEnter)
            } else {
                Ok(Command::KeyHotkey { hotkey: key })
            }
        }
        _ => Err(AppError::Cli(usage().to_string())),
    }
}

fn parse_u32(value: Option<&String>, field: &str) -> Result<u32, AppError> {
    let raw = value.ok_or_else(|| AppError::Cli(format!("missing {field}")))?;
    raw.parse::<u32>()
        .map_err(|_| AppError::Cli(format!("invalid {field}: {raw}")))
}

fn parse_u64(value: Option<&String>, field: &str) -> Result<u64, AppError> {
    let raw = value.ok_or_else(|| AppError::Cli(format!("missing {field}")))?;
    raw.parse::<u64>()
        .map_err(|_| AppError::Cli(format!("invalid {field}: {raw}")))
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

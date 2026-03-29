use desktop_core::{
    error::{AppError, ErrorCode},
    ipc,
    protocol::{
        Command, ObserveOptions, ObserveUntil, PointerButton, RequestEnvelope, ResponseEnvelope,
    },
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
    let request_id = next_request_id();
    match run(&args, json_output, &request_id) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            if json_output {
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
            } else {
                eprintln!("error: {err}");
            }
            std::process::exit(map_error_code(&err.code));
        }
    }
}

fn run(args: &[String], json_output: bool, request_id: &str) -> Result<i32, AppError> {
    let command = parse_command(args)?;
    let supports_active_window = command_supports_active_window(&command);
    let has_explicit_active_window_id = command_has_explicit_active_window_id(&command);
    let json_hints = command_json_hints(&command);
    let request = RequestEnvelope::new(request_id.to_string(), command);
    trace_log(format!(
        "run:request_start request_id={} command={}",
        request.request_id,
        request.command.name()
    ));
    let response = send_request_with_autostart(&request)?;

    if json_output {
        let mut prefix_fields: Vec<(String, String)> = Vec::new();
        if supports_active_window && !has_explicit_active_window_id {
            prefix_fields.push(("tip".to_string(), active_window_tip_message()));
        }
        for (idx, hint) in json_hints.iter().enumerate() {
            let key = if idx == 0 {
                "hint".to_string()
            } else {
                format!("hint_{}", idx + 1)
            };
            prefix_fields.push((key, (*hint).to_string()));
        }
        let rendered = render_response_with_prefix_fields(&response, &prefix_fields);
        println!(
            "{}",
            serde_json::to_string_pretty(&rendered).unwrap_or_else(|_| "{}".to_string())
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

fn command_supports_active_window(command: &Command) -> bool {
    matches!(
        command,
        Command::PointerMove { .. }
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
            | Command::ScreenCapture { .. }
            | Command::ScreenTokenize { .. }
    )
}

fn command_has_explicit_active_window_id(command: &Command) -> bool {
    match command {
        Command::PointerMove {
            active_window_id, ..
        }
        | Command::PointerDown {
            active_window_id, ..
        }
        | Command::PointerUp {
            active_window_id, ..
        }
        | Command::PointerClick {
            active_window_id, ..
        }
        | Command::PointerClickText {
            active_window_id, ..
        }
        | Command::PointerClickId {
            active_window_id, ..
        }
        | Command::PointerScroll {
            active_window_id, ..
        }
        | Command::PointerDrag {
            active_window_id, ..
        }
        | Command::UiType {
            active_window_id, ..
        }
        | Command::KeyHotkey {
            active_window_id, ..
        }
        | Command::KeyEnter {
            active_window_id, ..
        }
        | Command::KeyEscape {
            active_window_id, ..
        }
        | Command::ScreenCapture {
            active_window_id, ..
        }
        | Command::ScreenTokenize {
            active_window_id, ..
        } => active_window_id
            .as_deref()
            .map(|id| !id.trim().is_empty())
            .unwrap_or(false),
        _ => false,
    }
}

fn active_window_tip_message() -> String {
    let id = resolve_frontmost_window_id().unwrap_or_else(|| "unknown".to_string());
    format!("use --active-window {id} to avoid acting in the wrong window")
}

fn command_json_hints(command: &Command) -> Vec<&'static str> {
    match command {
        Command::WindowList => {
            vec![
                "compact output with | jq '.windows[] | \"\\\\(.id) \\\\(.visible) \\\\(.title)\"'",
            ]
        }
        Command::ScreenCapture { .. } => vec![
            "prefer `screen tokenize` for automation flows; use screenshot as last resort for visual artifacts/debug",
        ],
        Command::ScreenTokenize { .. } => {
            const TOKENIZE_HINTS: [&str; 2] = [
                "tokenize response includes request_id in JSON output; reuse it with `desktopctl request response <request_id>`",
                "compact output with | jq -r '.result.text_dump'",
            ];
            let idx = (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as usize)
                % TOKENIZE_HINTS.len();
            vec![TOKENIZE_HINTS[idx]]
        }
        Command::PointerScroll { .. } => {
            vec!["before scroll, move pointer into the target scroll area"]
        }
        Command::UiType { .. } => vec![
            "to replace existing field content, send `desktopctl keyboard press cmd+a` before typing",
        ],
        _ => Vec::new(),
    }
}

fn resolve_frontmost_window_id() -> Option<String> {
    let list_request_id = next_request_id();
    let request = RequestEnvelope::new(list_request_id, Command::WindowList);
    let response = send_request_with_autostart(&request).ok()?;
    let success = match response {
        ResponseEnvelope::Success(success) => success,
        ResponseEnvelope::Error(_) => return None,
    };
    let windows = success.result.get("windows")?.as_array()?;
    for window in windows {
        if !window
            .get("frontmost")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(id) = window.get("id").and_then(serde_json::Value::as_str) {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn render_response_with_prefix_fields(
    response: &ResponseEnvelope,
    prefix_fields: &[(String, String)],
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (key, value) in prefix_fields {
        out.insert(key.clone(), serde_json::Value::String(value.clone()));
    }
    let raw = serde_json::to_value(response).unwrap_or_else(|_| serde_json::json!({}));
    if let Some(obj) = raw.as_object() {
        for (k, v) in obj {
            out.insert(k.clone(), v.clone());
        }
    }
    serde_json::Value::Object(out)
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
            "usage: desktopctl window list [--json] | desktopctl window bounds (--title <text> | --id <id>) [--json] | desktopctl window focus (--title <text> | --id <id>)",
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
            if args.len() < 3 {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window bounds (--title <text> | --id <id>) [--json]",
                ));
            }
            let selector_flag = args[1].as_str();
            if selector_flag != "--title" && selector_flag != "--id" {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window bounds (--title <text> | --id <id>) [--json]",
                ));
            }
            let query = args[2].clone();
            if query.trim().is_empty() {
                return Err(AppError::invalid_argument(format!(
                    "missing {}: desktopctl window bounds {} <...>",
                    if selector_flag == "--id" {
                        "id"
                    } else {
                        "title"
                    },
                    selector_flag
                )));
            }
            if args.len() > 3 && args[3] != "--json" {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window bounds (--title <text> | --id <id>) [--json]",
                ));
            }
            Ok(Command::WindowBounds { title: query })
        }
        "focus" => {
            if args.len() != 3 {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window focus (--title <text> | --id <id>)",
                ));
            }
            let selector_flag = args[1].as_str();
            if selector_flag != "--title" && selector_flag != "--id" {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window focus (--title <text> | --id <id>)",
                ));
            }
            let query = args[2].clone();
            if query.trim().is_empty() {
                return Err(AppError::invalid_argument(format!(
                    "missing {}: desktopctl window focus {} <...>",
                    if selector_flag == "--id" {
                        "id"
                    } else {
                        "title"
                    },
                    selector_flag
                )));
            }
            Ok(Command::WindowFocus { title: query })
        }
        _ => Err(AppError::invalid_argument(
            "usage: desktopctl window list [--json] | desktopctl window bounds (--title <text> | --id <id>) [--json] | desktopctl window focus (--title <text> | --id <id>)",
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
        "list" => {
            let mut limit: Option<u64> = None;
            let mut i = 1;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => {
                        limit = Some(parse_u64(args.get(i + 1), "limit")?);
                        i += 2;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for request list: {flag}"
                        )));
                    }
                }
            }
            Ok(Command::RequestList { limit })
        }
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
            let mut active_window_id: Option<String> = None;
            let mut region: Option<desktop_core::protocol::Bounds> = None;
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
                        if let Some(value) = args.get(i + 1) {
                            if !value.starts_with("--") {
                                if value.trim().is_empty() {
                                    return Err(AppError::invalid_argument(
                                        "active window id must not be empty",
                                    ));
                                }
                                active_window_id = Some(value.clone());
                                i += 2;
                                continue;
                            }
                        }
                        i += 1;
                    }
                    "--region" => {
                        let x = parse_u32(args.get(i + 1), "region_x")?;
                        let y = parse_u32(args.get(i + 2), "region_y")?;
                        let width = parse_u32(args.get(i + 3), "region_width")?;
                        let height = parse_u32(args.get(i + 4), "region_height")?;
                        if width == 0 || height == 0 {
                            return Err(AppError::invalid_argument(
                                "region width/height must be > 0",
                            ));
                        }
                        region = Some(desktop_core::protocol::Bounds {
                            x: x as f64,
                            y: y as f64,
                            width: width as f64,
                            height: height as f64,
                        });
                        i += 5;
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
                active_window_id,
                region,
            })
        }
        "tokenize" => {
            let mut overlay_out_path: Option<String> = None;
            let mut window_query: Option<String> = None;
            let mut screenshot_path: Option<String> = None;
            let mut active_window = false;
            let mut active_window_id: Option<String> = None;
            let mut region: Option<desktop_core::protocol::Bounds> = None;
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
                    "--window-query" => {
                        let id = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --window-query: desktopctl screen tokenize [--json] [--overlay <path>] [--window-query <text>] [--screenshot <path>]",
                            )
                        })?;
                        if id.trim().is_empty() {
                            return Err(AppError::invalid_argument(
                                "window query must not be empty",
                            ));
                        }
                        window_query = Some(id.clone());
                        i += 2;
                    }
                    "--screenshot" => {
                        let path = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --screenshot: desktopctl screen tokenize [--json] [--overlay <path>] [--active-window [<id>]] [--window-query <text>] [--screenshot <path>]",
                            )
                        })?;
                        screenshot_path = Some(path.clone());
                        i += 2;
                    }
                    "--active-window" => {
                        active_window = true;
                        if let Some(value) = args.get(i + 1) {
                            if !value.starts_with("--") {
                                if value.trim().is_empty() {
                                    return Err(AppError::invalid_argument(
                                        "active window id must not be empty",
                                    ));
                                }
                                active_window_id = Some(value.clone());
                                i += 2;
                                continue;
                            }
                        }
                        i += 1;
                    }
                    "--region" => {
                        let x = parse_u32(args.get(i + 1), "region_x")?;
                        let y = parse_u32(args.get(i + 2), "region_y")?;
                        let width = parse_u32(args.get(i + 3), "region_width")?;
                        let height = parse_u32(args.get(i + 4), "region_height")?;
                        if width == 0 || height == 0 {
                            return Err(AppError::invalid_argument(
                                "region width/height must be > 0",
                            ));
                        }
                        region = Some(desktop_core::protocol::Bounds {
                            x: x as f64,
                            y: y as f64,
                            width: width as f64,
                            height: height as f64,
                        });
                        i += 5;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for screen tokenize: {flag}"
                        )));
                    }
                }
            }
            if window_query.is_some() && screenshot_path.is_some() {
                return Err(AppError::invalid_argument(
                    "--window-query cannot be combined with --screenshot for screen tokenize",
                ));
            }
            if active_window && window_query.is_some() {
                return Err(AppError::invalid_argument(
                    "--active-window cannot be combined with --window-query for screen tokenize",
                ));
            }
            if active_window && screenshot_path.is_some() {
                return Err(AppError::invalid_argument(
                    "--active-window cannot be combined with --screenshot for screen tokenize",
                ));
            }
            if active_window_id.is_some() && !active_window {
                return Err(AppError::invalid_argument(
                    "active window id requires --active-window",
                ));
            }
            Ok(Command::ScreenTokenize {
                overlay_out_path,
                window_query,
                screenshot_path,
                active_window,
                active_window_id,
                region,
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
            let mut absolute = false;
            let mut active_window = false;
            let mut active_window_id: Option<String> = None;
            let mut positional: Vec<&String> = Vec::new();
            let mut i = 1usize;
            while i < args.len() {
                let token = &args[i];
                if token == "--absolute" {
                    absolute = true;
                    i += 1;
                    continue;
                }
                if token == "--active-window" {
                    active_window = true;
                    if let Some(value) = args.get(i + 1) {
                        if !value.starts_with("--") {
                            if value.trim().is_empty() {
                                return Err(AppError::invalid_argument(
                                    "active window id must not be empty",
                                ));
                            }
                            active_window_id = Some(value.clone());
                            i += 2;
                            continue;
                        }
                    }
                    i += 1;
                    continue;
                }
                if token.starts_with("--") {
                    return Err(AppError::invalid_argument(format!(
                        "unknown flag for pointer move: {token}"
                    )));
                }
                positional.push(token);
                i += 1;
            }
            if positional.len() != 2 {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl pointer move [--absolute] <x> <y> [--active-window [<id>]]",
                ));
            }
            let x = parse_u32(Some(positional[0]), "x")?;
            let y = parse_u32(Some(positional[1]), "y")?;
            Ok(Command::PointerMove {
                x,
                y,
                absolute,
                active_window,
                active_window_id,
            })
        }
        "down" => {
            let x = parse_u32(args.get(1), "x")?;
            let y = parse_u32(args.get(2), "y")?;
            let mut active_window = false;
            let mut active_window_id: Option<String> = None;
            let mut button = PointerButton::Left;
            let mut i = 3usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--active-window" => {
                        active_window = true;
                        if let Some(value) = args.get(i + 1) {
                            if !value.starts_with("--") {
                                if value.trim().is_empty() {
                                    return Err(AppError::invalid_argument(
                                        "active window id must not be empty",
                                    ));
                                }
                                active_window_id = Some(value.clone());
                                i += 2;
                                continue;
                            }
                        }
                        i += 1;
                    }
                    "--button" => {
                        button = parse_pointer_button(args.get(i + 1))?;
                        i += 2;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for pointer down: {flag}"
                        )));
                    }
                }
            }
            Ok(Command::PointerDown {
                x,
                y,
                button,
                active_window,
                active_window_id,
            })
        }
        "up" => {
            let x = parse_u32(args.get(1), "x")?;
            let y = parse_u32(args.get(2), "y")?;
            let mut active_window = false;
            let mut active_window_id: Option<String> = None;
            let mut button = PointerButton::Left;
            let mut i = 3usize;
            while i < args.len() {
                match args[i].as_str() {
                    "--active-window" => {
                        active_window = true;
                        if let Some(value) = args.get(i + 1) {
                            if !value.starts_with("--") {
                                if value.trim().is_empty() {
                                    return Err(AppError::invalid_argument(
                                        "active window id must not be empty",
                                    ));
                                }
                                active_window_id = Some(value.clone());
                                i += 2;
                                continue;
                            }
                        }
                        i += 1;
                    }
                    "--button" => {
                        button = parse_pointer_button(args.get(i + 1))?;
                        i += 2;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for pointer up: {flag}"
                        )));
                    }
                }
            }
            Ok(Command::PointerUp {
                x,
                y,
                button,
                active_window,
                active_window_id,
            })
        }
        "click" => {
            if args.len() >= 2 && args[1] == "--text" {
                let text = args.get(2).cloned().ok_or_else(|| {
                    AppError::invalid_argument(
                        "usage: desktopctl pointer click --text <text> [--active-window [<id>]]",
                    )
                })?;
                let mut active_window = false;
                let mut active_window_id: Option<String> = None;
                let mut button = PointerButton::Left;
                let mut observe = ObserveOptions::default();
                let mut i = 3usize;
                while i < args.len() {
                    match args[i].as_str() {
                        "--active-window" => {
                            active_window = true;
                            if let Some(value) = args.get(i + 1) {
                                if !value.starts_with("--") {
                                    if value.trim().is_empty() {
                                        return Err(AppError::invalid_argument(
                                            "active window id must not be empty",
                                        ));
                                    }
                                    active_window_id = Some(value.clone());
                                    i += 2;
                                    continue;
                                }
                            }
                            i += 1;
                        }
                        "--button" => {
                            button = parse_pointer_button(args.get(i + 1))?;
                            i += 2;
                        }
                        "--observe" => {
                            observe.enabled = true;
                            i += 1;
                        }
                        "--no-observe" => {
                            observe.enabled = false;
                            i += 1;
                        }
                        "--observe-until" => {
                            let value = args.get(i + 1).ok_or_else(|| {
                                AppError::invalid_argument(
                                    "missing value for --observe-until (stable|change|first-change)",
                                )
                            })?;
                            observe.until = parse_observe_until(value)?;
                            i += 2;
                        }
                        "--observe-timeout" => {
                            observe.timeout_ms = parse_u64(args.get(i + 1), "observe_timeout_ms")?;
                            i += 2;
                        }
                        "--observe-settle-ms" => {
                            observe.settle_ms = parse_u64(args.get(i + 1), "observe_settle_ms")?;
                            i += 2;
                        }
                        flag => {
                            return Err(AppError::invalid_argument(format!(
                                "unknown flag for pointer click --text: {flag}",
                            )));
                        }
                    }
                }
                Ok(Command::PointerClickText {
                    text,
                    button,
                    active_window,
                    active_window_id,
                    observe,
                })
            } else if args.len() >= 2 && args[1] == "--id" {
                let id = args.get(2).cloned().ok_or_else(|| {
                    AppError::invalid_argument(
                        "usage: desktopctl pointer click --id <element_id> --active-window [<id>]",
                    )
                })?;
                let mut active_window = false;
                let mut active_window_id: Option<String> = None;
                let mut button = PointerButton::Left;
                let mut observe = ObserveOptions::default();
                let mut i = 3usize;
                while i < args.len() {
                    match args[i].as_str() {
                        "--active-window" => {
                            active_window = true;
                            if let Some(value) = args.get(i + 1) {
                                if !value.starts_with("--") {
                                    if value.trim().is_empty() {
                                        return Err(AppError::invalid_argument(
                                            "active window id must not be empty",
                                        ));
                                    }
                                    active_window_id = Some(value.clone());
                                    i += 2;
                                    continue;
                                }
                            }
                            i += 1;
                        }
                        "--button" => {
                            button = parse_pointer_button(args.get(i + 1))?;
                            i += 2;
                        }
                        "--observe" => {
                            observe.enabled = true;
                            i += 1;
                        }
                        "--no-observe" => {
                            observe.enabled = false;
                            i += 1;
                        }
                        "--observe-until" => {
                            let value = args.get(i + 1).ok_or_else(|| {
                                AppError::invalid_argument(
                                    "missing value for --observe-until (stable|change|first-change)",
                                )
                            })?;
                            observe.until = parse_observe_until(value)?;
                            i += 2;
                        }
                        "--observe-timeout" => {
                            observe.timeout_ms = parse_u64(args.get(i + 1), "observe_timeout_ms")?;
                            i += 2;
                        }
                        "--observe-settle-ms" => {
                            observe.settle_ms = parse_u64(args.get(i + 1), "observe_settle_ms")?;
                            i += 2;
                        }
                        flag => {
                            return Err(AppError::invalid_argument(format!(
                                "unknown flag for pointer click --id: {flag}",
                            )));
                        }
                    }
                }
                if !active_window {
                    return Err(AppError::invalid_argument(
                        "pointer click --id requires --active-window",
                    ));
                }
                Ok(Command::PointerClickId {
                    id,
                    button,
                    active_window,
                    active_window_id,
                    observe,
                })
            } else {
                let mut absolute = false;
                let mut button = PointerButton::Left;
                let mut observe = ObserveOptions::default();
                let mut active_window = false;
                let mut active_window_id: Option<String> = None;
                let mut positional: Vec<&String> = Vec::new();
                let mut i = 1usize;
                while i < args.len() {
                    let token = &args[i];
                    if token == "--absolute" {
                        absolute = true;
                        i += 1;
                        continue;
                    }
                    if token == "--observe" {
                        observe.enabled = true;
                        i += 1;
                        continue;
                    }
                    if token == "--no-observe" {
                        observe.enabled = false;
                        i += 1;
                        continue;
                    }
                    if token == "--observe-until" {
                        let value = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --observe-until (stable|change|first-change)",
                            )
                        })?;
                        observe.until = parse_observe_until(value)?;
                        i += 2;
                        continue;
                    }
                    if token == "--observe-timeout" {
                        observe.timeout_ms = parse_u64(args.get(i + 1), "observe_timeout_ms")?;
                        i += 2;
                        continue;
                    }
                    if token == "--observe-settle-ms" {
                        observe.settle_ms = parse_u64(args.get(i + 1), "observe_settle_ms")?;
                        i += 2;
                        continue;
                    }
                    if token == "--active-window" {
                        active_window = true;
                        if let Some(value) = args.get(i + 1) {
                            if !value.starts_with("--") {
                                if value.trim().is_empty() {
                                    return Err(AppError::invalid_argument(
                                        "active window id must not be empty",
                                    ));
                                }
                                active_window_id = Some(value.clone());
                                i += 2;
                                continue;
                            }
                        }
                        i += 1;
                        continue;
                    }
                    if token == "--button" {
                        button = parse_pointer_button(args.get(i + 1))?;
                        i += 2;
                        continue;
                    }
                    if token.starts_with("--") {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for pointer click: {}",
                            token
                        )));
                    }
                    positional.push(token);
                    i += 1;
                }
                if positional.len() != 2 {
                    return Err(AppError::invalid_argument(
                        "usage: desktopctl pointer click [--absolute] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>] <x> <y>",
                    ));
                }
                let x = parse_u32(Some(positional[0]), "x")?;
                let y = parse_u32(Some(positional[1]), "y")?;
                Ok(Command::PointerClick {
                    x,
                    y,
                    absolute,
                    button,
                    observe,
                    active_window,
                    active_window_id,
                })
            }
        }
        "drag" => {
            let x1 = parse_u32(args.get(1), "x1")?;
            let y1 = parse_u32(args.get(2), "y1")?;
            let x2 = parse_u32(args.get(3), "x2")?;
            let y2 = parse_u32(args.get(4), "y2")?;
            let mut hold_ms = 60;
            let mut active_window = false;
            let mut active_window_id: Option<String> = None;
            let mut i = 5usize;
            while i < args.len() {
                let token = &args[i];
                if token == "--active-window" {
                    active_window = true;
                    if let Some(value) = args.get(i + 1) {
                        if !value.starts_with("--") {
                            if value.trim().is_empty() {
                                return Err(AppError::invalid_argument(
                                    "active window id must not be empty",
                                ));
                            }
                            active_window_id = Some(value.clone());
                            i += 2;
                            continue;
                        }
                    }
                    i += 1;
                    continue;
                }
                if token.starts_with("--") {
                    return Err(AppError::invalid_argument(format!(
                        "unknown flag for pointer drag: {token}"
                    )));
                }
                if hold_ms != 60 {
                    return Err(AppError::invalid_argument(
                        "pointer drag accepts at most one hold_ms positional argument",
                    ));
                }
                hold_ms = parse_u64(Some(token), "hold_ms")?;
                i += 1;
            }
            Ok(Command::PointerDrag {
                x1,
                y1,
                x2,
                y2,
                hold_ms,
                active_window,
                active_window_id,
            })
        }
        "scroll" => {
            let mut id: Option<String> = None;
            let (dx_idx, dy_idx, mut i) = if args.len() >= 2 && args[1] == "--id" {
                let element_id = args.get(2).cloned().ok_or_else(|| {
                    AppError::invalid_argument(
                        "usage: desktopctl pointer scroll --id <element_id> <dx> <dy> [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]",
                    )
                })?;
                if element_id.trim().is_empty() {
                    return Err(AppError::invalid_argument("empty element id selector"));
                }
                id = Some(element_id);
                (3usize, 4usize, 5usize)
            } else {
                (1usize, 2usize, 3usize)
            };
            let dx = parse_i32(args.get(dx_idx), "dx")?;
            let dy = parse_i32(args.get(dy_idx), "dy")?;
            let mut observe = ObserveOptions::default();
            let mut active_window = false;
            let mut active_window_id: Option<String> = None;
            while i < args.len() {
                match args[i].as_str() {
                    "--active-window" => {
                        active_window = true;
                        if let Some(value) = args.get(i + 1) {
                            if !value.starts_with("--") {
                                if value.trim().is_empty() {
                                    return Err(AppError::invalid_argument(
                                        "active window id must not be empty",
                                    ));
                                }
                                active_window_id = Some(value.clone());
                                i += 2;
                                continue;
                            }
                        }
                        i += 1;
                    }
                    "--observe" => {
                        observe.enabled = true;
                        i += 1;
                    }
                    "--no-observe" => {
                        observe.enabled = false;
                        i += 1;
                    }
                    "--observe-until" => {
                        let value = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --observe-until (stable|change|first-change)",
                            )
                        })?;
                        observe.until = parse_observe_until(value)?;
                        i += 2;
                    }
                    "--observe-timeout" => {
                        observe.timeout_ms = parse_u64(args.get(i + 1), "observe_timeout_ms")?;
                        i += 2;
                    }
                    "--observe-settle-ms" => {
                        observe.settle_ms = parse_u64(args.get(i + 1), "observe_settle_ms")?;
                        i += 2;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for pointer scroll: {flag}"
                        )));
                    }
                }
            }
            Ok(Command::PointerScroll {
                id,
                dx,
                dy,
                observe,
                active_window,
                active_window_id,
            })
        }
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_pointer_button(value: Option<&String>) -> Result<PointerButton, AppError> {
    let value = value.ok_or_else(|| {
        AppError::invalid_argument("missing value for --button (expected left|right)")
    })?;
    match value.trim().to_ascii_lowercase().as_str() {
        "left" => Ok(PointerButton::Left),
        "right" => Ok(PointerButton::Right),
        other => Err(AppError::invalid_argument(format!(
            "invalid --button value: {other} (expected left|right)"
        ))),
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
            let (observe, active_window, active_window_id) =
                parse_observe_and_active_window_options(&args[2..], "keyboard type")?;
            Ok(Command::UiType {
                text,
                observe,
                active_window,
                active_window_id,
            })
        }
        "press" => {
            let key = args.get(1).cloned().ok_or_else(|| {
                AppError::invalid_argument("missing key: desktopctl keyboard press enter")
            })?;
            let (observe, active_window, active_window_id) =
                parse_observe_and_active_window_options(&args[2..], "keyboard press")?;
            if key.eq_ignore_ascii_case("enter") || key.eq_ignore_ascii_case("return") {
                Ok(Command::KeyEnter {
                    observe,
                    active_window,
                    active_window_id,
                })
            } else if key.eq_ignore_ascii_case("escape") || key.eq_ignore_ascii_case("esc") {
                Ok(Command::KeyEscape {
                    observe,
                    active_window,
                    active_window_id,
                })
            } else {
                Ok(Command::KeyHotkey {
                    hotkey: key,
                    observe,
                    active_window,
                    active_window_id,
                })
            }
        }
        _ => Err(AppError::invalid_argument(usage())),
    }
}

fn parse_observe_and_active_window_options(
    args: &[String],
    command_name: &str,
) -> Result<(ObserveOptions, bool, Option<String>), AppError> {
    let mut observe = ObserveOptions::default();
    let mut active_window = false;
    let mut active_window_id: Option<String> = None;
    let mut i = 0usize;
    while i < args.len() {
        match args[i].as_str() {
            "--observe" => {
                observe.enabled = true;
                i += 1;
            }
            "--no-observe" => {
                observe.enabled = false;
                i += 1;
            }
            "--observe-until" => {
                let value = args.get(i + 1).ok_or_else(|| {
                    AppError::invalid_argument(
                        "missing value for --observe-until (stable|change|first-change)",
                    )
                })?;
                observe.until = parse_observe_until(value)?;
                i += 2;
            }
            "--observe-timeout" => {
                observe.timeout_ms = parse_u64(args.get(i + 1), "observe_timeout_ms")?;
                i += 2;
            }
            "--observe-settle-ms" => {
                observe.settle_ms = parse_u64(args.get(i + 1), "observe_settle_ms")?;
                i += 2;
            }
            "--active-window" => {
                active_window = true;
                if let Some(value) = args.get(i + 1) {
                    if !value.starts_with("--") {
                        if value.trim().is_empty() {
                            return Err(AppError::invalid_argument(
                                "active window id must not be empty",
                            ));
                        }
                        active_window_id = Some(value.clone());
                        i += 2;
                        continue;
                    }
                }
                i += 1;
            }
            flag => {
                return Err(AppError::invalid_argument(format!(
                    "unknown flag for {command_name}: {flag}"
                )));
            }
        }
    }
    if active_window_id.is_some() && !active_window {
        return Err(AppError::invalid_argument(
            "active window id requires --active-window",
        ));
    }
    Ok((observe, active_window, active_window_id))
}

fn parse_observe_until(value: &str) -> Result<ObserveUntil, AppError> {
    match value {
        "stable" => Ok(ObserveUntil::Stable),
        "change" => Ok(ObserveUntil::Change),
        "first-change" => Ok(ObserveUntil::FirstChange),
        _ => Err(AppError::invalid_argument(format!(
            "invalid --observe-until value: {value} (expected stable|change|first-change)"
        ))),
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

fn parse_i32(value: Option<&String>, field: &str) -> Result<i32, AppError> {
    let raw = value.ok_or_else(|| AppError::invalid_argument(format!("missing {field}")))?;
    raw.parse::<i32>()
        .map_err(|_| AppError::invalid_argument(format!("invalid {field}: {raw}")))
}

fn usage() -> &'static str {
    "usage:
  desktopctl --json <command...>
  desktopctl app open <application> [--wait] [--timeout <ms>] [-- <open-args...>]
  desktopctl app hide <application>
  desktopctl app show <application>
  desktopctl app isolate <application>
  desktopctl window list [--json]
    hint: compact output with | jq '.windows[] | \"\\(.id) \\(.visible) \\(.title)\"'
  desktopctl window bounds (--title <text> | --id <id>) [--json]
  desktopctl window focus (--title <text> | --id <id>)
    hint: when starting, the focused window likely belongs to AI agent, get its ID with tokenise command, then open/focus target window, and in the end of the session focus AI agent window again
    hint: after focusing, use --active-window <id> on subsequent commands to ensure they target the correct window
  desktopctl screen screenshot [--out <path>] [--overlay] [--active-window [<id>]] [--region <x> <y> <width> <height>]
    note: --region is relative to the selected active-window/display target
    hint: prefer `screen tokenize` for automation flows; use screenshot as last resort for visual artifacts/debug
  desktopctl screen tokenize [--json] [--overlay <path>] [--active-window [<id>]] [--window-query <text>] [--screenshot <path>] [--region <x> <y> <width> <height>]
    note: --window-query cannot be combined with --screenshot
    note: --active-window cannot be combined with --window-query or --screenshot
    note: --region is relative to the selected window/screenshot target
    hint: tokenize response includes request_id in JSON output; reuse it with `desktopctl request response <request_id>`
    hint: compact output with | jq -r '.result.text_dump'
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
  desktopctl request list [--limit <n>]
  desktopctl request screenshot <request_id> [--out <path>]
  desktopctl request response <request_id>
    hint: use it to re-read output of previous commands, like tokenize, without perf penalty
  desktopctl replay record [--duration <ms>]
  desktopctl replay record --stop
  desktopctl replay load <session_dir>
  desktopctl pointer move [--absolute] <x> <y> [--active-window [<id>]]
    hint: use relative coordinates by default, it works better with tokenize
  desktopctl pointer down <x> <y> [--button <left|right>] [--active-window [<id>]]
    hint: include --active-window [<id>] to avoid acting in the wrong window (get id via window list or screen tokenize)
  desktopctl pointer up <x> <y> [--button <left|right>] [--active-window [<id>]]
  desktopctl pointer click [--absolute] [--button <left|right>] <x> <y> [--active-window [<id>]]
  desktopctl pointer click [--absolute] [--button <left|right>] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>] [--active-window [<id>]] <x> <y>
    note: prefer pointer click x y when you have coordinates
  desktopctl pointer click --text <text> [--button <left|right>] [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: use --button right to open context menus
  desktopctl pointer click --id <element_id> --active-window [<id>] [--button <left|right>] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: id might change after screen update, either re-tokenize, or use x y for clicks
  desktopctl pointer scroll <dx> <dy> [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
  desktopctl pointer scroll --id <element_id> <dx> <dy> [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: before scroll, move pointer into the target scroll area
    hint: scroll direction uses command deltas (`dy > 0` down, `dy < 0` up), independent of macOS natural/classic mode
    hint: for long lists, repeat scroll -> tokenize; save each request_id and inspect later via `desktopctl request response <request_id>`
  desktopctl pointer drag <x1> <y1> <x2> <y2> [hold_ms] [--active-window [<id>]]
  desktopctl keyboard type \"text\" [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: to replace existing field content, send `desktopctl keyboard press cmd+a` before typing
    hint: press enter, or click outside (app-dependent) of text area to apply your change
  desktopctl keyboard press <key-or-hotkey> [--active-window [<id>]] [--observe|--no-observe] [--observe-until <stable|change|first-change>] [--observe-timeout <ms>] [--observe-settle-ms <ms>]
    hint: common keys: delete, left/right/up/down, tab, home/end, pageup/pagedown, f1..f12 (hotkeys like cmd+left also supported)"
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
            "--json".to_string(),
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
    fn parses_window_bounds_with_id() {
        let command =
            parse_command(&["window", "bounds", "--id", "35a5c9", "--json"].map(str::to_string))
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

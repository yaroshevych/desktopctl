use crate::usage::usage;
use desktop_core::{
    error::AppError,
    protocol::{Command, ObserveOptions, ObserveUntil, PointerButton},
};

pub(crate) fn parse_command(args: &[String]) -> Result<Command, AppError> {
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
            "usage: desktopctl window list | desktopctl window bounds (--title <text> | --id <id>) | desktopctl window focus (--title <text> | --id <id>)",
        ));
    }

    match args[0].as_str() {
        "list" => {
            if args.len() > 1 {
                return Err(AppError::invalid_argument("usage: desktopctl window list"));
            }
            Ok(Command::WindowList)
        }
        "bounds" => {
            if args.len() < 3 {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window bounds (--title <text> | --id <id>)",
                ));
            }
            let selector_flag = args[1].as_str();
            if selector_flag != "--title" && selector_flag != "--id" {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window bounds (--title <text> | --id <id>)",
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
            if args.len() > 3 {
                return Err(AppError::invalid_argument(
                    "usage: desktopctl window bounds (--title <text> | --id <id>)",
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
            "usage: desktopctl window list | desktopctl window bounds (--title <text> | --id <id>) | desktopctl window focus (--title <text> | --id <id>)",
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
        "search" => {
            let text = args.get(1).cloned().ok_or_else(|| {
                AppError::invalid_argument(
                    "usage: desktopctl request search <text> [--limit <n>] [--command <screen_tokenize|...>]",
                )
            })?;
            if text.trim().is_empty() {
                return Err(AppError::invalid_argument(
                    "request search query must not be empty",
                ));
            }
            let mut limit: Option<u64> = None;
            let mut command: Option<String> = None;
            let mut i = 2;
            while i < args.len() {
                match args[i].as_str() {
                    "--limit" => {
                        limit = Some(parse_u64(args.get(i + 1), "limit")?);
                        i += 2;
                    }
                    "--command" => {
                        let value = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument("missing value for --command")
                        })?;
                        if value.trim().is_empty() {
                            return Err(AppError::invalid_argument(
                                "request search --command value must not be empty",
                            ));
                        }
                        command = Some(value.clone());
                        i += 2;
                    }
                    flag => {
                        return Err(AppError::invalid_argument(format!(
                            "unknown flag for request search: {flag}"
                        )));
                    }
                }
            }
            Ok(Command::RequestSearch {
                text,
                limit,
                command,
            })
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
                    "--overlay" => {
                        let path = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --overlay: desktopctl screen tokenize [--overlay <path>]",
                            )
                        })?;
                        overlay_out_path = Some(path.clone());
                        i += 2;
                    }
                    "--window-query" => {
                        let id = args.get(i + 1).ok_or_else(|| {
                            AppError::invalid_argument(
                                "missing value for --window-query: desktopctl screen tokenize [--overlay <path>] [--window-query <text>] [--screenshot <path>]",
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
                                "missing value for --screenshot: desktopctl screen tokenize [--overlay <path>] [--active-window [<id>]] [--window-query <text>] [--screenshot <path>]",
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
                    "usage: desktopctl screen find --text <text> [--all]",
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

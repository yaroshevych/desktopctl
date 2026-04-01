use clap::{
    Arg, ArgAction, ArgMatches, Command as ClapCommand, ValueHint, builder::PossibleValuesParser,
};
use desktop_core::{
    error::AppError,
    protocol::{Bounds, Command, ObserveOptions, ObserveUntil, PointerButton},
};

use crate::usage::help_notes;

const MAX_REPLAY_DURATION_MS: u64 = 30 * 60 * 1000;

pub(crate) fn render_help_if_requested(raw_args: &[String]) -> Result<Option<String>, AppError> {
    let help_flag_present = raw_args.iter().any(|arg| arg == "-h" || arg == "--help");
    let help_subcommand_present = raw_args.first().is_some_and(|arg| arg == "help");
    if !help_flag_present && !help_subcommand_present {
        return Ok(None);
    }

    let forwarded_args: Vec<String> = if help_subcommand_present {
        if raw_args.len() == 1 {
            vec!["--help".to_string()]
        } else {
            let mut args = raw_args[1..].to_vec();
            args.push("--help".to_string());
            args
        }
    } else {
        raw_args.to_vec()
    };

    let mut argv = Vec::with_capacity(forwarded_args.len() + 1);
    argv.push("desktopctl".to_string());
    argv.extend(forwarded_args);

    match clap_app().try_get_matches_from(argv) {
        Ok(_) => {
            let mut cmd = clap_app();
            let mut out = Vec::new();
            cmd.write_long_help(&mut out)
                .map_err(|err| AppError::internal(format!("failed to render help: {err}")))?;
            let help = String::from_utf8(out)
                .map_err(|err| AppError::internal(format!("invalid UTF-8 help output: {err}")))?;
            Ok(Some(help))
        }
        Err(err) => match err.kind() {
            clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion => {
                Ok(Some(err.to_string()))
            }
            _ => Err(AppError::invalid_argument(err.to_string())),
        },
    }
}

pub(crate) fn parse_command(args: &[String]) -> Result<Command, AppError> {
    if args.is_empty() {
        return Err(AppError::invalid_argument(
            "missing command; run `desktopctl --help`",
        ));
    }
    let mut argv = Vec::with_capacity(args.len() + 1);
    argv.push("desktopctl".to_string());
    argv.extend(args.iter().cloned());

    let matches = clap_app()
        .try_get_matches_from(argv)
        .map_err(|err| AppError::invalid_argument(err.to_string()))?;

    let (name, sub) = matches
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing command; run `desktopctl --help`"))?;

    match name {
        "app" => parse_app(sub),
        "window" => parse_window(sub),
        "screen" => parse_screen(sub),
        "clipboard" => parse_clipboard(sub),
        "debug" => parse_debug(sub),
        "request" => parse_request(sub),
        "replay" => parse_replay(sub),
        "pointer" => parse_pointer(sub),
        "keyboard" => parse_keyboard(sub),
        _ => Err(AppError::invalid_argument("unknown command")),
    }
}

fn parse_app(m: &ArgMatches) -> Result<Command, AppError> {
    let (name, sub) = m
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing app action"))?;
    match name {
        "hide" => Ok(Command::AppHide {
            name: join_many(sub, "application")?,
        }),
        "show" => Ok(Command::AppShow {
            name: join_many(sub, "application")?,
        }),
        "isolate" => Ok(Command::AppIsolate {
            name: join_many(sub, "application")?,
        }),
        "open" => Ok(Command::OpenApp {
            name: join_many(sub, "application")?,
            args: sub
                .get_many::<String>("open_args")
                .map(|v| v.cloned().collect())
                .unwrap_or_default(),
            wait: sub.get_flag("wait"),
            timeout_ms: sub
                .get_one::<String>("timeout")
                .map(|v| parse_u64(v, "timeout_ms"))
                .transpose()?,
        }),
        _ => Err(AppError::invalid_argument("unknown app action")),
    }
}

fn parse_window(m: &ArgMatches) -> Result<Command, AppError> {
    let (name, sub) = m
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing window action"))?;
    match name {
        "list" => Ok(Command::WindowList),
        "bounds" => Ok(Command::WindowBounds {
            title: window_selector(sub)?,
        }),
        "focus" => Ok(Command::WindowFocus {
            title: window_selector(sub)?,
        }),
        _ => Err(AppError::invalid_argument("unknown window action")),
    }
}

fn parse_screen(m: &ArgMatches) -> Result<Command, AppError> {
    let (name, sub) = m
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing screen action"))?;
    match name {
        "screenshot" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            Ok(Command::ScreenCapture {
                out_path: sub.get_one::<String>("out").cloned(),
                overlay: sub.get_flag("overlay"),
                active_window,
                active_window_id,
                region: parse_region(sub)?,
            })
        }
        "tokenize" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            let window_query = sub.get_one::<String>("window_query").cloned();
            let screenshot_path = sub.get_one::<String>("screenshot").cloned();
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
            Ok(Command::ScreenTokenize {
                overlay_out_path: sub.get_one::<String>("overlay").cloned(),
                window_query,
                screenshot_path,
                active_window,
                active_window_id,
                region: parse_region(sub)?,
            })
        }
        "find" => Ok(Command::ScreenFindText {
            text: sub
                .get_one::<String>("text")
                .cloned()
                .ok_or_else(|| AppError::invalid_argument("missing --text"))?,
            all: sub.get_flag("all"),
        }),
        "wait" => Ok(Command::WaitText {
            text: sub
                .get_one::<String>("text")
                .cloned()
                .ok_or_else(|| AppError::invalid_argument("missing --text"))?,
            timeout_ms: sub
                .get_one::<String>("timeout")
                .map(|v| parse_u64(v, "timeout_ms"))
                .transpose()?
                .unwrap_or(8_000),
            interval_ms: sub
                .get_one::<String>("interval")
                .map(|v| parse_u64(v, "interval_ms"))
                .transpose()?
                .unwrap_or(200),
            disappear: sub.get_flag("disappear"),
        }),
        _ => Err(AppError::invalid_argument("unknown screen action")),
    }
}

fn parse_clipboard(m: &ArgMatches) -> Result<Command, AppError> {
    let (name, sub) = m
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing clipboard action"))?;
    match name {
        "read" => Ok(Command::ClipboardRead),
        "write" => Ok(Command::ClipboardWrite {
            text: sub
                .get_one::<String>("text")
                .cloned()
                .ok_or_else(|| AppError::invalid_argument("missing text"))?,
        }),
        _ => Err(AppError::invalid_argument("unknown clipboard action")),
    }
}

fn parse_debug(m: &ArgMatches) -> Result<Command, AppError> {
    let (name, sub) = m
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing debug action"))?;
    match name {
        "permissions" => Ok(Command::PermissionsCheck),
        "ping" => Ok(Command::Ping),
        "snapshot" => Ok(Command::DebugSnapshot),
        "overlay" => {
            let (overlay_name, overlay_sub) = sub
                .subcommand()
                .ok_or_else(|| AppError::invalid_argument("missing debug overlay action"))?;
            match overlay_name {
                "start" => Ok(Command::OverlayStart {
                    duration_ms: overlay_sub
                        .get_one::<String>("duration")
                        .map(|v| parse_u64(v, "duration_ms"))
                        .transpose()?,
                }),
                "stop" => Ok(Command::OverlayStop),
                _ => Err(AppError::invalid_argument("unknown debug overlay action")),
            }
        }
        _ => Err(AppError::invalid_argument("unknown debug action")),
    }
}

fn parse_request(m: &ArgMatches) -> Result<Command, AppError> {
    let (name, sub) = m
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing request action"))?;
    match name {
        "show" => Ok(Command::RequestShow {
            request_id: required_string(sub, "request_id")?,
        }),
        "list" => Ok(Command::RequestList {
            limit: sub
                .get_one::<String>("limit")
                .map(|v| parse_u64(v, "limit"))
                .transpose()?,
        }),
        "screenshot" => Ok(Command::RequestScreenshot {
            request_id: required_string(sub, "request_id")?,
            out_path: sub.get_one::<String>("out").cloned(),
        }),
        "response" => Ok(Command::RequestResponse {
            request_id: required_string(sub, "request_id")?,
        }),
        "search" => {
            let text = required_string(sub, "text")?;
            if text.trim().is_empty() {
                return Err(AppError::invalid_argument(
                    "request search query must not be empty",
                ));
            }
            Ok(Command::RequestSearch {
                text,
                limit: sub
                    .get_one::<String>("limit")
                    .map(|v| parse_u64(v, "limit"))
                    .transpose()?,
                command: sub.get_one::<String>("command").cloned(),
            })
        }
        _ => Err(AppError::invalid_argument("unknown request action")),
    }
}

fn parse_replay(m: &ArgMatches) -> Result<Command, AppError> {
    let (name, sub) = m
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing replay action"))?;
    match name {
        "record" => {
            let stop = sub.get_flag("stop");
            let duration_ms = sub
                .get_one::<String>("duration")
                .map(|v| parse_u64(v, "duration_ms"))
                .transpose()?
                .unwrap_or(3_000);
            if !stop && duration_ms > MAX_REPLAY_DURATION_MS {
                return Err(AppError::invalid_argument(format!(
                    "duration_ms exceeds max of {MAX_REPLAY_DURATION_MS}"
                )));
            }
            Ok(Command::ReplayRecord { duration_ms, stop })
        }
        "load" => Ok(Command::ReplayLoad {
            session_dir: required_string(sub, "session_dir")?,
        }),
        _ => Err(AppError::invalid_argument("unknown replay action")),
    }
}

fn parse_pointer(m: &ArgMatches) -> Result<Command, AppError> {
    let (name, sub) = m
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing pointer action"))?;
    match name {
        "move" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            Ok(Command::PointerMove {
                x: parse_u32(&required_string(sub, "x")?, "x")?,
                y: parse_u32(&required_string(sub, "y")?, "y")?,
                absolute: sub.get_flag("absolute"),
                active_window,
                active_window_id,
            })
        }
        "down" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            Ok(Command::PointerDown {
                x: parse_u32(&required_string(sub, "x")?, "x")?,
                y: parse_u32(&required_string(sub, "y")?, "y")?,
                button: parse_pointer_button(sub.get_one::<String>("button"))?,
                active_window,
                active_window_id,
            })
        }
        "up" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            Ok(Command::PointerUp {
                x: parse_u32(&required_string(sub, "x")?, "x")?,
                y: parse_u32(&required_string(sub, "y")?, "y")?,
                button: parse_pointer_button(sub.get_one::<String>("button"))?,
                active_window,
                active_window_id,
            })
        }
        "click" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            let button = parse_pointer_button(sub.get_one::<String>("button"))?;
            let observe = parse_observe(sub)?;
            if let Some(text) = sub.get_one::<String>("text") {
                return Ok(Command::PointerClickText {
                    text: text.clone(),
                    button,
                    active_window,
                    active_window_id,
                    observe,
                });
            }
            if let Some(id) = sub.get_one::<String>("id") {
                if !active_window {
                    return Err(AppError::invalid_argument(
                        "pointer click --id requires --active-window",
                    ));
                }
                return Ok(Command::PointerClickId {
                    id: id.clone(),
                    button,
                    active_window,
                    active_window_id,
                    observe,
                });
            }

            Ok(Command::PointerClick {
                x: parse_u32(&required_string(sub, "x")?, "x")?,
                y: parse_u32(&required_string(sub, "y")?, "y")?,
                absolute: sub.get_flag("absolute"),
                button,
                observe,
                active_window,
                active_window_id,
            })
        }
        "scroll" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            Ok(Command::PointerScroll {
                id: sub.get_one::<String>("id").cloned(),
                dx: parse_i32(&required_string(sub, "dx")?, "dx")?,
                dy: parse_i32(&required_string(sub, "dy")?, "dy")?,
                observe: parse_observe(sub)?,
                active_window,
                active_window_id,
            })
        }
        "drag" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            Ok(Command::PointerDrag {
                x1: parse_u32(&required_string(sub, "x1")?, "x1")?,
                y1: parse_u32(&required_string(sub, "y1")?, "y1")?,
                x2: parse_u32(&required_string(sub, "x2")?, "x2")?,
                y2: parse_u32(&required_string(sub, "y2")?, "y2")?,
                hold_ms: sub
                    .get_one::<String>("hold_ms")
                    .map(|v| parse_u64(v, "hold_ms"))
                    .transpose()?
                    .unwrap_or(60),
                active_window,
                active_window_id,
            })
        }
        _ => Err(AppError::invalid_argument("unknown pointer action")),
    }
}

fn parse_keyboard(m: &ArgMatches) -> Result<Command, AppError> {
    let (name, sub) = m
        .subcommand()
        .ok_or_else(|| AppError::invalid_argument("missing keyboard action"))?;
    match name {
        "type" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            Ok(Command::UiType {
                text: required_string(sub, "text")?,
                observe: parse_observe(sub)?,
                active_window,
                active_window_id,
            })
        }
        "press" => {
            let (active_window, active_window_id) = parse_active_window(sub)?;
            let key = required_string(sub, "key")?;
            if key.eq_ignore_ascii_case("enter") || key.eq_ignore_ascii_case("return") {
                Ok(Command::KeyEnter {
                    observe: parse_observe(sub)?,
                    active_window,
                    active_window_id,
                })
            } else if key.eq_ignore_ascii_case("escape") || key.eq_ignore_ascii_case("esc") {
                Ok(Command::KeyEscape {
                    observe: parse_observe(sub)?,
                    active_window,
                    active_window_id,
                })
            } else {
                Ok(Command::KeyHotkey {
                    hotkey: key,
                    observe: parse_observe(sub)?,
                    active_window,
                    active_window_id,
                })
            }
        }
        _ => Err(AppError::invalid_argument("unknown keyboard action")),
    }
}

fn parse_observe(m: &ArgMatches) -> Result<ObserveOptions, AppError> {
    let mut observe = ObserveOptions::default();
    if m.get_flag("observe") {
        observe.enabled = true;
    }
    if m.get_flag("no_observe") {
        observe.enabled = false;
    }
    if let Some(until) = m.get_one::<String>("observe_until") {
        observe.until = parse_observe_until(until)?;
    }
    if let Some(timeout) = m.get_one::<String>("observe_timeout") {
        observe.timeout_ms = parse_u64(timeout, "observe_timeout_ms")?;
    }
    if let Some(settle) = m.get_one::<String>("observe_settle_ms") {
        observe.settle_ms = parse_u64(settle, "observe_settle_ms")?;
    }
    Ok(observe)
}

fn parse_active_window(m: &ArgMatches) -> Result<(bool, Option<String>), AppError> {
    let active_window = m.contains_id("active_window");
    let active_window_id = m.get_one::<String>("active_window").cloned();
    if active_window_id
        .as_deref()
        .map(str::trim)
        .is_some_and(str::is_empty)
    {
        return Err(AppError::invalid_argument(
            "active window id must not be empty",
        ));
    }
    Ok((active_window, active_window_id))
}

fn parse_region(m: &ArgMatches) -> Result<Option<Bounds>, AppError> {
    let Some(values) = m.get_many::<String>("region") else {
        return Ok(None);
    };
    let parsed: Vec<u32> = values
        .map(|v| parse_u32(v, "region"))
        .collect::<Result<Vec<_>, _>>()?;
    if parsed.len() != 4 {
        return Err(AppError::invalid_argument(
            "region requires 4 values: <x> <y> <width> <height>",
        ));
    }
    if parsed[2] == 0 || parsed[3] == 0 {
        return Err(AppError::invalid_argument(
            "region width/height must be > 0",
        ));
    }
    Ok(Some(Bounds {
        x: parsed[0] as f64,
        y: parsed[1] as f64,
        width: parsed[2] as f64,
        height: parsed[3] as f64,
    }))
}

fn window_selector(m: &ArgMatches) -> Result<String, AppError> {
    if let Some(id) = m.get_one::<String>("id") {
        if id.trim().is_empty() {
            return Err(AppError::invalid_argument("missing id"));
        }
        return Ok(id.clone());
    }
    if let Some(title) = m.get_one::<String>("title") {
        if title.trim().is_empty() {
            return Err(AppError::invalid_argument("missing title"));
        }
        return Ok(title.clone());
    }
    Err(AppError::invalid_argument(
        "expected one of --title or --id",
    ))
}

fn required_string(m: &ArgMatches, key: &str) -> Result<String, AppError> {
    m.get_one::<String>(key)
        .cloned()
        .ok_or_else(|| AppError::invalid_argument(format!("missing {key}")))
}

fn join_many(m: &ArgMatches, key: &str) -> Result<String, AppError> {
    let parts = m
        .get_many::<String>(key)
        .ok_or_else(|| AppError::invalid_argument(format!("missing {key}")))?
        .cloned()
        .collect::<Vec<_>>();
    let joined = parts.join(" ").trim().to_string();
    if joined.is_empty() {
        return Err(AppError::invalid_argument(format!("missing {key}")));
    }
    Ok(joined)
}

fn parse_observe_until(value: &str) -> Result<ObserveUntil, AppError> {
    match value {
        "stable" => Ok(ObserveUntil::Stable),
        "change" => Ok(ObserveUntil::Change),
        "first-change" => Ok(ObserveUntil::FirstChange),
        _ => Err(AppError::invalid_argument(format!(
            "invalid --observe-until value: {value} (expected stable|change|first-change)",
        ))),
    }
}

fn parse_pointer_button(value: Option<&String>) -> Result<PointerButton, AppError> {
    match value.map(|v| v.as_str()).unwrap_or("left") {
        "left" => Ok(PointerButton::Left),
        "right" => Ok(PointerButton::Right),
        other => Err(AppError::invalid_argument(format!(
            "invalid --button value: {other} (expected left|right)",
        ))),
    }
}

fn parse_u32(raw: &str, field: &str) -> Result<u32, AppError> {
    raw.parse::<u32>()
        .map_err(|_| AppError::invalid_argument(format!("invalid {field}: {raw}")))
}

fn parse_u64(raw: &str, field: &str) -> Result<u64, AppError> {
    raw.parse::<u64>()
        .map_err(|_| AppError::invalid_argument(format!("invalid {field}: {raw}")))
}

fn parse_i32(raw: &str, field: &str) -> Result<i32, AppError> {
    raw.parse::<i32>()
        .map_err(|_| AppError::invalid_argument(format!("invalid {field}: {raw}")))
}

fn clap_app() -> ClapCommand {
    ClapCommand::new("desktopctl")
        .about("DesktopCtl command-line interface")
        .after_long_help(help_notes())
        .arg_required_else_help(true)
        .disable_version_flag(true)
        .arg(
            Arg::new("markdown")
                .long("markdown")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Human-readable output"),
        )
        .arg(
            Arg::new("json")
                .long("json")
                .global(true)
                .action(ArgAction::SetTrue)
                .help("Machine-readable JSON output"),
        )
        .subcommand(app_subcommand())
        .subcommand(window_subcommand())
        .subcommand(screen_subcommand())
        .subcommand(pointer_subcommand())
        .subcommand(keyboard_subcommand())
        .subcommand(clipboard_subcommand())
        .subcommand(debug_subcommand())
        .subcommand(request_subcommand())
        .subcommand(replay_subcommand())
}

fn app_subcommand() -> ClapCommand {
    ClapCommand::new("app")
        .subcommand(
            ClapCommand::new("open")
                .arg(
                    Arg::new("application")
                        .required(true)
                        .num_args(1..)
                        .value_hint(ValueHint::CommandName),
                )
                .arg(Arg::new("wait").long("wait").action(ArgAction::SetTrue))
                .arg(Arg::new("timeout").long("timeout").value_name("ms"))
                .arg(
                    Arg::new("open_args")
                        .last(true)
                        .allow_hyphen_values(true)
                        .num_args(0..),
                ),
        )
        .subcommand(
            ClapCommand::new("hide").arg(Arg::new("application").required(true).num_args(1..)),
        )
        .subcommand(
            ClapCommand::new("show").arg(Arg::new("application").required(true).num_args(1..)),
        )
        .subcommand(
            ClapCommand::new("isolate").arg(Arg::new("application").required(true).num_args(1..)),
        )
}

fn window_subcommand() -> ClapCommand {
    ClapCommand::new("window")
        .subcommand(ClapCommand::new("list"))
        .subcommand(
            ClapCommand::new("bounds")
                .arg(Arg::new("title").long("title").value_name("text"))
                .arg(Arg::new("id").long("id").value_name("id"))
                .group(
                    clap::ArgGroup::new("selector")
                        .args(["title", "id"])
                        .required(true),
                ),
        )
        .subcommand(
            ClapCommand::new("focus")
                .arg(Arg::new("title").long("title").value_name("text"))
                .arg(Arg::new("id").long("id").value_name("id"))
                .group(
                    clap::ArgGroup::new("selector")
                        .args(["title", "id"])
                        .required(true),
                ),
        )
}

fn screen_subcommand() -> ClapCommand {
    ClapCommand::new("screen")
        .subcommand(
            ClapCommand::new("screenshot")
                .arg(Arg::new("out").long("out").value_name("path"))
                .arg(
                    Arg::new("overlay")
                        .long("overlay")
                        .action(ArgAction::SetTrue),
                )
                .arg(active_window_arg())
                .arg(region_arg()),
        )
        .subcommand(
            ClapCommand::new("tokenize")
                .arg(Arg::new("overlay").long("overlay").value_name("path"))
                .arg(
                    Arg::new("window_query")
                        .long("window-query")
                        .value_name("text"),
                )
                .arg(Arg::new("screenshot").long("screenshot").value_name("path"))
                .arg(active_window_arg())
                .arg(region_arg()),
        )
        .subcommand(
            ClapCommand::new("find")
                .arg(Arg::new("text").long("text").required(true))
                .arg(Arg::new("all").long("all").action(ArgAction::SetTrue)),
        )
        .subcommand(
            ClapCommand::new("wait")
                .arg(Arg::new("text").long("text").required(true))
                .arg(Arg::new("timeout").long("timeout").value_name("ms"))
                .arg(Arg::new("interval").long("interval").value_name("ms"))
                .arg(
                    Arg::new("disappear")
                        .long("disappear")
                        .action(ArgAction::SetTrue),
                ),
        )
}

fn pointer_subcommand() -> ClapCommand {
    ClapCommand::new("pointer")
        .subcommand(
            ClapCommand::new("move")
                .arg(
                    Arg::new("absolute")
                        .long("absolute")
                        .action(ArgAction::SetTrue),
                )
                .arg(Arg::new("x").required(true))
                .arg(Arg::new("y").required(true))
                .arg(active_window_arg()),
        )
        .subcommand(
            ClapCommand::new("down")
                .arg(Arg::new("x").required(true))
                .arg(Arg::new("y").required(true))
                .arg(button_arg())
                .arg(active_window_arg()),
        )
        .subcommand(
            ClapCommand::new("up")
                .arg(Arg::new("x").required(true))
                .arg(Arg::new("y").required(true))
                .arg(button_arg())
                .arg(active_window_arg()),
        )
        .subcommand(
            ClapCommand::new("click")
                .arg(
                    Arg::new("text")
                        .long("text")
                        .value_name("text")
                        .conflicts_with_all(["id", "x", "y", "absolute"]),
                )
                .arg(
                    Arg::new("id")
                        .long("id")
                        .value_name("element_id")
                        .conflicts_with_all(["text", "x", "y", "absolute"]),
                )
                .arg(
                    Arg::new("absolute")
                        .long("absolute")
                        .action(ArgAction::SetTrue),
                )
                .arg(button_arg())
                .arg(observe_arg())
                .arg(no_observe_arg())
                .arg(observe_until_arg())
                .arg(observe_timeout_arg())
                .arg(observe_settle_arg())
                .arg(active_window_arg())
                .arg(Arg::new("x").required(false))
                .arg(Arg::new("y").required(false)),
        )
        .subcommand(
            ClapCommand::new("scroll")
                .arg(Arg::new("id").long("id").value_name("element_id"))
                .arg(Arg::new("dx").required(true).allow_hyphen_values(true))
                .arg(Arg::new("dy").required(true).allow_hyphen_values(true))
                .arg(observe_arg())
                .arg(no_observe_arg())
                .arg(observe_until_arg())
                .arg(observe_timeout_arg())
                .arg(observe_settle_arg())
                .arg(active_window_arg()),
        )
        .subcommand(
            ClapCommand::new("drag")
                .arg(Arg::new("x1").required(true))
                .arg(Arg::new("y1").required(true))
                .arg(Arg::new("x2").required(true))
                .arg(Arg::new("y2").required(true))
                .arg(Arg::new("hold_ms").required(false))
                .arg(active_window_arg()),
        )
}

fn keyboard_subcommand() -> ClapCommand {
    ClapCommand::new("keyboard")
        .subcommand(
            ClapCommand::new("type")
                .arg(Arg::new("text").required(true))
                .arg(observe_arg())
                .arg(no_observe_arg())
                .arg(observe_until_arg())
                .arg(observe_timeout_arg())
                .arg(observe_settle_arg())
                .arg(active_window_arg()),
        )
        .subcommand(
            ClapCommand::new("press")
                .arg(Arg::new("key").required(true))
                .arg(observe_arg())
                .arg(no_observe_arg())
                .arg(observe_until_arg())
                .arg(observe_timeout_arg())
                .arg(observe_settle_arg())
                .arg(active_window_arg()),
        )
}

fn clipboard_subcommand() -> ClapCommand {
    ClapCommand::new("clipboard")
        .subcommand(ClapCommand::new("read"))
        .subcommand(ClapCommand::new("write").arg(Arg::new("text").required(true)))
}

fn debug_subcommand() -> ClapCommand {
    ClapCommand::new("debug")
        .subcommand(ClapCommand::new("permissions"))
        .subcommand(ClapCommand::new("ping"))
        .subcommand(
            ClapCommand::new("overlay")
                .subcommand(ClapCommand::new("start").arg(Arg::new("duration").long("duration")))
                .subcommand(ClapCommand::new("stop")),
        )
        .subcommand(ClapCommand::new("snapshot"))
}

fn request_subcommand() -> ClapCommand {
    ClapCommand::new("request")
        .subcommand(ClapCommand::new("show").arg(Arg::new("request_id").required(true)))
        .subcommand(ClapCommand::new("list").arg(Arg::new("limit").long("limit")))
        .subcommand(
            ClapCommand::new("screenshot")
                .arg(Arg::new("request_id").required(true))
                .arg(Arg::new("out").long("out")),
        )
        .subcommand(ClapCommand::new("response").arg(Arg::new("request_id").required(true)))
        .subcommand(
            ClapCommand::new("search")
                .arg(Arg::new("text").required(true))
                .arg(Arg::new("limit").long("limit"))
                .arg(Arg::new("command").long("command")),
        )
}

fn replay_subcommand() -> ClapCommand {
    ClapCommand::new("replay")
        .subcommand(
            ClapCommand::new("record")
                .arg(Arg::new("duration").long("duration").conflicts_with("stop"))
                .arg(Arg::new("stop").long("stop").action(ArgAction::SetTrue)),
        )
        .subcommand(ClapCommand::new("load").arg(Arg::new("session_dir").required(true)))
}

fn active_window_arg() -> Arg {
    Arg::new("active_window")
        .long("active-window")
        .num_args(0..=1)
        .value_name("id")
}

fn region_arg() -> Arg {
    Arg::new("region")
        .long("region")
        .num_args(4)
        .value_names(["x", "y", "width", "height"])
}

fn button_arg() -> Arg {
    Arg::new("button")
        .long("button")
        .value_parser(PossibleValuesParser::new(["left", "right"]))
        .default_value("left")
}

fn observe_arg() -> Arg {
    Arg::new("observe")
        .long("observe")
        .action(ArgAction::SetTrue)
        .conflicts_with("no_observe")
}

fn no_observe_arg() -> Arg {
    Arg::new("no_observe")
        .long("no-observe")
        .action(ArgAction::SetTrue)
        .conflicts_with("observe")
}

fn observe_until_arg() -> Arg {
    Arg::new("observe_until")
        .long("observe-until")
        .value_parser(PossibleValuesParser::new([
            "stable",
            "change",
            "first-change",
        ]))
}

fn observe_timeout_arg() -> Arg {
    Arg::new("observe_timeout").long("observe-timeout")
}

fn observe_settle_arg() -> Arg {
    Arg::new("observe_settle_ms").long("observe-settle-ms")
}

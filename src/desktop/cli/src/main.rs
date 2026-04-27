mod output;
mod parse;
mod transport;
mod usage;

use desktop_core::{
    error::AppError,
    protocol::{Command, RequestEnvelope, ResponseEnvelope},
};
use std::time::Instant;

pub(crate) use parse::parse_command;
#[cfg(test)]
pub(crate) use transport::send_request_with_hooks;
use transport::{map_error_code, next_request_id, send_request_with_autostart, trace_log};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OutputMode {
    Json,
    Markdown,
}

fn main() {
    let raw_args: Vec<String> = std::env::args().skip(1).collect();
    if raw_args.is_empty() {
        match parse::render_help_if_requested(&["--help".to_string()]) {
            Ok(Some(help)) => {
                println!("{help}");
                std::process::exit(0);
            }
            Ok(None) => {}
            Err(err) => {
                let request_id = next_request_id();
                print_error(&request_id, &err, OutputMode::Markdown);
                std::process::exit(map_error_code(&err.code));
            }
        }
    }
    match parse::render_help_if_requested(&raw_args) {
        Ok(Some(help)) => {
            println!("{help}");
            std::process::exit(0);
        }
        Ok(None) => {}
        Err(err) => {
            let request_id = next_request_id();
            print_error(&request_id, &err, OutputMode::Markdown);
            std::process::exit(map_error_code(&err.code));
        }
    }
    let (cli_options, args) = match split_cli_options(&raw_args) {
        Ok(v) => v,
        Err(err) => {
            let request_id = next_request_id();
            print_error(&request_id, &err, OutputMode::Markdown);
            std::process::exit(map_error_code(&err.code));
        }
    };
    let request_id = next_request_id();
    match run(&args, &request_id, cli_options) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            print_error(&request_id, &err, cli_options.output_mode);
            std::process::exit(map_error_code(&err.code));
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CliOptions {
    output_mode: OutputMode,
    background: bool,
}

fn run(args: &[String], request_id: &str, options: CliOptions) -> Result<i32, AppError> {
    let run_started = Instant::now();
    let command = parse_command(args)?;
    let passthrough_stored_response = matches!(command, Command::RequestResponse { .. });
    let request = RequestEnvelope::new(request_id.to_string(), command)
        .with_background_input(options.background);
    trace_log(format!(
        "run:request_start request_id={} command={}",
        request.request_id,
        request.command.name()
    ));
    let send_started = Instant::now();
    let response = send_request_with_autostart(&request)?;
    let send_elapsed_ms = send_started.elapsed().as_millis();

    let render_started = Instant::now();
    match options.output_mode {
        OutputMode::Json => {
            let rendered =
                output::render_response(&request.command, &response, passthrough_stored_response);
            println!(
                "{}",
                serde_json::to_string_pretty(&rendered).unwrap_or_else(|_| "{}".to_string())
            );
        }
        OutputMode::Markdown => {
            println!(
                "{}",
                output::render_markdown_response(
                    &request.command,
                    &response,
                    passthrough_stored_response
                )
            );
        }
    }
    let render_print_elapsed_ms = render_started.elapsed().as_millis();
    let total_elapsed_ms = run_started.elapsed().as_millis();
    trace_log(format!(
        "run:request_timing request_id={} command={} total_ms={} send_ms={} render_print_ms={}",
        request.request_id,
        request.command.name(),
        total_elapsed_ms,
        send_elapsed_ms,
        render_print_elapsed_ms
    ));
    let code = match response {
        ResponseEnvelope::Success(_) => 0,
        ResponseEnvelope::Error(err) => map_error_code(&err.error.code),
    };
    Ok(code)
}

fn split_cli_options(args: &[String]) -> Result<(CliOptions, Vec<String>), AppError> {
    let mut mode = OutputMode::Markdown;
    let mut background = false;
    let mut filtered: Vec<String> = Vec::new();
    for arg in args {
        match arg.as_str() {
            "--json" => mode = OutputMode::Json,
            "--markdown" => mode = OutputMode::Markdown,
            "--background" if !is_app_open_context(&filtered) => background = true,
            _ => filtered.push(arg.clone()),
        }
    }
    if filtered.is_empty() {
        return Err(AppError::invalid_argument(
            "missing command; run `desktopctl --help`",
        ));
    }
    Ok((
        CliOptions {
            output_mode: mode,
            background,
        },
        filtered,
    ))
}

fn is_app_open_context(filtered: &[String]) -> bool {
    filtered.len() >= 2 && filtered[0] == "app" && filtered[1] == "open"
}

fn print_error(request_id: &str, err: &AppError, output_mode: OutputMode) {
    match output_mode {
        OutputMode::Json => {
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
        }
        OutputMode::Markdown => {
            println!("{}", output::render_markdown_error(request_id, err));
        }
    }
}


#[cfg(test)]
mod tests;

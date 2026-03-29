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
mod tests;

use desktop_core::{
    error::{AppError, ErrorCode},
    ipc,
    protocol::{RequestEnvelope, ResponseEnvelope},
};
use std::{
    fs::OpenOptions,
    io::Write,
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) fn send_request_with_autostart(
    request: &RequestEnvelope,
) -> Result<ResponseEnvelope, AppError> {
    send_request_with_hooks(request, ipc::send_request, launch_daemon)
}

pub(crate) fn send_request_with_hooks<FSend, FLaunch>(
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

pub(crate) fn next_request_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

pub(crate) fn trace_log(message: impl AsRef<str>) {
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

pub(crate) fn map_error_code(code: &ErrorCode) -> i32 {
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

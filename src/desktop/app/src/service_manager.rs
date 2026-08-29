use std::{path::PathBuf, process::Command, thread, time::Duration};

use desktop_core::error::AppError;

use crate::service_client::ServiceClient;

pub fn ensure_running() -> Result<(), AppError> {
    if ServiceClient.status().is_ok() {
        return Ok(());
    }
    let binary = service_binary().ok_or_else(|| {
        AppError::daemon_not_running("desktopctld service binary was not found beside app")
    })?;
    Command::new(&binary)
        .arg("--resident")
        .spawn()
        .map_err(|error| {
            AppError::backend_unavailable(format!("start {} failed: {error}", binary.display()))
        })?;
    for _ in 0..50 {
        if ServiceClient.status().is_ok() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(100));
    }
    Err(AppError::daemon_not_running(
        "desktopctld did not become ready",
    ))
}

fn service_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DESKTOPCTL_DAEMON_BIN") {
        let path = PathBuf::from(path);
        if path.exists() {
            return Some(path);
        }
    }
    let path = std::env::current_exe().ok()?.parent()?.join("desktopctld");
    path.exists().then_some(path)
}

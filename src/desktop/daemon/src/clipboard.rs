use std::{
    io::Write,
    process::{Command, Stdio},
};

use desktop_core::error::AppError;

pub fn read_clipboard() -> Result<String, AppError> {
    let output = Command::new("pbpaste")
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run pbpaste: {err}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::backend_unavailable(format!(
            "pbpaste failed: {stderr}"
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn write_clipboard(text: &str) -> Result<(), AppError> {
    let mut child = Command::new("pbcopy")
        .stdin(Stdio::piped())
        .spawn()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run pbcopy: {err}")))?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin.write_all(text.as_bytes()).map_err(|err| {
            AppError::backend_unavailable(format!("failed writing to pbcopy stdin: {err}"))
        })?;
    }
    let status = child
        .wait()
        .map_err(|err| AppError::backend_unavailable(format!("failed waiting for pbcopy: {err}")))?;
    if !status.success() {
        return Err(AppError::backend_unavailable("pbcopy failed"));
    }
    Ok(())
}

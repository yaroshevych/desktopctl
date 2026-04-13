use std::process::Command;

use desktop_core::error::AppError;

use crate::platform::windowing::WindowInfo;

pub fn focus_window(window: &WindowInfo) -> Result<(), AppError> {
    let Some(hwnd) = parse_hwnd(&window.id) else {
        return Err(AppError::invalid_argument(format!(
            "window id is not a Windows hwnd id: {}",
            window.id
        )));
    };

    let script = format!(
        r#"
Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class DesktopCtlWin {{
    [DllImport("user32.dll")]
    public static extern bool ShowWindowAsync(IntPtr hWnd, int nCmdShow);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);
}}
"@;
$hwnd = [intptr]{hwnd}
[DesktopCtlWin]::ShowWindowAsync($hwnd, 5) | Out-Null
if (-not [DesktopCtlWin]::SetForegroundWindow($hwnd)) {{ exit 1 }}
"#
    );

    run_powershell(&script)
}

pub fn hide_application(_name: &str) -> Result<&'static str, AppError> {
    Err(AppError::backend_unavailable(
        "windows app hide backend is not implemented yet",
    ))
}

pub fn show_application(name: &str) -> Result<(), AppError> {
    let escaped = escape_ps_single_quoted(name);
    let script = format!("Start-Process -FilePath '{escaped}'");
    run_powershell(&script)
}

pub fn isolate_application(_name: &str) -> Result<u32, AppError> {
    Err(AppError::backend_unavailable(
        "windows app isolate backend is not implemented yet",
    ))
}

fn parse_hwnd(id: &str) -> Option<i64> {
    let value = id.strip_prefix("hwnd:")?;
    value.parse::<i64>().ok()
}

fn escape_ps_single_quoted(value: &str) -> String {
    value.replace('\'', "''")
}

fn run_powershell(script: &str) -> Result<(), AppError> {
    let output = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run powershell: {err}")))?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(AppError::backend_unavailable(format!(
        "powershell command failed: {stderr}"
    )))
}

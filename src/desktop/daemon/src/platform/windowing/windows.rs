use std::process::Command;

use desktop_core::{error::AppError, protocol::Bounds};
use serde::Deserialize;
use serde_json::Value;

use super::{FrontmostWindowContext, WindowInfo};

#[derive(Debug, Deserialize)]
struct RawWindowInfo {
    id: String,
    pid: i64,
    index: u32,
    app: String,
    title: String,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    frontmost: bool,
    visible: bool,
}

pub fn main_display_bounds() -> Option<Bounds> {
    let script = r#"
Add-Type -AssemblyName System.Windows.Forms
$bounds = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
[pscustomobject]@{
  x = [double]$bounds.X
  y = [double]$bounds.Y
  width = [double]$bounds.Width
  height = [double]$bounds.Height
} | ConvertTo-Json -Compress
"#;

    let out = run_powershell_capture(script).ok()?;
    let value: Value = serde_json::from_str(out.trim()).ok()?;
    Some(Bounds {
        x: value.get("x")?.as_f64()?.max(0.0),
        y: value.get("y")?.as_f64()?.max(0.0),
        width: value.get("width")?.as_f64()?.max(0.0),
        height: value.get("height")?.as_f64()?.max(0.0),
    })
}

pub fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    let windows = list_windows().ok()?;
    let frontmost = windows.into_iter().find(|window| window.frontmost)?;
    Some(FrontmostWindowContext {
        app: Some(frontmost.app),
        bounds: Some(frontmost.bounds),
    })
}

pub fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    let script = r#"
Add-Type @"
using System;
using System.Runtime.InteropServices;

public struct RECT {
    public int Left;
    public int Top;
    public int Right;
    public int Bottom;
}

public static class DesktopCtlWin {
    [DllImport("user32.dll")]
    public static extern IntPtr GetForegroundWindow();

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);
}
"@;

$front = [int64][DesktopCtlWin]::GetForegroundWindow()
$rows = @()
$indexByPid = @{}

Get-Process |
  Where-Object { $_.MainWindowHandle -ne 0 -and $_.MainWindowTitle -ne "" } |
  ForEach-Object {
    $pidKey = [string]$_.Id
    if (-not $indexByPid.ContainsKey($pidKey)) {
      $indexByPid[$pidKey] = 0
    }
    $indexByPid[$pidKey] = [int]$indexByPid[$pidKey] + 1

    $rect = New-Object RECT
    [DesktopCtlWin]::GetWindowRect([intptr]$_.MainWindowHandle, [ref]$rect) | Out-Null

    $rows += [pscustomobject]@{
      id = "hwnd:$($_.MainWindowHandle)"
      pid = [int64]$_.Id
      index = [int]$indexByPid[$pidKey]
      app = $_.ProcessName
      title = $_.MainWindowTitle
      x = [double]$rect.Left
      y = [double]$rect.Top
      width = [double]($rect.Right - $rect.Left)
      height = [double]($rect.Bottom - $rect.Top)
      frontmost = ([int64]$_.MainWindowHandle -eq $front)
      visible = $true
    }
  }

$rows | ConvertTo-Json -Compress
"#;

    let output = run_powershell_capture(script)?;
    let rows = parse_window_rows(output.trim())?;
    Ok(rows.into_iter().map(to_window_info).collect())
}

pub fn list_windows_basic() -> Result<Vec<WindowInfo>, AppError> {
    list_windows()
}

pub fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    let windows = list_windows()?;
    let frontmost_app = windows
        .iter()
        .find(|window| window.frontmost)
        .map(|window| window.app.to_lowercase());

    match frontmost_app {
        Some(app) => Ok(windows
            .into_iter()
            .filter(|window| window.app.to_lowercase() == app)
            .collect()),
        None => Ok(Vec::new()),
    }
}

fn to_window_info(raw: RawWindowInfo) -> WindowInfo {
    WindowInfo {
        id: raw.id,
        window_ref: None,
        parent_id: None,
        pid: raw.pid,
        index: raw.index,
        app: raw.app,
        title: raw.title,
        bounds: Bounds {
            x: raw.x.max(0.0),
            y: raw.y.max(0.0),
            width: raw.width.max(0.0),
            height: raw.height.max(0.0),
        },
        frontmost: raw.frontmost,
        visible: raw.visible,
        modal: None,
    }
}

fn parse_window_rows(raw_json: &str) -> Result<Vec<RawWindowInfo>, AppError> {
    if raw_json.trim().is_empty() || raw_json.trim() == "null" {
        return Ok(Vec::new());
    }

    let value: Value = serde_json::from_str(raw_json)
        .map_err(|err| AppError::backend_unavailable(format!("invalid window JSON: {err}")))?;

    if value.is_array() {
        serde_json::from_value(value)
            .map_err(|err| AppError::backend_unavailable(format!("invalid window rows: {err}")))
    } else {
        let row: RawWindowInfo = serde_json::from_value(value)
            .map_err(|err| AppError::backend_unavailable(format!("invalid window row: {err}")))?;
        Ok(vec![row])
    }
}

fn run_powershell_capture(script: &str) -> Result<String, AppError> {
    let output = Command::new("powershell")
        .arg("-NoProfile")
        .arg("-Command")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run powershell: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::backend_unavailable(format!(
            "powershell command failed: {stderr}"
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

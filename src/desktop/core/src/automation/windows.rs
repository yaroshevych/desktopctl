use std::process::Command;

use crate::error::AppError;

use super::{Automation, Point};

pub struct WindowsAutomation;

impl WindowsAutomation {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for WindowsAutomation {
    fn default() -> Self {
        Self::new()
    }
}

impl Automation for WindowsAutomation {
    fn check_accessibility_permission(&self) -> Result<(), AppError> {
        Ok(())
    }

    fn press_hotkey(&self, hotkey: &str) -> Result<(), AppError> {
        let chord = to_sendkeys_chord(hotkey)?;
        send_keys(&chord)
    }

    fn press_enter(&self) -> Result<(), AppError> {
        send_keys("{ENTER}")
    }

    fn press_escape(&self) -> Result<(), AppError> {
        send_keys("{ESC}")
    }

    fn type_text(&self, text: &str) -> Result<(), AppError> {
        send_keys(&escape_sendkeys_text(text))
    }

    fn move_mouse(&self, point: Point) -> Result<(), AppError> {
        mouse_call("SetCursorPos", point.x, point.y, 0, 0)
    }

    fn left_down(&self, point: Point) -> Result<(), AppError> {
        mouse_call("LeftDown", point.x, point.y, 0, 0)
    }

    fn left_drag(&self, point: Point) -> Result<(), AppError> {
        mouse_call("SetCursorPos", point.x, point.y, 0, 0)
    }

    fn left_up(&self, point: Point) -> Result<(), AppError> {
        mouse_call("LeftUp", point.x, point.y, 0, 0)
    }

    fn left_click(&self, point: Point) -> Result<(), AppError> {
        mouse_call("LeftClick", point.x, point.y, 0, 0)
    }

    fn right_down(&self, point: Point) -> Result<(), AppError> {
        mouse_call("RightDown", point.x, point.y, 0, 0)
    }

    fn right_up(&self, point: Point) -> Result<(), AppError> {
        mouse_call("RightUp", point.x, point.y, 0, 0)
    }

    fn right_click(&self, point: Point) -> Result<(), AppError> {
        mouse_call("RightClick", point.x, point.y, 0, 0)
    }

    fn scroll_wheel(&self, dx: i32, dy: i32) -> Result<(), AppError> {
        // Match existing semantics: positive dy means scroll down.
        let vertical = -dy;
        mouse_call("Scroll", 0, 0, dx, vertical)
    }
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

fn send_keys(keys: &str) -> Result<(), AppError> {
    let escaped = keys.replace('"', "\"\"");
    let script = format!(
        "Add-Type -AssemblyName System.Windows.Forms; [System.Windows.Forms.SendKeys]::SendWait(\"{escaped}\")"
    );
    run_powershell(&script)
}

fn mouse_call(action: &str, x: u32, y: u32, dx: i32, dy: i32) -> Result<(), AppError> {
    let script = format!(
        r#"
Add-Type @"
using System;
using System.Runtime.InteropServices;

public static class DesktopCtlMouse {{
    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int X, int Y);

    [DllImport("user32.dll")]
    public static extern void mouse_event(uint dwFlags, uint dx, uint dy, uint dwData, UIntPtr dwExtraInfo);

    private const uint LEFTDOWN = 0x0002;
    private const uint LEFTUP = 0x0004;
    private const uint RIGHTDOWN = 0x0008;
    private const uint RIGHTUP = 0x0010;
    private const uint WHEEL = 0x0800;
    private const uint HWHEEL = 0x01000;

    public static void Run(string action, int x, int y, int dx, int dy) {{
        if (action == "SetCursorPos") {{
            SetCursorPos(x, y);
            return;
        }}

        SetCursorPos(x, y);
        switch (action) {{
            case "LeftDown":
                mouse_event(LEFTDOWN, 0, 0, 0, UIntPtr.Zero);
                break;
            case "LeftUp":
                mouse_event(LEFTUP, 0, 0, 0, UIntPtr.Zero);
                break;
            case "LeftClick":
                mouse_event(LEFTDOWN, 0, 0, 0, UIntPtr.Zero);
                mouse_event(LEFTUP, 0, 0, 0, UIntPtr.Zero);
                break;
            case "RightDown":
                mouse_event(RIGHTDOWN, 0, 0, 0, UIntPtr.Zero);
                break;
            case "RightUp":
                mouse_event(RIGHTUP, 0, 0, 0, UIntPtr.Zero);
                break;
            case "RightClick":
                mouse_event(RIGHTDOWN, 0, 0, 0, UIntPtr.Zero);
                mouse_event(RIGHTUP, 0, 0, 0, UIntPtr.Zero);
                break;
            case "Scroll":
                if (dy != 0) mouse_event(WHEEL, 0, 0, unchecked((uint)(dy * 120)), UIntPtr.Zero);
                if (dx != 0) mouse_event(HWHEEL, 0, 0, unchecked((uint)(dx * 120)), UIntPtr.Zero);
                break;
            default:
                throw new ArgumentException("unsupported mouse action: " + action);
        }}
    }}
}}
"@;
[DesktopCtlMouse]::Run("{action}", {x}, {y}, {dx}, {dy})
"#
    );

    run_powershell(&script)
}

fn to_sendkeys_chord(input: &str) -> Result<String, AppError> {
    let parts: Vec<String> = input
        .split('+')
        .map(|item| item.trim().to_lowercase())
        .filter(|item| !item.is_empty())
        .collect();

    if parts.is_empty() {
        return Err(AppError::invalid_argument(format!(
            "invalid hotkey format: {input}"
        )));
    }

    let key = parts
        .last()
        .cloned()
        .ok_or_else(|| AppError::invalid_argument(format!("invalid hotkey format: {input}")))?;

    let mut prefix = String::new();
    for modifier in &parts[..parts.len() - 1] {
        match modifier.as_str() {
            "ctrl" | "control" => prefix.push('^'),
            "shift" => prefix.push('+'),
            "alt" | "option" => prefix.push('%'),
            "cmd" | "command" | "win" | "windows" => prefix.push('^'),
            _ => {
                return Err(AppError::invalid_argument(format!(
                    "invalid hotkey format: {input}"
                )));
            }
        }
    }

    let key_token = match key.as_str() {
        "enter" | "return" => "{ENTER}".to_string(),
        "escape" | "esc" => "{ESC}".to_string(),
        "tab" => "{TAB}".to_string(),
        "space" => " ".to_string(),
        "left" | "leftarrow" | "left_arrow" => "{LEFT}".to_string(),
        "right" | "rightarrow" | "right_arrow" => "{RIGHT}".to_string(),
        "up" | "uparrow" | "up_arrow" => "{UP}".to_string(),
        "down" | "downarrow" | "down_arrow" => "{DOWN}".to_string(),
        _ if key.len() == 1 => escape_sendkeys_text(&key),
        _ => {
            return Err(AppError::invalid_argument(format!(
                "invalid hotkey format: {input}"
            )));
        }
    };

    Ok(format!("{prefix}{key_token}"))
}

fn escape_sendkeys_text(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '{' => out.push_str("{{}"),
            '}' => out.push_str("{}}"),
            '+' => out.push_str("{+}"),
            '^' => out.push_str("{^}"),
            '%' => out.push_str("{%}"),
            '~' => out.push_str("{~}"),
            '(' => out.push_str("{(}"),
            ')' => out.push_str("{)}"),
            _ => out.push(ch),
        }
    }
    out
}

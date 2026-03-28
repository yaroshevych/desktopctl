use desktop_core::{error::AppError, protocol::Bounds};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: String,
    pub window_ref: Option<String>,
    pub pid: i64,
    pub index: u32,
    pub app: String,
    pub title: String,
    pub bounds: Bounds,
    pub frontmost: bool,
    pub visible: bool,
}

impl WindowInfo {
    pub fn as_json(&self) -> Value {
        let public_id = self.window_ref.as_deref().unwrap_or(self.id.as_str());
        json!({
            "id": public_id,
            "pid": self.pid,
            "index": self.index,
            "app": self.app,
            "title": self.title,
            "bounds": self.bounds,
            "frontmost": self.frontmost,
            "visible": self.visible
        })
    }
}

#[derive(Debug, Clone)]
pub struct FrontmostWindowContext {
    pub app: Option<String>,
    pub bounds: Option<Bounds>,
}

#[cfg(target_os = "macos")]
pub fn main_display_bounds() -> Option<Bounds> {
    use core_graphics::display::CGDisplay;

    let bounds = CGDisplay::main().bounds();
    Some(Bounds {
        x: bounds.origin.x,
        y: bounds.origin.y,
        width: bounds.size.width.max(0.0),
        height: bounds.size.height.max(0.0),
    })
}

#[cfg(not(target_os = "macos"))]
pub fn main_display_bounds() -> Option<Bounds> {
    None
}

#[cfg(target_os = "macos")]
pub fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    use std::process::Command as ProcessCommand;

    let script = r#"tell application "System Events"
	set frontProc to first application process whose frontmost is true
	set appName to name of frontProc
	if (count of windows of frontProc) is 0 then
	    return appName
	end if
	set winPos to position of front window of frontProc
	set winSize to size of front window of frontProc
	return appName & tab & (item 1 of winPos as string) & tab & (item 2 of winPos as string) & tab & (item 1 of winSize as string) & tab & (item 2 of winSize as string)
	end tell"#;
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let parts: Vec<&str> = raw.split('\t').map(str::trim).collect();
    let app = parts
        .first()
        .map(|v| v.to_string())
        .filter(|v| !v.is_empty());
    let bounds = if parts.len() >= 5 {
        let parsed: Vec<f64> = parts[1..5]
            .iter()
            .filter_map(|v| v.parse::<f64>().ok())
            .collect();
        if parsed.len() == 4 {
            Some(Bounds {
                x: parsed[0].max(0.0),
                y: parsed[1].max(0.0),
                width: parsed[2].max(0.0),
                height: parsed[3].max(0.0),
            })
        } else {
            None
        }
    } else {
        None
    };
    Some(FrontmostWindowContext { app, bounds })
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    None
}

#[cfg(target_os = "macos")]
pub fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    use std::process::Command as ProcessCommand;

    let script = r#"tell application "System Events"
set resultRows to {}
repeat with p in (application processes whose background only is false)
    set pname to (name of p) as text
    set pfront to (frontmost of p) as string
    set pvisible to (visible of p) as string
    set ppid to unix id of p
    set widx to 0
    repeat with w in (windows of p)
        set widx to widx + 1
        try
            set wname to (name of w) as text
        on error
            set wname to ""
        end try
        try
            set winPos to position of w
            set winSize to size of w
            set wx to item 1 of winPos
            set wy to item 2 of winPos
            set ww to item 1 of winSize
            set wh to item 2 of winSize
            set end of resultRows to (ppid as string) & tab & (widx as string) & tab & pname & tab & wname & tab & (wx as string) & tab & (wy as string) & tab & (ww as string) & tab & (wh as string) & tab & pfront & tab & pvisible
        end try
    end repeat
end repeat
set AppleScript's text item delimiters to linefeed
set outputText to resultRows as text
set AppleScript's text item delimiters to ""
return outputText
end tell"#;

    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::backend_unavailable(format!(
            "failed to enumerate windows: {stderr}"
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let mut windows: Vec<WindowInfo> = raw.lines().filter_map(parse_window_line).collect();
    windows.sort_by(|a, b| {
        b.frontmost
            .cmp(&a.frontmost)
            .then_with(|| a.app.to_lowercase().cmp(&b.app.to_lowercase()))
            .then_with(|| a.index.cmp(&b.index))
    });
    Ok(windows)
}

#[cfg(not(target_os = "macos"))]
pub fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

#[cfg(target_os = "macos")]
pub fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    use std::process::Command as ProcessCommand;

    let script = r#"tell application "System Events"
set resultRows to {}
set frontProc to first application process whose frontmost is true
set pname to (name of frontProc) as text
set pvisible to (visible of frontProc) as string
set ppid to unix id of frontProc
set widx to 0
repeat with w in (windows of frontProc)
    set widx to widx + 1
    try
        set wname to (name of w) as text
    on error
        set wname to ""
    end try
    try
        set winPos to position of w
        set winSize to size of w
        set wx to item 1 of winPos
        set wy to item 2 of winPos
        set ww to item 1 of winSize
        set wh to item 2 of winSize
        set end of resultRows to (ppid as string) & tab & (widx as string) & tab & pname & tab & wname & tab & (wx as string) & tab & (wy as string) & tab & (ww as string) & tab & (wh as string) & tab & "true" & tab & pvisible
    end try
end repeat
set AppleScript's text item delimiters to linefeed
set outputText to resultRows as text
set AppleScript's text item delimiters to ""
return outputText
end tell"#;

    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::backend_unavailable(format!(
            "failed to enumerate frontmost app windows: {stderr}"
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let mut windows: Vec<WindowInfo> = raw.lines().filter_map(parse_window_line).collect();
    windows.sort_by_key(|window| std::cmp::Reverse(window_area(&window.bounds)));
    Ok(windows)
}

#[cfg(not(target_os = "macos"))]
pub fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

fn parse_applescript_bool(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("true")
}

fn parse_window_line(line: &str) -> Option<WindowInfo> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() != 10 {
        return None;
    }

    let pid = fields[0].trim().parse::<i64>().ok()?;
    let index = fields[1].trim().parse::<u32>().ok()?;
    let app = fields[2].trim().to_string();
    let title = fields[3].trim().to_string();
    let x = fields[4].trim().parse::<f64>().ok()?;
    let y = fields[5].trim().parse::<f64>().ok()?;
    let width = fields[6].trim().parse::<f64>().ok()?;
    let height = fields[7].trim().parse::<f64>().ok()?;
    let frontmost = parse_applescript_bool(fields[8]);
    let visible = parse_applescript_bool(fields[9]);

    Some(WindowInfo {
        id: format!("{pid}:{index}"),
        window_ref: None,
        pid,
        index,
        app,
        title,
        bounds: Bounds {
            x: x.max(0.0),
            y: y.max(0.0),
            width: width.max(0.0),
            height: height.max(0.0),
        },
        frontmost,
        visible,
    })
}

fn window_area(bounds: &Bounds) -> u64 {
    let area = bounds.width.max(0.0) * bounds.height.max(0.0);
    area.round().max(0.0) as u64
}

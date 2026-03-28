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
    use core_foundation::{
        base::{CFType, TCFType},
        boolean::CFBoolean,
        dictionary::CFDictionary,
        number::CFNumber,
        string::CFString,
    };
    use core_graphics::{
        display::CGDisplay,
        window::{
            kCGNullWindowID, kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowLayer,
            kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowName,
            kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID,
        },
    };
    use std::collections::HashMap;
    use std::ffi::c_void;

    fn dict_number_i64(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<i64> {
        let value = dict.find(key)?;
        value
            .downcast::<CFNumber>()
            .and_then(|n| n.to_i64().or_else(|| n.to_f64().map(|v| v.round() as i64)))
    }

    fn dict_number_f64_untyped(dict: &CFDictionary, key: &str) -> Option<f64> {
        let key = CFString::new(key);
        let raw = dict.find(key.as_concrete_TypeRef() as *const c_void)?;
        let value = unsafe { CFType::wrap_under_get_rule(*raw as _) };
        value
            .downcast::<CFNumber>()
            .and_then(|n| n.to_f64().or_else(|| n.to_i64().map(|v| v as f64)))
    }

    fn dict_string(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<String> {
        let value = dict.find(key)?;
        value.downcast::<CFString>().map(|s| s.to_string())
    }

    fn dict_bool(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<bool> {
        let value = dict.find(key)?;
        value.downcast::<CFBoolean>().map(bool::from)
    }

    fn dict_bounds(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<Bounds> {
        let value = dict.find(key)?;
        let bounds = value.downcast::<CFDictionary>()?;
        let x = dict_number_f64_untyped(&bounds, "X")?;
        let y = dict_number_f64_untyped(&bounds, "Y")?;
        let width = dict_number_f64_untyped(&bounds, "Width")?;
        let height = dict_number_f64_untyped(&bounds, "Height")?;
        Some(Bounds {
            x: x.max(0.0),
            y: y.max(0.0),
            width: width.max(0.0),
            height: height.max(0.0),
        })
    }

    let key_number = unsafe { CFString::wrap_under_get_rule(kCGWindowNumber) };
    let key_pid = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
    let key_owner_name = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerName) };
    let key_name = unsafe { CFString::wrap_under_get_rule(kCGWindowName) };
    let key_layer = unsafe { CFString::wrap_under_get_rule(kCGWindowLayer) };
    let key_bounds = unsafe { CFString::wrap_under_get_rule(kCGWindowBounds) };
    let key_onscreen = unsafe { CFString::wrap_under_get_rule(kCGWindowIsOnscreen) };

    let frontmost_app = frontmost_window_context().and_then(|ctx| ctx.app);
    let option = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
    let rows = CGDisplay::window_list_info(option, Some(kCGNullWindowID)).ok_or_else(|| {
        AppError::backend_unavailable("failed to enumerate windows via CoreGraphics")
    })?;

    let mut per_pid_index: HashMap<i64, u32> = HashMap::new();
    let mut windows: Vec<WindowInfo> = Vec::new();
    for raw in rows.get_all_values() {
        let row = unsafe { CFDictionary::<CFString, CFType>::wrap_under_get_rule(raw as _) };
        let Some(layer) = dict_number_i64(&row, &key_layer) else {
            continue;
        };
        if layer != 0 {
            continue;
        }
        let Some(pid) = dict_number_i64(&row, &key_pid) else {
            continue;
        };
        let app = dict_string(&row, &key_owner_name).unwrap_or_default();
        if app.is_empty() || app == "Window Server" {
            continue;
        }
        let Some(bounds) = dict_bounds(&row, &key_bounds) else {
            continue;
        };
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            continue;
        }
        let cg_id = dict_number_i64(&row, &key_number).unwrap_or(0);
        let title = dict_string(&row, &key_name).unwrap_or_default();
        let visible = dict_bool(&row, &key_onscreen).unwrap_or(true);
        let next = per_pid_index.entry(pid).or_insert(0);
        *next = next.saturating_add(1);
        let index = *next;
        windows.push(WindowInfo {
            id: format!("{pid}:{cg_id}"),
            window_ref: None,
            pid,
            index,
            app: app.clone(),
            title,
            bounds,
            frontmost: frontmost_app.as_deref() == Some(app.as_str()),
            visible,
        });
    }

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

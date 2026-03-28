use desktop_core::protocol::Bounds;

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

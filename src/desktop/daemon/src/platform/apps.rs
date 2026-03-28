use desktop_core::error::AppError;

use super::windowing::WindowInfo;

#[cfg(target_os = "macos")]
pub fn focus_window(window: &WindowInfo) -> Result<(), AppError> {
    use std::process::Command as ProcessCommand;

    let escaped_app = window.app.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
set targetPid to {pid}
set targetIndex to {index}
repeat with p in (application processes whose background only is false)
    if (unix id of p) is targetPid then
        set frontmost of p to true
        set idx to 0
        repeat with w in (windows of p)
            set idx to idx + 1
            if idx is targetIndex then
                try
                    perform action "AXRaise" of w
                end try
                exit repeat
            end if
        end repeat
        return "ok"
    end if
end repeat
return ""
end tell
tell application "{escaped_app}" to activate"#,
        pid = window.pid,
        index = window.index
    );
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::internal(format!(
            "failed to focus window \"{}\": {stderr}",
            window.title
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn focus_window(_window: &WindowInfo) -> Result<(), AppError> {
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

#[cfg(target_os = "macos")]
pub fn hide_application(name: &str) -> Result<&'static str, AppError> {
    use std::process::Command as ProcessCommand;

    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
if exists process "{escaped}" then
    set visible of process "{escaped}" to false
    return "hidden"
else
    return "not_running"
end if
end tell"#
    );
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::internal(format!(
            "failed to hide application \"{name}\": {stderr}"
        )));
    }

    let state_buf = String::from_utf8_lossy(&output.stdout);
    let state = state_buf.trim();
    if state == "hidden" {
        Ok("hidden")
    } else {
        Ok("not_running")
    }
}

#[cfg(not(target_os = "macos"))]
pub fn hide_application(_name: &str) -> Result<&'static str, AppError> {
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

#[cfg(target_os = "macos")]
pub fn show_application(name: &str) -> Result<(), AppError> {
    use std::process::Command as ProcessCommand;

    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
if exists process "{escaped}" then
    set visible of process "{escaped}" to true
end if
end tell
tell application "{escaped}" to activate"#
    );
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::internal(format!(
            "failed to show application \"{name}\": {stderr}"
        )));
    }
    Ok(())
}

#[cfg(not(target_os = "macos"))]
pub fn show_application(_name: &str) -> Result<(), AppError> {
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

#[cfg(target_os = "macos")]
pub fn isolate_application(name: &str) -> Result<u32, AppError> {
    use std::process::Command as ProcessCommand;

    let escaped = name.replace('\\', "\\\\").replace('"', "\\\"");
    let script = format!(
        r#"tell application "System Events"
set targetName to "{escaped}"
set hiddenCount to 0
repeat with p in (application processes whose background only is false)
    set pname to (name of p) as text
    if pname is not targetName then
        try
            if visible of p then
                set visible of p to false
                set hiddenCount to hiddenCount + 1
            end if
        end try
    end if
end repeat
return hiddenCount as string
end tell
tell application "{escaped}" to activate"#
    );
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::internal(format!(
            "failed to isolate application \"{name}\": {stderr}"
        )));
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    Ok(value.parse::<u32>().unwrap_or(0))
}

#[cfg(not(target_os = "macos"))]
pub fn isolate_application(_name: &str) -> Result<u32, AppError> {
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

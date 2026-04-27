use desktop_core::error::AppError;
use serde_json::json;
use std::process::Command as ProcessCommand;

const SCREEN_RECORDING_REMEDIATION: &str = "grant Screen Recording for DesktopCtl.app in System Settings -> Privacy & Security -> Screen Recording, then rerun the command";
const ACCESSIBILITY_REMEDIATION: &str = "grant Accessibility for DesktopCtl.app in System Settings -> Privacy & Security -> Accessibility, then rerun the command";

pub fn accessibility_granted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn screen_recording_granted() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

pub fn ensure_screen_recording_permission() -> Result<(), AppError> {
    if screen_recording_granted() {
        return Ok(());
    }

    // Trigger macOS permission flow on demand for CLI/on-demand daemon paths.
    let _ = request_screen_recording_permission_prompt();
    if screen_recording_granted() {
        return Ok(());
    }
    let _ = open_screen_recording_settings();

    Err(
        AppError::permission_denied("screen recording permission is required")
            .with_details(json!({ "remediation": SCREEN_RECORDING_REMEDIATION })),
    )
}

pub fn screen_recording_remediation() -> &'static str {
    SCREEN_RECORDING_REMEDIATION
}

pub fn accessibility_remediation() -> &'static str {
    ACCESSIBILITY_REMEDIATION
}

fn request_screen_recording_permission_prompt() -> bool {
    unsafe { CGRequestScreenCaptureAccess() }
}

pub fn open_screen_recording_settings() -> bool {
    let status = ProcessCommand::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .status();
    matches!(status, Ok(s) if s.success())
}

pub fn open_accessibility_settings() -> bool {
    let status = ProcessCommand::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status();
    matches!(status, Ok(s) if s.success())
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

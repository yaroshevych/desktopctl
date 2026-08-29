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
    // On macOS 15+, CGRequestScreenCaptureAccess and open(systempreferences:) open System
    // Settings directly, causing repeated popups every time this is called from the daemon
    // (e.g. on every journal capture attempt). Return a structured error instead; the
    // remediation message tells the user to visit System Settings manually.
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

#[allow(dead_code)]
pub fn open_screen_recording_settings() -> bool {
    let status = ProcessCommand::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
        .status();
    matches!(status, Ok(s) if s.success())
}

#[allow(dead_code)]
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
    #[allow(dead_code)]
    fn CGRequestScreenCaptureAccess() -> bool;
}

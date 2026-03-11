use desktop_core::error::AppError;
use serde_json::json;

const SCREEN_RECORDING_REMEDIATION: &str = "grant Screen Recording for DesktopCtl.app in System Settings -> Privacy & Security -> Screen Recording, then rerun the command";

pub fn accessibility_granted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn screen_recording_granted() -> bool {
    unsafe { CGPreflightScreenCaptureAccess() }
}

pub fn ensure_screen_recording_permission() -> Result<(), AppError> {
    if screen_recording_granted() {
        Ok(())
    } else {
        Err(
            AppError::permission_denied("screen recording permission is required")
                .with_details(json!({ "remediation": SCREEN_RECORDING_REMEDIATION })),
        )
    }
}

pub fn screen_recording_remediation() -> &'static str {
    SCREEN_RECORDING_REMEDIATION
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn CGPreflightScreenCaptureAccess() -> bool;
}

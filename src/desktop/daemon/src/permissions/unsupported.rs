use desktop_core::error::AppError;
use serde_json::json;

const SCREEN_RECORDING_REMEDIATION: &str =
    "screen recording permissions are supported only on macOS";
const ACCESSIBILITY_REMEDIATION: &str = "accessibility permissions are supported only on macOS";

pub fn accessibility_granted() -> bool {
    false
}

pub fn screen_recording_granted() -> bool {
    false
}

pub fn ensure_screen_recording_permission() -> Result<(), AppError> {
    Err(
        AppError::permission_denied("screen recording permission is supported only on macOS")
            .with_details(json!({ "remediation": SCREEN_RECORDING_REMEDIATION })),
    )
}

#[derive(Debug, Clone, Copy, Default)]
pub struct StartupPermissionRequests {
    pub accessibility_requested: bool,
    pub screen_recording_requested: bool,
}

pub fn request_startup_permissions() -> StartupPermissionRequests {
    StartupPermissionRequests::default()
}

pub fn screen_recording_remediation() -> &'static str {
    SCREEN_RECORDING_REMEDIATION
}

pub fn accessibility_remediation() -> &'static str {
    ACCESSIBILITY_REMEDIATION
}

pub fn open_screen_recording_settings() -> bool {
    false
}

pub fn open_accessibility_settings() -> bool {
    false
}

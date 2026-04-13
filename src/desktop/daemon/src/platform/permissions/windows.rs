use desktop_core::error::AppError;

#[derive(Debug, Clone, Copy, Default)]
pub struct StartupPermissionRequests {
    pub accessibility_requested: bool,
    pub screen_recording_requested: bool,
}

pub fn accessibility_granted() -> bool {
    true
}

pub fn screen_recording_granted() -> bool {
    true
}

pub fn ensure_screen_recording_permission() -> Result<(), AppError> {
    Ok(())
}

pub fn request_startup_permissions() -> StartupPermissionRequests {
    StartupPermissionRequests::default()
}

pub fn screen_recording_remediation() -> &'static str {
    "screen capture permissions are handled per-app on Windows"
}

pub fn accessibility_remediation() -> &'static str {
    "UI automation availability depends on the target process integrity level"
}

pub fn open_screen_recording_settings() -> bool {
    false
}

pub fn open_accessibility_settings() -> bool {
    false
}

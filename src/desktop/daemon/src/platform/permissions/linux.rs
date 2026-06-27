//! Linux (XDG portal) permissions backend.
//!
//! On GNOME Wayland, capture and input consent are mediated by the
//! `org.freedesktop.portal.ScreenCast` and `RemoteDesktop` portals at session
//! setup (DesktopCtl-0f2). There is no persistent "granted" bit to poll the way
//! macOS exposes; consent is per-session and may be revoked or re-prompted.
//! Until the portal session manager lands, these report not-granted and surface
//! the `permission_required` failure state.

use desktop_core::error::AppError;

const SCREEN_RECORDING_REMEDIATION: &str =
    "grant screen capture via the ScreenCast portal consent dialog at session start";
const ACCESSIBILITY_REMEDIATION: &str =
    "AT-SPI accessibility is enabled per-session; no separate grant is required on GNOME";

pub fn accessibility_granted() -> bool {
    true
}

pub fn screen_recording_granted() -> bool {
    false
}

pub fn ensure_screen_recording_permission() -> Result<(), AppError> {
    Ok(())
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

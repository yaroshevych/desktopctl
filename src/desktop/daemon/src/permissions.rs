use desktop_core::error::AppError;
use objc2_foundation::{NSDictionary, NSNumber, NSString};
use serde_json::json;
use std::ffi::c_void;
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

#[derive(Debug, Clone, Copy, Default)]
pub struct StartupPermissionRequests {
    pub accessibility_requested: bool,
    pub screen_recording_requested: bool,
}

pub fn request_startup_permissions() -> StartupPermissionRequests {
    let mut requests = StartupPermissionRequests::default();

    if !accessibility_granted() {
        requests.accessibility_requested = request_accessibility_permission_prompt();
    }
    if !screen_recording_granted() {
        requests.screen_recording_requested = request_screen_recording_permission_prompt();
        if !screen_recording_granted() {
            let _ = open_screen_recording_settings();
        }
    }

    requests
}

pub fn screen_recording_remediation() -> &'static str {
    SCREEN_RECORDING_REMEDIATION
}

pub fn accessibility_remediation() -> &'static str {
    ACCESSIBILITY_REMEDIATION
}

fn request_accessibility_permission_prompt() -> bool {
    let key = NSString::from_str("AXTrustedCheckOptionPrompt");
    let prompt_enabled = NSNumber::new_bool(true);
    let options = NSDictionary::<NSString, NSNumber>::from_slices(&[&*key], &[&*prompt_enabled]);
    let options_ptr = (&*options) as *const NSDictionary<NSString, NSNumber> as *const c_void;

    unsafe { AXIsProcessTrustedWithOptions(options_ptr) }
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
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

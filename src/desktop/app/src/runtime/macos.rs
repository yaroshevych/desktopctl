use objc2_app_kit::{NSApplicationActivationOptions, NSRunningApplication};

/// Reactivates the application that was focused before DesktopCtl opened.
pub fn activate_pid_immediately(pid: i64) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    NSRunningApplication::runningApplicationWithProcessIdentifier(pid)
        .is_some_and(|app| app.activateWithOptions(NSApplicationActivationOptions::empty()))
}

use objc2::MainThreadMarker;
use objc2_app_kit::{
    NSApplication, NSApplicationActivationOptions, NSRunningApplication, NSWorkspace,
};
use objc2_foundation::NSProcessInfo;

/// Returns the application currently receiving key events, excluding DesktopCtl.
pub fn frontmost_application_pid() -> Option<i64> {
    let pid = NSWorkspace::sharedWorkspace()
        .frontmostApplication()?
        .processIdentifier();
    if pid <= 0 || pid as u32 == std::process::id() {
        return None;
    }
    Some(i64::from(pid))
}

/// Reactivates the application that was focused before DesktopCtl opened.
pub fn activate_pid_immediately(pid: i64) -> bool {
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    let Some(target) = NSRunningApplication::runningApplicationWithProcessIdentifier(pid) else {
        return false;
    };
    let Some(mtm) = MainThreadMarker::new() else {
        return false;
    };
    let current = NSApplication::sharedApplication(mtm);
    if NSProcessInfo::processInfo()
        .operatingSystemVersion()
        .majorVersion
        >= 14
    {
        current.yieldActivationToApplication(&target);
    } else {
        current.deactivate();
    }
    target.activateWithOptions(NSApplicationActivationOptions::empty())
}

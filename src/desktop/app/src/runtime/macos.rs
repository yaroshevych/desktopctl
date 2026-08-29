use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationOptions, NSRunningApplication};
use objc2_foundation::NSProcessInfo;

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

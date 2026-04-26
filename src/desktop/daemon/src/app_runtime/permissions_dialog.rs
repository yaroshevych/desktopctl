use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW;

const PERMISSIONS_TEXT: &str = "DesktopCtl controls mouse and keyboard input and reads screen content.\n\n\
Windows does not require special permission grants for UIAutomation, so no setup steps are needed.\n\n\
DesktopCtl is running and ready for agent connections.";

pub(crate) fn show() {
    show_permissions_dialog();
}

fn show_permissions_dialog() {
    unsafe {
        let title = wide_string("DesktopCtl - Setup");
        let message = PERMISSIONS_TEXT;
        let message_wide = wide_string(message);

        // MB_OK (0) - Just an OK button
        let _result = MessageBoxW(
            std::ptr::null_mut(),
            message_wide.as_ptr(),
            title.as_ptr(),
            0, // MB_OK
        );
    }
}

fn wide_string(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

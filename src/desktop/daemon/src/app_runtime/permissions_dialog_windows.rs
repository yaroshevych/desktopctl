use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW;

const MESSAGE: &str = "DesktopCtl controls mouse, keyboard, and reads screen content \
for local AI agents.\n\nOn Windows, no special permission grants are required.\n\n\
DesktopCtl is running and ready for agent connections.";

pub(crate) fn show() {
    unsafe {
        let title = wide("DesktopCtl - Setup");
        let text = wide(MESSAGE);
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0);
    }
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

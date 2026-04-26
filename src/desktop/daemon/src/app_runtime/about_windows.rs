use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::Foundation::S_OK;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW;

const ABOUT_TEXT: &str = "Computer Vision and mouse/keyboard control utility \
for local AI agents. Bring your own AI and automate UI interactions with \
mouse, keyboard, and GPU-accelerated text recognition.\n\n\
DesktopCtl does not collect your data and does not connect to the internet.\n\n\
Point your AI agent at the GitHub link below to read the docs and get started.";

const WEBSITE_URL: &str = "https://desktopctl.com";
const GITHUB_URL: &str = "https://github.com/yaroshevych/desktopctl";

pub(crate) fn show() {
    show_about_dialog();
}

fn show_about_dialog() {
    unsafe {
        let title = wide_string("About DesktopCtl");
        let message = format_about_message();
        let message_wide = wide_string(&message);

        // MB_YESNOCANCEL (3) - Yes, No, Cancel buttons
        // We'll repurpose these: Yes = Website, No = GitHub, Cancel = Close
        let result = MessageBoxW(
            std::ptr::null_mut(),
            message_wide.as_ptr(),
            title.as_ptr(),
            3, // MB_YESNOCANCEL
        );

        match result {
            6 => {
                // IDYES (6) - Open Website
                open_url(WEBSITE_URL);
            }
            7 => {
                // IDNO (7) - Open GitHub
                open_url(GITHUB_URL);
            }
            2 => {
                // IDCANCEL (2) - Close
            }
            _ => {}
        }
    }
}

fn format_about_message() -> String {
    format!("DesktopCtl\n\n{}\n\nClick 'Yes' to open website or 'No' to open GitHub.", ABOUT_TEXT)
}

fn open_url(url: &str) {
    unsafe {
        let url_wide = wide_string(url);
        let operation = wide_string("open");
        let _result = ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            url_wide.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            1, // SW_SHOWNORMAL
        );
    }
}

fn wide_string(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

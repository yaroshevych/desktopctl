use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use windows_sys::Win32::UI::WindowsAndMessaging::MessageBoxW;

use crate::app_policy;

pub(crate) fn show() {
    let cfg = app_policy::current();

    let message = match cfg.policy_mode {
        app_policy::PolicyMode::AllowAll => {
            format!(
                "Agent Access Policy: Allow All Apps\n\n\
                DesktopCtl is currently configured to allow agent access to all applications.\n\n\
                Clipboard access allowed: {}\n\n\
                To manage specific app permissions, use the CLI:\n\
                desktopctl policy list\n\
                desktopctl policy set --mode allow-only --apps \"App1, App2\"",
                cfg.clipboard_allowed
            )
        }
        app_policy::PolicyMode::AllowOnlySelected => {
            let apps_list = cfg.apps.join(", ");
            format!(
                "Agent Access Policy: Allow Only Selected Apps\n\n\
                Allowed apps: {}\n\n\
                Clipboard access allowed: {}\n\n\
                To change permissions, use the CLI:\n\
                desctopctl policy list\n\
                desktopctl policy set --mode allow-only --apps \"App1, App2\"",
                if apps_list.is_empty() {
                    "(none)"
                } else {
                    &apps_list
                },
                cfg.clipboard_allowed
            )
        }
        app_policy::PolicyMode::AllowAllExcept => {
            let apps_list = cfg.apps.join(", ");
            format!(
                "Agent Access Policy: Allow All Except\n\n\
                Blocked apps: {}\n\n\
                Clipboard access allowed: {}\n\n\
                To change permissions, use the CLI:\n\
                desktopctl policy list\n\
                desktopctl policy set --mode allow-all-except --apps \"App1, App2\"",
                if apps_list.is_empty() {
                    "(none)"
                } else {
                    &apps_list
                },
                cfg.clipboard_allowed
            )
        }
    };

    unsafe {
        let title = wide("Agent Permissions");
        let text = wide(&message);
        MessageBoxW(std::ptr::null_mut(), text.as_ptr(), title.as_ptr(), 0);
    }
}

fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

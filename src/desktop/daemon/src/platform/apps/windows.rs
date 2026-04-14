use std::{ffi::OsStr, os::windows::ffi::OsStrExt};

use desktop_core::error::AppError;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::System::Threading::AttachThreadInput;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::{
    Input::KeyboardAndMouse::{
        INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC,
        MapVirtualKeyW, SendInput, VK_MENU,
    },
    Shell::ShellExecuteW,
    WindowsAndMessaging::{
        BringWindowToTop, GetForegroundWindow, GetWindowThreadProcessId, SW_RESTORE,
        SetForegroundWindow, ShowWindow,
    },
};

use crate::platform::windowing::WindowInfo;

pub fn focus_window(window: &WindowInfo) -> Result<(), AppError> {
    let hwnd = parse_hwnd(&window.id).ok_or_else(|| {
        AppError::invalid_argument(format!("window id is not a Windows hwnd id: {}", window.id))
    })?;

    // SAFETY: hwnd comes from our own window enumeration and is used only for focus.
    unsafe {
        ShowWindow(hwnd, SW_RESTORE);
        if SetForegroundWindow(hwnd) != 0 {
            return Ok(());
        }

        let current_tid = GetCurrentThreadId();
        let foreground = GetForegroundWindow();
        let mut foreground_tid = 0_u32;
        if !foreground.is_null() {
            foreground_tid = GetWindowThreadProcessId(foreground, std::ptr::null_mut());
        }
        let target_tid = GetWindowThreadProcessId(hwnd, std::ptr::null_mut());

        let attached_foreground = foreground_tid != 0
            && foreground_tid != current_tid
            && AttachThreadInput(current_tid, foreground_tid, 1) != 0;
        let attached_target = target_tid != 0
            && target_tid != current_tid
            && AttachThreadInput(current_tid, target_tid, 1) != 0;

        let _ = send_alt_tap();
        BringWindowToTop(hwnd);
        let activated = SetForegroundWindow(hwnd) != 0;

        if attached_target {
            let _ = AttachThreadInput(current_tid, target_tid, 0);
        }
        if attached_foreground {
            let _ = AttachThreadInput(current_tid, foreground_tid, 0);
        }

        if !activated {
            return Err(AppError::backend_unavailable(format!(
                "unable to focus window {} (Windows foreground lock policy)",
                window.id
            )));
        }
    }

    Ok(())
}

pub fn hide_application(_name: &str) -> Result<&'static str, AppError> {
    Err(AppError::backend_unavailable(
        "windows app hide backend is not implemented yet",
    ))
}

pub fn show_application(name: &str) -> Result<(), AppError> {
    let operation = to_wide("open");
    let file = to_wide(name);
    // SAFETY: pointers are valid NUL-terminated UTF-16 buffers for call duration.
    let result = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            file.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_RESTORE,
        )
    };
    if (result as usize) <= 32 {
        return Err(AppError::backend_unavailable(format!(
            "failed to launch application '{name}' via ShellExecuteW"
        )));
    }
    Ok(())
}

pub fn isolate_application(_name: &str) -> Result<u32, AppError> {
    Err(AppError::backend_unavailable(
        "windows app isolate backend is not implemented yet",
    ))
}

fn parse_hwnd(id: &str) -> Option<HWND> {
    let value = id.strip_prefix("hwnd:")?.parse::<usize>().ok()?;
    Some(value as HWND)
}

fn to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value).encode_wide().chain(Some(0)).collect()
}

fn send_alt_tap() -> Result<(), AppError> {
    // SAFETY: MapVirtualKeyW is pure for this usage.
    let scan = unsafe { MapVirtualKeyW(VK_MENU as u32, MAPVK_VK_TO_VSC) } as u16;
    let down = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_MENU,
                wScan: scan,
                dwFlags: 0,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    let up = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: VK_MENU,
                wScan: scan,
                dwFlags: KEYEVENTF_KEYUP,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: `down` and `up` are valid INPUT structures.
    let sent = unsafe { SendInput(2, [down, up].as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    if sent != 2 {
        return Err(AppError::backend_unavailable(
            "failed to send synthetic Alt key event",
        ));
    }
    Ok(())
}

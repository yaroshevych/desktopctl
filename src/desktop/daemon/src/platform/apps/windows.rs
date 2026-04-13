use std::process::Command;

use desktop_core::error::AppError;
use windows_sys::Win32::Foundation::HWND;
use windows_sys::Win32::UI::WindowsAndMessaging::{SW_RESTORE, SetForegroundWindow, ShowWindow};

use crate::platform::windowing::WindowInfo;

pub fn focus_window(window: &WindowInfo) -> Result<(), AppError> {
    let hwnd = parse_hwnd(&window.id).ok_or_else(|| {
        AppError::invalid_argument(format!("window id is not a Windows hwnd id: {}", window.id))
    })?;

    // SAFETY: hwnd comes from our own window enumeration and is used only for focus.
    unsafe {
        ShowWindow(hwnd, SW_RESTORE);
        if SetForegroundWindow(hwnd) == 0 {
            return Err(AppError::backend_unavailable(format!(
                "SetForegroundWindow failed for {}",
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
    Command::new(name).spawn().map_err(|err| {
        AppError::backend_unavailable(format!("failed to launch application '{name}': {err}"))
    })?;
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

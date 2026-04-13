use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::Path};

use desktop_core::{error::AppError, protocol::Bounds};
use windows_sys::Win32::{
    Foundation::{HWND, LPARAM, RECT},
    Graphics::Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTOPRIMARY, MONITORINFO, MonitorFromWindow},
    System::Threading::{
        OpenProcess, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
        QueryFullProcessImageNameW,
    },
    UI::WindowsAndMessaging::{
        EnumWindows, GW_OWNER, GetForegroundWindow, GetWindow, GetWindowRect, GetWindowTextLengthW,
        GetWindowTextW, GetWindowThreadProcessId, IsWindowVisible,
    },
};

use super::{FrontmostWindowContext, WindowInfo};

#[derive(Debug)]
struct RawWindow {
    hwnd: HWND,
    pid: u32,
    title: String,
    bounds: Bounds,
    visible: bool,
}

pub fn main_display_bounds() -> Option<Bounds> {
    // SAFETY: Calling Win32 monitor APIs with a null HWND requests primary monitor.
    unsafe {
        let monitor = MonitorFromWindow(std::ptr::null_mut(), MONITOR_DEFAULTTOPRIMARY);
        if monitor.is_null() {
            return None;
        }
        let mut info = MONITORINFO {
            cbSize: std::mem::size_of::<MONITORINFO>() as u32,
            rcMonitor: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            rcWork: RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            },
            dwFlags: 0,
        };
        if GetMonitorInfoW(monitor, &mut info as *mut MONITORINFO) == 0 {
            return None;
        }
        Some(rect_to_bounds(info.rcMonitor))
    }
}

pub fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    // SAFETY: GetForegroundWindow has no preconditions.
    let frontmost = unsafe { GetForegroundWindow() };
    if frontmost.is_null() {
        return None;
    }

    let window = raw_window(frontmost)?;
    Some(FrontmostWindowContext {
        app: process_name(window.pid),
        bounds: Some(window.bounds),
    })
}

pub fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    // SAFETY: callback_data is a valid mutable Vec for the call duration.
    let mut rows: Vec<RawWindow> = Vec::new();
    let callback_data = &mut rows as *mut Vec<RawWindow>;
    // SAFETY: EnumWindows invokes the callback synchronously while callback_data is valid.
    let ok = unsafe { EnumWindows(Some(enum_windows_proc), callback_data as LPARAM) };
    if ok == 0 {
        return Err(AppError::backend_unavailable(
            "EnumWindows failed on Windows",
        ));
    }

    // SAFETY: GetForegroundWindow has no preconditions.
    let frontmost_hwnd = unsafe { GetForegroundWindow() };
    let mut per_pid_index: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();

    let mut windows = Vec::with_capacity(rows.len());
    for row in rows {
        let app = process_name(row.pid).unwrap_or_else(|| format!("pid-{}", row.pid));
        let index = per_pid_index.entry(row.pid).or_insert(0);
        *index = index.saturating_add(1);

        windows.push(WindowInfo {
            id: format!("hwnd:{}", row.hwnd as usize),
            window_ref: None,
            parent_id: None,
            pid: row.pid as i64,
            index: *index,
            app,
            title: row.title,
            bounds: row.bounds,
            frontmost: row.hwnd == frontmost_hwnd,
            visible: row.visible,
            modal: None,
        });
    }

    windows.sort_by(|a, b| {
        b.frontmost
            .cmp(&a.frontmost)
            .then_with(|| a.app.to_lowercase().cmp(&b.app.to_lowercase()))
            .then_with(|| a.index.cmp(&b.index))
    });

    Ok(windows)
}

pub fn list_windows_basic() -> Result<Vec<WindowInfo>, AppError> {
    list_windows()
}

pub fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    // SAFETY: GetForegroundWindow has no preconditions.
    let frontmost = unsafe { GetForegroundWindow() };
    if frontmost.is_null() {
        return Ok(Vec::new());
    }

    let mut front_pid = 0_u32;
    // SAFETY: frontmost is a window handle from GetForegroundWindow.
    unsafe { GetWindowThreadProcessId(frontmost, &mut front_pid as *mut u32) };

    let windows = list_windows()?;
    Ok(windows
        .into_iter()
        .filter(|window| window.pid as u32 == front_pid)
        .collect())
}

unsafe extern "system" fn enum_windows_proc(hwnd: HWND, lparam: LPARAM) -> i32 {
    // SAFETY: lparam is provided by list_windows and points to Vec<RawWindow>.
    let rows = unsafe { &mut *(lparam as *mut Vec<RawWindow>) };

    if let Some(row) = raw_window(hwnd) {
        rows.push(row);
    }
    1
}

fn raw_window(hwnd: HWND) -> Option<RawWindow> {
    if hwnd.is_null() {
        return None;
    }

    // SAFETY: hwnd is supplied by EnumWindows/GetForegroundWindow.
    unsafe {
        if IsWindowVisible(hwnd) == 0 {
            return None;
        }
        if !GetWindow(hwnd, GW_OWNER).is_null() {
            return None;
        }

        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        if GetWindowRect(hwnd, &mut rect as *mut RECT) == 0 {
            return None;
        }
        let bounds = rect_to_bounds(rect);
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return None;
        }

        let title = window_title(hwnd);

        let mut pid = 0_u32;
        GetWindowThreadProcessId(hwnd, &mut pid as *mut u32);
        if pid == 0 {
            return None;
        }

        Some(RawWindow {
            hwnd,
            pid,
            title,
            bounds,
            visible: true,
        })
    }
}

fn rect_to_bounds(rect: RECT) -> Bounds {
    let width = (rect.right - rect.left).max(0) as f64;
    let height = (rect.bottom - rect.top).max(0) as f64;
    Bounds {
        x: (rect.left as f64).max(0.0),
        y: (rect.top as f64).max(0.0),
        width,
        height,
    }
}

fn window_title(hwnd: HWND) -> String {
    // SAFETY: GetWindowTextLengthW/GetWindowTextW are valid for real HWND.
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len <= 0 {
            return String::new();
        }

        let mut buf = vec![0_u16; len as usize + 1];
        let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), len + 1);
        if copied <= 0 {
            return String::new();
        }
        OsString::from_wide(&buf[..copied as usize])
            .to_string_lossy()
            .to_string()
    }
}

fn process_name(pid: u32) -> Option<String> {
    // SAFETY: OpenProcess called with query-only rights for discovered PID.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }

        let mut buf = vec![0_u16; 32768];
        let mut size = buf.len() as u32;
        let ok = QueryFullProcessImageNameW(
            handle,
            PROCESS_NAME_WIN32,
            buf.as_mut_ptr(),
            &mut size as *mut u32,
        );
        let _ = windows_sys::Win32::Foundation::CloseHandle(handle);
        if ok == 0 || size == 0 {
            return None;
        }

        let full = OsString::from_wide(&buf[..size as usize]);
        let full = full.to_string_lossy();
        Path::new(full.as_ref())
            .file_stem()
            .map(|stem| stem.to_string_lossy().to_string())
            .filter(|name| !name.is_empty())
    }
}

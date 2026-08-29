use desktop_core::error::AppError;
use windows_sys::Win32::{
    Foundation::{CloseHandle, HWND},
    Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
    System::Threading::{
        GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
    },
    UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowThreadProcessId},
};

#[derive(Debug, Clone, Copy, Default)]
pub struct StartupPermissionRequests {
    pub accessibility_requested: bool,
    pub screen_recording_requested: bool,
}

pub fn accessibility_granted() -> bool {
    !is_uipi_blocked().unwrap_or(false)
}

pub fn screen_recording_granted() -> bool {
    true
}

pub fn ensure_screen_recording_permission() -> Result<(), AppError> {
    Ok(())
}

pub fn request_startup_permissions() -> StartupPermissionRequests {
    StartupPermissionRequests::default()
}

pub fn screen_recording_remediation() -> &'static str {
    "screen capture permissions are handled per-app on Windows"
}

pub fn accessibility_remediation() -> &'static str {
    "UI automation/input can be blocked by Windows integrity levels. If target app is elevated, run desktopctld elevated or relaunch target app non-elevated"
}

pub fn open_screen_recording_settings() -> bool {
    false
}

pub fn open_accessibility_settings() -> bool {
    false
}

fn is_uipi_blocked() -> Result<bool, AppError> {
    let current = current_process_elevated()?;
    let foreground: HWND = unsafe { GetForegroundWindow() };
    if foreground.is_null() {
        return Ok(false);
    }
    let mut pid = 0_u32;
    unsafe { GetWindowThreadProcessId(foreground, &mut pid as *mut u32) };
    if pid == 0 {
        return Ok(false);
    }
    let target = process_elevated(pid)?;
    Ok(target && !current)
}

fn current_process_elevated() -> Result<bool, AppError> {
    let process = unsafe { GetCurrentProcess() };
    token_elevated(process)
}

fn process_elevated(pid: u32) -> Result<bool, AppError> {
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if process.is_null() {
        return Ok(false);
    }
    let elevated = token_elevated(process);
    unsafe { CloseHandle(process) };
    elevated
}

fn token_elevated(process: windows_sys::Win32::Foundation::HANDLE) -> Result<bool, AppError> {
    let mut token: windows_sys::Win32::Foundation::HANDLE = std::ptr::null_mut();
    let ok = unsafe { OpenProcessToken(process, TOKEN_QUERY, &mut token as *mut _) };
    if ok == 0 || token.is_null() {
        return Ok(false);
    }
    let mut elevation = TOKEN_ELEVATION { TokenIsElevated: 0 };
    let mut returned = 0_u32;
    let info_ok = unsafe {
        GetTokenInformation(
            token,
            TokenElevation,
            &mut elevation as *mut _ as *mut _,
            std::mem::size_of::<TOKEN_ELEVATION>() as u32,
            &mut returned as *mut u32,
        )
    };
    unsafe { CloseHandle(token) };
    if info_ok == 0 {
        return Ok(false);
    }
    Ok(elevation.TokenIsElevated != 0)
}

use desktop_core::error::AppError;

use crate::platform::windowing::WindowInfo;

pub fn focus_window(_window: &WindowInfo) -> Result<(), AppError> {
    Err(AppError::backend_unavailable(
        "windows app focus backend not implemented yet",
    ))
}

pub fn hide_application(_name: &str) -> Result<&'static str, AppError> {
    Err(AppError::backend_unavailable(
        "windows app hide backend not implemented yet",
    ))
}

pub fn show_application(_name: &str) -> Result<(), AppError> {
    Err(AppError::backend_unavailable(
        "windows app show backend not implemented yet",
    ))
}

pub fn isolate_application(_name: &str) -> Result<u32, AppError> {
    Err(AppError::backend_unavailable(
        "windows app isolate backend not implemented yet",
    ))
}

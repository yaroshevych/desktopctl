use desktop_core::{error::AppError, protocol::Bounds};

use super::{FrontmostWindowContext, WindowInfo};

pub fn main_display_bounds() -> Option<Bounds> {
    None
}

pub fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    None
}

pub fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    Err(AppError::backend_unavailable(
        "windows window enumeration backend not implemented yet",
    ))
}

pub fn list_windows_basic() -> Result<Vec<WindowInfo>, AppError> {
    list_windows()
}

pub fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    Err(AppError::backend_unavailable(
        "windows frontmost app window backend not implemented yet",
    ))
}

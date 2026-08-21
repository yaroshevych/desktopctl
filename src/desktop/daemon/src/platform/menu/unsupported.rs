use super::{MenuActionResult, MenuSnapshot};
use desktop_core::error::{AppError, ErrorCode};

pub fn list(_pid: i64, _app: &str, _system: bool, _all: bool) -> Result<MenuSnapshot, AppError> {
    Err(AppError::new(
        ErrorCode::UnsupportedPlatform,
        "menu commands require macOS Accessibility API",
    ))
}

pub fn click(
    _pid: i64,
    _app: &str,
    _id: Option<&str>,
    _path: Option<&str>,
) -> Result<MenuActionResult, AppError> {
    Err(AppError::new(
        ErrorCode::UnsupportedPlatform,
        "menu commands require macOS Accessibility API",
    ))
}

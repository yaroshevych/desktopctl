use std::time::Duration;

use desktop_core::{
    error::AppError,
    protocol::{Bounds, TokenizePayload},
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    WindowMode,
    DesktopMode,
}

pub fn is_active() -> bool {
    false
}

pub fn tracked_window_bounds() -> Option<Bounds> {
    None
}

pub fn is_agent_active() -> bool {
    false
}

pub fn is_watch_mode_locked() -> bool {
    false
}

pub fn lock_watch_mode(
    _mode: WatchMode,
    _window_bounds: Option<Bounds>,
    _duration: Duration,
) -> Result<(), AppError> {
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

pub fn start_overlay() -> Result<bool, AppError> {
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

pub fn stop_overlay() -> Result<bool, AppError> {
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

pub fn watch_mode_changed(
    _mode: WatchMode,
    _window_bounds: Option<Bounds>,
) -> Result<(), AppError> {
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

#[allow(dead_code)]
pub fn agent_active_changed(_active: bool) -> Result<(), AppError> {
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

pub fn confidence_changed(_confidence: f32) -> Result<(), AppError> {
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

pub fn update_from_tokenize(_payload: &TokenizePayload) -> Result<(), AppError> {
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

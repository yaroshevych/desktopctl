use crate::error::AppError;

use super::{Automation, Point};

pub struct WindowsAutomation;

impl WindowsAutomation {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for WindowsAutomation {
    fn default() -> Self {
        Self::new()
    }
}

impl Automation for WindowsAutomation {
    fn check_accessibility_permission(&self) -> Result<(), AppError> {
        Ok(())
    }

    fn press_hotkey(&self, _hotkey: &str) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn press_enter(&self) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn press_escape(&self) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn type_text(&self, _text: &str) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn move_mouse(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn left_down(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn left_drag(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn left_up(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn left_click(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn right_down(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn right_up(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn right_click(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }

    fn scroll_wheel(&self, _dx: i32, _dy: i32) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(
            "windows input backend not implemented yet",
        ))
    }
}

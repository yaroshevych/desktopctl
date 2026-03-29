use crate::error::AppError;

use super::{Automation, Point};

pub struct StubAutomation;

impl Automation for StubAutomation {
    fn check_accessibility_permission(&self) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn press_hotkey(&self, _hotkey: &str) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn press_enter(&self) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn press_escape(&self) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn type_text(&self, _text: &str) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn move_mouse(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn left_down(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn left_drag(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn left_up(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn left_click(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn right_down(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn right_up(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn right_click(&self, _point: Point) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }

    fn scroll_wheel(&self, _dx: i32, _dy: i32) -> Result<(), AppError> {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }
}

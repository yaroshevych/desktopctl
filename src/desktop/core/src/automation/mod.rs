use std::thread;
use std::time::Duration;

use crate::error::AppError;

#[derive(Debug, Clone, Copy)]
pub struct Point {
    pub x: u32,
    pub y: u32,
}

impl Point {
    pub const fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }
}

pub trait Automation {
    fn check_accessibility_permission(&self) -> Result<(), AppError>;
    fn press_hotkey(&self, hotkey: &str) -> Result<(), AppError>;
    fn press_enter(&self) -> Result<(), AppError>;
    fn press_escape(&self) -> Result<(), AppError>;
    fn type_text(&self, text: &str) -> Result<(), AppError>;
    fn move_mouse(&self, point: Point) -> Result<(), AppError>;
    fn left_down(&self, point: Point) -> Result<(), AppError>;
    fn left_drag(&self, point: Point) -> Result<(), AppError>;
    fn left_up(&self, point: Point) -> Result<(), AppError>;
    fn left_click(&self, point: Point) -> Result<(), AppError>;
    fn right_down(&self, point: Point) -> Result<(), AppError>;
    fn right_up(&self, point: Point) -> Result<(), AppError>;
    fn right_click(&self, point: Point) -> Result<(), AppError>;
    fn scroll_wheel(&self, dx: i32, dy: i32) -> Result<(), AppError>;
    fn sleep_ms(&self, ms: u64) {
        thread::sleep(Duration::from_millis(ms));
    }
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacosAutomation;

#[cfg(not(target_os = "macos"))]
mod stub;
#[cfg(not(target_os = "macos"))]
pub use stub::StubAutomation;

pub fn new_backend() -> Result<Box<dyn Automation>, AppError> {
    #[cfg(target_os = "macos")]
    {
        return Ok(Box::new(MacosAutomation::new()));
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }
}

#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use crate::error::AppError;
use crate::protocol::Bounds;

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

#[derive(Debug, Clone)]
pub struct BackgroundInputTarget {
    pub pid: i32,
    pub window_id: u32,
    pub bounds: Bounds,
}

pub trait BackgroundInputBackend {
    fn preflight(&self, target: &BackgroundInputTarget) -> Result<(), AppError>;
    fn left_click(&self, target: &BackgroundInputTarget, point: Point) -> Result<(), AppError>;
    fn left_drag(
        &self,
        target: &BackgroundInputTarget,
        start: Point,
        end: Point,
        hold_ms: u64,
    ) -> Result<(), AppError>;
    fn scroll_wheel(
        &self,
        target: &BackgroundInputTarget,
        point: Point,
        dx: i32,
        dy: i32,
    ) -> Result<(), AppError>;
    fn type_text(&self, target: &BackgroundInputTarget, text: &str) -> Result<(), AppError>;
    fn press_hotkey(&self, target: &BackgroundInputTarget, hotkey: &str) -> Result<(), AppError>;
    fn press_enter(&self, target: &BackgroundInputTarget) -> Result<(), AppError> {
        self.press_hotkey(target, "enter")
    }
    fn press_escape(&self, target: &BackgroundInputTarget) -> Result<(), AppError> {
        self.press_hotkey(target, "escape")
    }
}

#[cfg(target_os = "macos")]
static NSEVENT_BACKGROUND_MOUSE_EVENTS: AtomicBool = AtomicBool::new(false);

#[cfg(target_os = "macos")]
pub fn set_nsevent_background_mouse_events(enabled: bool) {
    NSEVENT_BACKGROUND_MOUSE_EVENTS.store(enabled, Ordering::SeqCst);
}

#[cfg(target_os = "macos")]
pub(crate) fn nsevent_background_mouse_events_enabled() -> bool {
    NSEVENT_BACKGROUND_MOUSE_EVENTS.load(Ordering::SeqCst)
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::MacosAutomation;

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::WindowsAutomation;

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod stub;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use stub::StubAutomation;

pub fn new_backend() -> Result<Box<dyn Automation>, AppError> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(MacosAutomation::new()))
    }

    #[cfg(target_os = "windows")]
    {
        Ok(Box::new(WindowsAutomation::new()))
    }

    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        Err(AppError::backend_unavailable(format!(
            "unsupported platform: {}",
            std::env::consts::OS
        )))
    }
}

pub fn new_background_input_backend() -> Result<Box<dyn BackgroundInputBackend>, AppError> {
    #[cfg(target_os = "macos")]
    {
        Ok(Box::new(macos::MacosBackgroundInput::new()))
    }

    #[cfg(not(target_os = "macos"))]
    {
        Err(AppError::backend_unavailable(format!(
            "background input is unsupported on {}; switch to frontmost mode",
            std::env::consts::OS
        )))
    }
}

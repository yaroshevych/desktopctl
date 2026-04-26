use desktop_core::{
    error::AppError,
    protocol::{Bounds, ToggleState},
};

#[derive(Debug, Clone)]
pub struct AxElement {
    pub role: String,
    pub text: Option<String>,
    pub bounds: Bounds,
    pub ax_identifier: Option<String>,
    pub checked: Option<ToggleState>,
}

pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
    Ok(Vec::new())
}

pub fn collect_window_elements(
    _pid: i32,
    _native_window_id: u32,
    _target_window_bounds: Option<&Bounds>,
) -> Result<Vec<AxElement>, AppError> {
    Ok(Vec::new())
}

pub fn focused_frontmost_element() -> Result<Option<AxElement>, AppError> {
    Ok(None)
}

pub fn focused_frontmost_window_bounds() -> Result<Option<Bounds>, AppError> {
    Ok(None)
}

pub fn frontmost_app_pid() -> Option<i64> {
    None
}

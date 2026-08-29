#[cfg(not(target_os = "linux"))]
use crate::automation::new_backend;
use desktop_core::error::AppError;
use serde_json::{Value, json};

use crate::{daemon::window_target, platform};

#[cfg(not(target_os = "linux"))]
fn check_window_command_permission() -> Result<(), AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()
}

#[cfg(target_os = "linux")]
fn check_window_command_permission() -> Result<(), AppError> {
    Ok(())
}

pub(crate) fn list() -> Result<Value, AppError> {
    check_window_command_permission()?;
    let mut windows = window_target::list_windows()?;
    super::super::enrich_window_refs(&mut windows);
    Ok(json!({
        "windows": windows.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
    }))
}

pub(crate) fn bounds(title: String) -> Result<Value, AppError> {
    check_window_command_permission()?;
    let mut windows = window_target::list_windows()?;
    super::super::enrich_window_refs(&mut windows);
    let selected = window_target::select_window_candidate(&windows, &title)?;
    Ok(json!({
        "window": selected.as_json()
    }))
}

pub(crate) fn focus(title: String) -> Result<Value, AppError> {
    check_window_command_permission()?;
    let mut windows = window_target::list_windows()?;
    super::super::enrich_window_refs(&mut windows);
    let selected = window_target::select_window_candidate(&windows, &title)?;
    platform::apps::focus_window(selected)?;
    Ok(json!({
        "window": selected.as_json(),
        "focused": true
    }))
}

use desktop_core::{automation::new_backend, error::AppError};
use serde_json::{Value, json};

use crate::{platform, window_target};

pub(crate) fn list() -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let mut windows = window_target::list_windows()?;
    super::super::enrich_window_refs(&mut windows);
    Ok(json!({
        "windows": windows.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
    }))
}

pub(crate) fn bounds(title: String) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let mut windows = window_target::list_windows()?;
    super::super::enrich_window_refs(&mut windows);
    let selected = window_target::select_window_candidate(&windows, &title)?;
    Ok(json!({
        "window": selected.as_json()
    }))
}

pub(crate) fn focus(title: String) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let mut windows = window_target::list_windows()?;
    super::super::enrich_window_refs(&mut windows);
    let selected = window_target::select_window_candidate(&windows, &title)?;
    platform::apps::focus_window(selected)?;
    Ok(json!({
        "window": selected.as_json(),
        "focused": true
    }))
}

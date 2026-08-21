use crate::{daemon::guards, platform};
use desktop_core::error::{AppError, ErrorCode};
use serde_json::json;

pub(crate) fn list(
    active_window: bool,
    active_window_id: Option<String>,
    system: bool,
    all: bool,
) -> Result<serde_json::Value, AppError> {
    if !active_window {
        return Err(AppError::new(
            ErrorCode::ActiveWindowRequired,
            "menu commands require --active-window",
        ));
    }
    let guard = guards::prepare_active_window(true, active_window_id.as_deref())?;
    let target = guard.bound_active_window.as_ref().ok_or_else(|| {
        AppError::new(
            ErrorCode::ActiveWindowRequired,
            "menu commands require --active-window",
        )
    })?;
    let snapshot = platform::menu::list(target.pid, &target.app, system, all)?;
    Ok(json!({
        "active_window": true,
        "active_window_id": guard.bound_active_window_id,
        "app": target.app,
        "window_title": target.title,
        "pid": target.pid,
        "system": system,
        "all": all,
        "items": snapshot.items,
    }))
}

pub(crate) fn click(
    id: Option<String>,
    path: Option<String>,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<serde_json::Value, AppError> {
    if !active_window {
        return Err(AppError::new(
            ErrorCode::ActiveWindowRequired,
            "menu commands require --active-window",
        ));
    }
    if id.is_none() == path.is_none() {
        return Err(AppError::invalid_argument(
            "menu click requires exactly one of --id or path",
        ));
    }
    let guard = guards::prepare_active_window(true, active_window_id.as_deref())?;
    let target = guard.bound_active_window.as_ref().ok_or_else(|| {
        AppError::new(
            ErrorCode::ActiveWindowRequired,
            "menu commands require --active-window",
        )
    })?;
    let bound_pid = target.pid;
    let bound_ref = guard.bound_active_window_id.clone();
    // Rebuilds tree internally; verify active window and owner PID immediately before AXPress.
    if let Some(reference) = bound_ref.as_deref() {
        guards::assert_bound_window_matches(Some(reference))?;
    }
    let current = super::super::resolve_active_window_target()?;
    if current.pid != bound_pid {
        return Err(AppError::new(
            ErrorCode::MenuActionUnsupported,
            "active window owner changed before menu action",
        ));
    }
    platform::menu::click(bound_pid, &target.app, id.as_deref(), path.as_deref())?;
    Ok(
        json!({"active_window": true, "active_window_id": bound_ref, "app": target.app, "window_title": target.title, "id": id, "path": path}),
    )
}

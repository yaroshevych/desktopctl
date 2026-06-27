//! Linux (Ubuntu / GNOME Wayland) application & window-op backend.
//!
//! Window focus is performed by the companion DesktopCtl GNOME Shell extension
//! over private session D-Bus (`sh.desktopctl.Shell`), since GNOME exposes no
//! generic external window-management API.

use desktop_core::error::AppError;
use serde_json::json;
use std::process::Command as ProcessCommand;

use crate::platform::{linux::extension, windowing::WindowInfo};

const EXTENSION_REMEDIATION: &str =
    "window operations on GNOME Wayland require the DesktopCtl Shell extension";

fn extension_unavailable(op: &str) -> AppError {
    AppError::backend_unavailable(format!(
        "{op} requires the DesktopCtl GNOME Shell extension"
    ))
    .with_details(json!({
        "failure_state": "extension_unavailable",
        "remediation": EXTENSION_REMEDIATION,
    }))
}

pub fn focus_window(window: &WindowInfo) -> Result<(), AppError> {
    let id = window.window_ref.as_deref().unwrap_or(window.id.as_str());
    extension::activate_window(id)
}

pub fn hide_application(_name: &str) -> Result<&'static str, AppError> {
    Err(extension_unavailable("hide_application"))
}

pub fn show_application(name: &str) -> Result<(), AppError> {
    let needle = name.trim().to_lowercase();
    if let Some(window) = extension::list_windows()
        .unwrap_or_default()
        .into_iter()
        .find(|window| window.visible && window.app.to_lowercase().contains(&needle))
    {
        return focus_window(&window);
    }

    ProcessCommand::new(name).spawn().map_err(|err| {
        AppError::backend_unavailable(format!("failed to launch application '{name}': {err}"))
    })?;
    Ok(())
}

pub fn isolate_application(_name: &str) -> Result<u32, AppError> {
    Err(extension_unavailable("isolate_application"))
}

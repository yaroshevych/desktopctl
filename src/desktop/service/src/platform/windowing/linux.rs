//! Linux (GNOME Wayland) windowing backend.
//!
//! The complete, authoritative window list comes from the DesktopCtl GNOME
//! Shell extension via private D-Bus (DesktopCtl-76l): titles, app ids, PIDs,
//! bounds, workspace, stacking, focus, and stable opaque window_refs mapped to
//! Mutter objects. This module is a thin adapter that drives the blocking
//! D-Bus client in `platform::linux::extension`. When the extension is absent
//! the client surfaces the `extension_unavailable` failure state.
//!
//! AT-SPI top-levels remain a possible best-effort fallback (DesktopCtl-76l);
//! not yet wired in here.

use desktop_core::{error::AppError, protocol::Bounds};

use super::{FrontmostWindowContext, WindowInfo};
use crate::platform::linux::extension;

pub fn main_display_bounds() -> Option<Bounds> {
    // TODO(DesktopCtl): derive from monitor metadata (extension or portal).
    // Best-effort: unknown for now.
    None
}

pub fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    let win = extension::active_window().ok().flatten()?;
    Some(FrontmostWindowContext {
        app: if win.app.is_empty() {
            None
        } else {
            Some(win.app.clone())
        },
        bounds: Some(win.bounds),
    })
}

pub fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    extension::list_windows()
}

pub fn list_windows_basic() -> Result<Vec<WindowInfo>, AppError> {
    list_windows()
}

pub fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    let windows = list_windows()?;

    // Determine the active window's app: prefer the focused window in the list,
    // else fall back to GetActiveWindow.
    let active_app = windows
        .iter()
        .find(|w| w.frontmost)
        .map(|w| w.app.clone())
        .or_else(|| extension::active_window().ok().flatten().map(|w| w.app));

    let Some(app) = active_app.filter(|a| !a.is_empty()) else {
        return Ok(Vec::new());
    };

    Ok(windows.into_iter().filter(|w| w.app == app).collect())
}

pub fn list_windows_for_pid(pid: i64) -> Result<Vec<WindowInfo>, AppError> {
    let windows = list_windows()?;
    Ok(windows.into_iter().filter(|w| w.pid == pid).collect())
}

//! Daemon-side D-Bus client for the DesktopCtl GNOME Shell extension
//! (`sh.desktopctl.Shell`) — window enumeration and ops (DesktopCtl-cl0).
//!
//! The daemon is synchronous, so this uses zbus's BLOCKING API. The
//! `#[zbus::proxy]` macro generates a `ShellProxyBlocking` variant we drive
//! directly. When the extension's bus name is not present on the session bus,
//! callers surface the `extension_unavailable` failure state.

use std::collections::HashMap;

use desktop_core::{error::AppError, protocol::Bounds};
use serde_json::json;
use zbus::zvariant::OwnedValue;

use crate::platform::windowing::WindowInfo;

/// Opaque window dict as returned by the extension (`a{sv}` entries).
pub type WindowDict = HashMap<String, OwnedValue>;

const EXTENSION_REMEDIATION: &str =
    "install and enable the DesktopCtl GNOME Shell extension for window enumeration";

/// Build the `extension_unavailable` error. Used both when the bus name is
/// absent and when a D-Bus call fails because the service is gone.
pub fn extension_unavailable() -> AppError {
    AppError::backend_unavailable(
        "window enumeration requires the DesktopCtl GNOME Shell extension",
    )
    .with_details(json!({
        "failure_state": "extension_unavailable",
        "remediation": EXTENSION_REMEDIATION,
    }))
}

/// Map an arbitrary D-Bus / connection failure to an `AppError`. Connection or
/// name-resolution problems map to `extension_unavailable`; everything else is
/// surfaced as a generic backend error carrying the underlying detail.
fn map_zbus_err(err: zbus::Error) -> AppError {
    match &err {
        zbus::Error::Address(_) | zbus::Error::InputOutput(_) | zbus::Error::Handshake(_) => {
            extension_unavailable()
        }
        zbus::Error::MethodError(name, _, _) => {
            let n = name.as_str();
            if n.contains("ServiceUnknown")
                || n.contains("NameHasNoOwner")
                || n.contains("NoReply")
                || n.contains("AccessDenied")
            {
                extension_unavailable()
            } else {
                AppError::backend_unavailable(format!("extension call failed: {err}"))
            }
        }
        _ => AppError::backend_unavailable(format!("extension call failed: {err}")),
    }
}

#[zbus::proxy(
    interface = "sh.desktopctl.Shell",
    default_service = "sh.desktopctl.Shell",
    default_path = "/sh/desktopctl/Shell"
)]
pub trait Shell {
    /// Enumerate all windows: each entry is an `a{sv}` dict.
    fn list_windows(&self) -> zbus::Result<Vec<WindowDict>>;

    /// The currently active window, as an `a{sv}` dict.
    fn get_active_window(&self) -> zbus::Result<WindowDict>;

    fn activate_window(&self, id: &str) -> zbus::Result<bool>;

    fn close_window(&self, id: &str) -> zbus::Result<bool>;

    #[zbus(name = "MoveResizeWindow")]
    fn move_resize_window(&self, id: &str, x: i32, y: i32, w: u32, h: u32) -> zbus::Result<bool>;

    fn set_workspace(&self, id: &str, ws: u32) -> zbus::Result<bool>;
}

/// Connect to the extension's private session-bus interface, returning a
/// blocking proxy. Returns `extension_unavailable` when the session bus or the
/// extension service cannot be reached.
pub fn connect() -> Result<ShellProxyBlocking<'static>, AppError> {
    let conn = zbus::blocking::Connection::session().map_err(map_zbus_err)?;
    ShellProxyBlocking::new(&conn).map_err(map_zbus_err)
}

// --- Conversion helpers -----------------------------------------------------

fn get_string(dict: &WindowDict, key: &str) -> Option<String> {
    dict.get(key).and_then(|v| String::try_from(v.clone()).ok())
}

/// Read a numeric value, accepting any of the integer/float D-Bus types the
/// extension might emit (bounds are int32, pids int32, etc.).
fn get_f64(dict: &WindowDict, key: &str) -> Option<f64> {
    let v = dict.get(key)?;
    if let Ok(n) = f64::try_from(v.clone()) {
        return Some(n);
    }
    if let Ok(n) = i64::try_from(v.clone()) {
        return Some(n as f64);
    }
    if let Ok(n) = u64::try_from(v.clone()) {
        return Some(n as f64);
    }
    if let Ok(n) = i32::try_from(v.clone()) {
        return Some(n as f64);
    }
    if let Ok(n) = u32::try_from(v.clone()) {
        return Some(n as f64);
    }
    None
}

fn get_i64(dict: &WindowDict, key: &str) -> Option<i64> {
    get_f64(dict, key).map(|n| n as i64)
}

fn get_bool(dict: &WindowDict, key: &str) -> Option<bool> {
    dict.get(key).and_then(|v| bool::try_from(v.clone()).ok())
}

fn get_string_list(dict: &WindowDict, key: &str) -> Vec<String> {
    dict.get(key)
        .and_then(|v| Vec::<String>::try_from(v.clone()).ok())
        .unwrap_or_default()
}

/// Extract window bounds. Accepts either a nested `bounds` dict (`a{sv}` with
/// x/y/width/height) or flat top-level `x`/`y`/`width`/`height` keys.
pub fn bounds_from_dict(dict: &WindowDict) -> Bounds {
    if let Some(nested) = dict
        .get("bounds")
        .and_then(|v| WindowDict::try_from(v.clone()).ok())
    {
        return Bounds {
            x: get_f64(&nested, "x").unwrap_or(0.0),
            y: get_f64(&nested, "y").unwrap_or(0.0),
            width: get_f64(&nested, "width")
                .or_else(|| get_f64(&nested, "w"))
                .unwrap_or(0.0),
            height: get_f64(&nested, "height")
                .or_else(|| get_f64(&nested, "h"))
                .unwrap_or(0.0),
        };
    }

    Bounds {
        x: get_f64(dict, "x").unwrap_or(0.0),
        y: get_f64(dict, "y").unwrap_or(0.0),
        width: get_f64(dict, "width")
            .or_else(|| get_f64(dict, "w"))
            .unwrap_or(0.0),
        height: get_f64(dict, "height")
            .or_else(|| get_f64(dict, "h"))
            .unwrap_or(0.0),
    }
}

/// Convert one extension window dict into a [`WindowInfo`].
///
/// The extension's opaque `id` becomes our `window_ref` (the public,
/// Mutter-mapped identity). Returns `None` only when there is no usable id.
pub fn window_info_from_dict(dict: &WindowDict) -> Option<WindowInfo> {
    let id = get_string(dict, "id")?;

    let app = get_string(dict, "app_id")
        .or_else(|| get_string(dict, "app"))
        .unwrap_or_default();
    let title = get_string(dict, "title").unwrap_or_default();
    let pid = get_i64(dict, "pid").unwrap_or(0);
    let index = get_i64(dict, "stacking")
        .or_else(|| get_i64(dict, "index"))
        .unwrap_or(0)
        .max(0) as u32;

    let frontmost = get_bool(dict, "focused")
        .or_else(|| get_bool(dict, "frontmost"))
        .unwrap_or(false);

    // Window states (`as`): minimized/maximized/fullscreen/above/modal.
    let states = get_string_list(dict, "states");
    let minimized = states.iter().any(|s| s == "minimized");
    let visible = get_bool(dict, "visible").unwrap_or(!minimized);
    let modal = get_bool(dict, "modal").or_else(|| {
        if states.iter().any(|s| s == "modal") {
            Some(true)
        } else {
            None
        }
    });
    let parent_id = get_string(dict, "parent_id");

    Some(WindowInfo {
        id: id.clone(),
        window_ref: Some(id),
        parent_id,
        pid,
        index,
        app,
        title,
        bounds: bounds_from_dict(dict),
        frontmost,
        visible,
        modal,
    })
}

/// Convenience: fetch and convert the full window list.
pub fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    let proxy = connect()?;
    let dicts = proxy.list_windows().map_err(map_zbus_err)?;
    Ok(dicts.iter().filter_map(window_info_from_dict).collect())
}

/// Convenience: fetch and convert the active window, if any.
pub fn active_window() -> Result<Option<WindowInfo>, AppError> {
    let proxy = connect()?;
    let dict = proxy.get_active_window().map_err(map_zbus_err)?;
    Ok(window_info_from_dict(&dict))
}

pub fn activate_window(id: &str) -> Result<(), AppError> {
    let proxy = connect()?;
    if proxy.activate_window(id).map_err(map_zbus_err)? {
        Ok(())
    } else {
        Err(AppError::target_not_found(format!(
            "GNOME Shell extension could not find window {id}"
        )))
    }
}

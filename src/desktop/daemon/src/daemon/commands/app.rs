use std::process::Command as ProcessCommand;

use desktop_core::error::AppError;
use serde_json::{Value, json};

use crate::{platform, trace};

fn try_resolve_window_id_for_app(app_name: &str, frontmost_only: bool) -> Option<String> {
    let mut windows = if frontmost_only {
        super::super::window_target::list_frontmost_app_windows().ok()?
    } else {
        super::super::window_target::list_windows().ok()?
    };
    super::super::enrich_window_refs(&mut windows);
    select_app_window_id(&windows, app_name, frontmost_only)
}

fn select_app_window_id(
    windows: &[platform::windowing::WindowInfo],
    app_name: &str,
    allow_fallback: bool,
) -> Option<String> {
    let needle = app_name.trim().to_lowercase();
    windows
        .iter()
        .find(|window| {
            window.visible
                && window.bounds.width > 8.0
                && window.bounds.height > 8.0
                && window.app.to_lowercase().contains(&needle)
        })
        .or_else(|| {
            if allow_fallback {
                windows.iter().find(|window| {
                    window.visible && window.bounds.width > 8.0 && window.bounds.height > 8.0
                })
            } else {
                None
            }
        })
        .map(|window| {
            window
                .window_ref
                .clone()
                .unwrap_or_else(|| window.id.clone())
        })
}

pub(crate) fn hide(name: String) -> Result<Value, AppError> {
    trace::log(format!("app_hide:start name={name}"));
    let state = platform::apps::hide_application(&name)?;
    trace::log(format!("app_hide:ok name={name} state={state}"));
    Ok(json!({ "app": name, "state": state }))
}

pub(crate) fn show(name: String) -> Result<Value, AppError> {
    trace::log(format!("app_show:start name={name}"));
    platform::apps::show_application(&name)?;
    trace::log(format!("app_show:ok name={name}"));
    let window_id = try_resolve_window_id_for_app(&name, true);
    Ok(json!({ "app": name, "state": "shown", "window_id": window_id }))
}

pub(crate) fn isolate(name: String) -> Result<Value, AppError> {
    trace::log(format!("app_isolate:start name={name}"));
    let hidden = platform::apps::isolate_application(&name)?;
    let _ = super::super::wait_for_open_app(&name, 6_000);
    trace::log(format!("app_isolate:ok name={name} hidden={hidden}"));
    let window_id = try_resolve_window_id_for_app(&name, true);
    Ok(json!({ "app": name, "state": "isolated", "hidden_apps": hidden, "window_id": window_id }))
}

pub(crate) fn open(
    name: String,
    args: Vec<String>,
    wait: bool,
    timeout_ms: Option<u64>,
    background: bool,
) -> Result<Value, AppError> {
    #[cfg(target_os = "windows")]
    {
        let _ = background;
        let mut cmd = ProcessCommand::new(&name);
        if !args.is_empty() {
            cmd.args(&args);
        }
        cmd.spawn().map_err(|err| {
            AppError::backend_unavailable(format!("failed to launch application '{name}': {err}"))
        })?;

        if wait {
            super::super::wait_for_open_app(&name, timeout_ms.unwrap_or(8_000))?;
        }
        let window_id = try_resolve_window_id_for_app(&name, !background);
        return Ok(json!({ "window_id": window_id }));
    }

    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = ProcessCommand::new("open");
        if background {
            cmd.arg("-g");
        }
        cmd.arg("-a").arg(&name);
        if !args.is_empty() {
            cmd.args(&args);
        }

        let output = cmd.output().map_err(|err| {
            AppError::backend_unavailable(format!("failed to invoke open: {err}"))
        })?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(AppError::internal(stderr));
        }

        if !background {
            let escaped = name
                .replace('\n', "")
                .replace('\r', "")
                .replace('\\', "\\\\")
                .replace('"', "\\\"");
            let script = format!(r#"tell application "{escaped}" to activate"#);
            let activate = ProcessCommand::new("osascript")
                .arg("-e")
                .arg(script)
                .output()
                .map_err(|err| {
                    AppError::backend_unavailable(format!("failed to run osascript: {err}"))
                })?;
            if !activate.status.success() {
                let stderr = String::from_utf8_lossy(&activate.stderr).trim().to_string();
                return Err(AppError::internal(stderr));
            }
        }

        if wait {
            super::super::wait_for_open_app(&name, timeout_ms.unwrap_or(8_000))?;
        }
        let window_id = try_resolve_window_id_for_app(&name, !background);
        Ok(json!({ "window_id": window_id }))
    }
}

#[cfg(test)]
mod tests {
    use desktop_core::protocol::Bounds;

    use super::select_app_window_id;
    use crate::platform::windowing::WindowInfo;

    fn window(app: &str, id: &str) -> WindowInfo {
        WindowInfo {
            id: id.to_string(),
            window_ref: Some(format!("{app}_{id}")),
            parent_id: None,
            pid: 1,
            index: 0,
            app: app.to_string(),
            title: String::new(),
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            frontmost: false,
            visible: true,
            modal: None,
        }
    }

    #[test]
    fn background_open_window_resolution_does_not_fallback_to_unrelated_app() {
        let windows = vec![window("Ghostty", "410026")];

        assert_eq!(select_app_window_id(&windows, "Notes", false), None);
    }

    #[test]
    fn background_open_window_resolution_matches_target_app_from_all_windows() {
        let windows = vec![window("Ghostty", "410026"), window("Notes", "9123")];

        assert_eq!(
            select_app_window_id(&windows, "Notes", false),
            Some("Notes_9123".to_string())
        );
    }
}

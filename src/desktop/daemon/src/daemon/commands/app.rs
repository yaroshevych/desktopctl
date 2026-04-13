use std::process::Command as ProcessCommand;

use desktop_core::error::AppError;
use serde_json::{Value, json};

use crate::{platform, trace};

fn try_resolve_frontmost_window_id_for_app(app_name: &str) -> Option<String> {
    let mut windows = super::super::window_target::list_frontmost_app_windows().ok()?;
    super::super::enrich_window_refs(&mut windows);
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
            windows.iter().find(|window| {
                window.visible && window.bounds.width > 8.0 && window.bounds.height > 8.0
            })
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
    let window_id = try_resolve_frontmost_window_id_for_app(&name);
    Ok(json!({ "app": name, "state": "shown", "window_id": window_id }))
}

pub(crate) fn isolate(name: String) -> Result<Value, AppError> {
    trace::log(format!("app_isolate:start name={name}"));
    let hidden = platform::apps::isolate_application(&name)?;
    let _ = super::super::wait_for_open_app(&name, 6_000);
    trace::log(format!("app_isolate:ok name={name} hidden={hidden}"));
    let window_id = try_resolve_frontmost_window_id_for_app(&name);
    Ok(json!({ "app": name, "state": "isolated", "hidden_apps": hidden, "window_id": window_id }))
}

pub(crate) fn open(
    name: String,
    args: Vec<String>,
    wait: bool,
    timeout_ms: Option<u64>,
) -> Result<Value, AppError> {
    #[cfg(target_os = "windows")]
    {
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
        let window_id = try_resolve_frontmost_window_id_for_app(&name);
        return Ok(json!({ "window_id": window_id }));
    }

    #[cfg(not(target_os = "windows"))]
    {
        let mut cmd = ProcessCommand::new("open");
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

        if wait {
            super::super::wait_for_open_app(&name, timeout_ms.unwrap_or(8_000))?;
        }
        let window_id = try_resolve_frontmost_window_id_for_app(&name);
        Ok(json!({ "window_id": window_id }))
    }
}

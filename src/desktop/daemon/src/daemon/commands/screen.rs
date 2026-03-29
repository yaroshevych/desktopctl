use std::path::PathBuf;

use desktop_core::{automation::new_backend, error::AppError, protocol::Bounds};
use serde_json::{Value, json};

#[cfg(target_os = "macos")]
use crate::overlay;
use crate::{permissions, platform, trace, vision, window_target};

pub(crate) fn screenshot(
    out_path: Option<String>,
    overlay: bool,
    active_window: bool,
    active_window_id: Option<String>,
    region: Option<Bounds>,
) -> Result<Value, AppError> {
    trace::log("execute:screen_capture:start");
    permissions::ensure_screen_recording_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let capture_bounds = if active_window {
        let base = if let Some(reference) = guard.bound_active_window_id.as_deref() {
            super::super::assert_active_window_id_matches(reference)?.bounds
        } else {
            super::super::resolve_active_window_target()?.bounds
        };
        Some(super::super::resolve_capture_region_bounds(
            base,
            region.as_ref(),
        )?)
    } else if region.is_some() {
        let base = window_target::main_display_bounds().ok_or_else(|| {
            AppError::target_not_found("display bounds unavailable for screenshot --region")
        })?;
        Some(super::super::resolve_capture_region_bounds(
            base,
            region.as_ref(),
        )?)
    } else {
        None
    };
    let capture_out_path: Option<PathBuf> = out_path
        .map(Into::into)
        .or_else(|| Some(platform::capture::default_capture_path()));
    let capture = if let Some(bounds) = capture_bounds.clone() {
        platform::capture::capture_bounds(capture_out_path.clone(), bounds, None, true)?
    } else {
        platform::capture::capture_display(capture_out_path)?
    };
    let overlay_path = if overlay {
        let path = super::super::write_capture_overlay(&capture)?;
        Some(path.display().to_string())
    } else {
        None
    };
    trace::log(format!(
        "execute:screen_capture:ok snapshot_id={} event_count={}",
        capture.snapshot.snapshot_id,
        capture.event_ids.len()
    ));
    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "timestamp": capture.snapshot.timestamp,
        "path": capture
            .image_path
            .as_ref()
            .map(|path| path.display().to_string()),
        "overlay_path": overlay_path,
        "capture_scope": if active_window {
            "active_window"
        } else if region.is_some() {
            "region"
        } else {
            "display"
        },
        "window_bounds": capture_bounds,
        "display": capture.snapshot.display,
        "focused_app": capture.snapshot.focused_app,
        "event_ids": capture.event_ids
    }))
}

pub(crate) fn tokenize(
    overlay_out_path: Option<String>,
    window_query: Option<String>,
    screenshot_path: Option<String>,
    active_window: bool,
    active_window_id: Option<String>,
    region: Option<Bounds>,
    overlay_token_updates_enabled: bool,
) -> Result<Value, AppError> {
    trace::log("execute:screen_tokenize:start");
    let screenshot_mode = screenshot_path.is_some();
    let payload = if let Some(path_raw) = screenshot_path {
        if window_query.is_some() {
            return Err(AppError::invalid_argument(
                "--window-query cannot be combined with --screenshot for screen tokenize",
            ));
        }
        if active_window {
            return Err(AppError::invalid_argument(
                "--active-window cannot be combined with --screenshot for screen tokenize",
            ));
        }
        let screenshot = PathBuf::from(path_raw);
        if !screenshot.exists() {
            return Err(AppError::invalid_argument(format!(
                "screenshot file does not exist: {}",
                screenshot.display()
            )));
        }
        vision::pipeline::tokenize_screenshot(&screenshot, None, region.as_ref())?
    } else {
        permissions::ensure_screen_recording_permission()?;
        let backend = new_backend()?;
        backend.check_accessibility_permission()?;
        let guard = super::super::guards::prepare_active_window(
            active_window,
            active_window_id.as_deref(),
        )?;
        if active_window {
            if window_query.is_some() {
                return Err(AppError::invalid_argument(
                    "--active-window cannot be combined with --window-query for screen tokenize",
                ));
            }
            let frontmost_window = if let Some(reference) = guard.bound_active_window_id.as_deref()
            {
                super::super::assert_active_window_id_matches(reference)?
            } else {
                super::super::resolve_active_window_target()?
            };
            let bounds = frontmost_window.bounds.clone();
            let bounds = super::super::resolve_tokenize_region_bounds(bounds, region.as_ref())?;
            let app = Some(frontmost_window.app.clone());
            let title = Some(frontmost_window.title.clone())
                .or_else(|| app.clone())
                .unwrap_or_else(|| "active_window".to_string());
            let window_query = frontmost_window.id.clone();
            let mut payload =
                vision::pipeline::tokenize_window(vision::pipeline::TokenizeWindowMeta {
                    id: window_query,
                    title,
                    app,
                    bounds,
                })?;
            if let Some(first) = payload.windows.first_mut() {
                first.window_ref = frontmost_window.window_ref.clone();
            }
            payload
        } else if window_query.is_none() {
            let overlay_window_bounds = {
                #[cfg(target_os = "macos")]
                {
                    if overlay::is_active() {
                        overlay::tracked_window_bounds()
                    } else {
                        None
                    }
                }
                #[cfg(not(target_os = "macos"))]
                {
                    None
                }
            };
            if let Some(bounds) = overlay_window_bounds {
                let bounds = super::super::resolve_tokenize_region_bounds(bounds, region.as_ref())?;
                let window_meta = vision::pipeline::TokenizeWindowMeta {
                    id: "frontmost:1".to_string(),
                    title: "active_window".to_string(),
                    app: None,
                    bounds,
                };
                vision::pipeline::tokenize_window(window_meta)?
            } else if let Some(bounds) = window_target::frontmost_window_bounds() {
                let bounds = super::super::resolve_tokenize_region_bounds(bounds, region.as_ref())?;
                let app = window_target::frontmost_app_name();
                let title = app.clone().unwrap_or_else(|| "active_window".to_string());
                let window_meta = vision::pipeline::TokenizeWindowMeta {
                    id: "frontmost:1".to_string(),
                    title,
                    app,
                    bounds,
                };
                vision::pipeline::tokenize_window(window_meta)?
            } else {
                let mut windows = window_target::list_windows()?;
                super::super::enrich_window_refs(&mut windows);
                let target = window_target::resolve_tokenize_window_target(&windows, None)?;
                let bounds = super::super::resolve_tokenize_region_bounds(
                    target.bounds.clone(),
                    region.as_ref(),
                )?;
                let window_meta = vision::pipeline::TokenizeWindowMeta {
                    id: target.id.clone(),
                    title: target.title.clone(),
                    app: Some(target.app.clone()),
                    bounds,
                };
                let mut payload = vision::pipeline::tokenize_window(window_meta)?;
                if let Some(first) = payload.windows.first_mut() {
                    first.window_ref = target.window_ref.clone();
                }
                payload
            }
        } else {
            let mut windows = window_target::list_windows()?;
            super::super::enrich_window_refs(&mut windows);
            let target =
                window_target::resolve_tokenize_window_target(&windows, window_query.as_deref())?;
            let bounds = super::super::resolve_tokenize_region_bounds(
                target.bounds.clone(),
                region.as_ref(),
            )?;
            let window_meta = vision::pipeline::TokenizeWindowMeta {
                id: target.id.clone(),
                title: target.title.clone(),
                app: Some(target.app.clone()),
                bounds,
            };
            let mut payload = vision::pipeline::tokenize_window(window_meta)?;
            if let Some(first) = payload.windows.first_mut() {
                first.window_ref = target.window_ref.clone();
            }
            payload
        }
    };
    let mut payload = payload;
    if !screenshot_mode {
        super::super::attach_window_ref_to_payload(&mut payload);
        super::super::backfill_tokenize_window_positions(&mut payload);
    }
    if let Some(path_raw) = overlay_out_path {
        let overlay_path = PathBuf::from(path_raw);
        vision::pipeline::write_tokenize_overlay(&payload, &overlay_path)?;
        trace::log(format!(
            "execute:screen_tokenize:overlay_ok path={}",
            overlay_path.display()
        ));
    }
    #[cfg(target_os = "macos")]
    if overlay_token_updates_enabled {
        if let Err(err) = overlay::update_from_tokenize(&payload) {
            trace::log(format!("execute:screen_tokenize:overlay_update_warn {err}"));
        }
    } else {
        trace::log("execute:screen_tokenize:overlay_update_skipped transient_privacy");
    }
    let element_count: usize = payload.windows.iter().map(|w| w.elements.len()).sum();
    trace::log(format!(
        "execute:screen_tokenize:ok snapshot_id={} elements={}",
        payload.snapshot_id, element_count
    ));
    let mut value = serde_json::to_value(payload)
        .map_err(|err| AppError::internal(format!("failed to encode token payload: {err}")))?;
    super::super::append_tokenize_text_dump(&mut value);
    super::super::remap_tokenize_window_id_field(&mut value);
    Ok(value)
}

pub(crate) fn find_text(text: String, all: bool) -> Result<Value, AppError> {
    permissions::ensure_screen_recording_permission()?;
    super::super::find_text_targets(&text, all)
}

pub(crate) fn wait_text(
    text: String,
    timeout_ms: u64,
    interval_ms: u64,
    disappear: bool,
) -> Result<Value, AppError> {
    super::super::wait_for_text(&text, timeout_ms, interval_ms, disappear)
}

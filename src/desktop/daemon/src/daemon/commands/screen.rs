use std::{path::PathBuf, sync::mpsc, time::Instant};

use desktop_core::{automation::new_backend, error::AppError, protocol::Bounds};
use serde_json::{Value, json};

use crate::{
    daemon::{window_refs, window_target},
    platform::{self, permissions},
    trace, vision,
};

mod overlay_bridge;

fn resolve_active_window_from_app_windows(
    reference: &str,
    app_windows: &[platform::windowing::WindowInfo],
) -> Option<platform::windowing::WindowInfo> {
    let reference = reference.trim();
    if reference.is_empty() {
        return None;
    }
    let visible_match = |window: &&platform::windowing::WindowInfo| {
        window.visible && window.bounds.width > 8.0 && window.bounds.height > 8.0
    };
    let is_desktopctl_window = |window: &&platform::windowing::WindowInfo| {
        let app_lc = window.app.trim().to_ascii_lowercase();
        app_lc.contains("desktopctl")
    };
    if let Some((expected_pid, expected_window_id)) = window_refs::resolve_native_for_ref(reference)
    {
        if let Some(found) = app_windows
            .iter()
            .find(|window| {
                visible_match(window)
                    && window.pid == expected_pid
                    && window.id == expected_window_id
                    && !is_desktopctl_window(window)
            })
            .cloned()
        {
            return Some(found);
        }
    }
    None
}

fn bounds_match_with_tolerance(
    a: &desktop_core::protocol::Bounds,
    b: &desktop_core::protocol::Bounds,
    tolerance_px: f64,
) -> bool {
    (a.x - b.x).abs() <= tolerance_px
        && (a.y - b.y).abs() <= tolerance_px
        && (a.width - b.width).abs() <= tolerance_px
        && (a.height - b.height).abs() <= tolerance_px
}

fn mark_frontmost_if_current(
    mut window: platform::windowing::WindowInfo,
) -> platform::windowing::WindowInfo {
    if window.frontmost {
        return window;
    }
    let Ok(mut frontmost_windows) = window_target::list_frontmost_app_windows() else {
        return window;
    };
    super::super::enrich_window_refs(&mut frontmost_windows);
    if frontmost_windows
        .iter()
        .any(|frontmost| frontmost.pid == window.pid && frontmost.id == window.id)
    {
        trace::log("active_window_tokenize:frontmost_confirmed_for_explicit_id");
        window.frontmost = true;
    }
    window
}

fn tokenize_meta_for_window(
    window: &platform::windowing::WindowInfo,
    bounds: Bounds,
    require_background_capture: bool,
) -> Result<vision::pipeline::TokenizeWindowMeta, AppError> {
    let background_capture = should_use_background_capture(window, require_background_capture);
    let native_window_id = background_capture
        .then(|| super::super::explicit_background_capture_window_id(window))
        .transpose()?;
    Ok(vision::pipeline::TokenizeWindowMeta {
        id: window.id.clone(),
        title: window.title.clone(),
        app: Some(window.app.clone()),
        bounds,
        pid: background_capture
            .then(|| i32::try_from(window.pid).ok())
            .flatten(),
        native_window_id,
        capture_bounds: native_window_id.map(|_| window.bounds.clone()),
    })
}

fn should_use_background_capture(
    window: &platform::windowing::WindowInfo,
    require_background_capture: bool,
) -> bool {
    require_background_capture && !window.frontmost
}

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
    let mut background_capture_target: Option<(platform::windowing::WindowInfo, Bounds)> = None;
    let capture_bounds = if active_window {
        if let Some(reference) = guard.bound_active_window_id.as_deref() {
            let target = super::super::assert_active_window_id_matches(reference)?;
            let bounds = super::super::resolve_capture_region_bounds(
                target.bounds.clone(),
                region.as_ref(),
            )?;
            background_capture_target = Some((target, bounds.clone()));
            Some(bounds)
        } else {
            let base = super::super::resolve_active_window_target()?.bounds;
            Some(super::super::resolve_capture_region_bounds(
                base,
                region.as_ref(),
            )?)
        }
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
    capture_screenshot_response(
        capture_out_path_from(out_path),
        overlay,
        if active_window {
            "active_window"
        } else if region.is_some() {
            "region"
        } else {
            "display"
        },
        capture_bounds.clone(),
        background_capture_target,
    )
}

fn capture_out_path_from(out_path: Option<String>) -> Option<PathBuf> {
    out_path
        .map(Into::into)
        .or_else(|| Some(platform::capture::default_capture_path()))
}

fn capture_screenshot_response(
    capture_out_path: Option<PathBuf>,
    overlay: bool,
    capture_scope: &'static str,
    capture_bounds: Option<Bounds>,
    background_capture_target: Option<(platform::windowing::WindowInfo, Bounds)>,
) -> Result<Value, AppError> {
    let capture = if let Some((target, bounds)) = background_capture_target {
        let native_window_id = super::super::explicit_background_capture_window_id(&target)?;
        platform::capture::capture_window(
            capture_out_path.clone(),
            native_window_id,
            target.bounds,
            Some(bounds),
            Some(target.app),
        )?
    } else if let Some(bounds) = capture_bounds.clone() {
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
        "capture_scope": capture_scope,
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
    journal: bool,
    list_all_windows: bool,
    active_window: bool,
    active_window_id: Option<String>,
    region: Option<Bounds>,
    overlay_token_updates_enabled: bool,
) -> Result<Value, AppError> {
    trace::log("execute:screen_tokenize:start");
    let total_started = Instant::now();
    let mut stage_started = Instant::now();
    let mut stage_timings: Vec<(&'static str, u128)> = Vec::new();
    macro_rules! stage_done {
        ($label:expr) => {{
            stage_timings.push(($label, stage_started.elapsed().as_millis()));
            stage_started = Instant::now();
        }};
    }
    let screenshot_mode = screenshot_path.is_some();
    let mut bound_hint_active_window_id: Option<String> = None;
    let mut hint_snapshot_prefetch_rx: Option<
        mpsc::Receiver<Option<super::super::TokenizeHintSnapshot>>,
    > = None;
    let mut all_windows_prefetch_handle: Option<
        std::thread::JoinHandle<Result<Vec<platform::windowing::WindowInfo>, AppError>>,
    > = None;
    let mut active_window_prefetched_windows: Option<Vec<platform::windowing::WindowInfo>> = None;
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
        if list_all_windows {
            return Err(AppError::invalid_argument(
                "--list-windows cannot be combined with --screenshot for screen tokenize",
            ));
        }
        let screenshot = PathBuf::from(path_raw);
        if !screenshot.exists() {
            return Err(AppError::invalid_argument(format!(
                "screenshot file does not exist: {}",
                screenshot.display()
            )));
        }
        stage_done!("screenshot_validate");
        let payload = vision::pipeline::tokenize_screenshot(&screenshot, None, region.as_ref())?;
        stage_done!("screenshot_tokenize");
        payload
    } else {
        permissions::ensure_screen_recording_permission()?;
        stage_done!("screen_recording_permission");
        let backend = new_backend()?;
        stage_done!("automation_backend_init");
        backend.check_accessibility_permission()?;
        stage_done!("accessibility_permission");
        if active_window_id.is_some() && !active_window {
            return Err(AppError::invalid_argument(
                "active window id requires --active-window",
            ));
        }
        if list_all_windows {
            all_windows_prefetch_handle = Some(std::thread::spawn(move || {
                let mut windows = window_target::list_windows()?;
                super::super::enrich_window_refs(&mut windows);
                Ok(windows)
            }));
        }
        if active_window {
            if let Some(reference) = active_window_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                if let Ok(mut windows) = window_target::list_windows() {
                    super::super::enrich_window_refs(&mut windows);
                    active_window_prefetched_windows = Some(windows.clone());
                    let reference = reference.to_string();
                    let (reply_tx, reply_rx) =
                        mpsc::channel::<Option<super::super::TokenizeHintSnapshot>>();
                    std::thread::spawn(move || {
                        let snapshot =
                            super::super::collect_tokenize_new_window_hint_snapshot_from_windows(
                                &reference, windows,
                            );
                        let _ = reply_tx.send(snapshot);
                    });
                    hint_snapshot_prefetch_rx = Some(reply_rx);
                }
            }
        }
        if active_window {
            if window_query.is_some() {
                return Err(AppError::invalid_argument(
                    "--active-window cannot be combined with --window-query for screen tokenize",
                ));
            }
            let (frontmost_window, mut payload) = if let Some(reference) = active_window_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                let mut speculative_bounds: Option<desktop_core::protocol::Bounds> = None;
                let mut speculative_handle: Option<
                    std::thread::JoinHandle<
                        Result<desktop_core::protocol::TokenizePayload, AppError>,
                    >,
                > = None;
                let reference_owned = reference.to_string();
                let prefetched_match = active_window_prefetched_windows
                    .as_deref()
                    .and_then(|windows| {
                        resolve_active_window_from_app_windows(&reference_owned, windows)
                    })
                    .map(mark_frontmost_if_current);

                if let Some(prefetched) = prefetched_match.as_ref() {
                    if let Ok(bounds) = super::super::resolve_tokenize_region_bounds(
                        prefetched.bounds.clone(),
                        region.as_ref(),
                    ) {
                        trace::log("active_window_id_match:prefetched_windows_hit");
                        match tokenize_meta_for_window(prefetched, bounds.clone(), true) {
                            Ok(meta) => {
                                speculative_bounds = Some(bounds);
                                speculative_handle = Some(std::thread::spawn(move || {
                                    vision::pipeline::tokenize_window(meta)
                                }));
                                trace::log(
                                    "active_window_tokenize:speculative_start source=prefetched",
                                );
                            }
                            Err(err) => trace::log(format!(
                                "active_window_tokenize:speculative_skip {}",
                                err.message
                            )),
                        }
                    }
                }

                let strict_window = if let Some(prefetched) = prefetched_match {
                    trace::log("active_window_resolve:strict_prefetched_hit");
                    prefetched
                } else {
                    mark_frontmost_if_current(super::super::assert_active_window_id_matches(
                        &reference_owned,
                    )?)
                };
                let strict_bounds = super::super::resolve_tokenize_region_bounds(
                    strict_window.bounds.clone(),
                    region.as_ref(),
                )?;
                stage_done!("active_window_resolve");
                stage_done!("active_window_region_resolve");

                let strict_meta =
                    tokenize_meta_for_window(&strict_window, strict_bounds.clone(), true)?;
                let run_strict = || vision::pipeline::tokenize_window(strict_meta.clone());

                let payload = if let (Some(handle), Some(bounds)) =
                    (speculative_handle, speculative_bounds)
                {
                    if bounds_match_with_tolerance(&bounds, &strict_bounds, 2.0) {
                        match handle.join() {
                            Ok(Ok(payload)) => {
                                let post_validate =
                                    super::super::assert_active_window_id_matches(&reference_owned)
                                        .ok()
                                        .map(mark_frontmost_if_current)
                                        .and_then(|window| {
                                            super::super::resolve_tokenize_region_bounds(
                                                window.bounds.clone(),
                                                region.as_ref(),
                                            )
                                            .ok()
                                            .map(|resolved_bounds| (window, resolved_bounds))
                                        });
                                if let Some((post_window, post_bounds)) = post_validate {
                                    if bounds_match_with_tolerance(&bounds, &post_bounds, 2.0) {
                                        trace::log("active_window_tokenize:speculative_keep");
                                        payload
                                    } else {
                                        trace::log(
                                            "active_window_tokenize:speculative_discard reason=post_validate_mismatch",
                                        );
                                        let meta = tokenize_meta_for_window(
                                            &post_window,
                                            post_bounds,
                                            true,
                                        )?;
                                        vision::pipeline::tokenize_window(meta)?
                                    }
                                } else {
                                    trace::log(
                                        "active_window_tokenize:speculative_discard reason=post_validate_unavailable",
                                    );
                                    run_strict()?
                                }
                            }
                            Ok(Err(err)) => {
                                trace::log(format!(
                                    "active_window_tokenize:speculative_error fallback={}",
                                    err
                                ));
                                run_strict()?
                            }
                            Err(_) => {
                                trace::log(
                                    "active_window_tokenize:speculative_panic fallback=strict",
                                );
                                run_strict()?
                            }
                        }
                    } else {
                        trace::log(
                            "active_window_tokenize:speculative_discard reason=bounds_mismatch",
                        );
                        drop(handle);
                        run_strict()?
                    }
                } else {
                    run_strict()?
                };
                (strict_window, payload)
            } else {
                let resolved = super::super::resolve_active_window_target()?;
                stage_done!("active_window_resolve");
                let bounds = super::super::resolve_tokenize_region_bounds(
                    resolved.bounds.clone(),
                    region.as_ref(),
                )?;
                stage_done!("active_window_region_resolve");
                if let Some(reference) = resolved.window_ref.as_deref() {
                    let (reply_tx, reply_rx) =
                        mpsc::channel::<Option<super::super::TokenizeHintSnapshot>>();
                    let reference = reference.to_string();
                    std::thread::spawn(move || {
                        let snapshot =
                            super::super::collect_tokenize_new_window_hint_snapshot(&reference);
                        let _ = reply_tx.send(snapshot);
                    });
                    hint_snapshot_prefetch_rx = Some(reply_rx);
                }
                let meta = tokenize_meta_for_window(&resolved, bounds, false)?;
                let payload = vision::pipeline::tokenize_window(meta)?;
                (resolved, payload)
            };
            bound_hint_active_window_id = active_window_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
                .map(str::to_string)
                .or_else(|| frontmost_window.window_ref.clone());
            if hint_snapshot_prefetch_rx.is_none() {
                if let Some(reference) = bound_hint_active_window_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                {
                    let (reply_tx, reply_rx) =
                        mpsc::channel::<Option<super::super::TokenizeHintSnapshot>>();
                    let reference = reference.to_string();
                    std::thread::spawn(move || {
                        let snapshot =
                            super::super::collect_tokenize_new_window_hint_snapshot(&reference);
                        let _ = reply_tx.send(snapshot);
                    });
                    hint_snapshot_prefetch_rx = Some(reply_rx);
                }
            }
            stage_done!("active_window_tokenize");
            if let Some(first) = payload.windows.first_mut() {
                if first.window_ref.is_none() {
                    first.window_ref = active_window_id
                        .as_deref()
                        .map(str::trim)
                        .filter(|v| !v.is_empty())
                        .map(str::to_string)
                        .or_else(|| frontmost_window.window_ref.clone());
                }
                if first.os_bounds.is_none() {
                    first.os_bounds = Some(frontmost_window.bounds.clone());
                }
            }
            stage_done!("active_window_attach_window_ref");
            payload
        } else if window_query.is_none() {
            let overlay_window_bounds = overlay_bridge::tracked_window_bounds();
            stage_done!("frontmost_bounds_probe");
            if let Some(bounds) = overlay_window_bounds {
                let bounds = super::super::resolve_tokenize_region_bounds(bounds, region.as_ref())?;
                stage_done!("frontmost_region_resolve");
                let window_meta = vision::pipeline::TokenizeWindowMeta {
                    id: "frontmost:1".to_string(),
                    title: "active_window".to_string(),
                    app: None,
                    bounds,
                    pid: None,
                    native_window_id: None,
                    capture_bounds: None,
                };
                let payload = vision::pipeline::tokenize_window(window_meta)?;
                stage_done!("frontmost_tokenize");
                payload
            } else if let Some(bounds) = window_target::frontmost_window_bounds() {
                stage_done!("frontmost_bounds_lookup");
                let bounds = super::super::resolve_tokenize_region_bounds(bounds, region.as_ref())?;
                stage_done!("frontmost_region_resolve");
                let app = window_target::frontmost_app_name();
                stage_done!("frontmost_app_lookup");
                let title = app.clone().unwrap_or_else(|| "active_window".to_string());
                let window_meta = vision::pipeline::TokenizeWindowMeta {
                    id: "frontmost:1".to_string(),
                    title,
                    app,
                    bounds,
                    pid: None,
                    native_window_id: None,
                    capture_bounds: None,
                };
                let payload = vision::pipeline::tokenize_window(window_meta)?;
                stage_done!("frontmost_tokenize");
                payload
            } else {
                let mut windows = window_target::list_windows()?;
                stage_done!("window_list");
                super::super::enrich_window_refs(&mut windows);
                stage_done!("window_ref_enrich");
                let target = window_target::resolve_tokenize_window_target(&windows, None)?;
                stage_done!("window_target_resolve");
                let bounds = super::super::resolve_tokenize_region_bounds(
                    target.bounds.clone(),
                    region.as_ref(),
                )?;
                stage_done!("window_region_resolve");
                let window_meta = tokenize_meta_for_window(&target, bounds, false)?;
                let mut payload = vision::pipeline::tokenize_window(window_meta)?;
                stage_done!("window_tokenize");
                if let Some(first) = payload.windows.first_mut() {
                    first.window_ref = target.window_ref.clone();
                }
                stage_done!("window_attach_window_ref");
                payload
            }
        } else {
            let mut windows = window_target::list_windows()?;
            stage_done!("window_list");
            super::super::enrich_window_refs(&mut windows);
            stage_done!("window_ref_enrich");
            let target =
                window_target::resolve_tokenize_window_target(&windows, window_query.as_deref())?;
            stage_done!("window_target_resolve");
            let bounds = super::super::resolve_tokenize_region_bounds(
                target.bounds.clone(),
                region.as_ref(),
            )?;
            stage_done!("window_region_resolve");
            let window_meta = tokenize_meta_for_window(&target, bounds, true)?;
            let mut payload = vision::pipeline::tokenize_window(window_meta)?;
            stage_done!("window_tokenize");
            if let Some(first) = payload.windows.first_mut() {
                first.window_ref = target.window_ref.clone();
            }
            stage_done!("window_attach_window_ref");
            payload
        }
    };
    let mut payload = payload;
    if !screenshot_mode {
        let needs_attach = payload.windows.iter().any(|window| {
            window
                .window_ref
                .as_deref()
                .map(str::trim)
                .is_none_or(str::is_empty)
        });
        if needs_attach {
            super::super::attach_window_ref_to_payload(&mut payload);
        }
        let needs_backfill = payload
            .windows
            .iter()
            .any(|window| window.os_bounds.is_none());
        if needs_backfill {
            super::super::backfill_tokenize_window_positions(&mut payload);
        }
        stage_done!("payload_enrich");
    }
    if let Some(path_raw) = overlay_out_path {
        let overlay_path = PathBuf::from(path_raw);
        vision::pipeline::write_tokenize_overlay(&payload, &overlay_path)?;
        trace::log(format!(
            "execute:screen_tokenize:overlay_ok path={}",
            overlay_path.display()
        ));
        stage_done!("overlay_write");
    }
    if overlay_token_updates_enabled {
        if let Err(err) = overlay_bridge::update_from_tokenize(&payload) {
            trace::log(format!("execute:screen_tokenize:overlay_update_warn {err}"));
        }
        stage_done!("overlay_update");
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
    stage_done!("json_encode");
    if journal {
        apply_journal_redaction(&mut value);
        stage_done!("journal_redact");
    }
    if !journal {
        super::super::remap_tokenize_window_id_field(&mut value);
        stage_done!("window_id_remap");
        let precomputed_hint = hint_snapshot_prefetch_rx.and_then(|rx| rx.recv().ok().flatten());
        super::super::append_tokenize_new_window_hint(
            &mut value,
            bound_hint_active_window_id.as_deref(),
            precomputed_hint,
        );
        stage_timings.push(("new_window_hint", stage_started.elapsed().as_millis()));
    }
    if list_all_windows && !screenshot_mode {
        let windows = if let Some(handle) = all_windows_prefetch_handle.take() {
            match handle.join() {
                Ok(result) => result?,
                Err(_) => {
                    return Err(AppError::internal("all_windows prefetch worker panicked"));
                }
            }
        } else {
            let mut windows = window_target::list_windows()?;
            super::super::enrich_window_refs(&mut windows);
            windows
        };
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "all_windows".to_string(),
                Value::Array(windows.iter().map(|w| w.as_json()).collect::<Vec<Value>>()),
            );
        }
        if journal {
            apply_journal_all_windows_redaction(&mut value);
        }
        stage_timings.push(("all_windows_list", stage_started.elapsed().as_millis()));
    }
    let timing_breakdown = stage_timings
        .iter()
        .map(|(label, ms)| format!("{label}_ms={ms}"))
        .collect::<Vec<_>>()
        .join(" ");
    let total_ms = total_started.elapsed().as_millis();
    trace::log(format!(
        "execute:screen_tokenize:timing total_ms={} {}",
        total_ms, timing_breakdown
    ));
    if std::env::var("DESKTOPCTL_INCLUDE_DEBUG_TIMINGS")
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    {
        let mut stage_ms = serde_json::Map::new();
        for (label, ms) in &stage_timings {
            stage_ms.insert((*label).to_string(), serde_json::json!(ms));
        }
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "_debug".to_string(),
                serde_json::json!({
                    "perf_ms": {
                        "total": total_ms,
                        "stages": stage_ms
                    }
                }),
            );
        }
    }
    Ok(value)
}

fn apply_journal_redaction(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    obj.remove("window_id");
    obj.remove("hint");
    if let Some(windows) = obj
        .get_mut("windows")
        .and_then(serde_json::Value::as_array_mut)
    {
        for window in windows {
            let Some(window_obj) = window.as_object_mut() else {
                continue;
            };
            window_obj.remove("id");
            if let Some(elements) = window_obj
                .get_mut("elements")
                .and_then(serde_json::Value::as_array_mut)
            {
                for element in elements {
                    if let Some(element_obj) = element.as_object_mut() {
                        element_obj.remove("id");
                    }
                }
            }
        }
    }
}

fn apply_journal_all_windows_redaction(value: &mut Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    if let Some(windows) = obj
        .get_mut("all_windows")
        .and_then(serde_json::Value::as_array_mut)
    {
        for window in windows {
            if let Some(window_obj) = window.as_object_mut() {
                window_obj.remove("id");
            }
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn window(frontmost: bool) -> platform::windowing::WindowInfo {
        platform::windowing::WindowInfo {
            id: "123".to_string(),
            window_ref: Some("app_123".to_string()),
            parent_id: None,
            pid: 42,
            index: 0,
            app: "App".to_string(),
            title: "Title".to_string(),
            bounds: Bounds {
                x: 10.0,
                y: 20.0,
                width: 300.0,
                height: 200.0,
            },
            frontmost,
            visible: true,
            modal: None,
        }
    }

    #[test]
    fn foreground_explicit_active_window_keeps_ax_available() {
        assert!(!should_use_background_capture(&window(true), true));
    }

    #[test]
    fn background_explicit_active_window_uses_window_capture() {
        assert!(should_use_background_capture(&window(false), true));
    }

    #[test]
    fn implicit_active_window_uses_foreground_capture() {
        assert!(!should_use_background_capture(&window(false), false));
    }
}

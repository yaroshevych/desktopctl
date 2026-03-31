use std::{path::PathBuf, sync::mpsc, time::Instant};

use desktop_core::{automation::new_backend, error::AppError, protocol::Bounds};
use serde_json::{Value, json};

#[cfg(target_os = "macos")]
use crate::overlay;
use crate::{permissions, platform, trace, vision, window_refs, window_target};

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
    if let Some((expected_pid, expected_window_id)) = window_refs::resolve_native_for_ref(reference)
    {
        if let Some(found) = app_windows
            .iter()
            .find(|window| {
                visible_match(window)
                    && window.pid == expected_pid
                    && window.id == expected_window_id
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
        if active_window {
            if let Some(reference) = active_window_id
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                if let Ok(app_windows) = window_target::list_frontmost_app_windows() {
                    active_window_prefetched_windows = Some(app_windows.clone());
                    let reference = reference.to_string();
                    let (reply_tx, reply_rx) =
                        mpsc::channel::<Option<super::super::TokenizeHintSnapshot>>();
                    std::thread::spawn(move || {
                        let snapshot =
                            super::super::collect_tokenize_new_window_hint_snapshot_from_windows(
                                &reference,
                                app_windows,
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
                let prefetched_match =
                    active_window_prefetched_windows
                        .as_deref()
                        .and_then(|windows| {
                            resolve_active_window_from_app_windows(&reference_owned, windows)
                        });

                if let Some(prefetched) = prefetched_match.as_ref() {
                    let prefetched_title = prefetched.title.clone();
                    let prefetched_app = Some(prefetched.app.clone());
                    if let Ok(bounds) = super::super::resolve_tokenize_region_bounds(
                        prefetched.bounds.clone(),
                        region.as_ref(),
                    ) {
                        trace::log("active_window_id_match:prefetched_windows_hit");
                        let meta = vision::pipeline::TokenizeWindowMeta {
                            id: prefetched.id.clone(),
                            title: prefetched_title,
                            app: prefetched_app,
                            bounds: bounds.clone(),
                        };
                        speculative_bounds = Some(bounds);
                        speculative_handle = Some(std::thread::spawn(move || {
                            vision::pipeline::tokenize_window(meta)
                        }));
                        trace::log("active_window_tokenize:speculative_start source=prefetched");
                    }
                } else if let Some(frontmost_bounds) = window_target::frontmost_window_bounds() {
                    if let Ok(bounds) = super::super::resolve_tokenize_region_bounds(
                        frontmost_bounds,
                        region.as_ref(),
                    ) {
                        let app = window_target::frontmost_app_name();
                        let title = app.clone().unwrap_or_else(|| "active_window".to_string());
                        let meta = vision::pipeline::TokenizeWindowMeta {
                            id: reference_owned.clone(),
                            title,
                            app,
                            bounds: bounds.clone(),
                        };
                        speculative_bounds = Some(bounds);
                        speculative_handle = Some(std::thread::spawn(move || {
                            vision::pipeline::tokenize_window(meta)
                        }));
                        trace::log("active_window_tokenize:speculative_start source=frontmost");
                    }
                }

                let strict_window = if let Some(prefetched) = prefetched_match {
                    trace::log("active_window_resolve:strict_prefetched_hit");
                    prefetched
                } else {
                    super::super::assert_active_window_id_matches(&reference_owned)?
                };
                let strict_bounds = super::super::resolve_tokenize_region_bounds(
                    strict_window.bounds.clone(),
                    region.as_ref(),
                )?;
                stage_done!("active_window_resolve");
                stage_done!("active_window_region_resolve");

                let strict_app = Some(strict_window.app.clone());
                let strict_title = strict_window.title.clone();
                let strict_meta = vision::pipeline::TokenizeWindowMeta {
                    id: strict_window.id.clone(),
                    title: strict_title,
                    app: strict_app,
                    bounds: strict_bounds.clone(),
                };
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
                                        vision::pipeline::tokenize_window(
                                            vision::pipeline::TokenizeWindowMeta {
                                                id: post_window.id,
                                                title: post_window.title,
                                                app: Some(post_window.app),
                                                bounds: post_bounds,
                                            },
                                        )?
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
                let app = Some(resolved.app.clone());
                let title = resolved.title.clone();
                let window_query = resolved.id.clone();
                let payload =
                    vision::pipeline::tokenize_window(vision::pipeline::TokenizeWindowMeta {
                        id: window_query,
                        title,
                        app,
                        bounds,
                    })?;
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
            stage_done!("frontmost_bounds_probe");
            if let Some(bounds) = overlay_window_bounds {
                let bounds = super::super::resolve_tokenize_region_bounds(bounds, region.as_ref())?;
                stage_done!("frontmost_region_resolve");
                let window_meta = vision::pipeline::TokenizeWindowMeta {
                    id: "frontmost:1".to_string(),
                    title: "active_window".to_string(),
                    app: None,
                    bounds,
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
                let window_meta = vision::pipeline::TokenizeWindowMeta {
                    id: target.id.clone(),
                    title: target.title.clone(),
                    app: Some(target.app.clone()),
                    bounds,
                };
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
            let window_meta = vision::pipeline::TokenizeWindowMeta {
                id: target.id.clone(),
                title: target.title.clone(),
                app: Some(target.app.clone()),
                bounds,
            };
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
    #[cfg(target_os = "macos")]
    if overlay_token_updates_enabled {
        if let Err(err) = overlay::update_from_tokenize(&payload) {
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
    super::super::remap_tokenize_window_id_field(&mut value);
    stage_done!("window_id_remap");
    let precomputed_hint = hint_snapshot_prefetch_rx.and_then(|rx| rx.recv().ok().flatten());
    super::super::append_tokenize_new_window_hint(
        &mut value,
        bound_hint_active_window_id.as_deref(),
        precomputed_hint,
    );
    stage_timings.push(("new_window_hint", stage_started.elapsed().as_millis()));
    let timing_breakdown = stage_timings
        .iter()
        .map(|(label, ms)| format!("{label}_ms={ms}"))
        .collect::<Vec<_>>()
        .join(" ");
    trace::log(format!(
        "execute:screen_tokenize:timing total_ms={} {}",
        total_started.elapsed().as_millis(),
        timing_breakdown
    ));
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

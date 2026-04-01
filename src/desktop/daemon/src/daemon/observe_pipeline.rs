use super::*;
use std::path::PathBuf;

use image::{ImageFormat, RgbaImage};

pub(super) fn append_observe_payload(result: &mut Value, observe: Option<Value>) {
    let Some(observe) = observe else {
        return;
    };
    if let Some(object) = result.as_object_mut() {
        object.insert("observe".to_string(), observe);
    }
}

pub(super) fn capture_observe_start_state(options: &ObserveOptions) -> ObserveStartState {
    if !options.enabled {
        return ObserveStartState::default();
    }
    let active_window_id = resolve_active_window_target()
        .ok()
        .and_then(|window| window.window_ref.clone());
    let focused_element_id = focused_element_id_from_ax();
    ObserveStartState {
        active_window_id,
        focused_element_id,
    }
}

fn focused_element_id_from_ax() -> Option<String> {
    let ax = platform::ax::focused_frontmost_element().ok()??;
    vision::ax_merge::primary_id_for_ax(&ax).or_else(|| {
        let role = ax.role.trim().to_ascii_lowercase();
        if role.is_empty() {
            None
        } else {
            Some(format!("ax_{role}"))
        }
    })
}

fn observe_transition_state(start_state: &ObserveStartState) -> ObserveEndState {
    let active_window = resolve_active_window_target().ok();
    let active_window_id = active_window
        .as_ref()
        .and_then(|window| window.window_ref.clone());
    let focused_element_id = focused_element_id_from_ax();
    let active_window_changed = active_window_id != start_state.active_window_id;
    let focus_changed = focused_element_id != start_state.focused_element_id;
    ObserveEndState {
        focus_changed,
        focused_element_id,
        active_window_changed,
        active_window_id,
        active_window_bounds: active_window.map(|window| window.bounds),
    }
}

pub(super) fn observe_after_action(
    options: &ObserveOptions,
    start_state: &ObserveStartState,
    observe_scope: Option<&desktop_core::protocol::Bounds>,
    pre_click_tokens: Option<&[Value]>,
) -> Result<Option<Value>, AppError> {
    if !options.enabled {
        return Ok(None);
    }
    let start = Instant::now();
    let prev = vision::capture::capture_screen_png(None)?;
    let mut prev_thumb =
        vision::diff::thumbnail_from_rgba(&prev.image, OBSERVE_THUMB_WIDTH, OBSERVE_THUMB_HEIGHT);
    let mut last_capture = prev;
    let start_capture = last_capture.clone();
    let mut changed_any = false;
    let mut last_change_at: Option<Instant> = None;
    let mut quiet_frames = 0u32;
    let mut changed_regions: Vec<desktop_core::protocol::Bounds> = Vec::new();
    let effective_timeout_ms = options.timeout_ms.max(options.settle_ms).max(20);
    let timeout = Duration::from_millis(effective_timeout_ms);
    let mut sample_count = 0u64;
    let mut diff_ms_total = 0u64;

    loop {
        if start.elapsed() >= timeout {
            let final_regions =
                final_observe_regions_from_images(&start_capture, &last_capture, observe_scope);
            let (tokens, _, _) =
                observe_tokens_for_regions(&last_capture, &final_regions, observe_scope);
            let raw_tokens = tokens;
            let end_state = observe_transition_state(start_state);
            let origin_bounds = end_state.active_window_bounds.as_ref().or(observe_scope);
            let regions = normalize_observe_regions(&final_regions, origin_bounds);
            let start_tokens = observe_before_tokens_for_regions(
                pre_click_tokens,
                &start_capture,
                &final_regions,
                observe_scope,
                origin_bounds,
            );
            let tokens_delta = normalize_observe_tokens_delta(
                diff_observe_tokens(&start_tokens, &raw_tokens),
                origin_bounds,
            );
            let settle_ms = start.elapsed().as_millis() as u64;
            trace::log(format!(
                "observe:settle outcome=timeout settle_ms={} samples={} diff_ms_total={} regions={}",
                settle_ms,
                sample_count,
                diff_ms_total,
                final_regions.len()
            ));
            return Ok(Some(json!({
                "changed": !final_regions.is_empty(),
                "regions": regions,
                "tokens_delta": tokens_delta,
                "focus_changed": end_state.focus_changed,
                "focused_element_id": end_state.focused_element_id,
                "active_window_changed": end_state.active_window_changed,
                "active_window_id": end_state.active_window_id,
                "stability": "timeout",
                "elapsed_ms": settle_ms,
                "settle_ms": settle_ms
            })));
        }
        thread::sleep(Duration::from_millis(OBSERVE_SAMPLE_INTERVAL_MS));
        let curr = vision::capture::capture_screen_png(None)?;
        let curr_thumb = vision::diff::thumbnail_from_rgba(
            &curr.image,
            OBSERVE_THUMB_WIDTH,
            OBSERVE_THUMB_HEIGHT,
        );
        sample_count += 1;
        let diff_started = Instant::now();
        let frame_regions =
            vision::diff::diff_regions(&prev_thumb, &curr_thumb, OBSERVE_DIFF_THRESHOLD);
        diff_ms_total += diff_started.elapsed().as_millis() as u64;
        let significant_regions: Vec<_> = frame_regions
            .into_iter()
            .filter(|region| {
                region.width.saturating_mul(region.height).max(1)
                    >= OBSERVE_MIN_THUMB_COMPONENT_AREA
            })
            .collect();
        if !significant_regions.is_empty() {
            changed_any = true;
            last_change_at = Some(Instant::now());
            quiet_frames = 0;
            for changed_region in significant_regions {
                let upscaled = vision::diff::upscale_region(
                    changed_region,
                    curr.frame.width,
                    curr.frame.height,
                    curr_thumb.width,
                    curr_thumb.height,
                );
                let padded = pad_bounds(upscaled, OBSERVE_REGION_PAD_PX);
                if let Some(clipped) = clip_to_scope(&padded, observe_scope) {
                    merge_region_into_list(&mut changed_regions, clipped);
                }
            }
            if options.until == ObserveUntil::FirstChange {
                let final_regions =
                    final_observe_regions_from_images(&start_capture, &curr, observe_scope);
                let (tokens, _, _) =
                    observe_tokens_for_regions(&curr, &final_regions, observe_scope);
                let raw_tokens = tokens;
                let end_state = observe_transition_state(start_state);
                let origin_bounds = end_state.active_window_bounds.as_ref().or(observe_scope);
                let regions = normalize_observe_regions(&final_regions, origin_bounds);
                let start_tokens = observe_before_tokens_for_regions(
                    pre_click_tokens,
                    &start_capture,
                    &final_regions,
                    observe_scope,
                    origin_bounds,
                );
                let tokens_delta = normalize_observe_tokens_delta(
                    diff_observe_tokens(&start_tokens, &raw_tokens),
                    origin_bounds,
                );
                let settle_ms = start.elapsed().as_millis() as u64;
                trace::log(format!(
                    "observe:settle outcome=first_change settle_ms={} samples={} diff_ms_total={} regions={}",
                    settle_ms,
                    sample_count,
                    diff_ms_total,
                    final_regions.len()
                ));
                return Ok(Some(json!({
                    "changed": !final_regions.is_empty(),
                    "regions": regions,
                    "tokens_delta": tokens_delta,
                    "focus_changed": end_state.focus_changed,
                    "focused_element_id": end_state.focused_element_id,
                    "active_window_changed": end_state.active_window_changed,
                    "active_window_id": end_state.active_window_id,
                    "stability": "settled",
                    "elapsed_ms": settle_ms,
                    "settle_ms": settle_ms
                })));
            }
        } else {
            quiet_frames += 1;
            if changed_any {
                if quiet_frames >= OBSERVE_QUIET_FRAMES {
                    if let Some(last_change) = last_change_at {
                        if last_change.elapsed() < Duration::from_millis(options.settle_ms) {
                            last_capture = curr;
                            prev_thumb = curr_thumb;
                            continue;
                        }
                    }
                    let final_regions =
                        final_observe_regions_from_images(&start_capture, &curr, observe_scope);
                    let (tokens, _, _) =
                        observe_tokens_for_regions(&curr, &final_regions, observe_scope);
                    let raw_tokens = tokens;
                    let end_state = observe_transition_state(start_state);
                    let origin_bounds = end_state.active_window_bounds.as_ref().or(observe_scope);
                    let regions = normalize_observe_regions(&final_regions, origin_bounds);
                    let start_tokens = observe_before_tokens_for_regions(
                        pre_click_tokens,
                        &start_capture,
                        &final_regions,
                        observe_scope,
                        origin_bounds,
                    );
                    let tokens_delta = normalize_observe_tokens_delta(
                        diff_observe_tokens(&start_tokens, &raw_tokens),
                        origin_bounds,
                    );
                    let settle_ms = start.elapsed().as_millis() as u64;
                    trace::log(format!(
                        "observe:settle outcome=settled settle_ms={} samples={} diff_ms_total={} regions={}",
                        settle_ms,
                        sample_count,
                        diff_ms_total,
                        final_regions.len()
                    ));
                    return Ok(Some(json!({
                        "changed": !final_regions.is_empty(),
                        "regions": regions,
                        "tokens_delta": tokens_delta,
                        "focus_changed": end_state.focus_changed,
                        "focused_element_id": end_state.focused_element_id,
                        "active_window_changed": end_state.active_window_changed,
                        "active_window_id": end_state.active_window_id,
                        "stability": "settled",
                        "elapsed_ms": settle_ms,
                        "settle_ms": settle_ms
                    })));
                }
            } else if options.until == ObserveUntil::Stable && quiet_frames >= OBSERVE_QUIET_FRAMES
            {
                let elapsed_ms = start.elapsed().as_millis() as u64;
                if elapsed_ms < options.settle_ms {
                    last_capture = curr;
                    prev_thumb = curr_thumb;
                    continue;
                }
                let end_state = observe_transition_state(start_state);
                let tokens_delta = json!({
                    "added": [],
                    "removed": [],
                    "changed": []
                });
                trace::log(format!(
                    "observe:settle outcome=no_change settle_ms={} samples={} diff_ms_total={} regions={}",
                    elapsed_ms,
                    sample_count,
                    diff_ms_total,
                    changed_regions.len()
                ));
                return Ok(Some(json!({
                    "changed": false,
                    "regions": [],
                    "tokens_delta": tokens_delta,
                    "focus_changed": end_state.focus_changed,
                    "focused_element_id": end_state.focused_element_id,
                    "active_window_changed": end_state.active_window_changed,
                    "active_window_id": end_state.active_window_id,
                    "stability": "no_change",
                    "elapsed_ms": elapsed_ms,
                    "settle_ms": elapsed_ms
                })));
            }
        }
        last_capture = curr;
        prev_thumb = curr_thumb;
    }
}

fn observe_tokens_for_regions(
    capture: &vision::types::CapturedImage,
    regions: &[desktop_core::protocol::Bounds],
    observe_scope: Option<&desktop_core::protocol::Bounds>,
) -> (Vec<Value>, bool, usize) {
    let observe_started = Instant::now();
    let mut tokens: Vec<Value> = Vec::new();
    let (ax_available, ax_elements) = match platform::ax::collect_frontmost_window_elements() {
        Ok(items) => (true, items),
        Err(_) => (false, Vec::new()),
    };

    let mut ocr_regions = Vec::new();
    let mut ocr_tokens = 0usize;
    let ocr_started = Instant::now();
    if !regions.is_empty() {
        for (idx, core_region) in regions.iter().enumerate() {
            let dynamic_pad = observe_adaptive_ocr_pad(core_region, &ax_elements);
            let (padded, applied_pad) = expand_bounds_with_pad_clamped(
                core_region,
                dynamic_pad,
                capture.frame.width as f64,
                capture.frame.height as f64,
                observe_scope,
            );
            if let Some((x0, y0, x1, y1)) = logical_bounds_to_image_rect(
                &padded,
                capture.image.width(),
                capture.image.height(),
                capture.frame.width,
                capture.frame.height,
            ) {
                let crop_w = (x1 - x0).max(0) as u32;
                let crop_h = (y1 - y0).max(0) as u32;
                if crop_w <= 1 || crop_h <= 1 {
                    continue;
                }
                let crop =
                    image::imageops::crop_imm(&capture.image, x0 as u32, y0 as u32, crop_w, crop_h)
                        .to_image();
                dump_observe_region_screenshot(
                    &crop,
                    capture.frame.snapshot_id,
                    idx,
                    core_region,
                    &padded,
                );
                if let Ok(texts) = vision::ocr::recognize_text(&crop) {
                    let sx = padded.width.max(1.0) / crop_w.max(1) as f64;
                    let sy = padded.height.max(1.0) / crop_h.max(1) as f64;
                    let mut emitted = 0usize;
                    for text in texts {
                        if text.text.trim().is_empty() {
                            continue;
                        }
                        let logical_bounds = desktop_core::protocol::Bounds {
                            x: padded.x + text.bounds.x * sx,
                            y: padded.y + text.bounds.y * sy,
                            width: text.bounds.width * sx,
                            height: text.bounds.height * sy,
                        };
                        // Keep OCR boxes that intersect the changed region. IoU is too strict
                        // for small tokens when the region is large.
                        if !bounds_intersect(core_region, &logical_bounds) {
                            continue;
                        }
                        if let Some(scope) = observe_scope {
                            if iou(scope, &logical_bounds) <= 0.0 {
                                continue;
                            }
                        }
                        tokens.push(json!({
                            "id": observe_ocr_token_id(&text.text, &logical_bounds),
                            "source": "vision_ocr",
                            "text": text.text,
                            "confidence": text.confidence,
                            "bbox": [logical_bounds.x, logical_bounds.y, logical_bounds.width, logical_bounds.height]
                        }));
                        emitted += 1;
                    }
                    ocr_tokens += emitted;
                    ocr_regions.push(format!(
                        "#{idx}:core=({:.0},{:.0},{:.0},{:.0}) pad_req={:.1} pad_applied=(l:{:.1},r:{:.1},t:{:.1},b:{:.1}) pad=({:.0},{:.0},{:.0},{:.0}) crop={}x{} emitted={}",
                        core_region.x,
                        core_region.y,
                        core_region.width,
                        core_region.height,
                        dynamic_pad,
                        applied_pad.left,
                        applied_pad.right,
                        applied_pad.top,
                        applied_pad.bottom,
                        padded.x,
                        padded.y,
                        padded.width,
                        padded.height,
                        crop_w,
                        crop_h,
                        emitted
                    ));
                }
            }
        }
    }
    let ocr_elapsed = ocr_started.elapsed().as_millis() as u64;
    trace::log(format!(
        "observe:ocr elapsed_ms={} regions={} tokens={} details={}",
        ocr_elapsed,
        regions.len(),
        ocr_tokens,
        ocr_regions.join(" | ")
    ));

    let ax_started = Instant::now();
    let mut ax_count = 0usize;
    for ax in ax_elements {
        if let Some(scope) = observe_scope {
            if iou(scope, &ax.bounds) <= 0.0 {
                continue;
            }
        }
        if !regions.is_empty()
            && !regions
                .iter()
                .any(|region| bounds_intersect(region, &ax.bounds))
        {
            continue;
        }
        let id = vision::ax_merge::primary_id_for_ax(&ax)
            .unwrap_or_else(|| format!("ax_{}", ax.role.to_ascii_lowercase()));
        tokens.push(json!({
            "id": id,
            "source": format!("accessibility_ax:{}", ax.role),
            "text": ax.text,
            "checked": ax.checked,
            "bbox": [ax.bounds.x, ax.bounds.y, ax.bounds.width, ax.bounds.height]
        }));
        ax_count += 1;
    }
    let ax_elapsed = ax_started.elapsed().as_millis() as u64;
    let total_elapsed = observe_started.elapsed().as_millis() as u64;
    trace::log(format!(
        "observe:tokens elapsed_ms={} ocr_ms={} ax_ms={} total_tokens={} ax_count={}",
        total_elapsed,
        ocr_elapsed,
        ax_elapsed,
        tokens.len(),
        ax_count
    ));

    (tokens, ax_available, ax_count)
}

pub(super) fn observe_seed_tokens_from_tokenize_payload(
    payload: &desktop_core::protocol::TokenizePayload,
) -> Vec<Value> {
    let mut tokens = Vec::new();
    for window in &payload.windows {
        for element in &window.elements {
            let text = element.text.as_deref().unwrap_or("").trim();
            if text.is_empty() {
                continue;
            }
            tokens.push(json!({
                "id": element.id,
                "source": element.source,
                "text": text,
                "checked": element.checked,
                "bbox": [element.bbox[0], element.bbox[1], element.bbox[2], element.bbox[3]]
            }));
        }
    }
    tokens
}

fn observe_before_tokens_for_regions(
    pre_click_tokens: Option<&[Value]>,
    start_capture: &vision::types::CapturedImage,
    regions: &[desktop_core::protocol::Bounds],
    observe_scope: Option<&desktop_core::protocol::Bounds>,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Vec<Value> {
    let Some(seed) = pre_click_tokens else {
        return observe_tokens_for_regions(start_capture, regions, observe_scope).0;
    };
    let Some(origin) = origin else {
        return observe_tokens_for_regions(start_capture, regions, observe_scope).0;
    };
    let mut out = Vec::new();
    for token in seed {
        let Some(local) = token_bbox_bounds(token) else {
            continue;
        };
        let global = desktop_core::protocol::Bounds {
            x: origin.x + local.x,
            y: origin.y + local.y,
            width: local.width,
            height: local.height,
        };
        if !regions.is_empty()
            && !regions
                .iter()
                .any(|region| bounds_intersect(region, &global))
        {
            continue;
        }
        if let Some(scope) = observe_scope {
            if iou(scope, &global) <= 0.0 {
                continue;
            }
        }
        let mut cloned = token.clone();
        if let Some(obj) = cloned.as_object_mut() {
            obj.insert(
                "bbox".to_string(),
                json!([global.x, global.y, global.width, global.height]),
            );
        }
        out.push(cloned);
    }
    out
}

fn final_observe_regions_from_images(
    start_capture: &vision::types::CapturedImage,
    end_capture: &vision::types::CapturedImage,
    observe_scope: Option<&desktop_core::protocol::Bounds>,
) -> Vec<desktop_core::protocol::Bounds> {
    let start_gray = vision::diff::thumbnail_from_rgba(
        &start_capture.image,
        start_capture.image.width(),
        start_capture.image.height(),
    );
    let end_gray = vision::diff::thumbnail_from_rgba(
        &end_capture.image,
        end_capture.image.width(),
        end_capture.image.height(),
    );
    let frame_regions = vision::diff::diff_regions(&start_gray, &end_gray, OBSERVE_DIFF_THRESHOLD);
    let significant_regions: Vec<_> = frame_regions
        .into_iter()
        .filter(|region| {
            region.width.saturating_mul(region.height).max(1) >= OBSERVE_FINAL_MIN_COMPONENT_AREA
        })
        .collect();
    let mut merged: Vec<desktop_core::protocol::Bounds> = Vec::new();
    for changed_region in significant_regions {
        let upscaled = desktop_core::protocol::Bounds {
            x: changed_region.x as f64,
            y: changed_region.y as f64,
            width: changed_region.width as f64,
            height: changed_region.height as f64,
        };
        let padded = pad_bounds(upscaled, OBSERVE_FINAL_REGION_PAD_PX);
        if let Some(clipped) = clip_to_scope(&padded, observe_scope) {
            merge_region_into_list_with_threshold(&mut merged, clipped, 0.35);
        }
    }
    merged.sort_by(|a, b| {
        (a.y, a.x)
            .partial_cmp(&(b.y, b.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged
}

#[derive(Debug, Clone, Copy)]
struct AppliedPadding {
    left: f64,
    right: f64,
    top: f64,
    bottom: f64,
}

fn expand_bounds_with_pad_clamped(
    core: &desktop_core::protocol::Bounds,
    pad: f64,
    frame_width: f64,
    frame_height: f64,
    scope: Option<&desktop_core::protocol::Bounds>,
) -> (desktop_core::protocol::Bounds, AppliedPadding) {
    let core_x1 = core.x.max(0.0);
    let core_y1 = core.y.max(0.0);
    let core_x2 = (core.x + core.width).max(core_x1);
    let core_y2 = (core.y + core.height).max(core_y1);

    let (limit_x1, limit_y1, limit_x2, limit_y2) = if let Some(scope) = scope {
        (
            scope.x.max(0.0).min(frame_width),
            scope.y.max(0.0).min(frame_height),
            (scope.x + scope.width).max(0.0).min(frame_width),
            (scope.y + scope.height).max(0.0).min(frame_height),
        )
    } else {
        (0.0, 0.0, frame_width, frame_height)
    };

    let x1 = (core_x1 - pad).max(limit_x1).min(limit_x2);
    let y1 = (core_y1 - pad).max(limit_y1).min(limit_y2);
    let x2 = (core_x2 + pad).min(limit_x2).max(limit_x1);
    let y2 = (core_y2 + pad).min(limit_y2).max(limit_y1);

    let applied = AppliedPadding {
        left: (core_x1 - x1).max(0.0),
        right: (x2 - core_x2).max(0.0),
        top: (core_y1 - y1).max(0.0),
        bottom: (y2 - core_y2).max(0.0),
    };
    (
        desktop_core::protocol::Bounds {
            x: x1,
            y: y1,
            width: (x2 - x1).max(0.0),
            height: (y2 - y1).max(0.0),
        },
        applied,
    )
}

fn dump_observe_region_screenshot(
    image: &RgbaImage,
    snapshot_id: u64,
    region_idx: usize,
    core_region: &desktop_core::protocol::Bounds,
    padded_region: &desktop_core::protocol::Bounds,
) {
    let save_enabled = std::env::var("DESKTOPCTL_OBSERVE_SAVE_CROPS")
        .ok()
        .map(|v| {
            let lowered = v.trim().to_ascii_lowercase();
            lowered == "1" || lowered == "true" || lowered == "yes" || lowered == "on"
        })
        .unwrap_or(false);
    if !save_enabled {
        return;
    }
    let dir = PathBuf::from("/tmp/desktopctl-observe-crops");
    if let Err(err) = fs::create_dir_all(&dir) {
        trace::log(format!("observe:dump mkdir_failed err={err}"));
        return;
    }
    let file_name = format!(
        "snap{}_r{}_core_{}_{}_{}_{}_pad_{}_{}_{}_{}.png",
        snapshot_id,
        region_idx,
        round_nonnegative_i64(core_region.x),
        round_nonnegative_i64(core_region.y),
        round_nonnegative_i64(core_region.width),
        round_nonnegative_i64(core_region.height),
        round_nonnegative_i64(padded_region.x),
        round_nonnegative_i64(padded_region.y),
        round_nonnegative_i64(padded_region.width),
        round_nonnegative_i64(padded_region.height)
    );
    let out_path = dir.join(file_name);
    if let Err(err) = image.save_with_format(&out_path, ImageFormat::Png) {
        trace::log(format!(
            "observe:dump write_failed path={} err={err}",
            out_path.display()
        ));
    }
}

fn observe_adaptive_ocr_pad(
    core_region: &desktop_core::protocol::Bounds,
    ax_elements: &[platform::ax::AxElement],
) -> f64 {
    let mut dims: Vec<f64> = ax_elements
        .iter()
        .filter(|ax| {
            iou(core_region, &ax.bounds) > 0.01
                || iou(&inflate_bounds(core_region, 100.0), &ax.bounds) > 0.01
        })
        .filter(|ax| {
            matches!(
                ax.role.as_str(),
                "AXTextField"
                    | "AXTextArea"
                    | "AXButton"
                    | "AXCheckBox"
                    | "AXRadioButton"
                    | "AXPopUpButton"
            )
        })
        .map(|ax| ax.bounds.width.min(ax.bounds.height))
        .filter(|dim| *dim >= 8.0 && *dim <= 240.0)
        .collect();
    if dims.is_empty() {
        return OBSERVE_OCR_PAD_PX;
    }
    dims.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let min_dim = dims[0];
    (min_dim * 1.5).clamp(16.0, 96.0)
}

fn clip_to_scope(
    bounds: &desktop_core::protocol::Bounds,
    scope: Option<&desktop_core::protocol::Bounds>,
) -> Option<desktop_core::protocol::Bounds> {
    let Some(scope) = scope else {
        return Some(bounds.clone());
    };
    let x1 = bounds.x.max(scope.x);
    let y1 = bounds.y.max(scope.y);
    let x2 = (bounds.x + bounds.width).min(scope.x + scope.width);
    let y2 = (bounds.y + bounds.height).min(scope.y + scope.height);
    let w = x2 - x1;
    let h = y2 - y1;
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    Some(desktop_core::protocol::Bounds {
        x: x1,
        y: y1,
        width: w,
        height: h,
    })
}

fn merge_region_into_list(
    regions: &mut Vec<desktop_core::protocol::Bounds>,
    incoming: desktop_core::protocol::Bounds,
) {
    let mut merged = incoming;
    let mut idx = 0usize;
    while idx < regions.len() {
        if iou(&regions[idx], &merged) > 0.0 {
            merged = merge_bounds(Some(&regions[idx]), &merged);
            regions.swap_remove(idx);
            continue;
        }
        idx += 1;
    }
    regions.push(merged);
}

fn merge_region_into_list_with_threshold(
    regions: &mut Vec<desktop_core::protocol::Bounds>,
    incoming: desktop_core::protocol::Bounds,
    min_iou: f64,
) {
    let mut merged = incoming;
    let mut idx = 0usize;
    while idx < regions.len() {
        if iou(&regions[idx], &merged) >= min_iou {
            merged = merge_bounds(Some(&regions[idx]), &merged);
            regions.swap_remove(idx);
            continue;
        }
        idx += 1;
    }
    regions.push(merged);
}

fn pad_bounds(bounds: desktop_core::protocol::Bounds, pad: f64) -> desktop_core::protocol::Bounds {
    desktop_core::protocol::Bounds {
        x: (bounds.x - pad).max(0.0),
        y: (bounds.y - pad).max(0.0),
        width: bounds.width + pad * 2.0,
        height: bounds.height + pad * 2.0,
    }
}

fn diff_observe_tokens(before: &[Value], after: &[Value]) -> Value {
    use std::collections::{HashMap, HashSet};
    let mut before_map: HashMap<String, &Value> = HashMap::new();
    let mut after_map: HashMap<String, &Value> = HashMap::new();
    for token in before {
        before_map.insert(observe_token_key(token), token);
    }
    for token in after {
        after_map.insert(observe_token_key(token), token);
    }

    let mut added: Vec<Value> = Vec::new();
    let mut removed: Vec<Value> = Vec::new();
    let mut changed: Vec<Value> = Vec::new();
    let before_keys: HashSet<String> = before_map.keys().cloned().collect();
    let after_keys: HashSet<String> = after_map.keys().cloned().collect();

    for key in after_keys.difference(&before_keys) {
        if let Some(token) = after_map.get(key) {
            added.push((*token).clone());
        }
    }
    for key in before_keys.difference(&after_keys) {
        if let Some(token) = before_map.get(key) {
            removed.push((*token).clone());
        }
    }
    for key in before_keys.intersection(&after_keys) {
        let Some(before_token) = before_map.get(key) else {
            continue;
        };
        let Some(after_token) = after_map.get(key) else {
            continue;
        };
        if !observe_token_semantic_equal(before_token, after_token) {
            changed.push(json!({
                "before": (*before_token).clone(),
                "after": (*after_token).clone()
            }));
        }
    }

    json!({
        "added": added,
        "removed": removed,
        "changed": changed
    })
}

fn normalize_observe_regions(
    regions: &[desktop_core::protocol::Bounds],
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Vec<Value> {
    regions
        .iter()
        .map(|bounds| relative_bounds_json(bounds, origin))
        .collect()
}

fn normalize_observe_tokens_delta(
    mut delta: Value,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Value {
    for key in ["added", "removed"] {
        if let Some(items) = delta.get_mut(key).and_then(Value::as_array_mut) {
            for token in items {
                ensure_observe_token_id(token);
                rewrite_token_bbox_relative(token, origin);
            }
        }
    }
    if let Some(items) = delta.get_mut("changed").and_then(Value::as_array_mut) {
        for entry in items {
            if let Some(before) = entry.get_mut("before") {
                ensure_observe_token_id(before);
                rewrite_token_bbox_relative(before, origin);
            }
            if let Some(after) = entry.get_mut("after") {
                ensure_observe_token_id(after);
                rewrite_token_bbox_relative(after, origin);
            }
        }
    }
    remap_observe_ocr_ids_to_tokenize_ids(&mut delta, origin);
    reconcile_added_removed_pairs(&mut delta);
    sort_observe_tokens_delta(&mut delta);
    delta
}

#[derive(Debug, Clone)]
struct OcrIdCandidate {
    id: String,
    text_norm: String,
    bounds: desktop_core::protocol::Bounds,
}

fn remap_observe_ocr_ids_to_tokenize_ids(
    delta: &mut Value,
    origin: Option<&desktop_core::protocol::Bounds>,
) {
    let Some(window_bounds) = origin.cloned() else {
        return;
    };
    let app = window_target::frontmost_app_name();
    let window_meta = vision::pipeline::TokenizeWindowMeta {
        id: "frontmost:1".to_string(),
        title: app.clone().unwrap_or_else(|| "active_window".to_string()),
        app,
        bounds: window_bounds,
    };
    let payload = match vision::pipeline::tokenize_window(window_meta) {
        Ok(payload) => payload,
        Err(err) => {
            trace::log(format!(
                "observe:id_remap:tokenize_window_warn {}",
                err.message
            ));
            return;
        }
    };
    let candidates = collect_ocr_id_candidates(&payload);
    if candidates.is_empty() {
        return;
    }
    if let Some(items) = delta.get_mut("added").and_then(Value::as_array_mut) {
        for token in items {
            remap_single_observe_ocr_id(token, &candidates);
        }
    }
    if let Some(items) = delta.get_mut("changed").and_then(Value::as_array_mut) {
        for entry in items {
            if let Some(before) = entry.get_mut("before") {
                remap_single_observe_ocr_id(before, &candidates);
            }
            if let Some(after) = entry.get_mut("after") {
                remap_single_observe_ocr_id(after, &candidates);
            }
        }
    }
}

fn collect_ocr_id_candidates(
    payload: &desktop_core::protocol::TokenizePayload,
) -> Vec<OcrIdCandidate> {
    let mut out = Vec::new();
    for window in &payload.windows {
        for element in &window.elements {
            if element.source != "vision_ocr" {
                continue;
            }
            let id = element.id.trim();
            if id.is_empty() {
                continue;
            }
            let text = element.text.as_deref().unwrap_or("").trim();
            if text.is_empty() {
                continue;
            }
            out.push(OcrIdCandidate {
                id: id.to_string(),
                text_norm: normalize_observe_text(text),
                bounds: desktop_core::protocol::Bounds {
                    x: element.bbox[0],
                    y: element.bbox[1],
                    width: element.bbox[2],
                    height: element.bbox[3],
                },
            });
        }
    }
    out
}

fn remap_single_observe_ocr_id(token: &mut Value, candidates: &[OcrIdCandidate]) {
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if source != "vision_ocr" {
        return;
    }
    let text = token
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return;
    }
    let Some(bounds) = token_bbox_bounds(token) else {
        return;
    };
    let text_norm = normalize_observe_text(text);
    let mut best_idx: Option<usize> = None;
    let mut best_score = -1.0_f64;
    for (idx, candidate) in candidates.iter().enumerate() {
        if candidate.text_norm != text_norm {
            continue;
        }
        let score = iou(&candidate.bounds, &bounds);
        if score > best_score {
            best_score = score;
            best_idx = Some(idx);
        }
    }
    let Some(idx) = best_idx else {
        return;
    };
    if best_score < 0.10 {
        return;
    }
    if let Some(obj) = token.as_object_mut() {
        obj.insert("id".to_string(), Value::String(candidates[idx].id.clone()));
    }
}

fn reconcile_added_removed_pairs(delta: &mut Value) {
    use std::collections::{HashMap, VecDeque};

    let added = delta
        .get("added")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let removed = delta
        .get("removed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let changed = delta
        .get("changed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut removed_by_id: HashMap<String, VecDeque<Value>> = HashMap::new();
    let mut removed_unkeyed: Vec<Value> = Vec::new();
    for token in removed {
        if let Some(id) = token
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            removed_by_id
                .entry(id.to_string())
                .or_default()
                .push_back(token);
        } else {
            removed_unkeyed.push(token);
        }
    }

    let mut added_out: Vec<Value> = Vec::new();
    let mut removed_retain: Vec<Value> = Vec::new();
    for token in added {
        let Some(id) = token
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            added_out.push(token);
            continue;
        };

        let Some(queue) = removed_by_id.get_mut(id) else {
            added_out.push(token);
            continue;
        };
        let Some(before) = queue.pop_front() else {
            added_out.push(token);
            continue;
        };

        if observe_token_semantic_equal(&before, &token) {
            continue;
        }
        // Same ID but different semantic content: preserve both sides so deletions stay visible.
        removed_retain.push(before);
        added_out.push(token);
    }

    let mut removed_out: Vec<Value> = removed_unkeyed;
    removed_out.extend(removed_retain);
    for queue in removed_by_id.into_values() {
        removed_out.extend(queue);
    }

    if let Some(obj) = delta.as_object_mut() {
        obj.insert("added".to_string(), Value::Array(added_out));
        obj.insert("removed".to_string(), Value::Array(removed_out));
        obj.insert("changed".to_string(), Value::Array(changed));
    }
}

fn sort_observe_tokens_delta(delta: &mut Value) {
    if let Some(items) = delta.get_mut("added").and_then(Value::as_array_mut) {
        items.sort_by(token_position_compare);
    }
    if let Some(items) = delta.get_mut("removed").and_then(Value::as_array_mut) {
        items.sort_by(token_position_compare);
    }
    if let Some(items) = delta.get_mut("changed").and_then(Value::as_array_mut) {
        items.sort_by(changed_position_compare);
    }
}

fn token_position_compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    let ka = token_position_key(a);
    let kb = token_position_key(b);
    ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
}

fn changed_position_compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    let ka = a
        .get("after")
        .map(token_position_key)
        .unwrap_or_else(|| token_position_key(a));
    let kb = b
        .get("after")
        .map(token_position_key)
        .unwrap_or_else(|| token_position_key(b));
    ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
}

fn token_position_key(token: &Value) -> (f64, f64, f64, f64) {
    let b = token_bbox_bounds(token).unwrap_or(desktop_core::protocol::Bounds {
        x: f64::MAX,
        y: f64::MAX,
        width: 0.0,
        height: 0.0,
    });
    (b.y, b.x, b.height, b.width)
}

fn token_bbox_bounds(token: &Value) -> Option<desktop_core::protocol::Bounds> {
    let bbox = token.get("bbox")?.as_array()?;
    if bbox.len() != 4 {
        return None;
    }
    Some(desktop_core::protocol::Bounds {
        x: bbox[0].as_f64().unwrap_or(0.0),
        y: bbox[1].as_f64().unwrap_or(0.0),
        width: bbox[2].as_f64().unwrap_or(0.0),
        height: bbox[3].as_f64().unwrap_or(0.0),
    })
}

fn normalize_observe_text(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
        .trim()
        .to_ascii_lowercase()
}

fn rewrite_token_bbox_relative(token: &mut Value, origin: Option<&desktop_core::protocol::Bounds>) {
    let Some(bbox) = token.get("bbox").and_then(Value::as_array) else {
        return;
    };
    if bbox.len() != 4 {
        return;
    }
    let x = bbox[0].as_f64().unwrap_or(0.0);
    let y = bbox[1].as_f64().unwrap_or(0.0);
    let w = bbox[2].as_f64().unwrap_or(0.0);
    let h = bbox[3].as_f64().unwrap_or(0.0);
    let rel = relative_bounds(
        &desktop_core::protocol::Bounds {
            x,
            y,
            width: w,
            height: h,
        },
        origin,
    );
    if let Some(obj) = token.as_object_mut() {
        obj.insert(
            "bbox".to_string(),
            json!([
                round_nonnegative_i64(rel.x),
                round_nonnegative_i64(rel.y),
                round_nonnegative_i64(rel.width),
                round_nonnegative_i64(rel.height)
            ]),
        );
    }
}

fn ensure_observe_token_id(token: &mut Value) {
    if token
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
    {
        return;
    }
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let text = token
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let bbox_key = quantized_bbox_key(token.get("bbox").and_then(Value::as_array));
    let material = format!("{source}|{text}|{bbox_key}");
    let prefix = if source == "vision_ocr" {
        "ocr"
    } else if source.starts_with("accessibility_ax:") {
        "ax"
    } else {
        "tok"
    };
    if let Some(obj) = token.as_object_mut() {
        obj.insert(
            "id".to_string(),
            Value::String(format!("{prefix}_{:08x}", stable_hash32(&material))),
        );
    }
}

fn relative_bounds_json(
    bounds: &desktop_core::protocol::Bounds,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Value {
    let rel = relative_bounds(bounds, origin);
    json!({
        "x": round_nonnegative_i64(rel.x),
        "y": round_nonnegative_i64(rel.y),
        "width": round_nonnegative_i64(rel.width),
        "height": round_nonnegative_i64(rel.height)
    })
}

fn relative_bounds(
    bounds: &desktop_core::protocol::Bounds,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> desktop_core::protocol::Bounds {
    let mut out = bounds.clone();
    if let Some(window) = origin {
        out.x -= window.x;
        out.y -= window.y;
    }
    out.x = out.x.max(0.0);
    out.y = out.y.max(0.0);
    out.width = out.width.max(0.0);
    out.height = out.height.max(0.0);
    out
}

fn round_nonnegative_i64(value: f64) -> i64 {
    value.round().max(0.0) as i64
}

fn observe_ocr_token_id(text: &str, bounds: &desktop_core::protocol::Bounds) -> String {
    let (x, y, w, h) = quantized_observe_bbox(bounds);
    let material = format!("{}|{x},{y},{w},{h}", text.trim().to_ascii_lowercase());
    format!("ocr_{:08x}", stable_hash32(&material))
}

fn quantized_observe_bbox(bounds: &desktop_core::protocol::Bounds) -> (i64, i64, i64, i64) {
    let q = |v: f64| -> i64 { (v / 8.0).round() as i64 };
    (
        q(bounds.x.max(0.0)),
        q(bounds.y.max(0.0)),
        q(bounds.width.max(0.0)),
        q(bounds.height.max(0.0)),
    )
}

fn stable_hash32(input: &str) -> u32 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in input.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash & 0xffff_ffff) as u32
}

fn observe_token_key(token: &Value) -> String {
    if let Some(id) = token.get("id").and_then(Value::as_str) {
        if !id.trim().is_empty() {
            return format!("id:{id}");
        }
    }
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let text = token.get("text").and_then(Value::as_str).unwrap_or("");
    let bbox_key = quantized_bbox_key(token.get("bbox").and_then(Value::as_array));
    format!("fallback:{source}:{text}:{bbox_key}")
}

fn quantized_bbox_key(bbox: Option<&Vec<Value>>) -> String {
    let Some(bbox) = bbox else {
        return "[]".to_string();
    };
    if bbox.len() != 4 {
        return "[]".to_string();
    }
    let q = |v: Option<f64>| -> i64 {
        let n = v.unwrap_or(0.0);
        // Tolerate small OCR jitter by quantizing to 8px grid.
        (n / 8.0).round() as i64
    };
    let x = q(bbox[0].as_f64());
    let y = q(bbox[1].as_f64());
    let w = q(bbox[2].as_f64());
    let h = q(bbox[3].as_f64());
    format!("{x},{y},{w},{h}")
}

fn observe_token_semantic_equal(a: &Value, b: &Value) -> bool {
    let a_text = a.get("text").cloned().unwrap_or(Value::Null);
    let b_text = b.get("text").cloned().unwrap_or(Value::Null);
    let a_bbox = a.get("bbox").cloned().unwrap_or_else(|| json!([]));
    let b_bbox = b.get("bbox").cloned().unwrap_or_else(|| json!([]));
    let a_source = a.get("source").cloned().unwrap_or(Value::Null);
    let b_source = b.get("source").cloned().unwrap_or(Value::Null);
    let a_checked = a.get("checked").cloned().unwrap_or(Value::Null);
    let b_checked = b.get("checked").cloned().unwrap_or(Value::Null);
    a_text == b_text && a_bbox == b_bbox && a_source == b_source && a_checked == b_checked
}

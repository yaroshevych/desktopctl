use super::*;

mod token_collect;
mod token_delta;

use token_collect::observe_tokens_for_regions;
use token_delta::{
    diff_observe_tokens, normalize_observe_regions, normalize_observe_tokens_delta,
    token_bbox_bounds,
};

pub(super) fn append_observe_payload(result: &mut Value, observe: Option<Value>) {
    let Some(observe) = observe else {
        return;
    };
    if let Some(object) = result.as_object_mut() {
        object.insert("observe".to_string(), observe);
    }
}

pub(super) fn capture_observe_start_state(
    options: &ObserveOptions,
    observe_target: Option<&platform::windowing::WindowInfo>,
) -> ObserveStartState {
    if !options.enabled {
        return ObserveStartState::default();
    }
    let active_window = observe_target
        .cloned()
        .or_else(|| resolve_active_window_target().ok());
    let active_window_id = active_window
        .as_ref()
        .and_then(|window| window.window_ref.clone());
    let active_window_bounds = active_window.map(|window| window.bounds);
    let focused_element_id = focused_element_id_from_ax();
    ObserveStartState {
        active_window_id,
        active_window_bounds,
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
    let (active_window_id, active_window_bounds) =
        if let Some(bounds) = start_state.active_window_bounds.clone() {
            (start_state.active_window_id.clone(), Some(bounds))
        } else {
            let active_window = resolve_active_window_target().ok();
            (
                active_window
                    .as_ref()
                    .and_then(|window| window.window_ref.clone()),
                active_window.map(|window| window.bounds),
            )
        };
    let focused_element_id = focused_element_id_from_ax();
    let active_window_changed = active_window_id != start_state.active_window_id;
    let focus_changed = focused_element_id != start_state.focused_element_id;
    ObserveEndState {
        focus_changed,
        focused_element_id,
        active_window_changed,
        active_window_id,
        active_window_bounds,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ObservePostSampleAction {
    Continue,
    FirstChange,
    Settled,
    NoChange,
}

fn observe_timeout_reached(elapsed_ms: u64, timeout_ms: u64) -> bool {
    elapsed_ms >= timeout_ms
}

fn observe_post_sample_action(
    until: &ObserveUntil,
    changed_any: bool,
    significant_change: bool,
    quiet_frames: u32,
    elapsed_ms: u64,
    settle_ms: u64,
    last_change_elapsed_ms: Option<u64>,
) -> ObservePostSampleAction {
    if significant_change {
        if *until == ObserveUntil::FirstChange {
            return ObservePostSampleAction::FirstChange;
        }
        return ObservePostSampleAction::Continue;
    }

    if changed_any && quiet_frames >= OBSERVE_QUIET_FRAMES {
        if last_change_elapsed_ms.is_some_and(|ms| ms < settle_ms) {
            return ObservePostSampleAction::Continue;
        }
        return ObservePostSampleAction::Settled;
    }

    if !changed_any && *until == ObserveUntil::Stable && quiet_frames >= OBSERVE_QUIET_FRAMES {
        if elapsed_ms < settle_ms {
            return ObservePostSampleAction::Continue;
        }
        return ObservePostSampleAction::NoChange;
    }

    ObservePostSampleAction::Continue
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
    let mut sample_count = 0u64;
    let mut diff_ms_total = 0u64;

    loop {
        if observe_timeout_reached(start.elapsed().as_millis() as u64, effective_timeout_ms) {
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
            let action = observe_post_sample_action(
                &options.until,
                changed_any,
                true,
                quiet_frames,
                start.elapsed().as_millis() as u64,
                options.settle_ms,
                last_change_at.map(|t| t.elapsed().as_millis() as u64),
            );
            if action == ObservePostSampleAction::FirstChange {
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
            let action = observe_post_sample_action(
                &options.until,
                changed_any,
                false,
                quiet_frames,
                start.elapsed().as_millis() as u64,
                options.settle_ms,
                last_change_at.map(|t| t.elapsed().as_millis() as u64),
            );
            if action == ObservePostSampleAction::Settled {
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
            } else if action == ObservePostSampleAction::NoChange {
                let elapsed_ms = start.elapsed().as_millis() as u64;
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
    let change_ratio = observe_changed_pixel_ratio(&start_gray, &end_gray, OBSERVE_DIFF_THRESHOLD);
    let significant_regions: Vec<_> = frame_regions
        .into_iter()
        .filter(|region| {
            region.width.saturating_mul(region.height).max(1) >= OBSERVE_FINAL_MIN_COMPONENT_AREA
        })
        .collect();
    if significant_regions.is_empty() {
        return Vec::new();
    }

    if change_ratio >= OBSERVE_HIGH_CHANGE_RATIO {
        let mut coarse: Vec<desktop_core::protocol::Bounds> = Vec::new();
        for changed_region in significant_regions {
            let upscaled = desktop_core::protocol::Bounds {
                x: changed_region.x as f64,
                y: changed_region.y as f64,
                width: changed_region.width as f64,
                height: changed_region.height as f64,
            };
            let padded = pad_bounds(upscaled, OBSERVE_COARSE_REGION_PAD_PX);
            if let Some(clipped) = clip_to_scope(&padded, observe_scope) {
                merge_region_into_list_with_gap(
                    &mut coarse,
                    clipped,
                    OBSERVE_COARSE_MERGE_GAP_PX,
                    0.05,
                );
            }
        }
        if coarse.is_empty() {
            if let Some(scope) = observe_scope {
                return vec![scope.clone()];
            }
            return vec![desktop_core::protocol::Bounds {
                x: 0.0,
                y: 0.0,
                width: end_capture.frame.width.max(1) as f64,
                height: end_capture.frame.height.max(1) as f64,
            }];
        }
        let coarse = merge_overlapping_regions(coarse);
        return cap_regions_by_area_then_position(coarse, OBSERVE_COARSE_MAX_REGIONS);
    }

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
            merge_region_into_list_with_gap(&mut merged, clipped, OBSERVE_FINAL_MERGE_GAP_PX, 0.10);
        }
    }
    let merged = merge_overlapping_regions(merged);
    cap_regions_by_area_then_position(merged, OBSERVE_MAX_FINAL_REGIONS)
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

fn merge_region_into_list_with_gap(
    regions: &mut Vec<desktop_core::protocol::Bounds>,
    incoming: desktop_core::protocol::Bounds,
    max_gap: f64,
    min_iou: f64,
) {
    let mut merged = incoming;
    let mut idx = 0usize;
    while idx < regions.len() {
        if should_merge_regions(&regions[idx], &merged, max_gap, min_iou) {
            merged = merge_bounds(Some(&regions[idx]), &merged);
            regions.swap_remove(idx);
            continue;
        }
        idx += 1;
    }
    regions.push(merged);
}

fn should_merge_regions(
    a: &desktop_core::protocol::Bounds,
    b: &desktop_core::protocol::Bounds,
    max_gap: f64,
    min_iou: f64,
) -> bool {
    if iou(a, b) >= min_iou {
        return true;
    }
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;
    let dx = if ax2 < b.x {
        b.x - ax2
    } else if bx2 < a.x {
        a.x - bx2
    } else {
        0.0
    };
    let dy = if ay2 < b.y {
        b.y - ay2
    } else if by2 < a.y {
        a.y - by2
    } else {
        0.0
    };
    dx <= max_gap && dy <= max_gap
}

fn cap_regions_by_area_then_position(
    mut regions: Vec<desktop_core::protocol::Bounds>,
    max_regions: usize,
) -> Vec<desktop_core::protocol::Bounds> {
    if regions.len() <= max_regions {
        regions.sort_by(|a, b| {
            (a.y, a.x)
                .partial_cmp(&(b.y, b.x))
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        return regions;
    }
    regions.sort_by(|a, b| {
        let aa = a.width.max(0.0) * a.height.max(0.0);
        let bb = b.width.max(0.0) * b.height.max(0.0);
        bb.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
    });
    regions.truncate(max_regions);
    regions.sort_by(|a, b| {
        (a.y, a.x)
            .partial_cmp(&(b.y, b.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    regions
}

fn merge_overlapping_regions(
    regions: Vec<desktop_core::protocol::Bounds>,
) -> Vec<desktop_core::protocol::Bounds> {
    let mut merged: Vec<desktop_core::protocol::Bounds> = Vec::new();
    for incoming in regions {
        merge_region_into_list_overlap_only(&mut merged, incoming);
    }
    merged.sort_by(|a, b| {
        (a.y, a.x)
            .partial_cmp(&(b.y, b.x))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    merged
}

fn merge_region_into_list_overlap_only(
    regions: &mut Vec<desktop_core::protocol::Bounds>,
    incoming: desktop_core::protocol::Bounds,
) {
    let mut merged = incoming;
    let mut idx = 0usize;
    while idx < regions.len() {
        if bounds_intersect(&regions[idx], &merged) {
            merged = merge_bounds(Some(&regions[idx]), &merged);
            regions.swap_remove(idx);
            continue;
        }
        idx += 1;
    }
    regions.push(merged);
}

fn observe_changed_pixel_ratio(
    prev: &vision::diff::GrayThumbnail,
    curr: &vision::diff::GrayThumbnail,
    threshold: u8,
) -> f64 {
    if prev.width != curr.width
        || prev.height != curr.height
        || prev.pixels.len() != curr.pixels.len()
        || curr.pixels.is_empty()
    {
        return 1.0;
    }
    let mut changed = 0usize;
    for idx in 0..curr.pixels.len() {
        if prev.pixels[idx].abs_diff(curr.pixels[idx]) > threshold {
            changed += 1;
        }
    }
    changed as f64 / curr.pixels.len() as f64
}

fn pad_bounds(bounds: desktop_core::protocol::Bounds, pad: f64) -> desktop_core::protocol::Bounds {
    desktop_core::protocol::Bounds {
        x: (bounds.x - pad).max(0.0),
        y: (bounds.y - pad).max(0.0),
        width: bounds.width + pad * 2.0,
        height: bounds.height + pad * 2.0,
    }
}

#[cfg(test)]
mod tests {
    use super::token_delta::{OcrIdCandidate, remap_single_observe_ocr_id};
    use super::*;

    #[test]
    fn observe_timeout_reached_respects_threshold() {
        assert!(!observe_timeout_reached(499, 500));
        assert!(observe_timeout_reached(500, 500));
        assert!(observe_timeout_reached(777, 500));
    }

    #[test]
    fn observe_post_sample_action_first_change_branch() {
        let action = observe_post_sample_action(
            &ObserveUntil::FirstChange,
            true,
            true,
            0,
            100,
            250,
            Some(0),
        );
        assert_eq!(action, ObservePostSampleAction::FirstChange);
    }

    #[test]
    fn observe_post_sample_action_settled_after_quiet_period() {
        let action = observe_post_sample_action(
            &ObserveUntil::Stable,
            true,
            false,
            OBSERVE_QUIET_FRAMES,
            700,
            250,
            Some(400),
        );
        assert_eq!(action, ObservePostSampleAction::Settled);
    }

    #[test]
    fn observe_post_sample_action_waits_for_settle_window() {
        let action = observe_post_sample_action(
            &ObserveUntil::Stable,
            true,
            false,
            OBSERVE_QUIET_FRAMES,
            300,
            250,
            Some(20),
        );
        assert_eq!(action, ObservePostSampleAction::Continue);
    }

    #[test]
    fn observe_post_sample_action_no_change_after_settle() {
        let action = observe_post_sample_action(
            &ObserveUntil::Stable,
            false,
            false,
            OBSERVE_QUIET_FRAMES,
            300,
            250,
            None,
        );
        assert_eq!(action, ObservePostSampleAction::NoChange);
    }

    #[test]
    fn observe_transition_state_uses_bound_active_window_scope() {
        let bounds = desktop_core::protocol::Bounds {
            x: 10.0,
            y: 20.0,
            width: 300.0,
            height: 200.0,
        };
        let start_state = ObserveStartState {
            active_window_id: Some("system_settings_123".to_string()),
            active_window_bounds: Some(bounds.clone()),
            focused_element_id: None,
        };

        let end_state = observe_transition_state(&start_state);

        assert_eq!(
            end_state.active_window_id.as_deref(),
            Some("system_settings_123")
        );
        let end_bounds = end_state.active_window_bounds.expect("bound window bounds");
        assert!((end_bounds.x - bounds.x).abs() < 0.001);
        assert!((end_bounds.y - bounds.y).abs() < 0.001);
        assert!((end_bounds.width - bounds.width).abs() < 0.001);
        assert!((end_bounds.height - bounds.height).abs() < 0.001);
        assert!(!end_state.active_window_changed);
    }

    #[test]
    fn remap_single_observe_ocr_id_rejects_low_iou_match() {
        let mut token = json!({
            "id": "ocr_old",
            "source": "vision_ocr",
            "text": "Save",
            "bbox": [10.0, 10.0, 40.0, 20.0]
        });
        let candidates = vec![OcrIdCandidate {
            id: "ocr_new".to_string(),
            text_norm: "save".to_string(),
            bounds: desktop_core::protocol::Bounds {
                x: 200.0,
                y: 200.0,
                width: 40.0,
                height: 20.0,
            },
        }];
        remap_single_observe_ocr_id(&mut token, &candidates);
        assert_eq!(token.get("id").and_then(Value::as_str), Some("ocr_old"));
    }

    #[test]
    fn remap_single_observe_ocr_id_accepts_near_overlap_match() {
        let mut token = json!({
            "id": "ocr_old",
            "source": "vision_ocr",
            "text": "Save",
            "bbox": [10.0, 10.0, 40.0, 20.0]
        });
        let candidates = vec![OcrIdCandidate {
            id: "ocr_new".to_string(),
            text_norm: "save".to_string(),
            bounds: desktop_core::protocol::Bounds {
                x: 12.0,
                y: 11.0,
                width: 40.0,
                height: 20.0,
            },
        }];
        remap_single_observe_ocr_id(&mut token, &candidates);
        assert_eq!(token.get("id").and_then(Value::as_str), Some("ocr_new"));
    }

    #[test]
    fn should_merge_regions_when_close_without_overlap() {
        let a = desktop_core::protocol::Bounds {
            x: 100.0,
            y: 100.0,
            width: 40.0,
            height: 20.0,
        };
        let b = desktop_core::protocol::Bounds {
            x: 150.0,
            y: 104.0,
            width: 40.0,
            height: 20.0,
        };
        assert!(should_merge_regions(&a, &b, 16.0, 0.10));
    }

    #[test]
    fn cap_regions_by_area_then_position_limits_count() {
        let regions = vec![
            desktop_core::protocol::Bounds {
                x: 0.0,
                y: 100.0,
                width: 10.0,
                height: 10.0,
            },
            desktop_core::protocol::Bounds {
                x: 0.0,
                y: 0.0,
                width: 60.0,
                height: 20.0,
            },
            desktop_core::protocol::Bounds {
                x: 0.0,
                y: 50.0,
                width: 30.0,
                height: 20.0,
            },
        ];
        let capped = cap_regions_by_area_then_position(regions, 2);
        assert_eq!(capped.len(), 2);
    }

    #[test]
    fn observe_changed_pixel_ratio_reports_fraction() {
        let prev = vision::diff::GrayThumbnail {
            width: 2,
            height: 2,
            pixels: vec![0, 0, 0, 0],
        };
        let curr = vision::diff::GrayThumbnail {
            width: 2,
            height: 2,
            pixels: vec![0, 20, 0, 20],
        };
        let ratio = observe_changed_pixel_ratio(&prev, &curr, 8);
        assert!((ratio - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn merge_overlapping_regions_collapses_contained_boxes() {
        let regions = vec![
            desktop_core::protocol::Bounds {
                x: 100.0,
                y: 100.0,
                width: 300.0,
                height: 200.0,
            },
            desktop_core::protocol::Bounds {
                x: 150.0,
                y: 150.0,
                width: 20.0,
                height: 20.0,
            },
        ];
        let merged = merge_overlapping_regions(regions);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].x, 100.0);
        assert_eq!(merged[0].y, 100.0);
    }
}

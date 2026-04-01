use super::*;
use std::path::PathBuf;

use super::token_delta::{observe_ocr_token_id, round_nonnegative_i64};
use image::{ImageFormat, RgbaImage};

pub(super) fn observe_tokens_for_regions(
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

#[cfg(test)]
mod tests {
    use super::*;

    fn bounds(x: f64, y: f64, width: f64, height: f64) -> desktop_core::protocol::Bounds {
        desktop_core::protocol::Bounds {
            x,
            y,
            width,
            height,
        }
    }

    fn ax(role: &str, b: desktop_core::protocol::Bounds) -> platform::ax::AxElement {
        platform::ax::AxElement {
            role: role.to_string(),
            text: None,
            bounds: b,
            ax_identifier: None,
            checked: None,
        }
    }

    #[test]
    fn expand_bounds_with_pad_clamped_respects_scope_limits() {
        let core = bounds(100.0, 100.0, 40.0, 20.0);
        let scope = bounds(110.0, 90.0, 40.0, 40.0);
        let (expanded, applied) =
            expand_bounds_with_pad_clamped(&core, 30.0, 300.0, 300.0, Some(&scope));
        assert_eq!(expanded.x, 110.0);
        assert_eq!(expanded.y, 90.0);
        assert_eq!(expanded.width, 40.0);
        assert_eq!(expanded.height, 40.0);
        assert_eq!(applied.left, 0.0);
        assert_eq!(applied.right, 10.0);
        assert_eq!(applied.top, 10.0);
        assert_eq!(applied.bottom, 10.0);
    }

    #[test]
    fn observe_adaptive_ocr_pad_uses_default_when_no_matching_controls() {
        let core = bounds(100.0, 100.0, 40.0, 20.0);
        let elements = vec![
            ax("AXStaticText", bounds(100.0, 100.0, 30.0, 10.0)),
            ax("AXWindow", bounds(0.0, 0.0, 600.0, 400.0)),
        ];
        let pad = observe_adaptive_ocr_pad(&core, &elements);
        assert_eq!(pad, OBSERVE_OCR_PAD_PX);
    }

    #[test]
    fn observe_adaptive_ocr_pad_uses_smallest_whitelisted_dimension() {
        let core = bounds(100.0, 100.0, 40.0, 20.0);
        let elements = vec![
            ax("AXButton", bounds(95.0, 95.0, 30.0, 12.0)),
            ax("AXTextField", bounds(98.0, 98.0, 24.0, 24.0)),
        ];
        let pad = observe_adaptive_ocr_pad(&core, &elements);
        assert_eq!(pad, 18.0);
    }
}

use desktop_core::protocol::Bounds;
use image::RgbaImage;

use super::metal_pipeline::{self, ProcessedFrame};
use super::text_group::{
    self, TextBox, group_lines_into_paragraphs, group_words_into_lines, split_wide_textbox,
    tighten_to_content,
};

const GLYPH_MIN_W: usize = 6;
const GLYPH_MIN_H: usize = 6;
const GLYPH_MIN_AREA: usize = 24;
const GLYPH_MAX_AREA: usize = 1800;
const GLYPH_MIN_ASPECT: f64 = 0.2;
const GLYPH_MAX_ASPECT: f64 = 5.0;
const GLYPH_MAX_SIZE_PX: f64 = 44.0;
pub(crate) const GLYPH_IOU_TEXT_OVERLAP_MAX: f64 = 0.08;
const GLYPH_DEDUPE_IOU: f64 = 0.72;
const GLYPH_CAP: usize = 140;
const BOX_CAP: usize = 200;

pub(super) fn debug_enabled() -> bool {
    std::env::var("TOKENIZE_DEBUG").is_ok()
}

#[allow(dead_code)]
const fn _assert_pub_api() {}

// ── public API ──────────────────────────────────────────────────────────────

/// Detect UI element boxes without OCR text seeds (uses synthetic text detection).
pub fn detect_ui_boxes(image: &RgbaImage) -> Vec<Bounds> {
    detect_ui_boxes_with_text(image, &[])
}

/// Detect UI element boxes using text_bounds as anchor points (from OCR or synthetic).
///
/// Pipeline:
/// 1. GPU/CPU preprocessing: Sobel edge detection, integral images (SATs).
/// 2. Find text regions (OCR seeds or synthetic via thresholding).
/// 3. Group text into lines, then lines into paragraphs.
/// 4. For each text line, score candidate enclosing rectangles via O(1) SAT lookups.
/// 5. Detect empty text fields via sliding window + SAT scoring.
/// 6. Detect large containers via edge projection profiles.
pub fn detect_ui_boxes_with_text(image: &RgbaImage, text_bounds: &[Bounds]) -> Vec<Bounds> {
    detect_ui_boxes_with_labels(image, text_bounds, &[])
}

/// Same as detect_ui_boxes_with_text but accepts text strings for debug/tracking.
pub fn detect_ui_boxes_with_labels(
    image: &RgbaImage,
    text_bounds: &[Bounds],
    text_labels: &[&str],
) -> Vec<Bounds> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width < 24 || height < 24 {
        return Vec::new();
    }
    let img_w = width as f64;
    let img_h = height as f64;

    // Step 0: Preprocessing — compute edge map + integral images.
    let frame = metal_pipeline::process_cpu(image);

    // Step 1: Get word-level text regions as TextBoxes.
    let words: Vec<TextBox> = if text_bounds.len() >= 4 {
        let sanitized = sanitize_text_seeds(text_bounds, img_w, img_h);
        sanitized
            .into_iter()
            .map(|b| {
                // Match back to input text_bounds to find the text label.
                let label = text_bounds
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| {
                        overlap_1d(b.x, b.x + b.width, t.x, t.x + t.width)
                            * overlap_1d(b.y, b.y + b.height, t.y, t.y + t.height)
                            > 0.0
                    })
                    .max_by(|(_, a), (_, tb)| {
                        let ov_a = overlap_1d(b.x, b.x + b.width, a.x, a.x + a.width)
                            * overlap_1d(b.y, b.y + b.height, a.y, a.y + a.height);
                        let ov_b = overlap_1d(b.x, b.x + b.width, tb.x, tb.x + tb.width)
                            * overlap_1d(b.y, b.y + b.height, tb.y, tb.y + tb.height);
                        ov_a.partial_cmp(&ov_b).unwrap()
                    })
                    .and_then(|(i, _)| text_labels.get(i).map(|s| s.to_string()))
                    .unwrap_or_default();
                TextBox::from_bounds_with_text(b, label)
            })
            .collect()
    } else {
        text_component_boxes(&frame)
            .into_iter()
            .map(TextBox::from_bounds)
            .collect()
    };

    // Step 1b: Split overly wide OCR results at horizontal gaps.
    let words: Vec<TextBox> = words
        .into_iter()
        .flat_map(|tb| split_wide_textbox(tb, &frame))
        .map(|tb| {
            let tight = tighten_to_content(&tb.bounds, &frame);
            if debug_enabled() {
                let dx = tight.x - tb.bounds.x;
                let dy = tight.y - tb.bounds.y;
                let dw = tb.bounds.width - tight.width;
                let dh = tb.bounds.height - tight.height;
                if dx.abs() > 1.0 || dy.abs() > 1.0 || dw.abs() > 1.0 || dh.abs() > 1.0 {
                    eprintln!(
                        "  tighten {:?}: [{:.0},{:.0},{:.0},{:.0}] -> [{:.0},{:.0},{:.0},{:.0}]",
                        tb.text,
                        tb.bounds.x,
                        tb.bounds.y,
                        tb.bounds.width,
                        tb.bounds.height,
                        tight.x,
                        tight.y,
                        tight.width,
                        tight.height
                    );
                }
            }
            TextBox::from_bounds_with_text(tight, tb.text)
        })
        .collect();

    // Step 2: Group words → lines → paragraphs.
    let lines = group_words_into_lines(&words);
    let paragraphs = group_lines_into_paragraphs(&lines);

    let mut out: Vec<Bounds> = Vec::new();

    // Emit paragraphs.
    for para in &paragraphs {
        out.push(para.bounds.clone());
    }

    // Step 3: For each text line, find best enclosing control via SAT scoring.
    // Also track which lines have enclosing controls.
    let mut lines_with_controls = vec![false; lines.len()];
    for (i, line) in lines.iter().enumerate() {
        if let Some(control) = find_enclosing_control(&frame, &line.bounds) {
            out.push(control);
            lines_with_controls[i] = true;
        }
    }
    // Emit lines that don't have enclosing controls (standalone text).
    for (i, line) in lines.iter().enumerate() {
        if !lines_with_controls[i] {
            out.push(line.bounds.clone());
        }
    }

    // Step 4: Detect rectangular controls directly from edge image.
    // This catches text fields, buttons, and other bordered controls
    // regardless of whether they contain text.
    out.extend(detect_edge_rectangles(&frame));

    // Step 5: Detect containers via edge projections.
    out.extend(detect_containers(&frame));

    // Dedupe and clean up.
    let mut deduped = dedupe_boxes(out, 0.70);
    deduped = prune_nested_glitch_boxes(deduped);
    if deduped.len() > BOX_CAP {
        deduped.truncate(BOX_CAP);
    }
    deduped
}

// ── Step 1: Text detection ──────────────────────────────────────────────────

/// Shrink an OCR bounding box to tightly fit actual text content.
///
/// Two-pass approach:
/// 1. Try text_mask first (binary threshold of dark/light text) — this excludes
///    UI borders like pill backgrounds, rounded corners, etc.
/// 2. Fall back to edge image if text_mask gives nothing (e.g. low-contrast text).
fn sanitize_text_seeds(text_bounds: &[Bounds], img_w: f64, img_h: f64) -> Vec<Bounds> {
    let image_area = (img_w * img_h).max(1.0);
    let mut out: Vec<Bounds> = text_bounds
        .iter()
        .filter_map(|t| clamp_bounds(t, img_w, img_h))
        .filter(|t| {
            let area = t.width * t.height;
            area >= 8.0
                && area <= image_area * 0.06
                && t.width <= img_w * 0.90
                && t.height <= img_h * 0.28
        })
        .collect();
    out = dedupe_boxes(out, 0.96);
    out
}

fn text_component_boxes(frame: &ProcessedFrame) -> Vec<Bounds> {
    connected_component_boxes(
        &frame.text_mask,
        frame.width,
        frame.height,
        10,
        8,
        70,
        frame.width * frame.height / 6,
        0.2,
        30.0,
    )
}

// ── Step 3: SAT-based enclosing control detection ───────────────────────────

/// For a text line, scan outward in each direction using SAT strip queries
/// to find the border position with peak edge energy. Returns the enclosing box.
fn find_enclosing_control(frame: &ProcessedFrame, text: &Bounds) -> Option<Bounds> {
    let img_w = frame.width as f64;
    let img_h = frame.height as f64;
    let font_h = text.height.max(1.0);
    let w = frame.width;
    let h = frame.height;

    // Expansion limits relative to font size.
    let max_x = (font_h * 10.0).min(img_w * 0.4);
    let max_y = (font_h * 4.0).min(img_h * 0.2);

    // Strip thickness for edge detection (relative to font).
    let strip_thick = (font_h * 0.2).max(2.0).min(6.0) as usize;

    // Scan left: find column with peak horizontal edge energy.
    let left = scan_peak_edge_h(frame, text, -1, max_x, strip_thick);
    // Scan right.
    let right = scan_peak_edge_h(frame, text, 1, max_x, strip_thick);
    // Scan up.
    let top = scan_peak_edge_v(frame, text, -1, max_y, strip_thick);
    // Scan down.
    let bottom = scan_peak_edge_v(frame, text, 1, max_y, strip_thick);

    let cw = right - left;
    let ch = bottom - top;

    // Must have expanded meaningfully beyond text.
    if cw < text.width + font_h * 0.3 || ch < text.height + font_h * 0.15 {
        return None;
    }
    // Don't accept if expansion hit max in all 4 directions (no border found).
    let expanded_left = text.x - left;
    let expanded_right = right - (text.x + text.width);
    let expanded_top = text.y - top;
    let expanded_bottom = bottom - (text.y + text.height);
    let max_limit = max_x.min(max_y);
    let at_max_count = [expanded_left, expanded_right, expanded_top, expanded_bottom]
        .iter()
        .filter(|e| **e >= max_limit * 0.95)
        .count();
    if at_max_count >= 3 {
        return None; // No border found in most directions.
    }

    let candidate = Bounds {
        x: left.max(0.0),
        y: top.max(0.0),
        width: cw.min(img_w),
        height: ch.min(img_h),
    };

    // Validate: border energy should exceed interior.
    let corner_skip = (font_h * 0.3) as usize;
    let border_e = frame.border_energy(&candidate, strip_thick, corner_skip);
    let interior_e = frame.interior_edge_energy(&candidate);
    let glow_e = frame.glow_energy(&candidate, (font_h * 0.4) as usize);

    let effective_border = border_e + glow_e * 0.5;
    let noise_floor = interior_e.max(0.5);

    if effective_border / noise_floor < 1.3 && border_e < 4.0 {
        return None;
    }

    clamp_bounds(&candidate, img_w, img_h)
}

/// Scan outward horizontally from text edge, find the FIRST significant edge
/// using SAT strip queries. Returns x-coordinate of detected border.
fn scan_peak_edge_h(
    frame: &ProcessedFrame,
    text: &Bounds,
    direction: i32, // -1=left, +1=right
    max_expand: f64,
    strip_thick: usize,
) -> f64 {
    let w = frame.width;
    let h = frame.height;

    // Vertical range: extend slightly beyond text to catch borders.
    let y1 = (text.y as usize).max(1).min(h - 2);
    let y2 = ((text.y + text.height) as usize).min(h - 2);
    if y2 <= y1 {
        return if direction < 0 { text.x } else { text.x + text.width };
    }

    let start_x = if direction < 0 {
        (text.x as usize).max(1)
    } else {
        ((text.x + text.width) as usize).min(w - 2)
    };

    // Compute rolling baseline: average edge energy in a window behind us.
    let limit = max_expand as usize;
    let mut running_sum = 0.0f64;
    let mut running_count = 0usize;

    for step in 1..=limit {
        let x = if direction < 0 {
            if start_x < step { break; }
            start_x - step
        } else {
            let nx = start_x + step;
            if nx + strip_thick >= w { break; }
            nx
        };

        let sx1 = if direction < 0 { x.saturating_sub(strip_thick / 2) } else { x };
        let sx2 = (sx1 + strip_thick).min(w - 1);
        if sx2 <= sx1 { continue; }

        let e = frame.rect_sum(&frame.edge_sat, sx1, y1, sx2 - 1, y2 - 1);
        let pixels = (sx2 - sx1) as f64 * (y2 - y1) as f64;
        let mean_e = e / pixels.max(1.0);

        running_sum += mean_e;
        running_count += 1;
        let running_avg = running_sum / running_count as f64;

        // First significant edge: either absolute threshold or Nx above running average.
        if mean_e >= 4.0 && (mean_e >= running_avg * 2.5 || mean_e >= 8.0) {
            return x as f64;
        }
    }

    // No clear edge found.
    if direction < 0 {
        (text.x - max_expand).max(0.0)
    } else {
        (text.x + text.width + max_expand).min(w as f64)
    }
}

/// Scan outward vertically from text edge, find the FIRST significant edge.
fn scan_peak_edge_v(
    frame: &ProcessedFrame,
    text: &Bounds,
    direction: i32,
    max_expand: f64,
    strip_thick: usize,
) -> f64 {
    let w = frame.width;
    let h = frame.height;

    let x1 = (text.x as usize).max(1).min(w - 2);
    let x2 = ((text.x + text.width) as usize).min(w - 2);
    if x2 <= x1 {
        return if direction < 0 { text.y } else { text.y + text.height };
    }

    let start_y = if direction < 0 {
        (text.y as usize).max(1)
    } else {
        ((text.y + text.height) as usize).min(h - 2)
    };

    let limit = max_expand as usize;
    let mut running_sum = 0.0f64;
    let mut running_count = 0usize;

    for step in 1..=limit {
        let y = if direction < 0 {
            if start_y < step { break; }
            start_y - step
        } else {
            let ny = start_y + step;
            if ny + strip_thick >= h { break; }
            ny
        };

        let sy1 = if direction < 0 { y.saturating_sub(strip_thick / 2) } else { y };
        let sy2 = (sy1 + strip_thick).min(h - 1);
        if sy2 <= sy1 { continue; }

        let e = frame.rect_sum(&frame.edge_sat, x1, sy1, x2 - 1, sy2 - 1);
        let pixels = (x2 - x1) as f64 * (sy2 - sy1) as f64;
        let mean_e = e / pixels.max(1.0);

        running_sum += mean_e;
        running_count += 1;
        let running_avg = running_sum / running_count as f64;

        if mean_e >= 4.0 && (mean_e >= running_avg * 2.5 || mean_e >= 8.0) {
            return y as f64;
        }
    }

    if direction < 0 {
        (text.y - max_expand).max(0.0)
    } else {
        (text.y + text.height + max_expand).min(h as f64)
    }
}

// ── Step 4: Empty text field detection ──────────────────────────────────────

/// Detect empty text fields by sliding a window across the image.
/// An empty text field has: strong border + uniform (low-variance) interior +
/// text-field-like aspect ratio.
fn detect_empty_text_fields(frame: &ProcessedFrame) -> Vec<Bounds> {
    let w = frame.width;
    let h = frame.height;
    if w < 100 || h < 40 {
        return Vec::new();
    }

    let mut candidates: Vec<(f64, Bounds)> = Vec::new();

    // Try various text-field sizes.
    // Heights typical for text fields: 24-50px.
    // Widths: 80-600px.
    let heights = [24, 30, 36, 42, 50];
    let widths = [80, 120, 160, 200, 260, 340, 440, 560];

    let stride_x = 8;
    let stride_y = 6;

    for &th in &heights {
        if th >= h {
            continue;
        }
        for &tw in &widths {
            if tw >= w {
                continue;
            }
            let mut y = 0;
            while y + th < h {
                let mut x = 0;
                while x + tw < w {
                    let candidate = Bounds {
                        x: x as f64,
                        y: y as f64,
                        width: tw as f64,
                        height: th as f64,
                    };

                    let corner_skip = (th as f64 * 0.2) as usize;
                    let strip_w = 2.max((th as f64 * 0.08) as usize);
                    let border_e = frame.border_energy(&candidate, strip_w, corner_skip);
                    let interior_var = frame.interior_variance(&candidate);
                    let interior_edge = frame.interior_edge_energy(&candidate);

                    // Empty text field: strong border, very uniform interior, low interior edges.
                    let noise_floor = interior_edge.max(0.5);
                    let contrast = border_e / noise_floor;

                    if contrast >= 3.0 && interior_var < 150.0 && border_e >= 5.0 {
                        let score = contrast * (150.0 - interior_var).max(1.0);
                        candidates.push((score, candidate));
                    }

                    x += stride_x;
                }
                y += stride_y;
            }
        }
    }

    // NMS: keep top-scoring, non-overlapping candidates.
    candidates.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    let mut kept = Vec::new();
    for (_, candidate) in candidates {
        if kept.len() >= 20 {
            break;
        }
        let dominated = kept.iter().any(|existing: &Bounds| iou(&candidate, existing) >= 0.30);
        if !dominated {
            kept.push(candidate);
        }
    }
    kept
}

// ── Step 4b: Edge rectangle detection ───────────────────────────────────────

/// Detect rectangular UI controls (text fields, buttons, cards) directly from
/// the Sobel edge image. Threshold edges to binary, find connected components
/// that form rectangular shapes with control-like dimensions.
fn detect_edge_rectangles(frame: &ProcessedFrame) -> Vec<Bounds> {
    let w = frame.width;
    let h = frame.height;
    if w < 40 || h < 20 {
        return Vec::new();
    }

    // Threshold edge image to binary.
    let edge_threshold = 12u8;
    let mut edge_mask = vec![false; w * h];
    for i in 0..w * h {
        edge_mask[i] = frame.edge[i] >= edge_threshold;
    }

    // Connect nearby edge pixels into border strokes.
    let edge_mask = dilate_rect(&edge_mask, w, h, 2, 2);
    let edge_mask = erode_rect(&edge_mask, w, h, 1, 1);

    // Find connected components with control-like dimensions.
    // Max area capped: we want individual controls, not full dialog outlines.
    let max_control_area = (w * h / 8).min(200_000);
    let mut boxes = connected_component_boxes(
        &edge_mask, w, h,
        16,   // min_w (small enough for icons)
        16,   // min_h
        200,  // min_area
        max_control_area, // max_area
        0.15, // min_aspect (allow tall-ish elements)
        20.0, // max_aspect
    );

    // Strict validation: border energy must clearly exceed interior.
    // This eliminates phantom rectangles from noise in dark UIs.
    boxes.retain(|b| {
        let border_e = frame.border_energy(b, 3, 4);
        let interior_e = frame.interior_edge_energy(b);
        let interior_var = frame.interior_variance(b);
        let interior_mean = frame.interior_mean(b);

        // Border must have more edge energy than interior.
        let border_ok = border_e >= 6.0 && border_e >= interior_e * 1.3;
        // Interior should be relatively uniform.
        let uniform_ok = interior_var < 1500.0;

        // On dark backgrounds, require stronger borders — but only for
        // larger boxes. Small controls (icons, buttons) have strong borders
        // relative to their size, so don't penalize them.
        let area = b.width * b.height;
        let is_small = area < 6000.0; // roughly 80×75 or smaller
        let dark_bg = interior_mean < 80.0;
        let dark_ok = is_small || !dark_bg || (border_e >= 10.0 && border_e >= interior_e * 1.8);

        border_ok && uniform_ok && dark_ok
    });

    // Filter out boxes spanning too much of the image (window chrome, not controls).
    let img_w = w as f64;
    let img_h = h as f64;
    boxes.retain(|b| {
        b.width < img_w * 0.55 && b.height < img_h * 0.55
    });

    dedupe_boxes(boxes, 0.50)
}

// ── Step 5: Container detection ─────────────────────────────────────────────

/// Detect containers by finding horizontal and vertical edge lines from SAT,
/// then forming rectangles from pairs.
fn detect_containers(frame: &ProcessedFrame) -> Vec<Bounds> {
    let w = frame.width;
    let h = frame.height;
    if w < 100 || h < 100 {
        return Vec::new();
    }

    let strip_h = 3; // Height of horizontal strip to check for edge lines.

    // Find strong horizontal edge lines: rows where edge energy is consistently high.
    let mut h_lines: Vec<usize> = Vec::new();
    for y in strip_h..h - strip_h {
        // Use SAT to compute mean edge energy across the row.
        let x1 = 0;
        let x2 = w - 1;
        let e = frame.rect_sum(&frame.edge_sat, x1, y, x2, y + strip_h - 1);
        let mean_e = e / (w * strip_h) as f64;
        // A horizontal line should have mean edge energy above threshold.
        if mean_e >= 12.0 {
            h_lines.push(y);
        }
    }

    // Find strong vertical edge lines.
    let strip_w = 3;
    let mut v_lines: Vec<usize> = Vec::new();
    for x in strip_w..w - strip_w {
        let y1 = 0;
        let y2 = h - 1;
        let e = frame.rect_sum(&frame.edge_sat, x, y1, x + strip_w - 1, y2);
        let mean_e = e / (h * strip_w) as f64;
        if mean_e >= 12.0 {
            v_lines.push(x);
        }
    }

    // Compress nearby lines.
    let h_edges = compress_indices(&h_lines, 8);
    let v_edges = compress_indices(&v_lines, 8);

    // Form rectangles from h-edge pairs × v-edge pairs.
    let mut containers = Vec::new();
    for i in 0..h_edges.len() {
        for j in (i + 1)..h_edges.len() {
            let y1 = h_edges[i];
            let y2 = h_edges[j];
            let rh = y2 - y1;
            if rh < 60 || rh > h * 7 / 10 {
                continue;
            }
            for k in 0..v_edges.len() {
                for l in (k + 1)..v_edges.len() {
                    let x1 = v_edges[k];
                    let x2 = v_edges[l];
                    let rw = x2 - x1;
                    if rw < 80 || rw > w * 7 / 10 {
                        continue;
                    }
                    let candidate = Bounds {
                        x: x1 as f64,
                        y: y1 as f64,
                        width: rw as f64,
                        height: rh as f64,
                    };
                    // Validate with SAT: border energy should clearly exceed interior.
                    let border_e = frame.border_energy(&candidate, 3, 4);
                    let interior_e = frame.interior_edge_energy(&candidate);
                    if border_e >= 8.0 && border_e >= interior_e * 1.5 {
                        containers.push(candidate);
                    }
                }
            }
        }
    }
    dedupe_boxes(containers, 0.50)
}

// ── Glyph detection (icons) ─────────────────────────────────────────────────

pub fn detect_glyphs(image: &RgbaImage, text_bounds: &[Bounds]) -> Vec<Bounds> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width < 16 || height < 16 {
        return Vec::new();
    }
    let gray = grayscale(image);
    let mean_luma = gray.iter().map(|v| *v as f64).sum::<f64>() / gray.len().max(1) as f64;
    let mut mask = vec![false; width * height];
    if mean_luma >= 128.0 {
        for i in 0..mask.len() {
            mask[i] = gray[i] <= 110;
        }
    } else {
        for i in 0..mask.len() {
            mask[i] = gray[i] >= 164;
        }
    }
    let mask = erode_rect(&mask, width, height, 1, 1);
    let mask = dilate_rect(&mask, width, height, 1, 1);
    let mask = dilate_rect(&mask, width, height, 1, 1);
    let mask = erode_rect(&mask, width, height, 1, 1);
    let mut glyphs = connected_component_boxes(
        &mask,
        width,
        height,
        GLYPH_MIN_W,
        GLYPH_MIN_H,
        GLYPH_MIN_AREA,
        GLYPH_MAX_AREA,
        GLYPH_MIN_ASPECT,
        GLYPH_MAX_ASPECT,
    );
    glyphs.retain(|glyph| {
        glyph.width <= GLYPH_MAX_SIZE_PX
            && glyph.height <= GLYPH_MAX_SIZE_PX
            && !overlaps_text(glyph, text_bounds)
            && !inside_text_padding(glyph, text_bounds, 2.0)
            && near_text(glyph, text_bounds)
            && mask_fill_density(&mask, width, glyph) >= 0.16
    });
    let mut deduped = dedupe_boxes(glyphs, GLYPH_DEDUPE_IOU);
    if deduped.len() > GLYPH_CAP {
        deduped.truncate(GLYPH_CAP);
    }
    deduped
}

// ── Low-level utilities ─────────────────────────────────────────────────────

fn grayscale(image: &RgbaImage) -> Vec<u8> {
    image
        .pixels()
        .map(|p| {
            let [r, g, b, _a] = p.0;
            (0.299 * (r as f32) + 0.587 * (g as f32) + 0.114 * (b as f32)).round() as u8
        })
        .collect()
}

fn connected_component_boxes(
    mask: &[bool],
    width: usize,
    height: usize,
    min_w: usize,
    min_h: usize,
    min_area: usize,
    max_area: usize,
    min_aspect: f64,
    max_aspect: f64,
) -> Vec<Bounds> {
    let mut visited = vec![false; mask.len()];
    let mut boxes = Vec::new();
    for y in 0..height {
        for x in 0..width {
            let start = y * width + x;
            if visited[start] || !mask[start] {
                continue;
            }
            let mut queue = std::collections::VecDeque::from([(x, y)]);
            visited[start] = true;
            let (mut min_x, mut max_x, mut min_y, mut max_y) = (x, x, y, y);
            let mut pixels = 0usize;
            while let Some((cx, cy)) = queue.pop_front() {
                pixels += 1;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);
                for (nx, ny) in [
                    (cx + 1, cy),
                    (cx.wrapping_sub(1), cy),
                    (cx, cy + 1),
                    (cx, cy.wrapping_sub(1)),
                ] {
                    if nx < width && ny < height {
                        let n = ny * width + nx;
                        if !visited[n] && mask[n] {
                            visited[n] = true;
                            queue.push_back((nx, ny));
                        }
                    }
                }
            }
            let box_w = max_x - min_x + 1;
            let box_h = max_y - min_y + 1;
            let area = box_w * box_h;
            if box_w < min_w || box_h < min_h || area < min_area || area > max_area {
                continue;
            }
            let aspect = box_w as f64 / box_h as f64;
            if !(min_aspect..=max_aspect).contains(&aspect) {
                continue;
            }
            if pixels < min_area / 4 {
                continue;
            }
            if area > (width * height) / 6 && pixels < area / 8 {
                continue;
            }
            boxes.push(Bounds {
                x: min_x as f64,
                y: min_y as f64,
                width: box_w as f64,
                height: box_h as f64,
            });
        }
    }
    boxes
}

#[allow(dead_code)]
fn dilate_rect(mask: &[bool], width: usize, height: usize, rx: usize, ry: usize) -> Vec<bool> {
    let mut out = vec![false; width * height];
    for y in 0..height {
        let y0 = y.saturating_sub(ry);
        let y1 = (y + ry).min(height - 1);
        for x in 0..width {
            let x0 = x.saturating_sub(rx);
            let x1 = (x + rx).min(width - 1);
            'search: for sy in y0..=y1 {
                for sx in x0..=x1 {
                    if mask[sy * width + sx] {
                        out[y * width + x] = true;
                        break 'search;
                    }
                }
            }
        }
    }
    out
}

#[allow(dead_code)]
fn erode_rect(mask: &[bool], width: usize, height: usize, rx: usize, ry: usize) -> Vec<bool> {
    let mut out = vec![true; width * height];
    for y in 0..height {
        let y0 = y.saturating_sub(ry);
        let y1 = (y + ry).min(height - 1);
        for x in 0..width {
            let x0 = x.saturating_sub(rx);
            let x1 = (x + rx).min(width - 1);
            'search: for sy in y0..=y1 {
                for sx in x0..=x1 {
                    if !mask[sy * width + sx] {
                        out[y * width + x] = false;
                        break 'search;
                    }
                }
            }
        }
    }
    out
}

fn compress_indices(indices: &[usize], max_gap: usize) -> Vec<usize> {
    if indices.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut run_start = indices[0];
    let mut prev = indices[0];
    for &idx in &indices[1..] {
        if idx - prev > max_gap {
            out.push((run_start + prev) / 2);
            run_start = idx;
        }
        prev = idx;
    }
    out.push((run_start + prev) / 2);
    out
}

pub(super) fn overlap_1d(a1: f64, a2: f64, b1: f64, b2: f64) -> f64 {
    (a2.min(b2) - a1.max(b1)).max(0.0)
}

pub(super) fn axis_gap(a1: f64, a2: f64, b1: f64, b2: f64) -> f64 {
    if a2 < b1 {
        b1 - a2
    } else if b2 < a1 {
        a1 - b2
    } else {
        0.0
    }
}

pub(super) fn union_bounds(a: &Bounds, b: &Bounds) -> Bounds {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    let x2 = (a.x + a.width).max(b.x + b.width);
    let y2 = (a.y + a.height).max(b.y + b.height);
    Bounds {
        x,
        y,
        width: x2 - x,
        height: y2 - y,
    }
}

fn clamp_bounds(bounds: &Bounds, img_w: f64, img_h: f64) -> Option<Bounds> {
    let x = bounds.x.max(0.0);
    let y = bounds.y.max(0.0);
    let w = (bounds.width).min(img_w - x);
    let h = (bounds.height).min(img_h - y);
    if w < 2.0 || h < 2.0 {
        None
    } else {
        Some(Bounds {
            x,
            y,
            width: w,
            height: h,
        })
    }
}

#[allow(dead_code)]
fn rect_contains(outer: &Bounds, inner: &Bounds, tolerance: f64) -> bool {
    outer.x <= inner.x + tolerance
        && outer.y <= inner.y + tolerance
        && outer.x + outer.width >= inner.x + inner.width - tolerance
        && outer.y + outer.height >= inner.y + inner.height - tolerance
}

fn iou(a: &Bounds, b: &Bounds) -> f64 {
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = (a.x + a.width).min(b.x + b.width);
    let iy2 = (a.y + a.height).min(b.y + b.height);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    let inter = iw * ih;
    if inter <= 0.0 {
        return 0.0;
    }
    let union = (a.width * a.height) + (b.width * b.height) - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

fn dedupe_boxes(mut boxes: Vec<Bounds>, iou_threshold: f64) -> Vec<Bounds> {
    boxes.sort_by(|a, b| {
        let aa = a.width * a.height;
        let bb = b.width * b.height;
        bb.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut kept = Vec::new();
    'next: for candidate in boxes {
        if candidate.width < 2.0 || candidate.height < 2.0 {
            continue;
        }
        for existing in &kept {
            if iou(&candidate, existing) >= iou_threshold {
                continue 'next;
            }
        }
        kept.push(candidate);
    }
    kept.sort_by(|a, b| {
        a.y.partial_cmp(&b.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });
    kept
}

fn prune_nested_glitch_boxes(mut boxes: Vec<Bounds>) -> Vec<Bounds> {
    boxes.sort_by(|a, b| {
        let aa = a.width * a.height;
        let bb = b.width * b.height;
        aa.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut kept: Vec<Bounds> = Vec::new();
    'next: for candidate in boxes {
        for existing in &kept {
            if is_immediately_nested(existing, &candidate) {
                continue 'next;
            }
        }
        kept.push(candidate);
    }
    kept.sort_by(|a, b| {
        a.y.partial_cmp(&b.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });
    kept
}

fn is_immediately_nested(inner: &Bounds, outer: &Bounds) -> bool {
    if !rect_contains(outer, inner, 1.0) {
        return false;
    }
    let inner_area = (inner.width * inner.height).max(1.0);
    let outer_area = (outer.width * outer.height).max(1.0);
    let ratio = inner_area / outer_area;
    if ratio < 0.70 {
        return false;
    }
    let left = (inner.x - outer.x).max(0.0);
    let right = ((outer.x + outer.width) - (inner.x + inner.width)).max(0.0);
    let top = (inner.y - outer.y).max(0.0);
    let bottom = ((outer.y + outer.height) - (inner.y + inner.height)).max(0.0);
    let max_gap = left.max(right).max(top).max(bottom);
    max_gap <= (inner.height * 0.95).clamp(8.0, 26.0)
}

fn overlaps_text(candidate: &Bounds, texts: &[Bounds]) -> bool {
    texts
        .iter()
        .any(|text| iou(candidate, text) >= GLYPH_IOU_TEXT_OVERLAP_MAX)
}

fn inside_text_padding(candidate: &Bounds, texts: &[Bounds], padding: f64) -> bool {
    let cx = candidate.x + candidate.width / 2.0;
    let cy = candidate.y + candidate.height / 2.0;
    texts.iter().any(|text| {
        cx >= text.x - padding
            && cx <= text.x + text.width + padding
            && cy >= text.y - padding
            && cy <= text.y + text.height + padding
    })
}

fn near_text(candidate: &Bounds, texts: &[Bounds]) -> bool {
    if texts.is_empty() {
        return true; // No text context — don't filter by proximity.
    }
    let cx = candidate.x + candidate.width / 2.0;
    let cy = candidate.y + candidate.height / 2.0;
    texts.iter().any(|text| {
        let margin = (text.height * 2.4).clamp(18.0, 120.0);
        cx >= text.x - margin
            && cx <= text.x + text.width + margin
            && cy >= text.y - margin
            && cy <= text.y + text.height + margin
    })
}

fn mask_fill_density(mask: &[bool], width: usize, bounds: &Bounds) -> f64 {
    if width == 0 {
        return 0.0;
    }
    let height = mask.len() / width;
    let x1 = bounds.x.floor().max(0.0) as usize;
    let y1 = bounds.y.floor().max(0.0) as usize;
    let x2 = (bounds.x + bounds.width).ceil().min(width as f64) as usize;
    let y2 = (bounds.y + bounds.height).ceil().min(height as f64) as usize;
    if x2 <= x1 || y2 <= y1 {
        return 0.0;
    }
    let mut on = 0usize;
    for y in y1..y2 {
        let row = y * width;
        for x in x1..x2 {
            if mask[row + x] {
                on += 1;
            }
        }
    }
    on as f64 / ((x2 - x1) * (y2 - y1)).max(1) as f64
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prune_nested_glitch_boxes_works() {
        let inner = Bounds { x: 102.0, y: 84.0, width: 280.0, height: 88.0 };
        let outer = Bounds { x: 94.0, y: 76.0, width: 296.0, height: 104.0 };
        let kept = prune_nested_glitch_boxes(vec![outer.clone(), inner.clone()]);
        assert_eq!(kept.len(), 1);
        assert!(iou(&kept[0], &inner) > 0.95);
    }
}

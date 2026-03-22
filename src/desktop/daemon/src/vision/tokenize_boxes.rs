use desktop_core::protocol::Bounds;
use image::RgbaImage;

use super::metal_pipeline::{self, ProcessedFrame};

/// Internal box representation that carries optional text through the pipeline.
#[derive(Debug, Clone)]
struct TextBox {
    bounds: Bounds,
    text: String,
}

impl TextBox {
    fn from_bounds(b: Bounds) -> Self {
        Self {
            bounds: b,
            text: String::new(),
        }
    }

    fn from_bounds_with_text(b: Bounds, text: String) -> Self {
        Self { bounds: b, text }
    }

    fn merge(items: &[TextBox]) -> TextBox {
        let merged_bounds = items
            .iter()
            .skip(1)
            .fold(items[0].bounds.clone(), |acc, tb| {
                union_bounds(&acc, &tb.bounds)
            });
        // Sort by x position and join texts with space.
        let mut sorted: Vec<&TextBox> = items.iter().collect();
        sorted.sort_by(|a, b| {
            a.bounds
                .x
                .partial_cmp(&b.bounds.x)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        let text = sorted
            .iter()
            .map(|tb| tb.text.as_str())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join(" ");
        TextBox {
            bounds: merged_bounds,
            text,
        }
    }
}

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

fn debug_enabled() -> bool {
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
                let tight = tighten_to_content(&b, &frame);
                if debug_enabled() {
                    let dx = tight.x - b.x;
                    let dy = tight.y - b.y;
                    let dw = b.width - tight.width;
                    let dh = b.height - tight.height;
                    if dx.abs() > 1.0 || dy.abs() > 1.0 || dw.abs() > 1.0 || dh.abs() > 1.0 {
                        eprintln!(
                            "  tighten {:?}: [{:.0},{:.0},{:.0},{:.0}] -> [{:.0},{:.0},{:.0},{:.0}]",
                            label, b.x, b.y, b.width, b.height,
                            tight.x, tight.y, tight.width, tight.height
                        );
                    }
                }
                TextBox::from_bounds_with_text(tight, label)
            })
            .collect()
    } else {
        text_component_boxes(&frame)
            .into_iter()
            .map(TextBox::from_bounds)
            .collect()
    };

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
/// Uses the edge image (Sobel magnitude) which works for any text contrast.
/// A row/column is considered "has content" if any pixel exceeds the threshold.
fn tighten_to_content(bounds: &Bounds, frame: &ProcessedFrame) -> Bounds {
    let x1 = (bounds.x as usize).min(frame.width.saturating_sub(1));
    let y1 = (bounds.y as usize).min(frame.height.saturating_sub(1));
    let x2 = ((bounds.x + bounds.width) as usize).min(frame.width);
    let y2 = ((bounds.y + bounds.height) as usize).min(frame.height);
    if x2 <= x1 + 2 || y2 <= y1 + 2 {
        return bounds.clone();
    }

    // Edge threshold: pixel has content if edge magnitude > this.
    const EDGE_THRESH: u8 = 12;

    let has_content = |x: usize, y: usize| -> bool {
        frame.edge[y * frame.width + x] > EDGE_THRESH
    };

    // Scan from top.
    let mut top = y1;
    'top: for y in y1..y2 {
        for x in x1..x2 {
            if has_content(x, y) {
                top = y;
                break 'top;
            }
        }
    }

    // Scan from bottom.
    let mut bottom = y2;
    'bottom: for y in (y1..y2).rev() {
        for x in x1..x2 {
            if has_content(x, y) {
                bottom = y + 1;
                break 'bottom;
            }
        }
    }

    // Scan from left.
    let mut left = x1;
    'left: for x in x1..x2 {
        for y in top..bottom {
            if has_content(x, y) {
                left = x;
                break 'left;
            }
        }
    }

    // Scan from right.
    let mut right = x2;
    'right: for x in (x1..x2).rev() {
        for y in top..bottom {
            if has_content(x, y) {
                right = x + 1;
                break 'right;
            }
        }
    }

    // Don't tighten if result is too small (< 50% of original in either dimension).
    if right <= left || bottom <= top {
        return bounds.clone();
    }
    let new_w = (right - left) as f64;
    let new_h = (bottom - top) as f64;
    if new_w < bounds.width * 0.5 || new_h < bounds.height * 0.5 {
        return bounds.clone();
    }

    Bounds {
        x: left as f64,
        y: top as f64,
        width: new_w,
        height: new_h,
    }
}

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

// ── Step 2: Grouping ────────────────────────────────────────────────────────

/// Group word boxes into text lines based on vertical overlap and horizontal proximity.
/// All thresholds are relative to text height (font size proxy).
fn group_words_into_lines(words: &[TextBox]) -> Vec<TextBox> {
    let dbg = debug_enabled();
    if dbg {
        eprintln!("\n--- group_words_into_lines: {} words ---", words.len());
    }

    let clusters = cluster_textboxes(words, |a, b| {
        let min_h = a.bounds.height.min(b.bounds.height).max(1.0);
        let max_h = a.bounds.height.max(b.bounds.height);
        // Same font size: height ratio ≤ 1.6.
        if max_h > min_h * 1.6 {
            return false;
        }
        let vertical_overlap = overlap_1d(a.bounds.y, a.bounds.y + a.bounds.height, b.bounds.y, b.bounds.y + b.bounds.height);
        let vertical_ratio = vertical_overlap / min_h;
        let hgap = axis_gap(a.bounds.x, a.bounds.x + a.bounds.width, b.bounds.x, b.bounds.x + b.bounds.width);
        // Horizontal gap relative to font height.
        // Tight threshold to avoid merging tab-bar-style spaced items.
        vertical_ratio >= 0.40 && hgap <= min_h * 1.05
    });

    let mut lines = Vec::new();
    for (ci, cluster) in clusters.iter().enumerate() {
        if cluster.is_empty() {
            continue;
        }
        let merged = TextBox::merge(cluster);
        // Skip very tiny isolated blobs (single glyph-sized).
        let font_h = cluster.iter().map(|w| w.bounds.height).sum::<f64>() / cluster.len() as f64;
        if cluster.len() == 1 && merged.bounds.width < font_h * 1.2 {
            if dbg {
                eprintln!(
                    "  line {:2}: SKIP (tiny) [{:.0},{:.0},{:.0},{:.0}]  {:?}",
                    ci, merged.bounds.x, merged.bounds.y, merged.bounds.width, merged.bounds.height,
                    merged.text
                );
            }
            continue;
        }
        if dbg {
            eprintln!(
                "  line {:2}: {} words  [{:.0},{:.0},{:.0},{:.0}]  font_h={:.0}  {:?}",
                ci, cluster.len(), merged.bounds.x, merged.bounds.y,
                merged.bounds.width, merged.bounds.height, font_h,
                merged.text
            );
        }
        lines.push(merged);
    }
    if dbg {
        eprintln!("  => {} lines", lines.len());
    }
    lines
}

/// Group text lines into paragraphs — lines with similar font size, small vertical
/// gap relative to font size, and horizontal overlap or left-alignment.
fn group_lines_into_paragraphs(lines: &[TextBox]) -> Vec<TextBox> {
    let dbg = debug_enabled();
    if dbg {
        eprintln!("\n--- group_lines_into_paragraphs: {} lines ---", lines.len());
    }

    let clusters = cluster_textboxes(lines, |a, b| {
        let min_h = a.bounds.height.min(b.bounds.height).max(1.0);
        let max_h = a.bounds.height.max(b.bounds.height);
        // Same font size.
        if max_h > min_h * 1.6 {
            return false;
        }
        let vgap = axis_gap(a.bounds.y, a.bounds.y + a.bounds.height, b.bounds.y, b.bounds.y + b.bounds.height);
        // Gap relative to font height.
        if vgap > min_h * 2.2 {
            return false;
        }
        let hoverlap = overlap_1d(a.bounds.x, a.bounds.x + a.bounds.width, b.bounds.x, b.bounds.x + b.bounds.width);
        let min_w = a.bounds.width.min(b.bounds.width).max(1.0);
        let hoverlap_ratio = hoverlap / min_w;
        // Left-alignment relative to font height.
        let left_align = (a.bounds.x - b.bounds.x).abs() <= min_h * 1.8;
        // Center-alignment: centers of both lines are close horizontally.
        let center_a = a.bounds.x + a.bounds.width / 2.0;
        let center_b = b.bounds.x + b.bounds.width / 2.0;
        let center_align = (center_a - center_b).abs() <= min_h * 2.0;
        hoverlap_ratio >= 0.22 || left_align || center_align
    });

    let mut paragraphs = Vec::new();
    for (ci, cluster) in clusters.iter().enumerate() {
        if cluster.len() < 2 {
            if dbg && !cluster.is_empty() {
                eprintln!(
                    "  para {:2}: SKIP (single line)  [{:.0},{:.0},{:.0},{:.0}]  {:?}",
                    ci, cluster[0].bounds.x, cluster[0].bounds.y,
                    cluster[0].bounds.width, cluster[0].bounds.height,
                    cluster[0].text
                );
            }
            continue;
        }
        let merged = TextBox::merge(cluster);
        let font_h = cluster.iter().map(|l| l.bounds.height).sum::<f64>() / cluster.len() as f64;
        // Min dimensions relative to font size.
        if merged.bounds.width < font_h * 3.0 || merged.bounds.height < font_h * 1.2 {
            if dbg {
                eprintln!(
                    "  para {:2}: SKIP (too small) {} lines  [{:.0},{:.0},{:.0},{:.0}]  font_h={:.0}  {:?}",
                    ci, cluster.len(), merged.bounds.x, merged.bounds.y,
                    merged.bounds.width, merged.bounds.height, font_h, merged.text
                );
            }
            continue;
        }
        if dbg {
            eprintln!(
                "  para {:2}: {} lines  [{:.0},{:.0},{:.0},{:.0}]  font_h={:.0}  {:?}",
                ci, cluster.len(), merged.bounds.x, merged.bounds.y,
                merged.bounds.width, merged.bounds.height, font_h, merged.text
            );
        }
        paragraphs.push(merged);
    }
    if dbg {
        eprintln!("  => {} paragraphs", paragraphs.len());
    }
    paragraphs
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

fn cluster_textboxes<F>(items: &[TextBox], mut connected: F) -> Vec<Vec<TextBox>>
where
    F: FnMut(&TextBox, &TextBox) -> bool,
{
    if items.is_empty() {
        return Vec::new();
    }
    let n = items.len();
    let mut parent: Vec<usize> = (0..n).collect();
    fn find(parent: &mut [usize], idx: usize) -> usize {
        if parent[idx] != idx {
            let root = find(parent, parent[idx]);
            parent[idx] = root;
        }
        parent[idx]
    }
    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[rb] = ra;
        }
    }

    // Sort by Y midpoint for sweep-line prefiltering.
    let mut sorted: Vec<usize> = (0..n).collect();
    sorted.sort_by(|&a, &b| {
        let ya = items[a].bounds.y + items[a].bounds.height / 2.0;
        let yb = items[b].bounds.y + items[b].bounds.height / 2.0;
        ya.partial_cmp(&yb).unwrap_or(std::cmp::Ordering::Equal)
    });

    let dbg = debug_enabled();
    // Only compare items within a vertical band. Max band = 3× the larger height.
    for si in 0..n {
        let i = sorted[si];
        let yi_mid = items[i].bounds.y + items[i].bounds.height / 2.0;
        for sj in (si + 1)..n {
            let j = sorted[sj];
            let yj_mid = items[j].bounds.y + items[j].bounds.height / 2.0;
            let max_h = items[i].bounds.height.max(items[j].bounds.height);
            // Items sorted by Y — once gap exceeds band, no more matches possible.
            if yj_mid - yi_mid > max_h * 3.0 {
                break;
            }
            if connected(&items[i], &items[j]) {
                if dbg {
                    eprintln!(
                        "    cluster: merge {:?} + {:?}",
                        items[i].text, items[j].text
                    );
                }
                union(&mut parent, i, j);
            }
        }
    }
    let mut groups = std::collections::HashMap::<usize, Vec<TextBox>>::new();
    for (idx, item) in items.iter().enumerate() {
        let root = find(&mut parent, idx);
        groups.entry(root).or_default().push(item.clone());
    }
    let mut out: Vec<Vec<TextBox>> = groups.into_values().collect();
    out.sort_by_key(|group| group.len());
    out
}

fn cluster_bounds<F>(items: &[Bounds], mut connected: F) -> Vec<Vec<Bounds>>
where
    F: FnMut(&Bounds, &Bounds) -> bool,
{
    let tbs: Vec<TextBox> = items.iter().map(|b| TextBox::from_bounds(b.clone())).collect();
    cluster_textboxes(&tbs, |a, b| connected(&a.bounds, &b.bounds))
        .into_iter()
        .map(|group| group.into_iter().map(|tb| tb.bounds).collect())
        .collect()
}

fn overlap_1d(a1: f64, a2: f64, b1: f64, b2: f64) -> f64 {
    (a2.min(b2) - a1.max(b1)).max(0.0)
}

fn axis_gap(a1: f64, a2: f64, b1: f64, b2: f64) -> f64 {
    if a2 < b1 {
        b1 - a2
    } else if b2 < a1 {
        a1 - b2
    } else {
        0.0
    }
}

fn union_bounds(a: &Bounds, b: &Bounds) -> Bounds {
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
    fn group_words_merges_same_line() {
        let words: Vec<TextBox> = vec![
            Bounds { x: 40.0, y: 48.0, width: 56.0, height: 18.0 },
            Bounds { x: 122.0, y: 49.0, width: 72.0, height: 18.0 },
            Bounds { x: 220.0, y: 47.0, width: 65.0, height: 19.0 },
        ].into_iter().map(TextBox::from_bounds).collect();
        let lines = group_words_into_lines(&words);
        assert_eq!(lines.len(), 1, "same-line words should merge");
        assert!(lines[0].bounds.width >= 240.0);
    }

    #[test]
    fn group_words_separates_distant_lines() {
        let words: Vec<TextBox> = vec![
            Bounds { x: 100.0, y: 100.0, width: 80.0, height: 16.0 },
            Bounds { x: 184.0, y: 101.0, width: 64.0, height: 16.0 },
            Bounds { x: 102.0, y: 300.0, width: 92.0, height: 16.0 },
        ].into_iter().map(TextBox::from_bounds).collect();
        let lines = group_words_into_lines(&words);
        assert_eq!(lines.len(), 2, "distant lines should stay separate");
    }

    #[test]
    fn paragraph_groups_nearby_lines() {
        let lines: Vec<TextBox> = vec![
            Bounds { x: 60.0, y: 90.0, width: 220.0, height: 20.0 },
            Bounds { x: 68.0, y: 123.0, width: 240.0, height: 21.0 },
        ].into_iter().map(TextBox::from_bounds).collect();
        let paras = group_lines_into_paragraphs(&lines);
        assert_eq!(paras.len(), 1, "nearby lines should form paragraph");
    }

    #[test]
    fn prune_nested_glitch_boxes_works() {
        let inner = Bounds { x: 102.0, y: 84.0, width: 280.0, height: 88.0 };
        let outer = Bounds { x: 94.0, y: 76.0, width: 296.0, height: 104.0 };
        let kept = prune_nested_glitch_boxes(vec![outer.clone(), inner.clone()]);
        assert_eq!(kept.len(), 1);
        assert!(iou(&kept[0], &inner) > 0.95);
    }
}

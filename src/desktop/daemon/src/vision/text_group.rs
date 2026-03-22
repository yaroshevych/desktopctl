use desktop_core::protocol::Bounds;

use super::metal_pipeline::ProcessedFrame;
use super::tokenize_boxes::{axis_gap, debug_enabled, overlap_1d, union_bounds};

/// Max horizontal gap (as multiple of font height) for two words to be on the same line.
/// Tight to avoid merging tab-bar-style spaced items.
const WORD_HGAP_FONT_RATIO: f64 = 1.05;

/// Min horizontal gap (as multiple of font height) to split an OCR result.
/// Higher than WORD_HGAP_FONT_RATIO: only split at clearly separate groups,
/// not at normal inter-word spacing.
const SPLIT_HGAP_FONT_RATIO: f64 = 1.2;

// ── TextBox ─────────────────────────────────────────────────────────────────

/// Internal box representation that carries optional text through the pipeline.
#[derive(Debug, Clone)]
pub(crate) struct TextBox {
    pub bounds: Bounds,
    pub text: String,
}

impl TextBox {
    pub fn from_bounds(b: Bounds) -> Self {
        Self {
            bounds: b,
            text: String::new(),
        }
    }

    pub fn from_bounds_with_text(b: Bounds, text: String) -> Self {
        Self { bounds: b, text }
    }

    pub fn merge_refs(items: &[&TextBox]) -> TextBox {
        let merged_bounds = items
            .iter()
            .skip(1)
            .fold(items[0].bounds.clone(), |acc, tb| {
                union_bounds(&acc, &tb.bounds)
            });
        // Sort by x position and join texts with space.
        let mut sorted: Vec<&TextBox> = items.to_vec();
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

// ── Tightening ──────────────────────────────────────────────────────────────

/// Shrink bounds to the actual text content within the box.
/// First tries text_mask (excludes UI borders like pills), then falls back to
/// edge image for low-contrast text.
pub(crate) fn tighten_to_content(bounds: &Bounds, frame: &ProcessedFrame) -> Bounds {
    tighten_to_content_with_min_ratio(bounds, frame, 0.5)
}

fn tighten_to_content_with_min_ratio(
    bounds: &Bounds,
    frame: &ProcessedFrame,
    min_ratio: f64,
) -> Bounds {
    let x1 = (bounds.x as usize).min(frame.width.saturating_sub(1));
    let y1 = (bounds.y as usize).min(frame.height.saturating_sub(1));
    let x2 = ((bounds.x + bounds.width) as usize).min(frame.width);
    let y2 = ((bounds.y + bounds.height) as usize).min(frame.height);
    if x2 <= x1 + 2 || y2 <= y1 + 2 {
        return bounds.clone();
    }

    // Try text_mask first — it only marks actual text strokes, not UI borders.
    if let Some(tight) = tighten_with_mask(
        bounds,
        x1,
        y1,
        x2,
        y2,
        frame.width,
        &frame.text_mask,
        min_ratio,
    ) {
        return tight;
    }

    // Fall back to edge image for low-contrast text.
    const EDGE_THRESH: u8 = 12;
    if let Some(tight) = tighten_with_edge(
        bounds,
        x1,
        y1,
        x2,
        y2,
        frame.width,
        &frame.edge,
        EDGE_THRESH,
        min_ratio,
    ) {
        return tight;
    }

    bounds.clone()
}

/// Tighten bounds against a boolean content map in a single row-major pass.
/// Returns None if result is too small (below `min_ratio` of original in either dimension).
fn tighten_with_mask(
    bounds: &Bounds,
    x1: usize,
    y1: usize,
    x2: usize,
    y2: usize,
    width: usize,
    mask: &[bool],
    min_ratio: f64,
) -> Option<Bounds> {
    let mut left = x2;
    let mut right = x1;
    let mut top = y2;
    let mut bottom = y1;
    let mut found = false;

    for y in y1..y2 {
        let row = y * width + x1;
        for x_off in 0..(x2 - x1) {
            if mask[row + x_off] {
                found = true;
                let x = x1 + x_off;
                left = left.min(x);
                right = right.max(x + 1);
                top = top.min(y);
                bottom = bottom.max(y + 1);
            }
        }
    }
    if !found {
        return None;
    }

    let new_w = (right - left) as f64;
    let new_h = (bottom - top) as f64;
    if new_w < bounds.width * min_ratio || new_h < bounds.height * min_ratio {
        return None;
    }

    Some(Bounds {
        x: left as f64,
        y: top as f64,
        width: new_w,
        height: new_h,
    })
}

/// Tighten bounds against thresholded edge map in a single row-major pass.
fn tighten_with_edge(
    bounds: &Bounds,
    x1: usize,
    y1: usize,
    x2: usize,
    y2: usize,
    width: usize,
    edge: &[u8],
    threshold: u8,
    min_ratio: f64,
) -> Option<Bounds> {
    let mut left = x2;
    let mut right = x1;
    let mut top = y2;
    let mut bottom = y1;
    let mut found = false;

    for y in y1..y2 {
        let row = y * width + x1;
        for x_off in 0..(x2 - x1) {
            if edge[row + x_off] > threshold {
                found = true;
                let x = x1 + x_off;
                left = left.min(x);
                right = right.max(x + 1);
                top = top.min(y);
                bottom = bottom.max(y + 1);
            }
        }
    }
    if !found {
        return None;
    }

    let new_w = (right - left) as f64;
    let new_h = (bottom - top) as f64;
    if new_w < bounds.width * min_ratio || new_h < bounds.height * min_ratio {
        return None;
    }

    Some(Bounds {
        x: left as f64,
        y: top as f64,
        width: new_w,
        height: new_h,
    })
}

// ── Split wide OCR results ──────────────────────────────────────────────────

/// Split a TextBox at horizontal gaps in the grayscale image that are wider than
/// the line height. This breaks apart OCR results like "Thesaurus Apple" where
/// Vision merged two spatially separate words into one bounding box.
pub(crate) fn split_wide_textbox(tb: TextBox, frame: &ProcessedFrame) -> Vec<TextBox> {
    let dbg = debug_enabled();
    let mut tb = tb;
    // Tighten first so split gating uses actual text dimensions, not padded OCR
    // bounds.
    let tight = tighten_to_content(&tb.bounds, frame);
    let h = tight.height;
    if h < 4.0 || tight.width < h * 2.5 {
        tb.bounds = tight;
        return vec![tb];
    }

    // If tightening shrinks too much, keep scan on the original OCR box.
    // Thin symbols ("<", ">") can disappear during tighten, hiding real gaps.
    let use_original_scan =
        tight.width < tb.bounds.width * 0.72 || tight.height < tb.bounds.height * 0.72;
    let scan = if use_original_scan {
        &tb.bounds
    } else {
        &tight
    };
    if dbg && use_original_scan {
        eprintln!(
            "  split_scan_bounds {:?}: using original box [{:.0},{:.0},{:.0},{:.0}] over tight [{:.0},{:.0},{:.0},{:.0}]",
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

    let x1 = (scan.x as usize).min(frame.width.saturating_sub(1));
    let y1 = (scan.y as usize).min(frame.height.saturating_sub(1));
    let x2 = ((scan.x + scan.width) as usize).min(frame.width);
    let y2 = ((scan.y + scan.height) as usize).min(frame.height);
    if x2 <= x1 || y2 <= y1 {
        tb.bounds = tight;
        return vec![tb];
    }
    let region_w = x2 - x1;
    let region_h = y2 - y1;

    // Align split scan luminance with OCR preprocessing:
    // 1) invert on dark backgrounds, 2) local contrast stretch (1st..99th pct).
    let mut raw_bg_sum = 0u64;
    let mut raw_bg_count = 0u64;
    let top_row = y1 * frame.width + x1;
    let bottom_row = (y2 - 1) * frame.width + x1;
    for x_off in 0..region_w {
        raw_bg_sum += frame.gray[top_row + x_off] as u64;
        raw_bg_count += 1;
        if bottom_row != top_row {
            raw_bg_sum += frame.gray[bottom_row + x_off] as u64;
            raw_bg_count += 1;
        }
    }
    let raw_bg_luma = if raw_bg_count > 0 {
        raw_bg_sum as f64 / raw_bg_count as f64
    } else {
        128.0
    };
    let is_dark_bg = raw_bg_luma < 128.0;

    let mut hist = [0u32; 256];
    let mut region_raw = vec![0u8; region_w * region_h];
    for y_off in 0..region_h {
        let src_row = (y1 + y_off) * frame.width + x1;
        let dst_row = y_off * region_w;
        for x_off in 0..region_w {
            let raw = frame.gray[src_row + x_off];
            region_raw[dst_row + x_off] = raw;
            let prep = if is_dark_bg {
                255u8.saturating_sub(raw)
            } else {
                raw
            };
            hist[prep as usize] += 1;
        }
    }
    let hist_total = (region_w * region_h) as u32;
    let lo_target = hist_total / 100;
    let hi_target = hist_total.saturating_sub(hist_total / 100);
    let mut cumulative = 0u32;
    let mut lo = 0u8;
    let mut hi = 255u8;
    for (i, &count) in hist.iter().enumerate() {
        cumulative += count;
        if cumulative <= lo_target {
            lo = i as u8;
        }
        if cumulative < hi_target {
            hi = i as u8;
        }
    }
    let stretch_range = (hi as f64 - lo as f64).max(1.0);
    let scale = 255.0 / stretch_range;
    let mut scan_lut = [0.0f64; 256];
    for raw in 0u16..=255u16 {
        let raw_u8 = raw as u8;
        let prep = if is_dark_bg {
            255u8.saturating_sub(raw_u8)
        } else {
            raw_u8
        } as f64;
        scan_lut[raw as usize] = ((prep - lo as f64) * scale).clamp(0.0, 255.0);
    }

    // Cache OCR-space normalized pixels for the region once; downstream logic
    // reuses this buffer instead of rescanning frame.gray repeatedly.
    let mut norm = vec![0.0f64; region_w * region_h];
    for i in 0..region_raw.len() {
        norm[i] = scan_lut[region_raw[i] as usize];
    }

    // Background: top/bottom edge rows in OCR-aligned luminance.
    let mut bg_sum = 0.0f64;
    let mut bg_count = 0usize;
    for x_off in 0..region_w {
        bg_sum += norm[x_off];
        bg_count += 1;
        if region_h > 1 {
            bg_sum += norm[(region_h - 1) * region_w + x_off];
            bg_count += 1;
        }
    }
    let bg_luma = if bg_count > 0 {
        bg_sum / bg_count as f64
    } else {
        128.0
    };

    // Scan only the center band to avoid counting 1px UI separators that run
    // across the entire box width (toolbar rules, field borders) as "text".
    let vpad = ((region_h as f64) * 0.18).round() as usize;
    let scan_r1 = vpad.min(region_h.saturating_sub(1));
    let scan_r2 = region_h.saturating_sub(vpad).max(scan_r1 + 1);
    let scan_h = scan_r2.saturating_sub(scan_r1).max(1);
    let scan_y1 = y1 + scan_r1;
    let scan_y2 = y1 + scan_r2;

    // Find peak text intensity (max deviation from bg).
    let mut max_dev = 0.0f64;
    for y_off in scan_r1..scan_r2 {
        let row = y_off * region_w;
        for x_off in 0..region_w {
            let val = norm[row + x_off];
            let dev = (bg_luma - val).max(0.0);
            if dev > max_dev {
                max_dev = dev;
            }
        }
    }

    // Threshold at 40% of the full contrast range — catches real text strokes
    // but ignores anti-aliased edges and subtle gradients.
    let threshold = (max_dev * 0.40).max(20.0);

    // Drop rows that are too "full". They are usually horizontal UI rules and
    // borders, which otherwise break long gaps between words.
    let mut keep_row = vec![true; scan_h];
    for (i, y_off) in (scan_r1..scan_r2).enumerate() {
        let mut row_ink = 0usize;
        let row = y_off * region_w;
        for x_off in 0..region_w {
            let scan_luma = norm[row + x_off];
            if (bg_luma - scan_luma) > threshold {
                row_ink += 1;
            }
        }
        let fill_ratio = row_ink as f64 / region_w.max(1) as f64;
        if fill_ratio >= 0.72 {
            keep_row[i] = false;
        }
    }
    if !keep_row.iter().any(|&v| v) {
        keep_row.fill(true);
    }
    let usable_rows = keep_row.iter().filter(|&&v| v).count().max(1);

    // Build column occupancy: a column has text only if it has enough
    // in-band text pixels, not just a single hot/noisy pixel.
    let mut col_has_content = vec![false; region_w];
    let min_ink_px = ((usable_rows as f64) * 0.10).round() as usize;
    let min_ink_px = min_ink_px.max(3).min(usable_rows);
    let mut col_ink = vec![0usize; region_w];
    for (i, y_off) in (scan_r1..scan_r2).enumerate() {
        if !keep_row[i] {
            continue;
        }
        let row = y_off * region_w;
        for x_off in 0..region_w {
            let scan_luma = norm[row + x_off];
            if (bg_luma - scan_luma) > threshold {
                col_ink[x_off] += 1;
            }
        }
    }
    for x_off in 0..region_w {
        col_has_content[x_off] = col_ink[x_off] >= min_ink_px;
    }

    if dbg {
        let dropped_rows = keep_row.iter().filter(|&&v| !v).count();
        eprintln!(
            "  split_scan {:?}: bg_luma={:.0} dark_bg={} lo={} hi={} max_dev={:.0} thresh={:.0} min_gap={:.0} scan_y={}..{} min_ink={} dropped_rows={}",
            tb.text,
            bg_luma,
            is_dark_bg,
            lo,
            hi,
            max_dev,
            threshold,
            h * SPLIT_HGAP_FONT_RATIO,
            scan_y1,
            scan_y2,
            min_ink_px,
            dropped_rows
        );
    }

    // Find gap runs (consecutive empty columns).
    let min_gap = (h * SPLIT_HGAP_FONT_RATIO) as usize;
    let mut splits: Vec<usize> = Vec::new(); // x positions where we split (midpoint of gap)
    let mut gap_start: Option<usize> = None;

    for (i, &has) in col_has_content.iter().enumerate() {
        if !has {
            if gap_start.is_none() {
                gap_start = Some(i);
            }
        } else if let Some(gs) = gap_start {
            let gap_len = i - gs;
            if dbg && gap_len >= 3 {
                eprintln!(
                    "    gap at x={}..{} len={} (min={})",
                    x1 + gs,
                    x1 + i,
                    gap_len,
                    min_gap
                );
            }
            if gap_len >= min_gap {
                splits.push(x1 + gs + gap_len / 2);
            }
            gap_start = None;
        }
    }

    if splits.is_empty() {
        tb.bounds = tight;
        return vec![tb];
    }

    // Split the text and bounds at each gap.
    let mut result = Vec::new();
    let words: Vec<&str> = tb.text.split_whitespace().collect();

    // Create sub-boxes from x-ranges.
    let mut prev_x = tb.bounds.x;
    for &split_x in &splits {
        let sub_w = split_x as f64 - prev_x;
        if sub_w > 4.0 {
            let sub_bounds = Bounds {
                x: prev_x,
                y: tb.bounds.y,
                width: sub_w,
                height: tb.bounds.height,
            };
            result.push(sub_bounds);
        }
        prev_x = split_x as f64;
    }
    // Last segment.
    let last_w = (tb.bounds.x + tb.bounds.width) - prev_x;
    if last_w > 4.0 {
        result.push(Bounds {
            x: prev_x,
            y: tb.bounds.y,
            width: last_w,
            height: tb.bounds.height,
        });
    }

    // Tighten each sub-box to actual content.
    let result: Vec<Bounds> = result
        .into_iter()
        .map(|b| tighten_to_content_with_min_ratio(&b, frame, 0.2))
        .collect();

    // Distribute words across sub-boxes by x-position.
    let mut textboxes: Vec<TextBox> = result
        .into_iter()
        .map(|b| TextBox::from_bounds_with_text(b, String::new()))
        .collect();
    let lead_symbols = words
        .iter()
        .take_while(|w| !w.is_empty() && w.chars().all(|c| !c.is_alphanumeric()))
        .count();
    if lead_symbols > 1 && lead_symbols < words.len() && textboxes.len() > lead_symbols {
        let merged_bounds = textboxes[1..lead_symbols]
            .iter()
            .fold(textboxes[0].bounds.clone(), |acc, sub| {
                union_bounds(&acc, &sub.bounds)
            });
        let mut merged = Vec::with_capacity(textboxes.len() - lead_symbols + 1);
        merged.push(TextBox::from_bounds_with_text(merged_bounds, String::new()));
        merged.extend(textboxes.into_iter().skip(lead_symbols));
        textboxes = merged;
    }

    // Simple heuristic: if we have N sub-boxes and M words, distribute sequentially.
    if !words.is_empty() && !textboxes.is_empty() {
        if words.len() == textboxes.len() {
            for (i, word) in words.iter().enumerate() {
                textboxes[i].text = word.to_string();
            }
        } else if words.len() > textboxes.len() {
            // Special-case leading symbol tokens ("<", ">", etc.): keep them
            // grouped in the left-most split box instead of attaching to the
            // next word group.
            if textboxes.len() == 2 {
                if lead_symbols > 0 && lead_symbols < words.len() {
                    textboxes[0].text = words[..lead_symbols].join(" ");
                    textboxes[1].text = words[lead_symbols..].join(" ");
                    if dbg {
                        eprintln!(
                            "    split_assign symbols: left={:?} right={:?}",
                            textboxes[0].text, textboxes[1].text
                        );
                    }
                    if dbg {
                        eprintln!(
                            "  split_wide: {:?} [{:.0},{:.0},{:.0},{:.0}] → {} parts",
                            tb.text,
                            tb.bounds.x,
                            tb.bounds.y,
                            tb.bounds.width,
                            tb.bounds.height,
                            textboxes.len()
                        );
                        for (i, sub) in textboxes.iter().enumerate() {
                            eprintln!(
                                "    part {}: [{:.0},{:.0},{:.0},{:.0}] {:?}",
                                i,
                                sub.bounds.x,
                                sub.bounds.y,
                                sub.bounds.width,
                                sub.bounds.height,
                                sub.text
                            );
                        }
                    }
                    return textboxes;
                }
            }

            // More words than boxes: distribute proportionally by width.
            let total_w: f64 = textboxes.iter().map(|tb| tb.bounds.width).sum();
            let n_boxes = textboxes.len();
            let n_words = words.len();
            let mut word_idx = 0;
            for (i, sub) in textboxes.iter_mut().enumerate() {
                let share = if i == n_boxes - 1 {
                    n_words - word_idx
                } else {
                    ((sub.bounds.width / total_w) * n_words as f64).round() as usize
                };
                let end = (word_idx + share.max(1)).min(n_words);
                sub.text = words[word_idx..end].join(" ");
                word_idx = end;
            }
        } else {
            // Fewer words than boxes: one word per box, empty for extras.
            for (i, word) in words.iter().enumerate() {
                if i < textboxes.len() {
                    textboxes[i].text = word.to_string();
                }
            }
        }
    }

    if dbg {
        eprintln!(
            "  split_wide: {:?} [{:.0},{:.0},{:.0},{:.0}] → {} parts",
            tb.text,
            tb.bounds.x,
            tb.bounds.y,
            tb.bounds.width,
            tb.bounds.height,
            textboxes.len()
        );
        for (i, sub) in textboxes.iter().enumerate() {
            eprintln!(
                "    part {}: [{:.0},{:.0},{:.0},{:.0}] {:?}",
                i, sub.bounds.x, sub.bounds.y, sub.bounds.width, sub.bounds.height, sub.text
            );
        }
    }

    textboxes
}

// ── Grouping ────────────────────────────────────────────────────────────────

/// Group word boxes into text lines based on vertical overlap and horizontal proximity.
/// All thresholds are relative to text height (font size proxy).
pub(crate) fn group_words_into_lines(words: &[TextBox]) -> Vec<TextBox> {
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
        let vertical_overlap = overlap_1d(
            a.bounds.y,
            a.bounds.y + a.bounds.height,
            b.bounds.y,
            b.bounds.y + b.bounds.height,
        );
        let vertical_ratio = vertical_overlap / min_h;
        let hgap = axis_gap(
            a.bounds.x,
            a.bounds.x + a.bounds.width,
            b.bounds.x,
            b.bounds.x + b.bounds.width,
        );
        // Horizontal gap relative to font height.
        // Tight threshold to avoid merging tab-bar-style spaced items.
        vertical_ratio >= 0.40 && hgap <= min_h * WORD_HGAP_FONT_RATIO
    });

    let mut lines = Vec::new();
    for (ci, cluster) in clusters.iter().enumerate() {
        if cluster.is_empty() {
            continue;
        }
        let merged = TextBox::merge_refs(cluster);
        // Skip very tiny isolated blobs (single glyph-sized).
        let font_h = cluster.iter().map(|w| w.bounds.height).sum::<f64>() / cluster.len() as f64;
        if cluster.len() == 1 && merged.bounds.width < font_h * 1.2 {
            let keep_tiny_symbol = merged.text.contains('<') || merged.text.contains('>');
            if keep_tiny_symbol {
                if dbg {
                    eprintln!(
                        "  line {:2}: KEEP (symbol) [{:.0},{:.0},{:.0},{:.0}]  {:?}",
                        ci,
                        merged.bounds.x,
                        merged.bounds.y,
                        merged.bounds.width,
                        merged.bounds.height,
                        merged.text
                    );
                }
                lines.push(merged);
                continue;
            }
            if dbg {
                eprintln!(
                    "  line {:2}: SKIP (tiny) [{:.0},{:.0},{:.0},{:.0}]  {:?}",
                    ci,
                    merged.bounds.x,
                    merged.bounds.y,
                    merged.bounds.width,
                    merged.bounds.height,
                    merged.text
                );
            }
            continue;
        }
        if dbg {
            eprintln!(
                "  line {:2}: {} words  [{:.0},{:.0},{:.0},{:.0}]  font_h={:.0}  {:?}",
                ci,
                cluster.len(),
                merged.bounds.x,
                merged.bounds.y,
                merged.bounds.width,
                merged.bounds.height,
                font_h,
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
pub(crate) fn group_lines_into_paragraphs(lines: &[TextBox]) -> Vec<TextBox> {
    let dbg = debug_enabled();
    if dbg {
        eprintln!(
            "\n--- group_lines_into_paragraphs: {} lines ---",
            lines.len()
        );
    }

    let clusters = cluster_textboxes(lines, |a, b| {
        let min_h = a.bounds.height.min(b.bounds.height).max(1.0);
        let max_h = a.bounds.height.max(b.bounds.height);
        // Same font size.
        if max_h > min_h * 1.6 {
            return false;
        }
        let vgap = axis_gap(
            a.bounds.y,
            a.bounds.y + a.bounds.height,
            b.bounds.y,
            b.bounds.y + b.bounds.height,
        );
        // Gap relative to font height.
        // Keep this fairly tight to avoid merging unrelated stacked UI labels.
        if vgap > min_h * 1.4 {
            return false;
        }
        let hoverlap = overlap_1d(
            a.bounds.x,
            a.bounds.x + a.bounds.width,
            b.bounds.x,
            b.bounds.x + b.bounds.width,
        );
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
                    ci,
                    cluster[0].bounds.x,
                    cluster[0].bounds.y,
                    cluster[0].bounds.width,
                    cluster[0].bounds.height,
                    cluster[0].text
                );
            }
            continue;
        }
        let merged = TextBox::merge_refs(cluster);
        let font_h = cluster.iter().map(|l| l.bounds.height).sum::<f64>() / cluster.len() as f64;
        // Min dimensions relative to font size.
        if merged.bounds.width < font_h * 3.0 || merged.bounds.height < font_h * 1.2 {
            if dbg {
                eprintln!(
                    "  para {:2}: SKIP (too small) {} lines  [{:.0},{:.0},{:.0},{:.0}]  font_h={:.0}  {:?}",
                    ci,
                    cluster.len(),
                    merged.bounds.x,
                    merged.bounds.y,
                    merged.bounds.width,
                    merged.bounds.height,
                    font_h,
                    merged.text
                );
            }
            continue;
        }
        if dbg {
            eprintln!(
                "  para {:2}: {} lines  [{:.0},{:.0},{:.0},{:.0}]  font_h={:.0}  {:?}",
                ci,
                cluster.len(),
                merged.bounds.x,
                merged.bounds.y,
                merged.bounds.width,
                merged.bounds.height,
                font_h,
                merged.text
            );
        }
        paragraphs.push(merged);
    }
    if dbg {
        eprintln!("  => {} paragraphs", paragraphs.len());
    }
    paragraphs
}

// ── Clustering ──────────────────────────────────────────────────────────────

fn cluster_textboxes<'a, F>(items: &'a [TextBox], mut connected: F) -> Vec<Vec<&'a TextBox>>
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
    let mut groups = std::collections::HashMap::<usize, Vec<usize>>::new();
    for idx in 0..n {
        let root = find(&mut parent, idx);
        groups.entry(root).or_default().push(idx);
    }
    let mut keyed: Vec<(f64, f64, Vec<usize>)> = groups
        .into_values()
        .map(|group| {
            let min_y = group
                .iter()
                .map(|idx| items[*idx].bounds.y)
                .fold(f64::INFINITY, f64::min);
            let min_x = group
                .iter()
                .map(|idx| items[*idx].bounds.x)
                .fold(f64::INFINITY, f64::min);
            (min_y, min_x, group)
        })
        .collect();
    // Reading order: top-to-bottom, then left-to-right.
    keyed.sort_by(|(ay, ax, _), (by, bx, _)| {
        let y_ord = ay.partial_cmp(by).unwrap_or(std::cmp::Ordering::Equal);
        if y_ord != std::cmp::Ordering::Equal {
            return y_ord;
        }
        ax.partial_cmp(bx).unwrap_or(std::cmp::Ordering::Equal)
    });
    keyed
        .into_iter()
        .map(|(_, _, group)| group.into_iter().map(|idx| &items[idx]).collect::<Vec<_>>())
        .collect()
}

// ── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_words_merges_same_line() {
        // Words must be within WORD_HGAP_FONT_RATIO (1.05×) of font height apart.
        // height=18, so max gap = 18*1.05 = 18.9px.
        let words: Vec<TextBox> = vec![
            Bounds {
                x: 40.0,
                y: 48.0,
                width: 56.0,
                height: 18.0,
            },
            Bounds {
                x: 106.0,
                y: 49.0,
                width: 72.0,
                height: 18.0,
            },
            Bounds {
                x: 188.0,
                y: 47.0,
                width: 65.0,
                height: 19.0,
            },
        ]
        .into_iter()
        .map(TextBox::from_bounds)
        .collect();
        let lines = group_words_into_lines(&words);
        assert_eq!(lines.len(), 1, "same-line words should merge");
        assert!(lines[0].bounds.width >= 200.0);
    }

    #[test]
    fn group_words_separates_distant_lines() {
        let words: Vec<TextBox> = vec![
            Bounds {
                x: 100.0,
                y: 100.0,
                width: 80.0,
                height: 16.0,
            },
            Bounds {
                x: 184.0,
                y: 101.0,
                width: 64.0,
                height: 16.0,
            },
            Bounds {
                x: 102.0,
                y: 300.0,
                width: 92.0,
                height: 16.0,
            },
        ]
        .into_iter()
        .map(TextBox::from_bounds)
        .collect();
        let lines = group_words_into_lines(&words);
        assert_eq!(lines.len(), 2, "distant lines should stay separate");
    }

    #[test]
    fn paragraph_groups_nearby_lines() {
        let lines: Vec<TextBox> = vec![
            Bounds {
                x: 60.0,
                y: 90.0,
                width: 220.0,
                height: 20.0,
            },
            Bounds {
                x: 68.0,
                y: 123.0,
                width: 240.0,
                height: 21.0,
            },
        ]
        .into_iter()
        .map(TextBox::from_bounds)
        .collect();
        let paras = group_lines_into_paragraphs(&lines);
        assert_eq!(paras.len(), 1, "nearby lines should form paragraph");
    }
}

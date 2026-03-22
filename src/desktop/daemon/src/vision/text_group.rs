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

    pub fn merge(items: &[TextBox]) -> TextBox {
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
    if let Some(tight) = tighten_with(bounds, x1, y1, x2, y2, |x, y| {
        frame.text_mask[y * frame.width + x]
    }, min_ratio) {
        return tight;
    }

    // Fall back to edge image for low-contrast text.
    const EDGE_THRESH: u8 = 12;
    if let Some(tight) = tighten_with(bounds, x1, y1, x2, y2, |x, y| {
        frame.edge[y * frame.width + x] > EDGE_THRESH
    }, min_ratio) {
        return tight;
    }

    bounds.clone()
}

/// Generic tighten: scan inward from each edge using a pixel predicate.
/// Returns None if result is too small (below `min_ratio` of original in either dimension).
fn tighten_with<F>(
    bounds: &Bounds,
    x1: usize,
    y1: usize,
    x2: usize,
    y2: usize,
    has_content: F,
    min_ratio: f64,
) -> Option<Bounds>
where
    F: Fn(usize, usize) -> bool,
{
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

    if right <= left || bottom <= top {
        return None;
    }
    let new_w = (right - left) as f64;
    let new_h = (bottom - top) as f64;
    // Don't tighten if result is too small.
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
    let h = tb.bounds.height;
    if h < 4.0 || tb.bounds.width < h * 2.5 {
        return vec![tb];
    }

    // Tighten first to get accurate text height (OCR boxes are often padded).
    let tight = tighten_to_content(&tb.bounds, frame);
    let h = tight.height;

    // If tightening shrinks too much, keep scan on the original OCR box.
    // Thin symbols ("<", ">") can disappear during tighten, hiding real gaps.
    let use_original_scan =
        tight.width < tb.bounds.width * 0.72 || tight.height < tb.bounds.height * 0.72;
    let scan = if use_original_scan { &tb.bounds } else { &tight };
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
        return vec![tb];
    }

    // Find background and text intensities from the region.
    // Background: sample top and bottom edge rows (most likely pure bg).
    // Text: find the peak deviation from background.
    let mut bg_sum = 0u64;
    let mut bg_count = 0u64;
    for x in x1..x2 {
        bg_sum += frame.gray[y1 * frame.width + x] as u64;
        bg_sum += frame.gray[(y2 - 1) * frame.width + x] as u64;
        bg_count += 2;
    }
    let bg_luma = if bg_count > 0 {
        bg_sum as f64 / bg_count as f64
    } else {
        128.0
    };
    let is_dark_bg = bg_luma < 128.0;
    // Scan only the center band to avoid counting 1px UI separators that run
    // across the entire box width (toolbar rules, field borders) as "text".
    let region_h = y2 - y1;
    let vpad = ((region_h as f64) * 0.18).round() as usize;
    let scan_y1 = (y1 + vpad).min(y2.saturating_sub(1));
    let scan_y2 = y2.saturating_sub(vpad).max(scan_y1 + 1);
    let scan_h = scan_y2.saturating_sub(scan_y1).max(1);

    // Find peak text intensity (max deviation from bg).
    let mut max_dev = 0.0f64;
    for x in x1..x2 {
        for y in scan_y1..scan_y2 {
            let val = frame.gray[y * frame.width + x] as f64;
            let dev = if is_dark_bg {
                val - bg_luma
            } else {
                bg_luma - val
            };
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
    let scan_w = x2 - x1;
    let mut keep_row = vec![true; scan_h];
    for y in scan_y1..scan_y2 {
        let mut row_ink = 0usize;
        for x in x1..x2 {
            let val = frame.gray[y * frame.width + x] as f64;
            let is_text = if is_dark_bg {
                val - bg_luma > threshold
            } else {
                bg_luma - val > threshold
            };
            if is_text {
                row_ink += 1;
            }
        }
        let fill_ratio = row_ink as f64 / scan_w.max(1) as f64;
        if fill_ratio >= 0.72 {
            keep_row[y - scan_y1] = false;
        }
    }
    if !keep_row.iter().any(|&v| v) {
        keep_row.fill(true);
    }
    let usable_rows = keep_row.iter().filter(|&&v| v).count().max(1);

    // Build column occupancy: a column has text only if it has enough
    // in-band text pixels, not just a single hot/noisy pixel.
    let mut col_has_content = vec![false; x2 - x1];
    let min_ink_px = ((usable_rows as f64) * 0.10).round() as usize;
    let min_ink_px = min_ink_px.max(3).min(usable_rows);
    for x in x1..x2 {
        let mut ink = 0usize;
        for y in scan_y1..scan_y2 {
            if !keep_row[y - scan_y1] {
                continue;
            }
            let val = frame.gray[y * frame.width + x] as f64;
            let is_text = if is_dark_bg {
                val - bg_luma > threshold
            } else {
                bg_luma - val > threshold
            };
            if is_text {
                ink += 1;
                if ink >= min_ink_px {
                    col_has_content[x - x1] = true;
                    break;
                }
            }
        }
    }

    if dbg {
        let dropped_rows = keep_row.iter().filter(|&&v| !v).count();
        eprintln!(
            "  split_scan {:?}: bg_luma={:.0} dark_bg={} max_dev={:.0} thresh={:.0} min_gap={:.0} scan_y={}..{} min_ink={} dropped_rows={}",
            tb.text,
            bg_luma,
            is_dark_bg,
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
                let lead_symbols = words
                    .iter()
                    .take_while(|w| {
                        !w.is_empty() && w.chars().all(|c| !c.is_alphanumeric())
                    })
                    .count();
                if lead_symbols > 0 && lead_symbols < words.len() {
                    textboxes[0].text = words[..lead_symbols].join(" ");
                    textboxes[1].text = words[lead_symbols..].join(" ");
                    if dbg {
                        eprintln!(
                            "    split_assign symbols: left={:?} right={:?}",
                            textboxes[0].text,
                            textboxes[1].text
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
            tb.text, tb.bounds.x, tb.bounds.y, tb.bounds.width, tb.bounds.height,
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
        let merged = TextBox::merge(cluster);
        // Skip very tiny isolated blobs (single glyph-sized).
        let font_h =
            cluster.iter().map(|w| w.bounds.height).sum::<f64>() / cluster.len() as f64;
        if cluster.len() == 1 && merged.bounds.width < font_h * 1.2 {
            if dbg {
                eprintln!(
                    "  line {:2}: SKIP (tiny) [{:.0},{:.0},{:.0},{:.0}]  {:?}",
                    ci, merged.bounds.x, merged.bounds.y, merged.bounds.width,
                    merged.bounds.height, merged.text
                );
            }
            continue;
        }
        if dbg {
            eprintln!(
                "  line {:2}: {} words  [{:.0},{:.0},{:.0},{:.0}]  font_h={:.0}  {:?}",
                ci, cluster.len(), merged.bounds.x, merged.bounds.y,
                merged.bounds.width, merged.bounds.height, font_h, merged.text
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
                    ci, cluster[0].bounds.x, cluster[0].bounds.y,
                    cluster[0].bounds.width, cluster[0].bounds.height, cluster[0].text
                );
            }
            continue;
        }
        let merged = TextBox::merge(cluster);
        let font_h =
            cluster.iter().map(|l| l.bounds.height).sum::<f64>() / cluster.len() as f64;
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

// ── Clustering ──────────────────────────────────────────────────────────────

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

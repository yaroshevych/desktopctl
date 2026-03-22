use desktop_core::protocol::Bounds;
use std::collections::VecDeque;

use super::metal_pipeline::ProcessedFrame;

pub(super) fn debug_enabled() -> bool {
    std::env::var("TOKENIZE_DEBUG").is_ok() || std::env::var("TOKENIZE_CONTROLS_DEBUG").is_ok()
}

// ── public API ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ControlKind {
    TextField,
    Button,
}

#[derive(Debug, Clone)]
pub struct DetectedControl {
    pub bounds: Bounds,
    pub kind: ControlKind,
}

#[derive(Debug, Clone, Copy)]
struct EdgeHit {
    pos: f64,
    found: bool,
}

/// Detect text fields and buttons by expanding from text anchors until a border
/// or contrast boundary is found.
///
/// For each text line, uses `find_enclosing_control` to expand outward via SAT
/// edge strip queries. Then classifies: wide + single-line text = TextField,
/// compact + short text = Button.
pub fn detect_controls(frame: &ProcessedFrame, text_lines: &[Bounds]) -> Vec<DetectedControl> {
    let dbg = debug_enabled();
    let mut controls = Vec::new();

    for (idx, text) in text_lines.iter().enumerate() {
        let Some(enclosing) = find_enclosing_control(frame, text) else {
            if dbg {
                eprintln!(
                    "  detect_control: NONE for text=[{:.0},{:.0},{:.0},{:.0}]",
                    text.x, text.y, text.width, text.height,
                );
            }
            continue;
        };

        // Reject if the enclosing box encloses other text lines — that means
        // expansion stopped at text edges or a container border, not a single
        // control's border.
        let font_h = text.height.max(1.0);
        let contains_others = text_lines.iter().enumerate().any(|(j, other)| {
            if j == idx {
                return false;
            }
            // Check 1: same-row text overlaps the enclosing box horizontally.
            let cy = text.y + text.height / 2.0;
            let oy = other.y + other.height / 2.0;
            let same_row = (cy - oy).abs() <= font_h * 1.5;
            if same_row {
                let ox1 = other.x;
                let ox2 = other.x + other.width;
                let ex1 = enclosing.x;
                let ex2 = enclosing.x + enclosing.width;
                if overlap_1d(ox1, ox2, ex1, ex2) > 0.0 {
                    return true;
                }
            }
            // Check 2: any text center inside the enclosing box (cross-row).
            let cx = other.x + other.width / 2.0;
            let cy2 = other.y + other.height / 2.0;
            if cx >= enclosing.x
                && cx <= enclosing.x + enclosing.width
                && cy2 >= enclosing.y
                && cy2 <= enclosing.y + enclosing.height
            {
                return true;
            }
            // Check 3: any substantial overlap with another text line means
            // this is likely a container/group boundary, not a single control.
            let ov_x = overlap_1d(
                other.x,
                other.x + other.width,
                enclosing.x,
                enclosing.x + enclosing.width,
            );
            let ov_y = overlap_1d(
                other.y,
                other.y + other.height,
                enclosing.y,
                enclosing.y + enclosing.height,
            );
            let ov_area = ov_x * ov_y;
            let other_area = (other.width * other.height).max(1.0);
            ov_area / other_area >= 0.15
        });
        if contains_others {
            if dbg {
                eprintln!(
                    "  detect_control: CONTAINER [{:.0},{:.0},{:.0},{:.0}] text=[{:.0},{:.0},{:.0},{:.0}]",
                    enclosing.x,
                    enclosing.y,
                    enclosing.width,
                    enclosing.height,
                    text.x,
                    text.y,
                    text.width,
                    text.height,
                );
            }
            continue;
        }

        if has_intervening_text(idx, text, &enclosing, text_lines) {
            if dbg {
                eprintln!(
                    "  detect_control: BLOCKED_BY_TEXT [{:.0},{:.0},{:.0},{:.0}] text=[{:.0},{:.0},{:.0},{:.0}]",
                    enclosing.x,
                    enclosing.y,
                    enclosing.width,
                    enclosing.height,
                    text.x,
                    text.y,
                    text.width,
                    text.height,
                );
            }
            continue;
        }

        let kind = classify_control(&enclosing, text);

        if dbg {
            eprintln!(
                "  detect_control: {:?} [{:.0},{:.0},{:.0},{:.0}] text=[{:.0},{:.0},{:.0},{:.0}]",
                kind,
                enclosing.x,
                enclosing.y,
                enclosing.width,
                enclosing.height,
                text.x,
                text.y,
                text.width,
                text.height,
            );
        }

        controls.push(DetectedControl {
            bounds: enclosing,
            kind,
        });
    }

    // Dedupe: if two controls overlap significantly, keep the one with better
    // border energy.
    controls = dedupe_controls(controls, frame);
    controls
}

// ── SAT-based enclosing control detection ───────────────────────────────────

/// For a text line, scan outward in each direction using SAT strip queries
/// to find the border position with peak edge energy. Returns the enclosing box.
fn find_enclosing_control(frame: &ProcessedFrame, text: &Bounds) -> Option<Bounds> {
    let dbg = debug_enabled();
    let img_w = frame.width as f64;
    let img_h = frame.height as f64;
    let font_h = text.height.max(1.0);

    // Expansion limits relative to font size.
    // max_x is generous: text fields can be much wider than their text content.
    let max_x = (font_h * 20.0).min(img_w * 0.5);
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

    // If any side failed to find a real edge (and fell back to max-expand),
    // this is not an enclosed control box.
    if !(left.found && right.found && (top.found || bottom.found)) {
        if dbg {
            eprintln!(
                "    find_enclosing REJECT(non-enclosed): text=[{:.0},{:.0},{:.0},{:.0}] found=[L{} R{} T{} B{}]",
                text.x,
                text.y,
                text.width,
                text.height,
                left.found as u8,
                right.found as u8,
                top.found as u8,
                bottom.found as u8,
            );
        }
        return None;
    }

    let left = left.pos;
    let right = right.pos;
    let top = top.pos;
    let bottom = bottom.pos;

    let cw = right - left;
    let ch = bottom - top;

    // Must have expanded meaningfully beyond text.
    if cw < text.width + font_h * 0.3 || ch < text.height + font_h * 0.15 {
        if dbg {
            eprintln!(
                "    find_enclosing REJECT(too small): text=[{:.0},{:.0},{:.0},{:.0}] lrtb=[{:.0},{:.0},{:.0},{:.0}] cw={:.0} ch={:.0}",
                text.x, text.y, text.width, text.height, left, right, top, bottom, cw, ch,
            );
        }
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
        if dbg {
            eprintln!(
                "    find_enclosing REJECT(at_max={}): text=[{:.0},{:.0},{:.0},{:.0}] expand=[{:.0},{:.0},{:.0},{:.0}] max_limit={:.0}",
                at_max_count,
                text.x,
                text.y,
                text.width,
                text.height,
                expanded_left,
                expanded_right,
                expanded_top,
                expanded_bottom,
                max_limit,
            );
        }
        return None; // No border found in most directions.
    }

    let candidate = Bounds {
        x: left.max(0.0),
        y: top.max(0.0),
        width: cw.min(img_w),
        height: ch.min(img_h),
    };

    let corner_skip = (font_h * 0.3) as usize;
    let pre_border_e = frame.border_energy(&candidate, strip_thick, corner_skip);
    let edge_thr = sobel_edge_threshold(pre_border_e);
    let Some(candidate) =
        sobel_enclosed_candidate(frame, text, &candidate, edge_thr, corner_skip, dbg)
    else {
        if dbg {
            eprintln!(
                "    find_enclosing REJECT(open-border): text=[{:.0},{:.0},{:.0},{:.0}] box=[{:.0},{:.0},{:.0},{:.0}]",
                text.x,
                text.y,
                text.width,
                text.height,
                candidate.x,
                candidate.y,
                candidate.width,
                candidate.height,
            );
        }
        return None;
    };

    // Validate border-vs-interior evidence.
    let directions_found = [expanded_left, expanded_right, expanded_top, expanded_bottom]
        .iter()
        .filter(|e| **e < max_limit * 0.90)
        .count();

    let border_e = frame.border_energy(&candidate, strip_thick, corner_skip);
    let interior_e = frame.interior_edge_energy(&candidate);
    let glow_e = frame.glow_energy(&candidate, (font_h * 0.4) as usize);

    let effective_border = border_e + glow_e * 0.5;
    let noise_floor = interior_e.max(0.5);
    let ratio = effective_border / noise_floor;
    let energy_ok = ratio >= 1.15 || border_e >= 3.0 || (glow_e >= 1.2 && ratio >= 0.95);
    if !energy_ok && directions_found < 3 {
        if dbg {
            eprintln!(
                "    find_enclosing REJECT(energy): text=[{:.0},{:.0},{:.0},{:.0}] border={:.1} interior={:.1} glow={:.1} ratio={:.2} dirs_found={}",
                text.x,
                text.y,
                text.width,
                text.height,
                border_e,
                interior_e,
                glow_e,
                ratio,
                directions_found,
            );
        }
        return None;
    }

    clamp_bounds(&candidate, img_w, img_h)
}

fn sobel_edge_threshold(border_e: f64) -> u8 {
    (border_e * 0.6).clamp(3.0, 20.0) as u8
}

fn sobel_enclosed_candidate(
    frame: &ProcessedFrame,
    text: &Bounds,
    candidate: &Bounds,
    edge_thr: u8,
    corner_skip: usize,
    dbg: bool,
) -> Option<Bounds> {
    let (top_y, bottom_y) = consensus_top_bottom_edges(frame, text, candidate, edge_thr)?;
    let x1 = candidate.x.floor() as i32;
    let x2 = (candidate.x + candidate.width).ceil() as i32 - 1;
    if x2 - x1 < 8 || bottom_y - top_y < 8 {
        return None;
    }

    let cs = ((corner_skip as i32) + 3).min(((x2 - x1) / 3).max(2));
    let span_x1 = x1 + cs;
    let span_x2 = x2 - cs;
    if span_x2 <= span_x1 {
        return None;
    }

    let top_y = best_horizontal_level(frame, top_y, span_x1, span_x2, edge_thr, 4);
    let bottom_y = best_horizontal_level(frame, bottom_y, span_x1, span_x2, edge_thr, 4);
    let top_cov = horizontal_edge_coverage(frame, top_y, span_x1, span_x2, edge_thr);
    let bottom_cov = horizontal_edge_coverage(frame, bottom_y, span_x1, span_x2, edge_thr);
    let top_ok = top_cov >= 0.999;
    let bottom_ok = bottom_cov >= 0.999;
    let loop_ok = sobel_edge_bucket_enclosed(frame, candidate, top_y, bottom_y, edge_thr, cs);
    if !(top_ok && bottom_ok && loop_ok) {
        if dbg {
            eprintln!(
                "      sobel-loop miss: top={} ({:.2}) bottom={} ({:.2}) loop={} span=[{},{}] thr={}",
                top_y, top_cov, bottom_y, bottom_cov, loop_ok, span_x1, span_x2, edge_thr
            );
        }
        return None;
    }

    let h = frame.height as i32;
    Some(Bounds {
        x: candidate.x,
        y: top_y.clamp(0, h - 1) as f64,
        width: candidate.width,
        height: (bottom_y - top_y + 1).max(1) as f64,
    })
}

fn consensus_top_bottom_edges(
    frame: &ProcessedFrame,
    text: &Bounds,
    candidate: &Bounds,
    edge_thr: u8,
) -> Option<(i32, i32)> {
    let w = frame.width as i32;
    let x_start = text.x.floor() as i32 + 1;
    let x_end = (text.x + text.width).ceil() as i32 - 1;
    if x_end <= x_start {
        return None;
    }

    let probe_count = ((text.width / 28.0).round() as usize).clamp(6, 14);
    let mut probes = Vec::with_capacity(probe_count);
    for i in 0..probe_count {
        let t = if probe_count == 1 {
            0.5
        } else {
            i as f64 / (probe_count - 1) as f64
        };
        let x = (x_start as f64 + (x_end - x_start) as f64 * t).round() as i32;
        probes.push(x.clamp(0, w - 1));
    }

    let margin = 14;
    let top_limit = candidate.y.floor() as i32 - margin;
    let bottom_limit = (candidate.y + candidate.height).ceil() as i32 - 1 + margin;
    let start_up = text.y.floor() as i32 - 1;
    let start_down = (text.y + text.height).ceil() as i32 + 1;

    let mut up_hits = Vec::new();
    let mut down_hits = Vec::new();
    for &x in &probes {
        if let Some(y) = first_edge_hit_y(frame, x, start_up, top_limit, -1, edge_thr) {
            up_hits.push(y);
        }
        if let Some(y) = first_edge_hit_y(frame, x, start_down, bottom_limit, 1, edge_thr) {
            down_hits.push(y);
        }
    }

    let min_hits = ((probe_count as f64) * 0.6).ceil() as usize;
    let top_y = dominant_level(&up_hits, 2, min_hits)?;
    let bottom_y = dominant_level(&down_hits, 2, min_hits)?;
    if top_y >= bottom_y {
        return None;
    }

    let text_top = text.y.floor() as i32;
    let text_bottom = (text.y + text.height).ceil() as i32;
    if top_y >= text_top || bottom_y <= text_bottom {
        return None;
    }
    Some((top_y, bottom_y))
}

fn first_edge_hit_y(
    frame: &ProcessedFrame,
    x: i32,
    start_y: i32,
    end_y: i32,
    step: i32,
    edge_thr: u8,
) -> Option<i32> {
    if step == 0 {
        return None;
    }
    let h = frame.height as i32;
    let w = frame.width as i32;
    let mut y = start_y.clamp(0, h - 1);
    let end = end_y.clamp(0, h - 1);
    while if step > 0 { y <= end } else { y >= end } {
        let mut hit = false;
        for dx in -1..=1 {
            let xx = (x + dx).clamp(0, w - 1) as usize;
            let yy = y as usize;
            if frame.edge[yy * frame.width + xx] >= edge_thr {
                hit = true;
                break;
            }
        }
        if hit {
            return Some(y);
        }
        y += step;
    }
    None
}

fn dominant_level(hits: &[i32], tol: i32, min_hits: usize) -> Option<i32> {
    if hits.is_empty() {
        return None;
    }
    let mut best_count = 0usize;
    let mut best_mean = 0i32;
    for &cand in hits {
        let mut count = 0usize;
        let mut sum = 0i32;
        for &y in hits {
            if (y - cand).abs() <= tol {
                count += 1;
                sum += y;
            }
        }
        if count > best_count {
            best_count = count;
            best_mean = (sum as f64 / count as f64).round() as i32;
        }
    }
    if best_count < min_hits {
        None
    } else {
        Some(best_mean)
    }
}

fn horizontal_edge_continuous(
    frame: &ProcessedFrame,
    y: i32,
    x1: i32,
    x2: i32,
    edge_thr: u8,
) -> bool {
    horizontal_edge_coverage(frame, y, x1, x2, edge_thr) >= 0.999
}

fn horizontal_edge_coverage(frame: &ProcessedFrame, y: i32, x1: i32, x2: i32, edge_thr: u8) -> f64 {
    let w = frame.width as i32;
    let h = frame.height as i32;
    let yy = y.clamp(0, h - 1);
    let mut hits = 0usize;
    let mut total = 0usize;
    for x in x1..=x2 {
        total += 1;
        let xx = x.clamp(0, w - 1);
        let mut hit = false;
        for dy in -1..=1 {
            let y2 = (yy + dy).clamp(0, h - 1) as usize;
            let x2 = xx as usize;
            if frame.edge[y2 * frame.width + x2] >= edge_thr {
                hit = true;
                break;
            }
        }
        if hit {
            hits += 1;
        }
    }
    if total == 0 {
        0.0
    } else {
        hits as f64 / total as f64
    }
}

fn best_horizontal_level(
    frame: &ProcessedFrame,
    y: i32,
    x1: i32,
    x2: i32,
    edge_thr: u8,
    radius: i32,
) -> i32 {
    let mut best_y = y;
    let mut best_cov = -1.0f64;
    for dy in -radius..=radius {
        let yy = y + dy;
        let cov = horizontal_edge_coverage(frame, yy, x1, x2, edge_thr);
        if cov > best_cov {
            best_cov = cov;
            best_y = yy;
        }
    }
    best_y
}

fn sobel_edge_bucket_enclosed(
    frame: &ProcessedFrame,
    candidate: &Bounds,
    top_y: i32,
    bottom_y: i32,
    edge_thr: u8,
    corner_skip: i32,
) -> bool {
    let w = frame.width as i32;
    let h = frame.height as i32;
    let x1 = candidate.x.floor() as i32;
    let x2 = (candidate.x + candidate.width).ceil() as i32 - 1;
    if x2 - x1 < 8 || bottom_y - top_y < 8 {
        return false;
    }
    let thr = edge_thr.saturating_sub(1).max(3);
    let outward = 12;
    let rx1 = (x1 - outward).clamp(0, w - 1);
    let rx2 = (x2 + outward).clamp(0, w - 1);
    let ry1 = (top_y - outward).clamp(0, h - 1);
    let ry2 = (bottom_y + outward).clamp(0, h - 1);
    if rx2 <= rx1 || ry2 <= ry1 {
        return false;
    }

    let rw = (rx2 - rx1 + 1) as usize;
    let rh = (ry2 - ry1 + 1) as usize;
    let n = rw * rh;
    let mut seeds = Vec::<usize>::new();
    let top_band_y1 = (top_y - 2).clamp(0, h - 1);
    let top_band_y2 = (top_y + 2).clamp(0, h - 1);
    let seed_x1 = (x1 + corner_skip).clamp(0, w - 1);
    let seed_x2 = (x2 - corner_skip).clamp(0, w - 1);
    if seed_x2 <= seed_x1 {
        return false;
    }
    for y in top_band_y1..=top_band_y2 {
        for x in seed_x1..=seed_x2 {
            if x < rx1 || x > rx2 || y < ry1 || y > ry2 {
                continue;
            }
            if frame.edge[y as usize * frame.width + x as usize] < thr {
                continue;
            }
            let lx = (x - rx1) as usize;
            let ly = (y - ry1) as usize;
            let idx = ly * rw + lx;
            seeds.push(idx);
        }
    }
    if seeds.is_empty() {
        return false;
    }

    let mut seen = vec![false; n];
    let mut parent = vec![usize::MAX; n];
    let mut q = VecDeque::<usize>::new();
    let min_cycle_component = ((x2 - x1 + 1 + bottom_y - top_y + 2) as usize / 4).max(24);

    for seed in seeds {
        if seen[seed] {
            continue;
        }
        seen[seed] = true;
        parent[seed] = seed;
        q.push_back(seed);

        let mut has_cycle = false;
        let mut escaped = false;
        let mut component_size = 0usize;

        while let Some(idx) = q.pop_front() {
            component_size += 1;
            let x = idx % rw;
            let y = idx / rw;
            if x == 0 || x + 1 == rw || y == 0 || y + 1 == rh {
                escaped = true;
            }

            let y0 = y.saturating_sub(1);
            let y1n = (y + 1).min(rh - 1);
            let x0 = x.saturating_sub(1);
            let x1n = (x + 1).min(rw - 1);
            for ny in y0..=y1n {
                for nx in x0..=x1n {
                    if nx == x && ny == y {
                        continue;
                    }
                    let nidx = ny * rw + nx;
                    let nx_abs = rx1 + nx as i32;
                    let ny_abs = ry1 + ny as i32;
                    if frame.edge[ny_abs as usize * frame.width + nx_abs as usize] < thr {
                        continue;
                    }

                    if !seen[nidx] {
                        seen[nidx] = true;
                        parent[nidx] = idx;
                        q.push_back(nidx);
                        continue;
                    }
                    if parent[idx] != nidx {
                        has_cycle = true;
                    }
                }
            }

            if has_cycle && !escaped && component_size >= min_cycle_component {
                return true;
            }
        }

        if has_cycle && !escaped && component_size >= min_cycle_component {
            return true;
        }
    }
    false
}

/// Scan outward horizontally from text edge, find the FIRST significant edge
/// using SAT strip queries. Returns x-coordinate of detected border.
fn scan_peak_edge_h(
    frame: &ProcessedFrame,
    text: &Bounds,
    direction: i32, // -1=left, +1=right
    max_expand: f64,
    strip_thick: usize,
) -> EdgeHit {
    let w = frame.width;
    let h = frame.height;
    let font_h = text.height.max(1.0);

    // Vertical range: extend slightly beyond text to catch borders.
    let y1 = (text.y as usize).max(1).min(h - 2);
    let y2 = ((text.y + text.height) as usize).min(h - 2);
    if y2 <= y1 {
        return if direction < 0 {
            EdgeHit {
                pos: text.x,
                found: false,
            }
        } else {
            EdgeHit {
                pos: text.x + text.width,
                found: false,
            }
        };
    }

    let start_x = if direction < 0 {
        (text.x as usize).max(1)
    } else {
        ((text.x + text.width) as usize).min(w - 2)
    };

    // Skip past text content: anti-aliased text edges extend a few pixels
    // and would trigger the border detector immediately.
    let skip = (font_h * 0.3).max(3.0) as usize;

    // Compute rolling baseline: average edge energy in a window behind us.
    let limit = max_expand as usize;
    let mut running_sum = 0.0f64;
    let mut running_count = 0usize;

    for step in 1..=limit {
        let x = if direction < 0 {
            if start_x < step {
                break;
            }
            start_x - step
        } else {
            let nx = start_x + step;
            if nx + strip_thick >= w {
                break;
            }
            nx
        };

        let sx1 = if direction < 0 {
            x.saturating_sub(strip_thick / 2)
        } else {
            x
        };
        let sx2 = (sx1 + strip_thick).min(w - 1);
        if sx2 <= sx1 {
            continue;
        }

        let e = frame.rect_sum(&frame.edge_sat, sx1, y1, sx2 - 1, y2 - 1);
        let pixels = (sx2 - sx1) as f64 * (y2 - y1) as f64;
        let mean_e = e / pixels.max(1.0);

        // Skip initial zone near text to avoid triggering on text content edges.
        // Don't accumulate text-edge energy into running average.
        if step <= skip {
            continue;
        }

        running_sum += mean_e;
        running_count += 1;
        let running_avg = running_sum / running_count as f64;

        // First significant edge: either absolute threshold or Nx above running average.
        if mean_e >= 4.0 && (mean_e >= running_avg * 2.5 || mean_e >= 8.0) {
            return EdgeHit {
                pos: x as f64,
                found: true,
            };
        }
    }

    // No clear edge found.
    if direction < 0 {
        EdgeHit {
            pos: (text.x - max_expand).max(0.0),
            found: false,
        }
    } else {
        EdgeHit {
            pos: (text.x + text.width + max_expand).min(w as f64),
            found: false,
        }
    }
}

/// Scan outward vertically from text edge, find the FIRST significant edge.
fn scan_peak_edge_v(
    frame: &ProcessedFrame,
    text: &Bounds,
    direction: i32,
    max_expand: f64,
    strip_thick: usize,
) -> EdgeHit {
    let w = frame.width;
    let h = frame.height;
    let font_h = text.height.max(1.0);

    let x1 = (text.x as usize).max(1).min(w - 2);
    let x2 = ((text.x + text.width) as usize).min(w - 2);
    if x2 <= x1 {
        return if direction < 0 {
            EdgeHit {
                pos: text.y,
                found: false,
            }
        } else {
            EdgeHit {
                pos: text.y + text.height,
                found: false,
            }
        };
    }

    let start_y = if direction < 0 {
        (text.y as usize).max(1)
    } else {
        ((text.y + text.height) as usize).min(h - 2)
    };

    let skip = (font_h * 0.3).max(3.0) as usize;

    let limit = max_expand as usize;
    let mut running_sum = 0.0f64;
    let mut running_count = 0usize;

    for step in 1..=limit {
        let y = if direction < 0 {
            if start_y < step {
                break;
            }
            start_y - step
        } else {
            let ny = start_y + step;
            if ny + strip_thick >= h {
                break;
            }
            ny
        };

        let sy1 = if direction < 0 {
            y.saturating_sub(strip_thick / 2)
        } else {
            y
        };
        let sy2 = (sy1 + strip_thick).min(h - 1);
        if sy2 <= sy1 {
            continue;
        }

        let e = frame.rect_sum(&frame.edge_sat, x1, sy1, x2 - 1, sy2 - 1);
        let pixels = (x2 - x1) as f64 * (sy2 - sy1) as f64;
        let mean_e = e / pixels.max(1.0);

        if step <= skip {
            continue;
        }

        running_sum += mean_e;
        running_count += 1;
        let running_avg = running_sum / running_count as f64;

        if mean_e >= 4.0 && (mean_e >= running_avg * 2.5 || mean_e >= 8.0) {
            return EdgeHit {
                pos: y as f64,
                found: true,
            };
        }
    }

    if direction < 0 {
        EdgeHit {
            pos: (text.y - max_expand).max(0.0),
            found: false,
        }
    } else {
        EdgeHit {
            pos: (text.y + text.height + max_expand).min(h as f64),
            found: false,
        }
    }
}

fn has_intervening_text(
    anchor_idx: usize,
    anchor: &Bounds,
    candidate: &Bounds,
    text_lines: &[Bounds],
) -> bool {
    let font_h = anchor.height.max(1.0);
    let right_gap = (candidate.x + candidate.width) - (anchor.x + anchor.width);
    let left_gap = anchor.x - candidate.x;
    let top_gap = anchor.y - candidate.y;
    let bottom_gap = (candidate.y + candidate.height) - (anchor.y + anchor.height);

    for (j, other) in text_lines.iter().enumerate() {
        if j == anchor_idx {
            continue;
        }

        let v_overlap = overlap_1d(
            anchor.y,
            anchor.y + anchor.height,
            other.y,
            other.y + other.height,
        );
        let h_overlap = overlap_1d(
            anchor.x,
            anchor.x + anchor.width,
            other.x,
            other.x + other.width,
        );
        let v_ratio = v_overlap / anchor.height.max(1.0);
        let h_ratio = h_overlap / anchor.width.max(1.0);

        if right_gap > font_h * 0.2 && v_ratio >= 0.35 {
            let gap = other.x - (anchor.x + anchor.width);
            if gap >= 0.0 && gap <= right_gap + 1.0 {
                return true;
            }
        }
        if left_gap > font_h * 0.2 && v_ratio >= 0.35 {
            let gap = anchor.x - (other.x + other.width);
            if gap >= 0.0 && gap <= left_gap + 1.0 {
                return true;
            }
        }
        if top_gap > font_h * 0.2 && h_ratio >= 0.30 {
            let gap = anchor.y - (other.y + other.height);
            if gap >= 0.0 && gap <= top_gap + 1.0 {
                return true;
            }
        }
        if bottom_gap > font_h * 0.2 && h_ratio >= 0.30 {
            let gap = other.y - (anchor.y + anchor.height);
            if gap >= 0.0 && gap <= bottom_gap + 1.0 {
                return true;
            }
        }
    }
    false
}

// ── Classification ──────────────────────────────────────────────────────────

/// Classify an enclosing control as TextField or Button based on geometry.
fn classify_control(_enclosing: &Bounds, _text: &Bounds) -> ControlKind {
    // Temporary simplification: treat all detected enclosed controls uniformly.
    ControlKind::Button
}

fn dedupe_controls(controls: Vec<DetectedControl>, frame: &ProcessedFrame) -> Vec<DetectedControl> {
    if controls.len() <= 1 {
        return controls;
    }
    let n = controls.len();
    let mut keep = vec![true; n];
    for i in 0..n {
        if !keep[i] {
            continue;
        }
        for j in (i + 1)..n {
            if !keep[j] {
                continue;
            }
            let ov = iou(&controls[i].bounds, &controls[j].bounds);
            if ov >= 0.50 {
                // Keep the one with better border energy.
                let ei = frame.border_energy(&controls[i].bounds, 3, 4);
                let ej = frame.border_energy(&controls[j].bounds, 3, 4);
                if ei >= ej {
                    keep[j] = false;
                } else {
                    keep[i] = false;
                    break;
                }
            }
        }
    }
    controls
        .into_iter()
        .zip(keep.iter())
        .filter(|&(_, k)| *k)
        .map(|(c, _)| c)
        .collect()
}

// ── Utilities ───────────────────────────────────────────────────────────────

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

fn iou(a: &Bounds, b: &Bounds) -> f64 {
    let inter_w = overlap_1d(a.x, a.x + a.width, b.x, b.x + b.width);
    let inter_h = overlap_1d(a.y, a.y + a.height, b.y, b.y + b.height);
    let inter = inter_w * inter_h;
    let union = a.width * a.height + b.width * b.height - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

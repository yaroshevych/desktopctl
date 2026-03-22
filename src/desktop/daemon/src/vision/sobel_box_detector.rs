use std::collections::VecDeque;

use super::{Bounds, PixelRect, ProcessedFrame, clamp_bounds_to_pixel_rect};

#[derive(Default)]
pub(super) struct SobelScratch {
    seen: Vec<u32>,
    visit_mark: u32,
    queue: VecDeque<usize>,
    seeds: Vec<usize>,
    mid_row_min_x: Vec<i32>,
    mid_row_max_x: Vec<i32>,
}

impl SobelScratch {
    #[inline]
    fn begin_component_map(&mut self, need_len: usize) -> u32 {
        if self.seen.len() < need_len {
            self.seen.resize(need_len, 0);
        }
        if self.visit_mark == u32::MAX {
            self.seen.fill(0);
            self.visit_mark = 1;
        } else if self.visit_mark == 0 {
            self.visit_mark = 1;
        }
        let mark = self.visit_mark;
        self.visit_mark = self.visit_mark.saturating_add(1);
        self.queue.clear();
        mark
    }
}

pub(super) fn refine_enclosed_candidate(
    frame: &ProcessedFrame,
    text: &Bounds,
    candidate: &Bounds,
    pre_border_e: f64,
    corner_skip: usize,
    scratch: &mut SobelScratch,
    dbg: bool,
) -> Option<Bounds> {
    let edge_thr = sobel_edge_threshold(pre_border_e);
    sobel_enclosed_candidate(frame, text, candidate, edge_thr, corner_skip, scratch, dbg)
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
    scratch: &mut SobelScratch,
    dbg: bool,
) -> Option<Bounds> {
    let candidate_px = clamp_bounds_to_pixel_rect(candidate, frame.width, frame.height)?;
    let (top_y, bottom_y) = consensus_top_bottom_edges(frame, text, candidate_px, edge_thr)?;
    let x1 = candidate_px.x1;
    let x2 = candidate_px.x2;
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
    let top_cov = horizontal_edge_confidence(frame, top_y, span_x1, span_x2, edge_thr);
    let bottom_cov = horizontal_edge_confidence(frame, bottom_y, span_x1, span_x2, edge_thr);
    // Use a magnitude-weighted gate instead of strict binary coverage.
    let min_cov = (0.82 + 0.10 * (edge_thr as f64 / 20.0)).clamp(0.82, 0.92);
    let top_ok = top_cov >= min_cov;
    let bottom_ok = bottom_cov >= min_cov;
    let span_bounds = sobel_edge_bucket_span(
        frame,
        text,
        candidate_px,
        top_y,
        bottom_y,
        edge_thr,
        cs,
        scratch,
    );
    let loop_ok = span_bounds.is_some();
    // Focused controls can have anti-aliased/colored borders where horizontal
    // coverage drops despite a closed Sobel loop. Treat loop closure as the
    // primary enclosure signal and keep a lighter fallback gate for top/bottom.
    let weak_horizontal_ok = top_cov >= 0.40 && bottom_cov >= 0.40;
    if !(loop_ok && ((top_ok && bottom_ok) || weak_horizontal_ok)) {
        if dbg {
            eprintln!(
                "      sobel-loop miss: top={} ({:.2}) bottom={} ({:.2}) loop={} span=[{},{}] thr={} min_cov={:.2}",
                top_y, top_cov, bottom_y, bottom_cov, loop_ok, span_x1, span_x2, edge_thr, min_cov
            );
        }
        return None;
    }
    let (sx1, sx2) = span_bounds?;
    if sx2 - sx1 < 8 {
        return None;
    }

    let h = frame.height as i32;
    Some(Bounds {
        x: sx1 as f64,
        y: top_y.clamp(0, h - 1) as f64,
        width: (sx2 - sx1 + 1) as f64,
        height: (bottom_y - top_y + 1).max(1) as f64,
    })
}

fn consensus_top_bottom_edges(
    frame: &ProcessedFrame,
    text: &Bounds,
    candidate: PixelRect,
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
    let top_limit = candidate.y1 - margin;
    let bottom_limit = candidate.y2 + margin;
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

fn horizontal_edge_confidence(
    frame: &ProcessedFrame,
    y: i32,
    x1: i32,
    x2: i32,
    edge_thr: u8,
) -> f64 {
    let w = frame.width as i32;
    let h = frame.height as i32;
    let yy = y.clamp(0, h - 1);
    let mut sum = 0.0f64;
    let mut total = 0usize;
    let floor = (edge_thr as i32 / 2).max(2) as f64;
    let denom = ((edge_thr as f64) - floor).max(1.0);
    for x in x1..=x2 {
        total += 1;
        let xx = x.clamp(0, w - 1);
        let mut max_e = 0u8;
        for dy in -1..=1 {
            let y2 = (yy + dy).clamp(0, h - 1) as usize;
            let x2 = xx as usize;
            max_e = max_e.max(frame.edge[y2 * frame.width + x2]);
        }
        let e = max_e as f64;
        let s = if e <= floor {
            0.0
        } else if e >= edge_thr as f64 {
            1.0
        } else {
            (e - floor) / denom
        };
        sum += s;
    }
    if total == 0 { 0.0 } else { sum / total as f64 }
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
        let cov = horizontal_edge_confidence(frame, yy, x1, x2, edge_thr);
        if cov > best_cov {
            best_cov = cov;
            best_y = yy;
        }
    }
    best_y
}

fn sobel_edge_bucket_span(
    frame: &ProcessedFrame,
    text: &Bounds,
    candidate: PixelRect,
    top_y: i32,
    bottom_y: i32,
    edge_thr: u8,
    corner_skip: i32,
    scratch: &mut SobelScratch,
) -> Option<(i32, i32)> {
    let w = frame.width as i32;
    let h = frame.height as i32;
    let x1 = candidate.x1;
    let x2 = candidate.x2;
    if x2 - x1 < 8 || bottom_y - top_y < 8 {
        return None;
    }
    let thr = edge_thr.saturating_sub(2).max(1);
    let outward = 12;
    let rx1 = (x1 - outward).clamp(0, w - 1);
    let rx2 = (x2 + outward).clamp(0, w - 1);
    let ry1 = (top_y - outward).clamp(0, h - 1);
    let ry2 = (bottom_y + outward).clamp(0, h - 1);
    if rx2 <= rx1 || ry2 <= ry1 {
        return None;
    }

    let rw = (rx2 - rx1 + 1) as usize;
    let rh = (ry2 - ry1 + 1) as usize;
    let n = rw * rh;
    let mark = scratch.begin_component_map(n);
    scratch.seeds.clear();
    let top_band_y1 = (top_y - 2).clamp(0, h - 1);
    let top_band_y2 = (top_y + 2).clamp(0, h - 1);
    let bottom_band_y1 = (bottom_y - 2).clamp(0, h - 1);
    let bottom_band_y2 = (bottom_y + 2).clamp(0, h - 1);
    // Seed around the anchor text, not around scanned left/right border.
    let seed_pad = (text.height * 1.2).round() as i32 + corner_skip;
    let seed_x1 = (text.x.floor() as i32 - seed_pad).clamp(rx1, rx2);
    let seed_x2 = ((text.x + text.width).ceil() as i32 + seed_pad).clamp(rx1, rx2);
    if seed_x2 <= seed_x1 {
        return None;
    }
    for y in top_band_y1..=top_band_y2 {
        gather_band_seeds(
            frame,
            y,
            seed_x1,
            seed_x2,
            rx1,
            ry1,
            rw,
            thr,
            &mut scratch.seeds,
        );
    }
    if scratch.seeds.is_empty() {
        for y in bottom_band_y1..=bottom_band_y2 {
            gather_band_seeds(
                frame,
                y,
                seed_x1,
                seed_x2,
                rx1,
                ry1,
                rw,
                thr,
                &mut scratch.seeds,
            );
        }
    }
    if scratch.seeds.is_empty() {
        return None;
    }

    let min_component = ((x2 - x1 + 1 + bottom_y - top_y + 2) as usize / 4).max(24);
    let h_band = (bottom_y - top_y + 1).max(1);
    let mid_y1 = (top_y + h_band / 4).clamp(0, h - 1);
    let mid_y2 = (bottom_y - h_band / 4).clamp(0, h - 1);
    let mid_rows = (mid_y2 - mid_y1 + 1).max(0) as usize;
    if mid_rows == 0 {
        return None;
    }
    if scratch.mid_row_min_x.len() < mid_rows {
        scratch.mid_row_min_x.resize(mid_rows, i32::MAX);
        scratch.mid_row_max_x.resize(mid_rows, i32::MIN);
    }
    let side_tol = ((h_band as f64) * 0.12).round().clamp(2.0, 7.0) as i32;

    let seed_count = scratch.seeds.len();
    for si in 0..seed_count {
        let seed = scratch.seeds[si];
        if scratch.seen[seed] == mark {
            continue;
        }
        scratch.seen[seed] = mark;
        scratch.queue.push_back(seed);

        for row in 0..mid_rows {
            scratch.mid_row_min_x[row] = i32::MAX;
            scratch.mid_row_max_x[row] = i32::MIN;
        }

        let mut component_size = 0usize;
        let mut touches_top = false;
        let mut touches_bottom = false;
        let mut comp_min_x = i32::MAX;
        let mut comp_max_x = i32::MIN;

        while let Some(idx) = scratch.queue.pop_front() {
            component_size += 1;
            let x = idx % rw;
            let y = idx / rw;
            let x_abs = rx1 + x as i32;
            let y_abs = ry1 + y as i32;
            comp_min_x = comp_min_x.min(x_abs);
            comp_max_x = comp_max_x.max(x_abs);
            if y_abs >= top_band_y1 && y_abs <= top_band_y2 {
                touches_top = true;
            }
            if y_abs >= bottom_band_y1 && y_abs <= bottom_band_y2 {
                touches_bottom = true;
            }
            if y_abs >= mid_y1 && y_abs <= mid_y2 {
                let row = (y_abs - mid_y1) as usize;
                scratch.mid_row_min_x[row] = scratch.mid_row_min_x[row].min(x_abs);
                scratch.mid_row_max_x[row] = scratch.mid_row_max_x[row].max(x_abs);
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

                    if scratch.seen[nidx] != mark {
                        scratch.seen[nidx] = mark;
                        scratch.queue.push_back(nidx);
                    }
                }
            }
        }

        let mut left_hits = 0usize;
        let mut right_hits = 0usize;
        for row in 0..mid_rows {
            let row_min = scratch.mid_row_min_x[row];
            let row_max = scratch.mid_row_max_x[row];
            if row_min == i32::MAX {
                continue;
            }
            if row_min <= comp_min_x + side_tol {
                left_hits += 1;
            }
            if row_max >= comp_max_x - side_tol {
                right_hits += 1;
            }
        }
        let need = (mid_rows / 5).max(3);
        let side_support = left_hits >= need && right_hits >= need;
        if component_size >= min_component && touches_top && touches_bottom && side_support {
            return Some((comp_min_x, comp_max_x));
        }
    }
    None
}

fn gather_band_seeds(
    frame: &ProcessedFrame,
    y: i32,
    x1: i32,
    x2: i32,
    rx1: i32,
    ry1: i32,
    rw: usize,
    thr: u8,
    out: &mut Vec<usize>,
) {
    if y < 0 || y >= frame.height as i32 {
        return;
    }
    for x in x1..=x2 {
        if x < 0 || x >= frame.width as i32 {
            continue;
        }
        let idx = y as usize * frame.width + x as usize;
        if frame.edge[idx] < thr {
            continue;
        }
        let lx = (x - rx1) as usize;
        let ly = (y - ry1) as usize;
        out.push(ly * rw + lx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_frame(width: usize, height: usize, edge: Vec<u8>) -> ProcessedFrame {
        let sw = width + 1;
        let sh = height + 1;
        let sat_len = sw * sh;
        let mut edge_sat = vec![0.0f64; sat_len];
        for y in 0..height {
            for x in 0..width {
                let idx = y * width + x;
                let si = (y + 1) * sw + (x + 1);
                let e = edge[idx] as f64;
                edge_sat[si] = e + edge_sat[si - 1] + edge_sat[si - sw] - edge_sat[si - sw - 1];
            }
        }
        ProcessedFrame {
            edge_sat,
            gray_sat: vec![0.0; sat_len],
            gray_sq_sat: vec![0.0; sat_len],
            gray: vec![0; width * height],
            edge,
            text_mask: vec![false; width * height],
            text_mask_sat: vec![0.0; sat_len],
            width,
            height,
        }
    }

    fn draw_rect_border(
        edge: &mut [u8],
        width: usize,
        x1: usize,
        y1: usize,
        x2: usize,
        y2: usize,
        val: u8,
        draw_top: bool,
    ) {
        if draw_top {
            for x in x1..=x2 {
                edge[y1 * width + x] = val;
            }
        }
        for x in x1..=x2 {
            edge[y2 * width + x] = val;
        }
        for y in y1..=y2 {
            edge[y * width + x1] = val;
            edge[y * width + x2] = val;
        }
    }

    #[test]
    fn refine_accepts_closed_border() {
        let width = 100usize;
        let height = 60usize;
        let mut edge = vec![0u8; width * height];
        draw_rect_border(&mut edge, width, 12, 10, 78, 34, 28, true);
        let frame = make_frame(width, height, edge);
        let text = Bounds {
            x: 32.0,
            y: 18.0,
            width: 18.0,
            height: 8.0,
        };
        let candidate = Bounds {
            x: 10.0,
            y: 8.0,
            width: 72.0,
            height: 30.0,
        };
        let mut scratch = SobelScratch::default();
        let out =
            refine_enclosed_candidate(&frame, &text, &candidate, 12.0, 3, &mut scratch, false);
        assert!(out.is_some(), "closed border should be accepted");
    }

    #[test]
    fn refine_rejects_open_top_border() {
        let width = 100usize;
        let height = 60usize;
        let mut edge = vec![0u8; width * height];
        draw_rect_border(&mut edge, width, 12, 10, 78, 34, 28, false);
        let frame = make_frame(width, height, edge);
        let text = Bounds {
            x: 32.0,
            y: 18.0,
            width: 18.0,
            height: 8.0,
        };
        let candidate = Bounds {
            x: 10.0,
            y: 8.0,
            width: 72.0,
            height: 30.0,
        };
        let mut scratch = SobelScratch::default();
        let out =
            refine_enclosed_candidate(&frame, &text, &candidate, 12.0, 3, &mut scratch, false);
        assert!(out.is_none(), "open top border should be rejected");
    }
}

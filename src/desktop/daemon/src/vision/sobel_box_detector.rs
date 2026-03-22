use std::collections::VecDeque;

use super::{Bounds, PixelRect, ProcessedFrame, clamp_bounds_to_pixel_rect};

#[derive(Default)]
pub(super) struct SobelScratch {
    seen: Vec<u32>,
    visit_mark: u32,
    queue: VecDeque<usize>,
    seeds: Vec<usize>,
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

#[derive(Debug, Clone)]
struct EdgeConsensus {
    top_anchor: i32,
    bottom_anchor: i32,
    top_levels: Vec<i32>,
    bottom_levels: Vec<i32>,
    top_raw_hits: Vec<i32>,
    bottom_raw_hits: Vec<i32>,
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
    let consensus = consensus_top_bottom_edges(frame, text, candidate_px, edge_thr)?;
    let top_y = consensus.top_anchor;
    let bottom_y = consensus.bottom_anchor;
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

    let top_anchor = best_horizontal_level(frame, top_y, span_x1, span_x2, edge_thr, 4);
    let bottom_anchor = best_horizontal_level(frame, bottom_y, span_x1, span_x2, edge_thr, 4);
    let top_cov_anchor = horizontal_edge_confidence(frame, top_anchor, span_x1, span_x2, edge_thr);
    let bottom_cov_anchor =
        horizontal_edge_confidence(frame, bottom_anchor, span_x1, span_x2, edge_thr);
    // Use a magnitude-weighted gate instead of strict binary coverage.
    let min_cov = (0.82 + 0.10 * (edge_thr as f64 / 20.0)).clamp(0.82, 0.92);
    let mut top_y = top_anchor;
    let mut bottom_y = bottom_anchor;
    let mut top_cov = top_cov_anchor;
    let mut bottom_cov = bottom_cov_anchor;
    let mut span_bounds = sobel_edge_bucket_span(
        frame,
        text,
        candidate_px,
        top_y,
        bottom_y,
        edge_thr,
        cs,
        scratch,
    );
    if span_bounds.is_none() {
        if let Some((alt_top, alt_bottom, alt_span, alt_top_cov, alt_bottom_cov)) =
            find_best_closed_span(
                frame,
                text,
                candidate_px,
                top_anchor,
                bottom_anchor,
                &consensus.top_levels,
                &consensus.bottom_levels,
                &consensus.top_raw_hits,
                &consensus.bottom_raw_hits,
                span_x1,
                span_x2,
                edge_thr,
                cs,
                scratch,
            )
        {
            top_y = alt_top;
            bottom_y = alt_bottom;
            top_cov = alt_top_cov;
            bottom_cov = alt_bottom_cov;
            span_bounds = Some(alt_span);
        }
    }
    let loop_ok = span_bounds.is_some();
    let top_ok = top_cov >= min_cov;
    let bottom_ok = bottom_cov >= min_cov;
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

fn find_best_closed_span(
    frame: &ProcessedFrame,
    text: &Bounds,
    candidate: PixelRect,
    top_anchor: i32,
    bottom_anchor: i32,
    top_seed_levels: &[i32],
    bottom_seed_levels: &[i32],
    top_probe_hits: &[i32],
    bottom_probe_hits: &[i32],
    span_x1: i32,
    span_x2: i32,
    edge_thr: u8,
    corner_skip: i32,
    scratch: &mut SobelScratch,
) -> Option<(i32, i32, (i32, i32), f64, f64)> {
    let top_candidates = horizontal_candidates(
        frame,
        top_anchor,
        span_x1,
        span_x2,
        edge_thr,
        8,
        top_seed_levels,
        top_probe_hits,
    );
    let bottom_candidates = horizontal_candidates(
        frame,
        bottom_anchor,
        span_x1,
        span_x2,
        edge_thr,
        8,
        bottom_seed_levels,
        bottom_probe_hits,
    );
    let mut best: Option<(i32, i32, (i32, i32), f64, f64, f64)> = None;
    for &(ty, tcov) in &top_candidates {
        for &(by, bcov) in &bottom_candidates {
            if by - ty < 8 {
                continue;
            }
            if let Some(span) = sobel_edge_bucket_span(
                frame,
                text,
                candidate,
                ty,
                by,
                edge_thr,
                corner_skip,
                scratch,
            ) {
                let score = tcov.min(bcov) * 2.0 + (tcov + bcov) * 0.5;
                match best {
                    Some((_, _, _, _, _, best_score)) if score <= best_score => {}
                    _ => {
                        best = Some((ty, by, span, tcov, bcov, score));
                    }
                }
            }
        }
    }
    best.map(|(ty, by, span, tcov, bcov, _)| (ty, by, span, tcov, bcov))
}

fn horizontal_candidates(
    frame: &ProcessedFrame,
    center_y: i32,
    x1: i32,
    x2: i32,
    edge_thr: u8,
    radius: i32,
    seed_levels: &[i32],
    probe_hits: &[i32],
) -> Vec<(i32, f64)> {
    let mut ys =
        Vec::with_capacity(seed_levels.len() + probe_hits.len() + (radius as usize) * 2 + 1);
    ys.extend_from_slice(seed_levels);
    ys.extend_from_slice(probe_hits);
    for dy in -radius..=radius {
        ys.push(center_y + dy);
    }
    ys.sort_unstable();
    ys.dedup();
    let mut out = Vec::with_capacity(ys.len());
    for yy in ys {
        let cov = horizontal_edge_confidence(frame, yy, x1, x2, edge_thr);
        out.push((yy, cov));
    }
    out.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    out.truncate(5);
    out
}

fn consensus_top_bottom_edges(
    frame: &ProcessedFrame,
    text: &Bounds,
    candidate: PixelRect,
    edge_thr: u8,
) -> Option<EdgeConsensus> {
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
    let text_top = text.y.floor() as i32;
    let text_bottom = (text.y + text.height).ceil() as i32;
    // Keep a guard band away from glyph strokes to avoid selecting text edges
    // as control border candidates.
    let text_edge_guard = ((text.height * 0.22).round() as i32).clamp(3, 10);
    let top_limit = candidate.y1 - margin;
    let bottom_limit = candidate.y2 + margin;
    let start_up = text_top - text_edge_guard;
    let start_down = text_bottom + text_edge_guard;

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
    let top_levels = dominant_levels(&up_hits, 2, min_hits, 4);
    let bottom_levels = dominant_levels(&down_hits, 2, min_hits, 4);
    let top_y = *top_levels.first()?;
    let bottom_y = *bottom_levels.first()?;
    if top_y >= bottom_y {
        return None;
    }

    if top_y >= text_top - text_edge_guard || bottom_y <= text_bottom + text_edge_guard {
        return None;
    }
    Some(EdgeConsensus {
        top_anchor: top_y,
        bottom_anchor: bottom_y,
        top_levels,
        bottom_levels,
        top_raw_hits: unique_levels(&up_hits),
        bottom_raw_hits: unique_levels(&down_hits),
    })
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

fn dominant_levels(hits: &[i32], tol: i32, min_hits: usize, max_levels: usize) -> Vec<i32> {
    if hits.is_empty() || max_levels == 0 {
        return Vec::new();
    }
    let mut remaining = hits.to_vec();
    let mut out = Vec::new();
    while !remaining.is_empty() && out.len() < max_levels {
        let mut best_count = 0usize;
        let mut best_center = 0i32;
        let mut best_mean = 0i32;
        for &cand in &remaining {
            let mut count = 0usize;
            let mut sum = 0i32;
            for &y in &remaining {
                if (y - cand).abs() <= tol {
                    count += 1;
                    sum += y;
                }
            }
            if count > best_count {
                best_count = count;
                best_center = cand;
                best_mean = (sum as f64 / count as f64).round() as i32;
            }
        }
        if best_count < min_hits {
            break;
        }
        out.push(best_mean);
        let clear_tol = tol.saturating_mul(2).max(1);
        remaining.retain(|y| (*y - best_center).abs() > clear_tol);
    }
    out
}

fn unique_levels(levels: &[i32]) -> Vec<i32> {
    let mut out = levels.to_vec();
    out.sort_unstable();
    out.dedup();
    out
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
    let min_span = ((x2 - x1 + 1) as f64 * 0.72).round().max(18.0) as i32;

    let seed_count = scratch.seeds.len();
    for si in 0..seed_count {
        let seed = scratch.seeds[si];
        if scratch.seen[seed] == mark {
            continue;
        }
        scratch.seen[seed] = mark;
        scratch.queue.push_back(seed);

        let mut component_size = 0usize;
        let mut top_hits = 0usize;
        let mut bottom_hits = 0usize;
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
                top_hits += 1;
            }
            if y_abs >= bottom_band_y1 && y_abs <= bottom_band_y2 {
                bottom_hits += 1;
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

        let span = comp_max_x - comp_min_x + 1;
        if component_size >= min_component && top_hits >= 3 && bottom_hits >= 3 && span >= min_span
        {
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

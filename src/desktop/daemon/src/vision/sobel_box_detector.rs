use std::collections::VecDeque;

use super::{Bounds, ProcessedFrame};

pub(super) fn refine_enclosed_candidate(
    frame: &ProcessedFrame,
    text: &Bounds,
    candidate: &Bounds,
    pre_border_e: f64,
    corner_skip: usize,
    dbg: bool,
) -> Option<Bounds> {
    let edge_thr = sobel_edge_threshold(pre_border_e);
    sobel_enclosed_candidate(frame, text, candidate, edge_thr, corner_skip, dbg)
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

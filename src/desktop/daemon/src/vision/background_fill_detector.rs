use super::{Bounds, ProcessedFrame};

pub(super) fn distinct_background_delta(
    frame: &ProcessedFrame,
    candidate: &Bounds,
    text: &Bounds,
    font_h: f64,
) -> Option<f64> {
    let text_area = (text.width * text.height).max(1.0);
    let cand_area = (candidate.width * candidate.height).max(1.0);
    let width_ratio = candidate.width / text.width.max(1.0);
    let area_ratio = cand_area / text_area;
    // Background fallback is only meant for compact control-like candidates,
    // not full-panel containers or tiny-symbol expansions.
    if width_ratio > 4.5 || area_ratio > 8.0 {
        return None;
    }
    if candidate.width > (font_h * 14.0).min(frame.width as f64 * 0.40) {
        return None;
    }

    let w = frame.width as i32;
    let h = frame.height as i32;
    let x1 = candidate.x.floor() as i32;
    let y1 = candidate.y.floor() as i32;
    let x2 = (candidate.x + candidate.width).ceil() as i32 - 1;
    let y2 = (candidate.y + candidate.height).ceil() as i32 - 1;
    if x2 - x1 < 10 || y2 - y1 < 10 {
        return None;
    }

    let outer_band = (font_h * 0.45).round().clamp(3.0, 10.0) as i32;
    let tx_pad_x = (font_h * 0.8).round().clamp(4.0, 18.0) as i32;
    let tx_pad_y = (font_h * 0.45).round().clamp(2.0, 10.0) as i32;
    let tx1 = text.x.floor() as i32 - tx_pad_x;
    let ty1 = text.y.floor() as i32 - tx_pad_y;
    let tx2 = (text.x + text.width).ceil() as i32 - 1 + tx_pad_x;
    let ty2 = (text.y + text.height).ceil() as i32 - 1 + tx_pad_y;

    let mut inner_sum = 0.0f64;
    let mut inner_sum_sq = 0.0f64;
    let mut inner_n = 0usize;
    for y in (y1 + 1).max(0)..=(y2 - 1).min(h - 1) {
        for x in (x1 + 1).max(0)..=(x2 - 1).min(w - 1) {
            if x >= tx1 && x <= tx2 && y >= ty1 && y <= ty2 {
                continue;
            }
            let idx = y as usize * frame.width + x as usize;
            if frame.text_mask[idx] {
                continue;
            }
            let g = frame.gray[idx] as f64;
            inner_sum += g;
            inner_sum_sq += g * g;
            inner_n += 1;
        }
    }

    let rx1 = (x1 - outer_band).clamp(0, w - 1);
    let ry1 = (y1 - outer_band).clamp(0, h - 1);
    let rx2 = (x2 + outer_band).clamp(0, w - 1);
    let ry2 = (y2 + outer_band).clamp(0, h - 1);
    let mut outer_sum = 0.0f64;
    let mut outer_sum_sq = 0.0f64;
    let mut outer_n = 0usize;
    for y in ry1..=ry2 {
        for x in rx1..=rx2 {
            if x >= x1 && x <= x2 && y >= y1 && y <= y2 {
                continue;
            }
            let dx = if x < x1 {
                x1 - x
            } else if x > x2 {
                x - x2
            } else {
                0
            };
            let dy = if y < y1 {
                y1 - y
            } else if y > y2 {
                y - y2
            } else {
                0
            };
            if dx.max(dy) > outer_band {
                continue;
            }
            let idx = y as usize * frame.width + x as usize;
            if frame.text_mask[idx] {
                continue;
            }
            let g = frame.gray[idx] as f64;
            outer_sum += g;
            outer_sum_sq += g * g;
            outer_n += 1;
        }
    }

    if inner_n < 30 || outer_n < 50 {
        return None;
    }

    let inner_mean = inner_sum / inner_n as f64;
    let outer_mean = outer_sum / outer_n as f64;
    let delta = (inner_mean - outer_mean).abs();

    let inner_var = (inner_sum_sq / inner_n as f64) - inner_mean * inner_mean;
    let outer_var = (outer_sum_sq / outer_n as f64) - outer_mean * outer_mean;
    let inner_std = inner_var.max(0.0).sqrt();
    let outer_std = outer_var.max(0.0).sqrt();
    let texture = inner_std.min(outer_std);

    // Require both absolute and relative separation so flat noise doesn't pass.
    if delta >= 7.0 && (delta >= texture * 0.8 + 3.0 || delta >= 12.0) {
        Some(delta)
    } else {
        None
    }
}

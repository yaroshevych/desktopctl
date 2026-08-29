use super::{Bounds, PixelRect, ProcessedFrame, clamp_bounds_to_pixel_rect};

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

    let cand = clamp_bounds_to_pixel_rect(candidate, frame.width, frame.height)?;
    if cand.width() < 11 || cand.height() < 11 {
        return None;
    }

    let outer_band = (font_h * 0.45).round().clamp(3.0, 10.0) as i32;
    let tx_pad_x = (font_h * 0.8).round().clamp(4.0, 18.0) as i32;
    let tx_pad_y = (font_h * 0.45).round().clamp(2.0, 10.0) as i32;
    let tx_rect = clamp_bounds_to_pixel_rect(
        &Bounds {
            x: text.x - tx_pad_x as f64,
            y: text.y - tx_pad_y as f64,
            width: text.width + (tx_pad_x * 2) as f64,
            height: text.height + (tx_pad_y * 2) as f64,
        },
        frame.width,
        frame.height,
    );
    let inner_rect = PixelRect {
        x1: cand.x1 + 1,
        y1: cand.y1 + 1,
        x2: cand.x2 - 1,
        y2: cand.y2 - 1,
    };
    if inner_rect.x2 <= inner_rect.x1 || inner_rect.y2 <= inner_rect.y1 {
        return None;
    }
    let mut inner = stats_on_rect(frame, inner_rect);
    if let Some(tx) = tx_rect.and_then(|r| intersect_rect(inner_rect, r)) {
        inner = inner.subtract(stats_on_rect(frame, tx));
    }

    let outer_rect = PixelRect {
        x1: (cand.x1 - outer_band).clamp(0, frame.width as i32 - 1),
        y1: (cand.y1 - outer_band).clamp(0, frame.height as i32 - 1),
        x2: (cand.x2 + outer_band).clamp(0, frame.width as i32 - 1),
        y2: (cand.y2 + outer_band).clamp(0, frame.height as i32 - 1),
    };
    let outer = stats_on_rect(frame, outer_rect).subtract(stats_on_rect(frame, cand));

    if inner.count < 30 || outer.count < 50 {
        return None;
    }
    let inner_text_ratio = inner.text_count as f64 / inner.count.max(1) as f64;
    let outer_text_ratio = outer.text_count as f64 / outer.count.max(1) as f64;
    if inner_text_ratio > 0.20 || outer_text_ratio > 0.20 {
        return None;
    }

    let inner_mean = inner.sum / inner.count as f64;
    let outer_mean = outer.sum / outer.count as f64;
    let delta = (inner_mean - outer_mean).abs();

    let inner_var = (inner.sum_sq / inner.count as f64) - inner_mean * inner_mean;
    let outer_var = (outer.sum_sq / outer.count as f64) - outer_mean * outer_mean;
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

#[derive(Clone, Copy, Debug, Default)]
struct RegionStats {
    sum: f64,
    sum_sq: f64,
    count: usize,
    text_count: usize,
}

impl RegionStats {
    #[inline]
    fn subtract(self, other: RegionStats) -> RegionStats {
        RegionStats {
            sum: self.sum - other.sum,
            sum_sq: self.sum_sq - other.sum_sq,
            count: self.count.saturating_sub(other.count),
            text_count: self.text_count.saturating_sub(other.text_count),
        }
    }
}

fn intersect_rect(a: PixelRect, b: PixelRect) -> Option<PixelRect> {
    let x1 = a.x1.max(b.x1);
    let y1 = a.y1.max(b.y1);
    let x2 = a.x2.min(b.x2);
    let y2 = a.y2.min(b.y2);
    if x2 < x1 || y2 < y1 {
        None
    } else {
        Some(PixelRect { x1, y1, x2, y2 })
    }
}

fn stats_on_rect(frame: &ProcessedFrame, rect: PixelRect) -> RegionStats {
    let x1 = rect.x1.max(0) as usize;
    let y1 = rect.y1.max(0) as usize;
    let x2 = rect.x2.max(0) as usize;
    let y2 = rect.y2.max(0) as usize;
    if x2 < x1 || y2 < y1 {
        return RegionStats::default();
    }
    let count = (x2 - x1 + 1) * (y2 - y1 + 1);
    RegionStats {
        sum: frame.rect_sum(&frame.gray_sat, x1, y1, x2, y2),
        sum_sq: frame.rect_sum(&frame.gray_sq_sat, x1, y1, x2, y2),
        count,
        text_count: frame.rect_sum(&frame.text_mask_sat, x1, y1, x2, y2).round() as usize,
    }
}

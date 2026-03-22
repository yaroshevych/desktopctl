use desktop_core::protocol::Bounds;

use super::metal_pipeline::ProcessedFrame;
#[path = "background_fill_detector.rs"]
mod background_fill_detector;
#[path = "sobel_box_detector.rs"]
mod sobel_box_detector;

// Threshold rationale and tuning notes:
// src/desktop/daemon/src/vision/CONTROL_DETECTION.md

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

#[derive(Debug, Clone, Copy)]
pub(super) struct PixelRect {
    pub x1: i32,
    pub y1: i32,
    pub x2: i32,
    pub y2: i32,
}

impl PixelRect {
    #[inline]
    pub fn width(self) -> i32 {
        self.x2 - self.x1 + 1
    }

    #[inline]
    pub fn height(self) -> i32 {
        self.y2 - self.y1 + 1
    }
}

#[derive(Debug, Clone, Copy)]
enum ScanAxis {
    Horizontal,
    Vertical,
}

#[derive(Debug, Clone, Copy)]
enum ConflictReason {
    ContainsOtherText,
    InterveningText,
}

/// Detect enclosed control boxes by expanding from text anchors until border
/// evidence is found.
///
/// For now we optimize only for box geometry quality; caller-visible kind is
/// intentionally normalized to `Button` until control-type classification is
/// revisited.
pub fn detect_controls(frame: &ProcessedFrame, text_lines: &[Bounds]) -> Vec<DetectedControl> {
    let dbg = debug_enabled();
    let mut controls = Vec::new();
    let mut sobel_scratch = sobel_box_detector::SobelScratch::default();
    let mut y_centers: Vec<(f64, usize)> = text_lines
        .iter()
        .enumerate()
        .map(|(idx, b)| (b.y + b.height * 0.5, idx))
        .collect();
    y_centers.sort_by(|(ay, _), (by, _)| ay.partial_cmp(by).unwrap_or(std::cmp::Ordering::Equal));

    for (idx, text) in text_lines.iter().enumerate() {
        let Some(enclosing) = find_enclosing_control(frame, text, &mut sobel_scratch) else {
            if dbg {
                eprintln!(
                    "  detect_control: NONE for text=[{:.0},{:.0},{:.0},{:.0}]",
                    text.x, text.y, text.width, text.height,
                );
            }
            continue;
        };

        if let Some(reason) =
            conflict_with_other_text(idx, text, &enclosing, text_lines, &y_centers)
        {
            if dbg {
                let reason = match reason {
                    ConflictReason::ContainsOtherText => "CONTAINER",
                    ConflictReason::InterveningText => "BLOCKED_BY_TEXT",
                };
                eprintln!(
                    "  detect_control: {} [{:.0},{:.0},{:.0},{:.0}] text=[{:.0},{:.0},{:.0},{:.0}]",
                    reason,
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
fn find_enclosing_control(
    frame: &ProcessedFrame,
    text: &Bounds,
    sobel_scratch: &mut sobel_box_detector::SobelScratch,
) -> Option<Bounds> {
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

    // Scan outward from the anchor in all four directions.
    let left = scan_peak_edge(frame, text, ScanAxis::Horizontal, -1, max_x, strip_thick);
    let right = scan_peak_edge(frame, text, ScanAxis::Horizontal, 1, max_x, strip_thick);
    let top = scan_peak_edge(frame, text, ScanAxis::Vertical, -1, max_y, strip_thick);
    let bottom = scan_peak_edge(frame, text, ScanAxis::Vertical, 1, max_y, strip_thick);

    // If any side failed to find a real edge (and fell back to max-expand),
    // this is not an enclosed control box.
    if !(left.found && right.found && top.found && bottom.found) {
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
    let mut used_bg_fallback = false;
    let mut bg_delta = 0.0f64;
    let candidate = if let Some(refined) = sobel_box_detector::refine_enclosed_candidate(
        frame,
        text,
        &candidate,
        pre_border_e,
        corner_skip,
        sobel_scratch,
        dbg,
    ) {
        refined
    } else if let Some(delta) =
        background_fill_detector::distinct_background_delta(frame, &candidate, text, font_h)
    {
        used_bg_fallback = true;
        bg_delta = delta;
        if dbg {
            eprintln!(
                "    find_enclosing BG-FALLBACK(delta={:.1}): text=[{:.0},{:.0},{:.0},{:.0}] box=[{:.0},{:.0},{:.0},{:.0}]",
                delta,
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
        candidate
    } else {
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
    let energy_ok = ratio >= 1.15
        || border_e >= 3.0
        || (glow_e >= 1.2 && ratio >= 0.95)
        || (used_bg_fallback && bg_delta >= 7.0);
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

/// Scan outward from a text edge and return the first significant strip edge.
fn scan_peak_edge(
    frame: &ProcessedFrame,
    text: &Bounds,
    axis: ScanAxis,
    direction: i32,
    max_expand: f64,
    strip_thick: usize,
) -> EdgeHit {
    let w = frame.width;
    let h = frame.height;
    if w < 4 || h < 4 {
        return match (axis, direction) {
            (ScanAxis::Horizontal, d) if d < 0 => EdgeHit {
                pos: text.x,
                found: false,
            },
            (ScanAxis::Horizontal, _) => EdgeHit {
                pos: text.x + text.width,
                found: false,
            },
            (ScanAxis::Vertical, d) if d < 0 => EdgeHit {
                pos: text.y,
                found: false,
            },
            (ScanAxis::Vertical, _) => EdgeHit {
                pos: text.y + text.height,
                found: false,
            },
        };
    }
    let font_h = text.height.max(1.0);
    let skip = (font_h * 0.3).max(3.0) as usize;
    let limit = max_expand as usize;

    let (fixed_1, fixed_2, start_pos, miss_pos) = match axis {
        ScanAxis::Horizontal => {
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
            let miss = if direction < 0 {
                (text.x - max_expand).max(0.0)
            } else {
                (text.x + text.width + max_expand).min(w as f64)
            };
            (y1, y2, start_x, miss)
        }
        ScanAxis::Vertical => {
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
            let miss = if direction < 0 {
                (text.y - max_expand).max(0.0)
            } else {
                (text.y + text.height + max_expand).min(h as f64)
            };
            (x1, x2, start_y, miss)
        }
    };

    let mut running_sum = 0.0f64;
    let mut running_count = 0usize;

    for step in 1..=limit {
        let pos = if direction < 0 {
            if start_pos < step {
                break;
            }
            start_pos - step
        } else {
            let next = start_pos + step;
            match axis {
                ScanAxis::Horizontal if next + strip_thick >= w => break,
                ScanAxis::Vertical if next + strip_thick >= h => break,
                _ => {}
            }
            next
        };

        let (x1, y1, x2, y2, pixels) = match axis {
            ScanAxis::Horizontal => {
                let sx1 = if direction < 0 {
                    pos.saturating_sub(strip_thick / 2)
                } else {
                    pos
                };
                let sx2 = (sx1 + strip_thick).min(w - 1);
                if sx2 <= sx1 {
                    continue;
                }
                let px = (sx2 - sx1) as f64 * (fixed_2 - fixed_1) as f64;
                (sx1, fixed_1, sx2 - 1, fixed_2 - 1, px)
            }
            ScanAxis::Vertical => {
                let sy1 = if direction < 0 {
                    pos.saturating_sub(strip_thick / 2)
                } else {
                    pos
                };
                let sy2 = (sy1 + strip_thick).min(h - 1);
                if sy2 <= sy1 {
                    continue;
                }
                let px = (fixed_2 - fixed_1) as f64 * (sy2 - sy1) as f64;
                (fixed_1, sy1, fixed_2 - 1, sy2 - 1, px)
            }
        };

        let e = frame.rect_sum(&frame.edge_sat, x1, y1, x2, y2);
        let mean_e = e / pixels.max(1.0);

        if step <= skip {
            continue;
        }
        running_sum += mean_e;
        running_count += 1;
        let running_avg = running_sum / running_count as f64;
        if mean_e >= 4.0 && (mean_e >= running_avg * 2.5 || mean_e >= 8.0) {
            return EdgeHit {
                pos: pos as f64,
                found: true,
            };
        }
    }

    EdgeHit {
        pos: miss_pos,
        found: false,
    }
}

fn conflict_with_other_text(
    anchor_idx: usize,
    anchor: &Bounds,
    candidate: &Bounds,
    text_lines: &[Bounds],
    sorted_y_centers: &[(f64, usize)],
) -> Option<ConflictReason> {
    let font_h = anchor.height.max(1.0);
    let anchor_cy = anchor.y + anchor.height * 0.5;
    let query_y1 = (candidate.y.min(anchor.y) - font_h * 2.0).max(0.0);
    let query_y2 = (candidate.y + candidate.height).max(anchor.y + anchor.height) + font_h * 2.0;
    let lo = sorted_y_centers.partition_point(|(cy, _)| *cy < query_y1);
    let hi = sorted_y_centers.partition_point(|(cy, _)| *cy <= query_y2);

    let right_gap = (candidate.x + candidate.width) - (anchor.x + anchor.width);
    let left_gap = anchor.x - candidate.x;
    let top_gap = anchor.y - candidate.y;
    let bottom_gap = (candidate.y + candidate.height) - (anchor.y + anchor.height);

    for &(_, j) in &sorted_y_centers[lo..hi] {
        if j == anchor_idx {
            continue;
        }
        let other = &text_lines[j];

        let other_cy = other.y + other.height * 0.5;
        let same_row = (anchor_cy - other_cy).abs() <= font_h * 1.5;
        if same_row
            && overlap_1d(
                other.x,
                other.x + other.width,
                candidate.x,
                candidate.x + candidate.width,
            ) > 0.0
        {
            return Some(ConflictReason::ContainsOtherText);
        }

        let other_cx = other.x + other.width * 0.5;
        if other_cx >= candidate.x
            && other_cx <= candidate.x + candidate.width
            && other_cy >= candidate.y
            && other_cy <= candidate.y + candidate.height
        {
            return Some(ConflictReason::ContainsOtherText);
        }

        let ov_x = overlap_1d(
            other.x,
            other.x + other.width,
            candidate.x,
            candidate.x + candidate.width,
        );
        let ov_y = overlap_1d(
            other.y,
            other.y + other.height,
            candidate.y,
            candidate.y + candidate.height,
        );
        let ov_area = ov_x * ov_y;
        let other_area = (other.width * other.height).max(1.0);
        if ov_area / other_area >= 0.15 {
            return Some(ConflictReason::ContainsOtherText);
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
                return Some(ConflictReason::InterveningText);
            }
        }
        if left_gap > font_h * 0.2 && v_ratio >= 0.35 {
            let gap = anchor.x - (other.x + other.width);
            if gap >= 0.0 && gap <= left_gap + 1.0 {
                return Some(ConflictReason::InterveningText);
            }
        }
        if top_gap > font_h * 0.2 && h_ratio >= 0.30 {
            let gap = anchor.y - (other.y + other.height);
            if gap >= 0.0 && gap <= top_gap + 1.0 {
                return Some(ConflictReason::InterveningText);
            }
        }
        if bottom_gap > font_h * 0.2 && h_ratio >= 0.30 {
            let gap = other.y - (anchor.y + anchor.height);
            if gap >= 0.0 && gap <= bottom_gap + 1.0 {
                return Some(ConflictReason::InterveningText);
            }
        }
    }
    None
}

// ── Classification ──────────────────────────────────────────────────────────

/// Placeholder until dedicated control-type classification is reintroduced.
fn classify_control(_enclosing: &Bounds, _text: &Bounds) -> ControlKind {
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

pub(super) fn clamp_bounds_to_pixel_rect(
    bounds: &Bounds,
    img_w: usize,
    img_h: usize,
) -> Option<PixelRect> {
    if img_w == 0 || img_h == 0 {
        return None;
    }
    let x1 = bounds.x.floor() as i32;
    let y1 = bounds.y.floor() as i32;
    let x2 = (bounds.x + bounds.width).ceil() as i32 - 1;
    let y2 = (bounds.y + bounds.height).ceil() as i32 - 1;
    if x2 < x1 || y2 < y1 {
        return None;
    }
    let max_x = img_w as i32 - 1;
    let max_y = img_h as i32 - 1;
    let rect = PixelRect {
        x1: x1.clamp(0, max_x),
        y1: y1.clamp(0, max_y),
        x2: x2.clamp(0, max_x),
        y2: y2.clamp(0, max_y),
    };
    if rect.x2 < rect.x1 || rect.y2 < rect.y1 {
        None
    } else {
        Some(rect)
    }
}

fn clamp_bounds(bounds: &Bounds, img_w: f64, img_h: f64) -> Option<Bounds> {
    let Some(rect) =
        clamp_bounds_to_pixel_rect(bounds, img_w.max(0.0) as usize, img_h.max(0.0) as usize)
    else {
        return None;
    };
    let w = rect.width() as f64;
    let h = rect.height() as f64;
    if w < 2.0 || h < 2.0 {
        return None;
    }
    Some(Bounds {
        x: rect.x1 as f64,
        y: rect.y1 as f64,
        width: w,
        height: h,
    })
}

fn iou(a: &Bounds, b: &Bounds) -> f64 {
    let inter_w = overlap_1d(a.x, a.x + a.width, b.x, b.x + b.width);
    let inter_h = overlap_1d(a.y, a.y + a.height, b.y, b.y + b.height);
    let inter = inter_w * inter_h;
    let union = a.width * a.height + b.width * b.height - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

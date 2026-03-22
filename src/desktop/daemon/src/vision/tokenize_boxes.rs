use desktop_core::protocol::Bounds;

use super::metal_pipeline::ProcessedFrame;

pub(super) fn debug_enabled() -> bool {
    std::env::var("TOKENIZE_DEBUG").is_ok()
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

    // Validate border-vs-interior evidence.
    let directions_found = [expanded_left, expanded_right, expanded_top, expanded_bottom]
        .iter()
        .filter(|e| **e < max_limit * 0.90)
        .count();

    let corner_skip = (font_h * 0.3) as usize;
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

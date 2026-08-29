use std::time::{Duration, SystemTime, UNIX_EPOCH};

use desktop_core::error::AppError;

use crate::automation::{Automation, Point};

// ── RNG (xorshift64, no external crate) ─────────────────────────────────────

fn xorshift(s: &mut u64) -> u32 {
    let mut x = *s;
    x ^= x << 13;
    x ^= x >> 7;
    x ^= x << 17;
    *s = x;
    (x & 0xFFFF_FFFF) as u32
}

fn new_rng() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0xDEAD_BEEF_CAFE_BABE)
}

fn rng_range(s: &mut u64, lo: u32, hi: u32) -> u32 {
    if lo >= hi {
        return lo;
    }
    lo + xorshift(s) % (hi - lo + 1)
}

// ── Bezier path ──────────────────────────────────────────────────────────────

fn bezier4(p0: f64, p1: f64, p2: f64, p3: f64, t: f64) -> f64 {
    let u = 1.0 - t;
    u * u * u * p0 + 3.0 * u * u * t * p1 + 3.0 * u * t * t * p2 + t * t * t * p3
}

/// Returns interpolated pixel positions along a curved path from `from` to `to`.
/// The curve deviates organically from the straight line via random control points.
pub(super) fn bezier_path(from: Point, to: Point) -> Vec<Point> {
    let dx = to.x as i64 - from.x as i64;
    let dy = to.y as i64 - from.y as i64;
    let dist = ((dx * dx + dy * dy) as f64).sqrt();

    if dist < 2.0 {
        return vec![to];
    }

    let steps = ((dist / 4.0).round() as usize).clamp(4, 80);
    let mut rng = new_rng();

    // Unit perpendicular vector for random lateral deviation
    let perp_x = -(dy as f64) / dist;
    let perp_y = dx as f64 / dist;
    let sign = if xorshift(&mut rng) & 1 == 0 {
        1.0f64
    } else {
        -1.0
    };
    let offset = dist * (0.05 + rng_range(&mut rng, 0, 20) as f64 / 100.0);

    // Two control points placed asymmetrically along the path
    let cp1_x = from.x as f64 + dx as f64 * 0.33 + perp_x * offset * sign;
    let cp1_y = from.y as f64 + dy as f64 * 0.33 + perp_y * offset * sign;
    let cp2_x = from.x as f64 + dx as f64 * 0.67 + perp_x * offset * sign * 0.6;
    let cp2_y = from.y as f64 + dy as f64 * 0.67 + perp_y * offset * sign * 0.6;

    (1..=steps)
        .map(|i| {
            let t = i as f64 / steps as f64;
            let x = bezier4(from.x as f64, cp1_x, cp2_x, to.x as f64, t);
            let y = bezier4(from.y as f64, cp1_y, cp2_y, to.y as f64, t);
            Point::new(x.round() as u32, y.round() as u32)
        })
        .collect()
}

// ── Mouse movement ───────────────────────────────────────────────────────────

/// Smooth step: slow start → peak speed at midpoint → deceleration near target.
fn ease_in_out(t: f64) -> f64 {
    t * t * (3.0 - 2.0 * t)
}

/// Move the mouse along a curved Bezier path with an ease-in-out speed profile.
/// Shorter moves are proportionally faster; longer moves plateau around 600 ms.
pub(super) fn human_mouse_move(
    backend: &dyn Automation,
    from: Point,
    to: Point,
) -> Result<(), AppError> {
    let path = bezier_path(from, to);
    let steps = path.len();
    if steps == 0 {
        return Ok(());
    }

    let dx = to.x as i64 - from.x as i64;
    let dy = to.y as i64 - from.y as i64;
    let dist = ((dx * dx + dy * dy) as f64).sqrt();
    // Total travel time scales sub-linearly with distance: fast for short hops.
    let total_ms = (dist * 1.8 + 180.0).min(600.0).max(60.0);

    for (i, &point) in path.iter().enumerate() {
        backend.move_mouse(point)?;
        // Derive per-step sleep from the derivative of ease_in_out at this position.
        let t0 = i as f64 / steps as f64;
        let t1 = (i + 1) as f64 / steps as f64;
        let dt = ease_in_out(t1) - ease_in_out(t0);
        let step_ms = (dt * total_ms) as u64;
        if step_ms > 0 {
            std::thread::sleep(Duration::from_millis(step_ms));
        }
    }
    Ok(())
}

// ── Typing ───────────────────────────────────────────────────────────────────

/// Type `text` one character at a time with natural per-keystroke delays.
/// `wpm` controls the base speed; 0 uses a default of 60 WPM.
pub(super) fn human_type_text(
    backend: &dyn Automation,
    text: &str,
    wpm: u32,
) -> Result<(), AppError> {
    if text.is_empty() {
        return Ok(());
    }
    let mut rng = new_rng();
    let base_ms = {
        let w = if wpm == 0 { 60 } else { wpm.max(10) };
        // ms per character at 5 chars/word
        60_000 / (w * 5)
    };
    for (i, ch) in text.chars().enumerate() {
        if i > 0 {
            let jitter = rng_range(&mut rng, 0, base_ms * 4 / 5);
            let delay = if xorshift(&mut rng) & 1 == 0 {
                base_ms.saturating_sub(jitter / 2)
            } else {
                base_ms + jitter / 2
            };
            // Longer natural pause after word boundaries and punctuation
            let delay = if matches!(ch, ' ' | '.' | ',' | '\n' | ';' | ':') {
                delay + base_ms / 2
            } else {
                delay
            };
            std::thread::sleep(Duration::from_millis(delay as u64));
        }
        backend.type_char(ch)?;
    }
    Ok(())
}

// ── Jitter ───────────────────────────────────────────────────────────────────

/// Sleep a uniformly random duration within [`min_ms`, `max_ms`].
/// Both zero skips the sleep entirely.
pub(super) fn jitter_sleep(min_ms: u32, max_ms: u32) {
    if min_ms == 0 && max_ms == 0 {
        return;
    }
    let mut rng = new_rng();
    let ms = rng_range(&mut rng, min_ms, max_ms.max(min_ms));
    if ms > 0 {
        std::thread::sleep(Duration::from_millis(ms as u64));
    }
}

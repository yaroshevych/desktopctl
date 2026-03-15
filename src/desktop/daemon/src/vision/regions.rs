use desktop_core::protocol::Bounds;
use image::RgbaImage;
use serde::Serialize;
use std::collections::VecDeque;

#[derive(Debug, Clone, Serialize, Default)]
pub struct SettingsRegions {
    pub window_bounds: Option<Bounds>,
    pub sidebar_bounds: Option<Bounds>,
    pub content_bounds: Option<Bounds>,
    pub table_bounds: Option<Bounds>,
}

pub fn detect_settings_regions(image: &RgbaImage) -> SettingsRegions {
    let width = image.width() as i32;
    let height = image.height() as i32;
    if width < 320 || height < 200 {
        return SettingsRegions::default();
    }

    // Step 1: Detect mode via traffic lights (VM app window's dots survive downscaling)
    let tl_pos = find_traffic_lights(image);
    let dark_mode_guess = tl_pos
        .map(|(red_cx, red_cy)| {
            let green_cx = red_cx + 40;
            detect_dark_mode(image, green_cx, red_cy)
        })
        .unwrap_or(false);
    let tl_window = tl_pos.and_then(|pos| detect_window_from_selected_tl(image, pos));
    let window_from_tl = tl_window.is_some();

    // Step 2: Find window bounds
    let (window, dark_mode) = if let Some(bounds) = tl_window {
        (bounds, dark_mode_guess)
    } else if dark_mode_guess {
        // Dark mode: use dark-panel flood fill
        if let Some(bounds) = detect_dark_content_bounds(image) {
            (bounds, true)
        } else if let Some((window, sidebar, content)) =
            detect_via_column_variance(image, tl_pos)
        {
            // Flood fill failed (dark bg blends with dark window) — use column variance
            // transition to find sidebar/content divider directly.
            let add_pair = detect_add_button_pair(image, &content, true);
            let table = find_table(image, &content, add_pair);
            return SettingsRegions {
                window_bounds: Some(window),
                sidebar_bounds: Some(sidebar),
                content_bounds: Some(content),
                table_bounds: table,
            };
        } else if let Some(bounds) = detect_neutral_content_bounds(image) {
            (bounds, false)
        } else {
            return SettingsRegions::default();
        }
    } else {
        // Light mode: use neutral-panel flood fill
        if let Some(bounds) = detect_neutral_content_bounds(image) {
            (bounds, false)
        } else {
            return SettingsRegions::default();
        }
    };

    let has_tl = window_from_tl || has_window_traffic_lights_in_region(image, &window);
    let title_h = (window.height * 0.085).clamp(30.0, 56.0);

    let (window, sidebar, content) = if has_tl {
        // Flood fill captured the full window (including sidebar)
        let mut content = Bounds {
            x: (window.x + (window.width * 0.24).clamp(148.0, 235.0)).max(0.0),
            y: (window.y + title_h).max(0.0),
            width: (window.width - (window.width * 0.24).clamp(148.0, 235.0)).max(0.0),
            height: (window.height - title_h).max(0.0),
        };
        let mut sidebar = Bounds {
            x: window.x,
            y: content.y,
            width: (content.x - window.x).max(0.0),
            height: content.height.max(0.0),
        };
        if let Some((s, c)) = refine_sidebar_content_split(image, &window, title_h) {
            sidebar = s;
            content = c;
        }
        (window, sidebar, content)
    } else {
        // Flood fill captured just the content area (sidebar was different shade)
        let content = window.clone();
        let content_title_h = (content.height * 0.075).clamp(30.0, 56.0);
        let sidebar_w = (content.width * 0.30).clamp(160.0, 330.0);
        let window_x = (content.x - sidebar_w).max(0.0);
        let window_y = (content.y - content_title_h).max(0.0);
        let window = Bounds {
            x: window_x,
            y: window_y,
            width: content.width + (content.x - window_x),
            height: content.height + (content.y - window_y),
        };
        let sidebar = Bounds {
            x: window.x,
            y: content.y,
            width: (content.x - window.x).max(0.0),
            height: content.height.max(0.0),
        };
        (window, sidebar, content)
    };

    let add_pair = detect_add_button_pair(image, &content, dark_mode);
    let table = find_table(image, &content, add_pair);

    SettingsRegions {
        window_bounds: Some(window),
        sidebar_bounds: Some(sidebar),
        content_bounds: Some(content),
        table_bounds: table,
    }
}

fn detect_window_from_selected_tl(image: &RgbaImage, tl_pos: (i32, i32)) -> Option<Bounds> {
    let width = image.width() as i32;
    let height = image.height() as i32;
    if width < 320 || height < 220 {
        return None;
    }
    let (red_x, red_y) = tl_pos;
    let passes = luminance_passes(image);

    // Labeled fixtures put red center roughly +24..+34 px right and +24..+32 px down
    // from the window top-left.
    let est_x = (red_x - 26).clamp(0, width - 2);
    let est_y = (red_y - 26).clamp(0, height - 2);

    let y_scan_top = (est_y + 42).clamp(1, height - 2);
    let y_scan_bottom = (est_y + 980).clamp(y_scan_top + 100, height - 1);
    let expected_w = 715i32;
    let expected_h = 625i32;

    let left_min = (est_x - 16).clamp(1, width - 2);
    let left_max = (est_x + 28).clamp(left_min + 1, width - 1);
    let left_detected = strongest_vertical_edge(
        &passes,
        image.width() as usize,
        y_scan_top,
        y_scan_bottom,
        left_min,
        left_max,
    );
    let left_fallback = est_x;
    let left = if let Some(lx) = left_detected {
        let edge = vertical_edge_response(
            &passes,
            image.width() as usize,
            lx,
            y_scan_top,
            y_scan_bottom,
        );
        if (lx - est_x).abs() <= 6 && edge >= 8.0 {
            lx
        } else {
            left_fallback
        }
    } else {
        left_fallback
    };

    let top_x0 = (est_x + 74).clamp(1, width - 2);
    let top_x1 = (est_x + 980).clamp(top_x0 + 64, width - 1);
    let top_min = (est_y - 12).clamp(1, height - 2);
    let top_max = (est_y + 24).clamp(top_min + 1, height - 1);
    let top_detected = strongest_horizontal_edge(
        &passes,
        image.width() as usize,
        top_x0,
        top_x1,
        top_min,
        top_max,
    );
    let top_fallback = est_y;
    let top = if let Some(ty) = top_detected {
        let edge =
            horizontal_edge_response(&passes, image.width() as usize, ty, top_x0, top_x1);
        if (ty - est_y).abs() <= 12 && edge >= 8.0 {
            ty
        } else {
            top_fallback
        }
    } else {
        top_fallback
    };

    let right_min = (est_x + 520).clamp(left + 360, width - 2);
    let right_max = (est_x + 1120).clamp(right_min + 1, width - 1);
    let right_detected = strongest_vertical_edge(
        &passes,
        image.width() as usize,
        y_scan_top,
        y_scan_bottom,
        right_min,
        right_max,
    );
    let right_fallback = (left + expected_w).clamp(left + 560, width - 1);
    let right = if let Some(rx) = right_detected {
        let w = rx - left;
        let edge = vertical_edge_response(
            &passes,
            image.width() as usize,
            rx,
            y_scan_top,
            y_scan_bottom,
        );
        if (w - expected_w).abs() <= 12 && edge >= 8.0 {
            rx
        } else {
            right_fallback
        }
    } else {
        right_fallback
    };

    let bottom_x0 = (left + 86).clamp(top_x0, width - 2);
    let bottom_x1 = (right - 18).clamp(bottom_x0 + 64, width - 1);
    let bottom_min = (est_y + 460).clamp(top + 160, height - 2);
    let bottom_max = (est_y + 980).clamp(bottom_min + 1, height - 1);
    let bottom_detected = strongest_horizontal_edge(
        &passes,
        image.width() as usize,
        bottom_x0,
        bottom_x1,
        bottom_min,
        bottom_max,
    );
    let bottom_fallback = (top + expected_h).clamp(top + 500, height - 1);
    let bottom = if let Some(by) = bottom_detected {
        let h = by - top;
        let edge = horizontal_edge_response(
            &passes,
            image.width() as usize,
            by,
            bottom_x0,
            bottom_x1,
        );
        if (h - expected_h).abs() <= 20 && edge >= 8.0 {
            by
        } else {
            bottom_fallback
        }
    } else {
        bottom_fallback
    };

    let x = left as f64;
    let y = top as f64;
    let w = (right - left).max(0) as f64;
    let h = (bottom - top).max(0) as f64;
    if !(620.0..=820.0).contains(&w) || !(560.0..=700.0).contains(&h) {
        return None;
    }

    // Ensure the detected window aligns with the traffic-light anchor.
    let dx = red_x as f64 - x;
    let dy = red_y as f64 - y;
    if !(16.0..=50.0).contains(&dx) || !(16.0..=48.0).contains(&dy) {
        return None;
    }

    Some(Bounds {
        x,
        y,
        width: w,
        height: h,
    })
}

/// Scan the entire image for the red/yellow/green traffic light triplet.
/// Returns the center of the red dot.
fn find_traffic_lights(image: &RgbaImage) -> Option<(i32, i32)> {
    let candidates = traffic_light_triplet_candidates(image);
    if candidates.is_empty() {
        return None;
    }
    select_settings_tl_candidate(image, &candidates).or_else(|| {
        candidates
            .into_iter()
            .min_by(|a, b| a.0.total_cmp(&b.0))
            .map(|(_, x, y)| (x, y))
    })
}

fn select_settings_tl_candidate(
    image: &RgbaImage,
    candidates: &[(f64, i32, i32)],
) -> Option<(i32, i32)> {
    if candidates.is_empty() {
        return None;
    }
    let passes = luminance_passes(image);
    let mut best: Option<(f64, i32, i32)> = None; // (score, red_x, red_y)

    for (geom_score, red_x, red_y) in candidates {
        let Some(score) =
            settings_tl_candidate_score(image, &passes, *geom_score, *red_x, *red_y)
        else {
            continue;
        };

        match best {
            Some((best_score, _, _)) if score <= best_score => {}
            _ => best = Some((score, *red_x, *red_y)),
        }
    }

    best.map(|(_, x, y)| (x, y))
}

fn settings_tl_candidate_score(
    image: &RgbaImage,
    passes: &[Vec<u8>; 3],
    geom_score: f64,
    red_x: i32,
    red_y: i32,
) -> Option<f64> {
    let img_w = image.width() as i32;
    let img_h = image.height() as i32;
    let est_x = (red_x - 26).clamp(0, img_w - 2);
    let est_y = (red_y - 24).clamp(0, img_h - 2);
    let y_scan_top = (est_y + 48).clamp(1, img_h - 2);
    let y_scan_bottom = (est_y + 980).clamp(y_scan_top + 120, img_h - 1);
    if y_scan_bottom <= y_scan_top + 80 {
        return None;
    }

    let left_min = (est_x - 24).clamp(1, img_w - 2);
    let left_max = (est_x + 36).clamp(left_min + 1, img_w - 1);
    let left_x = strongest_vertical_edge(
        passes,
        image.width() as usize,
        y_scan_top,
        y_scan_bottom,
        left_min,
        left_max,
    );

    let right_min = (est_x + 520).clamp(left_max + 24, img_w - 2);
    let right_max = (est_x + 1120).clamp(right_min + 1, img_w - 1);
    let right_x = strongest_vertical_edge(
        passes,
        image.width() as usize,
        y_scan_top,
        y_scan_bottom,
        right_min,
        right_max,
    );

    let top_x0 = (est_x + 70).clamp(1, img_w - 2);
    let top_x1 = (est_x + 980).clamp(top_x0 + 64, img_w - 1);
    let top_min = (est_y - 10).clamp(1, img_h - 2);
    let top_max = (est_y + 22).clamp(top_min + 1, img_h - 1);
    let top_y = strongest_horizontal_edge(
        passes,
        image.width() as usize,
        top_x0,
        top_x1,
        top_min,
        top_max,
    );

    let bottom_x0 = left_x.unwrap_or(top_x0).max(top_x0);
    let bottom_x1 = right_x.unwrap_or(top_x1).min(top_x1).max(bottom_x0 + 64);
    let bottom_min = (est_y + 460).clamp(top_max + 10, img_h - 2);
    let bottom_max = (est_y + 980).clamp(bottom_min + 1, img_h - 1);
    let bottom_y = strongest_horizontal_edge(
        passes,
        image.width() as usize,
        bottom_x0,
        bottom_x1,
        bottom_min,
        bottom_max,
    );

    let mut score = -(geom_score * 0.7);
    score += check_title_bar_context(image, red_x, red_y, red_x + 40) * 25.0;
    // Settings window is rarely the top-most title bar near menu bar.
    score += red_y as f64 * 0.08;
    if est_y < 80 {
        score -= 140.0;
    }
    if red_y < 90 {
        score -= 120.0;
    }
    if red_x < 140 && red_y < 120 {
        score -= 110.0;
    }

    if let Some(x) = left_x {
        score += vertical_edge_response(
            passes,
            image.width() as usize,
            x,
            y_scan_top,
            y_scan_bottom,
        ) * 1.2;
    } else {
        score -= 12.0;
    }
    if let Some(x) = right_x {
        score += vertical_edge_response(
            passes,
            image.width() as usize,
            x,
            y_scan_top,
            y_scan_bottom,
        );
    } else {
        score -= 12.0;
    }
    if let Some(y) = top_y {
        score += horizontal_edge_response(passes, image.width() as usize, y, top_x0, top_x1) * 1.1;
    } else {
        score -= 8.0;
    }
    if let Some(y) = bottom_y {
        score +=
            horizontal_edge_response(passes, image.width() as usize, y, bottom_x0, bottom_x1) * 0.9;
    } else {
        score -= 8.0;
    }

    if let (Some(lx), Some(rx)) = (left_x, right_x) {
        let w = (rx - lx).max(0) as f64;
        score += 16.0 - ((w - 715.0).abs() * 0.04).min(18.0);
    }
    if let (Some(ty), Some(by)) = (top_y, bottom_y) {
        let h = (by - ty).max(0) as f64;
        score += 14.0 - ((h - 625.0).abs() * 0.04).min(16.0);
    }

    Some(score)
}

fn traffic_light_triplet_candidates(image: &RgbaImage) -> Vec<(f64, i32, i32)> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width < 80 || height < 30 {
        return Vec::new();
    }

    // Build color classification mask
    // 0 = none, 1 = red, 2 = yellow, 3 = green
    let mut color_map = vec![0u8; width * height];
    for y in 0..height {
        for x in 0..width {
            let [r, g, b, _] = image.get_pixel(x as u32, y as u32).0;
            color_map[y * width + x] = classify_traffic_light_pixel(r, g, b);
        }
    }

    // Find connected components for each color
    let mut visited = vec![false; width * height];
    let mut components: Vec<(u8, i32, i32, usize)> = Vec::new(); // (color, cx, cy, size)

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let color = color_map[idx];
            if color == 0 || visited[idx] {
                continue;
            }

            // BFS flood fill for this color
            let mut queue = VecDeque::from([(x, y)]);
            visited[idx] = true;
            let mut sum_x = 0i64;
            let mut sum_y = 0i64;
            let mut count = 0usize;
            let mut min_x = x;
            let mut max_x = x;
            let mut min_y = y;
            let mut max_y = y;

            while let Some((cx, cy)) = queue.pop_front() {
                sum_x += cx as i64;
                sum_y += cy as i64;
                count += 1;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);

                for (dx, dy) in [(-1i32, 0), (1, 0), (0, -1), (0, 1)] {
                    let nx = cx as i32 + dx;
                    let ny = cy as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= width as i32 || ny >= height as i32 {
                        continue;
                    }
                    let ni = ny as usize * width + nx as usize;
                    if !visited[ni] && color_map[ni] == color {
                        visited[ni] = true;
                        queue.push_back((nx as usize, ny as usize));
                    }
                }
            }

            // Traffic light dots at 1x are ~10-14px diameter, area ~80-160px
            let comp_w = max_x - min_x + 1;
            let comp_h = max_y - min_y + 1;
            if count < 20 || count > 400 || comp_w > 24 || comp_h > 24 || comp_w < 4 || comp_h < 4
            {
                continue;
            }
            // Roughly circular: aspect ratio and fill
            let aspect = comp_w as f64 / comp_h as f64;
            if !(0.5..=2.0).contains(&aspect) {
                continue;
            }
            let fill = count as f64 / (comp_w * comp_h) as f64;
            if fill < 0.45 {
                continue;
            }

            let cx = (sum_x as f64 / count as f64).round() as i32;
            let cy = (sum_y as f64 / count as f64).round() as i32;
            components.push((color, cx, cy, count));
        }
    }

    // Match triplets: red → yellow → green, left-to-right, horizontally aligned
    let reds: Vec<_> = components.iter().filter(|c| c.0 == 1).collect();
    let yellows: Vec<_> = components.iter().filter(|c| c.0 == 2).collect();
    let greens: Vec<_> = components.iter().filter(|c| c.0 == 3).collect();

    let mut candidates: Vec<(f64, i32, i32)> = Vec::new(); // (score, red_cx, red_cy)

    for r in &reds {
        for y in &yellows {
            let dy_ry = (r.2 - y.2).abs();
            if dy_ry > 4 {
                continue;
            }
            let dx_ry = y.1 - r.1;
            if !(8..=30).contains(&dx_ry) {
                continue;
            }

            for g in &greens {
                let dy_rg = (r.2 - g.2).abs();
                if dy_rg > 4 {
                    continue;
                }
                let dx_rg = g.1 - r.1;
                if !(20..=56).contains(&dx_rg) {
                    continue;
                }
                let dx_yg = g.1 - y.1;
                if !(8..=30).contains(&dx_yg) {
                    continue;
                }

                // Check size similarity: all three dots should be similar size
                let sizes = [r.3, y.3, g.3];
                let max_size = *sizes.iter().max().unwrap();
                let min_size = *sizes.iter().min().unwrap();
                if max_size > min_size * 4 {
                    continue;
                }

                // Context check: the area around the triplet should look like a
                // title bar (neutral gray pixels), not a colorful sidebar
                let title_bar_score =
                    check_title_bar_context(image, r.1, r.2, g.1);
                // Reject if title bar context is poor (< 50% neutral)
                if title_bar_score < 0.5 {
                    continue;
                }

                // Prefer triplets NOT in the very top-left of image (host traffic lights)
                let near_origin = if r.1 < 30 && r.2 < 30 { 1000.0 } else { 0.0 };
                let spacing_err = (dx_ry as f64 - 20.0).abs() + (dx_yg as f64 - 20.0).abs();
                // Lower score is better
                let score = near_origin + spacing_err - title_bar_score * 30.0;
                candidates.push((score, r.1, r.2));
            }
        }
    }

    candidates
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn traffic_light_candidates_for_test(image: &RgbaImage) -> Vec<(i32, i32)> {
    traffic_light_triplet_candidates(image)
        .into_iter()
        .map(|(_, x, y)| (x, y))
        .collect()
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn selected_traffic_light_anchor_for_test(image: &RgbaImage) -> Option<(i32, i32)> {
    find_traffic_lights(image)
}

#[cfg(test)]
#[allow(dead_code)]
pub(crate) fn scored_traffic_light_candidates_for_test(image: &RgbaImage) -> Vec<(f64, i32, i32)> {
    let candidates = traffic_light_triplet_candidates(image);
    let passes = luminance_passes(image);
    let mut scored = Vec::new();
    for (geom_score, red_x, red_y) in candidates {
        if let Some(score) = settings_tl_candidate_score(image, &passes, geom_score, red_x, red_y) {
            scored.push((score, red_x, red_y));
        }
    }
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    scored
}

fn classify_traffic_light_pixel(r: u8, g: u8, b: u8) -> u8 {
    // Red dot: high red, low green and blue
    if r > 190 && g < 130 && b < 130 {
        return 1;
    }
    // Yellow dot: high red and green, low blue
    if r > 180 && g > 145 && b < 130 {
        return 2;
    }
    // Green dot: high green, lower red and blue
    if g > 155 && r < 145 && b < 135 {
        return 3;
    }
    0
}

/// Check if the area around a traffic light triplet looks like a window title bar.
/// Returns a score 0.0-1.0 where 1.0 means high confidence it's a real title bar.
fn check_title_bar_context(image: &RgbaImage, red_cx: i32, red_cy: i32, green_cx: i32) -> f64 {
    let width = image.width() as i32;
    let height = image.height() as i32;
    let mut neutral_count = 0;
    let mut total = 0;

    // Sample pixels to the right of the green dot (should be title bar)
    for dx in (20..160).step_by(8) {
        let x = (green_cx + dx).clamp(0, width - 1);
        let y = red_cy.clamp(0, height - 1);
        let [r, g, b, _] = image.get_pixel(x as u32, y as u32).0;
        let max = r.max(g).max(b) as i16;
        let min = r.min(g).min(b) as i16;
        let chroma = max - min;
        // Title bar is neutral (low chroma, any luma — works for both light and dark mode)
        if chroma <= 35 {
            neutral_count += 1;
        }
        total += 1;
    }

    // Also sample pixels above and below the dots (should be title bar too)
    for dy in [-8, 8] {
        let y = (red_cy + dy).clamp(0, height - 1);
        for dx in [0, 10, 20] {
            let x = (red_cx + dx).clamp(0, width - 1);
            let [r, g, b, _] = image.get_pixel(x as u32, y as u32).0;
            let max = r.max(g).max(b) as i16;
            let min = r.min(g).min(b) as i16;
            let chroma = max - min;
            if chroma <= 35 {
                neutral_count += 1;
            }
            total += 1;
        }
    }

    if total == 0 {
        return 0.0;
    }
    neutral_count as f64 / total as f64
}

/// Sample title bar pixels to detect dark mode.
/// Returns true if dark mode.
fn detect_dark_mode(image: &RgbaImage, green_cx: i32, dot_cy: i32) -> bool {
    let width = image.width() as i32;
    let mut total_luma = 0u64;
    let mut count = 0u32;

    // Sample 20 pixels in the title bar, to the right of the green dot
    for dx in (20..220).step_by(10) {
        let x = (green_cx + dx).min(width - 1).max(0);
        let y = dot_cy.max(0).min(image.height() as i32 - 1);
        let [r, g, b, _] = image.get_pixel(x as u32, y as u32).0;
        total_luma += (r as u64 + g as u64 + b as u64) / 3;
        count += 1;
    }

    if count == 0 {
        return false;
    }
    let avg_luma = total_luma / count as u64;
    avg_luma <= 150
}

// ── Table detection ────────────────────────────────────────────────────────

fn find_table(
    image: &RgbaImage,
    content: &Bounds,
    add_pair: Option<(f64, f64)>,
) -> Option<Bounds> {
    detect_table_bounds_from_borders(image, content, add_pair)
        .or_else(|| {
            add_pair.map(|(add_x, add_y)| {
                let height = 44.0;
                let x = (add_x - 12.0).max(content.x + 2.0);
                let y = (add_y - (height - 8.0)).max(content.y + 8.0);
                let right = (content.x + content.width - 6.0).max(x + 140.0);
                Bounds {
                    x,
                    y,
                    width: (right - x).max(140.0),
                    height,
                }
            })
        })
        .or_else(|| fallback_table_bounds(content))
}

// ── Existing helpers (kept with improvements) ──────────────────────────────

fn refine_sidebar_content_split(
    image: &RgbaImage,
    window: &Bounds,
    title_h: f64,
) -> Option<(Bounds, Bounds)> {
    let width = image.width() as i32;
    let height = image.height() as i32;
    if width <= 0 || height <= 0 {
        return None;
    }
    let y_top = (window.y + title_h + 6.0).floor().clamp(0.0, height as f64 - 2.0) as i32;
    let y_bottom = (window.y + window.height - 8.0)
        .ceil()
        .clamp(y_top as f64 + 2.0, height as f64 - 1.0) as i32;
    let x_min = (window.x + window.width * 0.17)
        .floor()
        .clamp(1.0, width as f64 - 2.0) as i32;
    let x_max = (window.x + window.width * 0.42)
        .ceil()
        .clamp(x_min as f64 + 1.0, width as f64 - 1.0) as i32;
    if x_max - x_min < 24 || y_bottom - y_top < 40 {
        return None;
    }

    let passes = luminance_passes(image);
    let divider_x = strongest_vertical_edge(
        &passes,
        image.width() as usize,
        y_top,
        y_bottom,
        x_min,
        x_max,
    )?;
    let divider_x = divider_x as f64;
    let min_divider = window.x + 120.0;
    let max_divider = window.x + window.width - 160.0;
    if divider_x < min_divider || divider_x > max_divider {
        return None;
    }

    let content_x = (divider_x + 1.0).max(0.0);
    let content_y = (window.y + title_h).max(0.0);
    let content_w = (window.x + window.width - content_x).max(0.0);
    let content_h = (window.y + window.height - content_y).max(0.0);
    if content_w < 220.0 || content_h < 180.0 {
        return None;
    }

    let content = Bounds {
        x: content_x,
        y: content_y,
        width: content_w,
        height: content_h,
    };
    let sidebar = Bounds {
        x: window.x,
        y: content.y,
        width: (content.x - window.x).max(0.0),
        height: content.height,
    };
    Some((sidebar, content))
}

fn detect_neutral_content_bounds(image: &RgbaImage) -> Option<Bounds> {
    flood_fill_content_bounds(image, is_neutral_panel_pixel)
}

fn detect_dark_content_bounds(image: &RgbaImage) -> Option<Bounds> {
    flood_fill_content_bounds(image, is_dark_panel_pixel)
}

fn flood_fill_content_bounds(
    image: &RgbaImage,
    classify: fn(u8, u8, u8) -> bool,
) -> Option<Bounds> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width < 320 || height < 200 {
        return None;
    }
    let stride = if width > 2200 || height > 1400 { 2 } else { 1 };
    let grid_w = width.div_ceil(stride);
    let grid_h = height.div_ceil(stride);

    let mut mask = vec![false; grid_w * grid_h];
    for gy in 0..grid_h {
        let y = (gy * stride).min(height - 1);
        for gx in 0..grid_w {
            let x = (gx * stride).min(width - 1);
            let px = image.get_pixel(x as u32, y as u32).0;
            if classify(px[0], px[1], px[2]) {
                mask[gy * grid_w + gx] = true;
            }
        }
    }

    let mut visited = vec![false; grid_w * grid_h];
    let mut best: Option<(f64, Bounds)> = None;
    let min_box_w = ((grid_w as f64) * 0.12).ceil() as usize;
    let min_box_h = ((grid_h as f64) * 0.18).ceil() as usize;
    let img_center_x = width as f64 / 2.0;
    let img_center_y = height as f64 / 2.0;

    for gy in 0..grid_h {
        for gx in 0..grid_w {
            let idx = gy * grid_w + gx;
            if !mask[idx] || visited[idx] {
                continue;
            }
            let mut queue = VecDeque::from([(gx, gy)]);
            visited[idx] = true;

            let mut pixels = 0usize;
            let mut min_x = gx;
            let mut max_x = gx;
            let mut min_y = gy;
            let mut max_y = gy;

            while let Some((cx, cy)) = queue.pop_front() {
                pixels += 1;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);

                if cx + 1 < grid_w {
                    let n = cy * grid_w + (cx + 1);
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        queue.push_back((cx + 1, cy));
                    }
                }
                if cx > 0 {
                    let n = cy * grid_w + (cx - 1);
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        queue.push_back((cx - 1, cy));
                    }
                }
                if cy + 1 < grid_h {
                    let n = (cy + 1) * grid_w + cx;
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        queue.push_back((cx, cy + 1));
                    }
                }
                if cy > 0 {
                    let n = (cy - 1) * grid_w + cx;
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        queue.push_back((cx, cy - 1));
                    }
                }
            }

            let box_w = max_x - min_x + 1;
            let box_h = max_y - min_y + 1;
            if box_w < min_box_w || box_h < min_box_h {
                continue;
            }

            let aspect = box_w as f64 / box_h as f64;
            if !(0.85..=2.2).contains(&aspect) {
                continue;
            }

            let fill_ratio = pixels as f64 / (box_w * box_h) as f64;
            if fill_ratio < 0.58 {
                continue;
            }

            let x = (min_x * stride) as f64;
            let y = (min_y * stride) as f64;
            let mut w = (box_w * stride) as f64;
            let mut h = (box_h * stride) as f64;
            if x + w > width as f64 {
                w = (width as f64 - x).max(0.0);
            }
            if y + h > height as f64 {
                h = (height as f64 - y).max(0.0);
            }
            if w < 160.0 || h < 150.0 {
                continue;
            }
            let center_x = x + w / 2.0;
            let center_y = y + h / 2.0;
            let center_penalty = ((center_x - img_center_x).abs() / width as f64)
                + ((center_y - img_center_y).abs() / height as f64);
            let edge_penalty = if x <= 1.0 || y <= 1.0 || x + w >= width as f64 - 1.0 {
                0.2
            } else {
                0.0
            };
            let score =
                (w * h) * fill_ratio * (1.0 - (0.42 * center_penalty) - edge_penalty).max(0.2);
            let bounds = Bounds {
                x,
                y,
                width: w,
                height: h,
            };
            match &best {
                Some((best_score, _)) if score <= *best_score => {}
                _ => best = Some((score, bounds)),
            }
        }
    }

    best.map(|(_, bounds)| bounds)
}

#[derive(Debug, Clone)]
struct SymbolComponent {
    center_x: f64,
    center_y: f64,
    width: usize,
    height: usize,
    vertical_near_center: usize,
    horizontal_near_center: usize,
}

fn has_window_traffic_lights_in_region(image: &RgbaImage, bounds: &Bounds) -> bool {
    let width = image.width() as i32;
    let height = image.height() as i32;
    let x0 = bounds.x.floor().max(0.0) as i32;
    let y0 = bounds.y.floor().max(0.0) as i32;
    let x1 = (bounds.x + bounds.width).ceil().min(width as f64) as i32;
    let y1 = (bounds.y + bounds.height).ceil().min(height as f64) as i32;
    if x1 - x0 < 100 || y1 - y0 < 60 {
        return false;
    }
    let sx0 = (x0 + 6).clamp(0, width - 1);
    let sy0 = (y0 + 6).clamp(0, height - 1);
    let sx1 = (x0 + 96).clamp(sx0 + 1, width);
    let sy1 = (y0 + 46).clamp(sy0 + 1, height);

    let mut red = 0usize;
    let mut yellow = 0usize;
    let mut green = 0usize;
    for y in sy0..sy1 {
        for x in sx0..sx1 {
            let [r, g, b, _] = image.get_pixel(x as u32, y as u32).0;
            if r > 190 && g < 130 && b < 130 {
                red += 1;
            } else if r > 180 && g > 145 && b < 130 {
                yellow += 1;
            } else if g > 155 && r < 145 && b < 135 {
                green += 1;
            }
        }
    }
    red >= 6 && yellow >= 6 && green >= 6
}

fn detect_add_button_pair(
    image: &RgbaImage,
    content: &Bounds,
    dark_mode: bool,
) -> Option<(f64, f64)> {
    let width = image.width() as i32;
    let height = image.height() as i32;
    let x0 = (content.x + 4.0).floor().max(0.0) as i32;
    let x1 = (content.x + content.width * 0.48).ceil().min(width as f64) as i32;
    let y0 = (content.y + content.height * 0.14).floor().max(0.0) as i32;
    let y1 = (content.y + content.height * 0.46)
        .ceil()
        .min(height as f64) as i32;
    if x1 - x0 < 40 || y1 - y0 < 60 {
        return None;
    }

    let roi_w = (x1 - x0) as usize;
    let roi_h = (y1 - y0) as usize;
    let mut mask = vec![false; roi_w * roi_h];
    for ry in 0..roi_h {
        for rx in 0..roi_w {
            let px = image
                .get_pixel((x0 as usize + rx) as u32, (y0 as usize + ry) as u32)
                .0;
            if is_symbol_pixel(px[0], px[1], px[2], dark_mode) {
                mask[ry * roi_w + rx] = true;
            }
        }
    }

    let mut visited = vec![false; roi_w * roi_h];
    let mut components = Vec::new();
    for ry in 0..roi_h {
        for rx in 0..roi_w {
            let idx = ry * roi_w + rx;
            if !mask[idx] || visited[idx] {
                continue;
            }

            let mut queue = VecDeque::from([(rx, ry)]);
            visited[idx] = true;
            let mut points = Vec::new();
            let mut min_x = rx;
            let mut max_x = rx;
            let mut min_y = ry;
            let mut max_y = ry;

            while let Some((cx, cy)) = queue.pop_front() {
                points.push((cx, cy));
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);

                if cx + 1 < roi_w {
                    let n = cy * roi_w + (cx + 1);
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        queue.push_back((cx + 1, cy));
                    }
                }
                if cx > 0 {
                    let n = cy * roi_w + (cx - 1);
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        queue.push_back((cx - 1, cy));
                    }
                }
                if cy + 1 < roi_h {
                    let n = (cy + 1) * roi_w + cx;
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        queue.push_back((cx, cy + 1));
                    }
                }
                if cy > 0 {
                    let n = (cy - 1) * roi_w + cx;
                    if mask[n] && !visited[n] {
                        visited[n] = true;
                        queue.push_back((cx, cy - 1));
                    }
                }
            }

            let pixel_count = points.len();
            let comp_w = max_x - min_x + 1;
            let comp_h = max_y - min_y + 1;
            if !(4..=130).contains(&pixel_count) || comp_w > 24 || comp_h > 18 {
                continue;
            }

            let center_x = (min_x + max_x) as f64 / 2.0;
            let center_y = (min_y + max_y) as f64 / 2.0;
            let mut vertical_near_center = 0usize;
            let mut horizontal_near_center = 0usize;
            for (px, py) in points {
                if (px as isize - center_x.round() as isize).abs() <= 1 {
                    vertical_near_center += 1;
                }
                if (py as isize - center_y.round() as isize).abs() <= 1 {
                    horizontal_near_center += 1;
                }
            }

            components.push(SymbolComponent {
                center_x: x0 as f64 + center_x,
                center_y: y0 as f64 + center_y,
                width: comp_w,
                height: comp_h,
                vertical_near_center,
                horizontal_near_center,
            });
        }
    }

    let plus_candidates = components
        .iter()
        .filter(|c| {
            (4..=12).contains(&c.width)
                && (6..=12).contains(&c.height)
                && c.vertical_near_center >= 3
                && c.horizontal_near_center >= 3
        })
        .collect::<Vec<_>>();
    let minus_candidates = components
        .iter()
        .filter(|c| {
            (6..=20).contains(&c.width)
                && (1..=5).contains(&c.height)
                && c.horizontal_near_center >= 4
                && c.vertical_near_center <= 8
        })
        .collect::<Vec<_>>();

    let expected_y = y0 as f64 + (y1 - y0) as f64 * 0.52;
    let mut best: Option<(f64, f64, f64)> = None;
    for plus in &plus_candidates {
        for minus in &minus_candidates {
            if minus.center_x <= plus.center_x {
                continue;
            }
            let dx = minus.center_x - plus.center_x;
            let dy = (minus.center_y - plus.center_y).abs();
            if !(8.0..=34.0).contains(&dx) || dy > 4.0 {
                continue;
            }
            let bg_penalty = if dark_mode {
                0.0 // skip bg penalty in dark mode — it's calibrated for light
            } else {
                local_background_penalty(image, plus.center_x, plus.center_y)
            };
            let score = (dx - 24.0).abs()
                + (dy * 2.0)
                + ((plus.center_y - expected_y).abs() * 0.06)
                + (bg_penalty * 1.4);
            match best {
                Some((best_score, _, _)) if score >= best_score => {}
                _ => best = Some((score, plus.center_x, plus.center_y)),
            }
        }
    }

    best.map(|(_, x, y)| (x, y))
}

fn detect_table_bounds_from_borders(
    image: &RgbaImage,
    content: &Bounds,
    add_pair: Option<(f64, f64)>,
) -> Option<Bounds> {
    let width = image.width() as i32;
    let height = image.height() as i32;
    if width < 40 || height < 40 {
        return None;
    }

    let x0 = (content.x + 4.0).floor().max(0.0) as i32;
    let x1 = (content.x + content.width - 4.0).ceil().min(width as f64) as i32;
    let y0 = (content.y + 6.0).floor().max(0.0) as i32;
    let y1 = (content.y + content.height * 0.62)
        .ceil()
        .min(height as f64) as i32;
    if x1 - x0 < 180 || y1 - y0 < 48 {
        return None;
    }

    let passes = luminance_passes(image);
    let img_w = image.width() as usize;

    let anchor_y = add_pair
        .map(|(_, y)| y.round() as i32)
        .unwrap_or((y0 + y1) / 2);
    // Widened bottom border search range (was +2..+20, now -4..+28)
    let bottom_min = (anchor_y - 4).clamp(y0 + 24, y1 - 2);
    let bottom_max = (anchor_y + 28).clamp(bottom_min + 1, y1 - 1);
    let y_bottom = strongest_horizontal_edge(&passes, img_w, x0, x1, bottom_min, bottom_max)?;

    let top_min = (y0 + 6).min(y_bottom - 10);
    let top_max = (y_bottom - 24).clamp(top_min + 1, y_bottom - 1);
    let y_top = strongest_horizontal_edge(&passes, img_w, x0, x1, top_min, top_max)?;
    if y_bottom - y_top < 24 || y_bottom - y_top > 116 {
        return None;
    }

    // Widened left border search range for add_pair (was -24..+10, now -20..-6)
    let (left_min, left_max) = if let Some((add_x, _)) = add_pair {
        let min = (add_x.round() as i32 - 20).clamp(x0, x1 - 2);
        let max = (add_x.round() as i32 - 6).clamp(min + 1, x1 - 1);
        (min, max)
    } else {
        let max = (x0 as f64 + (x1 - x0) as f64 * 0.35).round() as i32;
        (x0, max.clamp(x0 + 1, x1 - 1))
    };
    let x_left = strongest_vertical_edge(&passes, img_w, y_top, y_bottom, left_min, left_max)?;

    let right_min = (x0 as f64 + (x1 - x0) as f64 * 0.58).round() as i32;
    let right_max = (x1 - 1).max(right_min + 1);
    let x_right = strongest_vertical_edge(
        &passes,
        img_w,
        y_top,
        y_bottom,
        right_min.clamp(x_left + 24, x1 - 2),
        right_max,
    )?;
    if x_right - x_left < 160 {
        return None;
    }

    if let Some((add_x, add_y)) = add_pair {
        let expected_x = x_left as f64 + 12.0;
        let expected_y = y_bottom as f64 - 8.0;
        if (expected_x - add_x).abs() > 18.0 || (expected_y - add_y).abs() > 18.0 {
            return None;
        }
    }

    Some(Bounds {
        x: x_left as f64,
        y: y_top as f64,
        width: (x_right - x_left).max(0) as f64,
        height: (y_bottom - y_top).max(0) as f64,
    })
}

fn luminance_passes(image: &RgbaImage) -> [Vec<u8>; 3] {
    let mut raw = Vec::with_capacity((image.width() * image.height()) as usize);
    for pixel in image.pixels() {
        let [r, g, b, _] = pixel.0;
        let luma = ((r as u16 * 30 + g as u16 * 59 + b as u16 * 11) / 100) as u8;
        raw.push(luma);
    }
    let stretch = raw
        .iter()
        .map(|v| (((*v as i16 - 128) * 2 + 128).clamp(0, 255)) as u8)
        .collect::<Vec<_>>();
    let bw = raw
        .iter()
        .map(|v| if *v >= 176 { 255 } else { 0 })
        .collect::<Vec<_>>();
    [raw, stretch, bw]
}

fn strongest_horizontal_edge(
    passes: &[Vec<u8>; 3],
    image_width: usize,
    x0: i32,
    x1: i32,
    y0: i32,
    y1: i32,
) -> Option<i32> {
    if y1 <= y0 + 1 || x1 <= x0 + 1 {
        return None;
    }
    let total_pixels = ((x1 - x0) as f64).max(1.0);
    let mut best: Option<(f64, i32)> = None;
    for y in y0.max(1)..=y1 {
        let mut score = 0.0;
        let mut samples = 0.0;
        let mut longest_run = 0i32;
        let mut current_run = 0i32;
        let stride = if x1 - x0 > 500 { 2 } else { 1 };
        for x in (x0..x1).step_by(stride as usize) {
            let idx = y as usize * image_width + x as usize;
            let idx_up = (y as usize - 1) * image_width + x as usize;
            let mut diff = 0u8;
            for pass in passes {
                let d = pass[idx].abs_diff(pass[idx_up]);
                if d > diff {
                    diff = d;
                }
            }
            if diff >= 8 {
                score += diff as f64;
                samples += 1.0;
                current_run += 1;
                if current_run > longest_run {
                    longest_run = current_run;
                }
            } else {
                current_run = 0;
            }
        }
        if samples < 24.0 {
            continue;
        }
        let coverage = samples / total_pixels;
        if coverage < 0.25 {
            continue;
        }
        // Require longest contiguous run >= 35% of scan range
        let run_ratio = longest_run as f64 / total_pixels;
        if run_ratio < 0.35 {
            continue;
        }
        let avg = score / samples;
        // Score = coverage^0.5 * avg_gradient
        let edge_score = coverage.sqrt() * avg;
        match best {
            Some((best_score, _)) if edge_score <= best_score => {}
            _ => best = Some((edge_score, y)),
        }
    }
    best.map(|(_, y)| y)
}

fn horizontal_edge_response(
    passes: &[Vec<u8>; 3],
    image_width: usize,
    y: i32,
    x0: i32,
    x1: i32,
) -> f64 {
    if y <= 0 || x1 <= x0 + 1 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut n = 0.0;
    let stride = if x1 - x0 > 500 { 2 } else { 1 };
    for x in (x0..x1).step_by(stride as usize) {
        let idx = y as usize * image_width + x as usize;
        let idx_up = (y as usize - 1) * image_width + x as usize;
        let mut diff = 0u8;
        for pass in passes {
            let d = pass[idx].abs_diff(pass[idx_up]);
            if d > diff {
                diff = d;
            }
        }
        sum += diff as f64;
        n += 1.0;
    }
    if n <= 0.0 { 0.0 } else { sum / n }
}

fn strongest_vertical_edge(
    passes: &[Vec<u8>; 3],
    image_width: usize,
    y0: i32,
    y1: i32,
    x0: i32,
    x1: i32,
) -> Option<i32> {
    if x1 <= x0 + 1 || y1 <= y0 + 1 {
        return None;
    }
    let total_pixels = ((y1 - y0) as f64).max(1.0);
    let mut best: Option<(f64, i32)> = None;
    for x in x0.max(1)..=x1 {
        let mut score = 0.0;
        let mut samples = 0.0;
        let mut longest_run = 0i32;
        let mut current_run = 0i32;
        let stride = if y1 - y0 > 180 { 2 } else { 1 };
        for y in (y0..y1).step_by(stride as usize) {
            let idx = y as usize * image_width + x as usize;
            let idx_left = y as usize * image_width + (x as usize - 1);
            let mut diff = 0u8;
            for pass in passes {
                let d = pass[idx].abs_diff(pass[idx_left]);
                if d > diff {
                    diff = d;
                }
            }
            if diff >= 8 {
                score += diff as f64;
                samples += 1.0;
                current_run += 1;
                if current_run > longest_run {
                    longest_run = current_run;
                }
            } else {
                current_run = 0;
            }
        }
        if samples < 14.0 {
            continue;
        }
        let coverage = samples / total_pixels;
        if coverage < 0.25 {
            continue;
        }
        let run_ratio = longest_run as f64 / total_pixels;
        if run_ratio < 0.35 {
            continue;
        }
        let avg = score / samples;
        let edge_score = coverage.sqrt() * avg;
        match best {
            Some((best_score, _)) if edge_score <= best_score => {}
            _ => best = Some((edge_score, x)),
        }
    }
    best.map(|(_, x)| x)
}

fn vertical_edge_response(
    passes: &[Vec<u8>; 3],
    image_width: usize,
    x: i32,
    y0: i32,
    y1: i32,
) -> f64 {
    if x <= 0 || y1 <= y0 + 1 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut n = 0.0;
    let stride = if y1 - y0 > 240 { 2 } else { 1 };
    for y in (y0..y1).step_by(stride as usize) {
        let idx = y as usize * image_width + x as usize;
        let idx_left = y as usize * image_width + (x as usize - 1);
        let mut diff = 0u8;
        for pass in passes {
            let d = pass[idx].abs_diff(pass[idx_left]);
            if d > diff {
                diff = d;
            }
        }
        sum += diff as f64;
        n += 1.0;
    }
    if n <= 0.0 { 0.0 } else { sum / n }
}

fn fallback_table_bounds(content: &Bounds) -> Option<Bounds> {
    if content.width <= 0.0 || content.height <= 0.0 {
        return None;
    }
    Some(Bounds {
        x: (content.x + 18.0).max(0.0),
        y: (content.y + 66.0).max(0.0),
        width: (content.width - 30.0).max(140.0),
        height: (content.height * 0.14).clamp(56.0, 92.0),
    })
}

fn is_neutral_panel_pixel(r: u8, g: u8, b: u8) -> bool {
    let max = r.max(g).max(b) as i16;
    let min = r.min(g).min(b) as i16;
    let chroma = max - min;
    let luma = (r as u16 + g as u16 + b as u16) / 3;
    chroma <= 30 && (112..=246).contains(&luma)
}

fn is_dark_panel_pixel(r: u8, g: u8, b: u8) -> bool {
    let max = r.max(g).max(b) as i16;
    let min = r.min(g).min(b) as i16;
    let chroma = max - min;
    let luma = (r as u16 + g as u16 + b as u16) / 3;
    // Tighter chroma than light mode to separate from nature backgrounds
    chroma <= 22 && (28..=85).contains(&luma)
}

fn is_symbol_pixel(r: u8, g: u8, b: u8, dark_mode: bool) -> bool {
    let max = r.max(g).max(b) as i16;
    let min = r.min(g).min(b) as i16;
    let chroma = max - min;
    let luma = (r as u16 + g as u16 + b as u16) / 3;
    if dark_mode {
        // In dark mode, +/- symbols are lighter gray on dark background
        (120..=220).contains(&luma) && chroma <= 30
    } else {
        (70..=200).contains(&luma) && chroma <= 30
    }
}

/// Fallback for dark mode when flood fill fails (e.g., dark background blends with
/// dark window). Uses the column-variance transition — the sidebar has uniform dark
/// pixels (std < ~12) while the content area has list items that spike variance.
/// Anchors the search on the traffic-light position.
fn detect_via_column_variance(
    image: &RgbaImage,
    tl_pos: Option<(i32, i32)>,
) -> Option<(Bounds, Bounds, Bounds)> {
    let width = image.width() as i32;
    let height = image.height() as i32;
    let (tl_x, tl_y) = tl_pos?;

    // Estimate window top-left from traffic light anchor.
    // Traffic lights sit ~20px from window left edge, ~18px from top.
    let window_x = (tl_x - 22).max(0) as f64;
    let window_y = (tl_y - 20).max(0) as f64;
    let title_h = 44.0f64;

    let y_top = (window_y + title_h + 4.0) as i32;
    let y_bottom = ((height as f64) * 0.72) as i32;

    // Search for the sidebar/content divider between traffic-light-right and 75% of width.
    let x_start = (tl_x + 80).clamp(0, width - 2) as usize;
    let x_end = ((width as f64 * 0.75) as i32).clamp(x_start as i32 + 40, width - 1) as usize;
    if x_end <= x_start || y_bottom <= y_top + 40 {
        return None;
    }

    // Compute per-column luma std over the content band.
    let col_stds: Vec<f64> = (x_start..=x_end)
        .map(|x| col_luma_std(image, x as i32, y_top, y_bottom))
        .collect();

    // Find the rightmost "uniform" column (std < LOW_THR) such that the next
    // LOOKAHEAD columns have an average std > HIGH_THR.  This marks the last
    // column of the sidebar before the content area starts.
    const LOW_THR: f64 = 15.0;
    const HIGH_THR: f64 = 28.0;
    const LOOKAHEAD: usize = 40;

    // Walk through runs of uniform columns.  For each run, check if the high-variance
    // content immediately after it qualifies (lookahead avg > HIGH_THR).  Pick the run
    // with the largest width; prefer rightmost on ties.  This avoids picking small
    // isolated low-variance patches in forest backgrounds.
    const MIN_BLOCK_WIDTH: usize = 20;
    let mut best: Option<(usize, usize)> = None; // (block_width, run_end_idx)
    let n = col_stds.len();
    let mut i = 0;
    while i < n.saturating_sub(LOOKAHEAD) {
        if col_stds[i] >= LOW_THR {
            i += 1;
            continue;
        }
        let run_start = i;
        while i < n && col_stds[i] < LOW_THR {
            i += 1;
        }
        let run_end = i - 1;
        let block_width = run_end - run_start + 1;
        if block_width < MIN_BLOCK_WIDTH {
            continue;
        }
        // Check lookahead from just after the run.
        let look_start = run_end + 1;
        let look_end = (look_start + LOOKAHEAD).min(n);
        if look_end <= look_start + 1 {
            continue;
        }
        let ahead_avg: f64 = col_stds[look_start..look_end].iter().sum::<f64>()
            / (look_end - look_start) as f64;
        if ahead_avg > HIGH_THR {
            match best {
                Some((bw, _)) if block_width < bw => {}
                _ => best = Some((block_width, run_end)),
            }
        }
    }
    let divider_idx = best?.1;
    let divider_x = (x_start + divider_idx) as i32;

    // Validate 1: content to the right must be genuinely non-uniform (list items create variance).
    let right_std = col_luma_std(image, divider_x + 10, y_top, y_bottom);
    if right_std < 20.0 {
        return None;
    }

    // Validate 2: the region around the divider must be DARK on both sides.
    // Light-mode images have content_mean > 100; dark forest blending produces < 100.
    let sidebar_mean = col_luma_mean(image, (window_x as i32).max(0), divider_x, y_top, y_bottom);
    let content_mean = col_luma_mean(image, divider_x + 2, divider_x + 60, y_top, y_bottom);
    if sidebar_mean >= 80.0 || content_mean >= 100.0 {
        return None;
    }

    let content_x = (divider_x + 1) as f64;
    let content_y = window_y + title_h;
    // Content runs to near the right edge of the image (conservative — add-button
    // detection is robust to excess width).
    let content_w = (width as f64 - content_x - 10.0).max(200.0);
    let content_h = (height as f64 * 0.82 - content_y).max(200.0);

    let sidebar_w = (content_x - window_x).clamp(100.0, 400.0);
    let window = Bounds {
        x: window_x,
        y: window_y,
        width: (content_x + content_w - window_x).max(400.0),
        height: (content_h + title_h).max(300.0),
    };
    let sidebar = Bounds {
        x: window_x,
        y: content_y,
        width: sidebar_w,
        height: content_h,
    };
    let content = Bounds {
        x: content_x,
        y: content_y,
        width: content_w,
        height: content_h,
    };
    Some((window, sidebar, content))
}

/// Average luma for a horizontal band of columns x ∈ [x_start, x_end) over y ∈ [y_top, y_bottom).
fn col_luma_mean(image: &RgbaImage, x_start: i32, x_end: i32, y_top: i32, y_bottom: i32) -> f64 {
    let img_w = image.width() as i32;
    let img_h = image.height() as i32;
    let x0 = x_start.clamp(0, img_w - 1);
    let x1 = x_end.clamp(x0, img_w);
    let y0 = y_top.clamp(0, img_h - 1);
    let y1 = y_bottom.clamp(y0, img_h);
    if x1 <= x0 || y1 <= y0 {
        return 0.0;
    }
    let mut sum = 0u64;
    let mut n = 0u64;
    let x_stride = ((x1 - x0) / 10).max(1) as usize;
    let y_stride = ((y1 - y0) / 10).max(1) as usize;
    let mut x = x0;
    while x < x1 {
        let mut y = y0;
        while y < y1 {
            let [r, g, b, _] = image.get_pixel(x as u32, y as u32).0;
            sum += (r as u64 * 30 + g as u64 * 59 + b as u64 * 11) / 100;
            n += 1;
            y += y_stride as i32;
        }
        x += x_stride as i32;
    }
    if n == 0 { 128.0 } else { sum as f64 / n as f64 }
}

/// Luma standard deviation for a single column over y ∈ [y_top, y_bottom).
fn col_luma_std(image: &RgbaImage, x: i32, y_top: i32, y_bottom: i32) -> f64 {
    let img_w = image.width() as i32;
    let img_h = image.height() as i32;
    let x = x.clamp(0, img_w - 1) as u32;
    let y0 = y_top.clamp(0, img_h - 1);
    let y1 = y_bottom.clamp(y0, img_h);
    if y1 <= y0 {
        return 0.0;
    }
    // Stride 3 for speed — we only need a relative measure.
    let mut sum = 0u64;
    let mut sum_sq = 0u64;
    let mut n = 0u64;
    let mut y = y0;
    while y < y1 {
        let [r, g, b, _] = image.get_pixel(x, y as u32).0;
        let luma = (r as u64 * 30 + g as u64 * 59 + b as u64 * 11) / 100;
        sum += luma;
        sum_sq += luma * luma;
        n += 1;
        y += 3;
    }
    if n < 2 {
        return 0.0;
    }
    let mean = sum as f64 / n as f64;
    let var = (sum_sq as f64 / n as f64) - mean * mean;
    var.max(0.0).sqrt()
}

fn local_background_penalty(image: &RgbaImage, x: f64, y: f64) -> f64 {
    let width = image.width() as i32;
    let height = image.height() as i32;
    let offsets = [
        (-8, 0),
        (8, 0),
        (0, -8),
        (0, 8),
        (-10, -6),
        (10, -6),
        (-10, 6),
        (10, 6),
    ];
    let mut penalty = 0.0f64;
    let mut samples = 0.0f64;
    for (dx, dy) in offsets {
        let sx = (x.round() as i32 + dx).clamp(0, width - 1);
        let sy = (y.round() as i32 + dy).clamp(0, height - 1);
        let [r, g, b, _] = image.get_pixel(sx as u32, sy as u32).0;
        let max = r.max(g).max(b) as i16;
        let min = r.min(g).min(b) as i16;
        let chroma = (max - min) as f64;
        let luma = (r as f64 + g as f64 + b as f64) / 3.0;
        if luma < 165.0 {
            penalty += 0.8;
        } else if luma < 182.0 {
            penalty += 0.25;
        }
        if chroma > 24.0 {
            penalty += 0.6;
        } else if chroma > 16.0 {
            penalty += 0.2;
        }
        samples += 1.0;
    }
    if samples <= 0.0 {
        1.0
    } else {
        penalty / samples
    }
}

#[cfg(test)]
mod tests {
    use super::detect_settings_regions;
    use image::{Rgba, RgbaImage};

    #[test]
    fn detects_large_neutral_window_region() {
        let mut img = RgbaImage::from_pixel(420, 280, Rgba([18, 68, 22, 255]));
        for y in 52..242 {
            for x in 74..356 {
                img.put_pixel(x, y, Rgba([211, 212, 210, 255]));
            }
        }
        let regions = detect_settings_regions(&img);
        let window = regions.window_bounds.expect("window");
        assert!(window.x <= 90.0);
        assert!(window.y <= 70.0);
        assert!(window.width >= 250.0);
        assert!(window.height >= 150.0);
        assert!(regions.content_bounds.is_some());
        assert!(regions.table_bounds.is_some());
    }
}

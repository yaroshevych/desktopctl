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
    let Some(neutral_bounds) = detect_neutral_content_bounds(image) else {
        return SettingsRegions::default();
    };

    let full_window_detected = has_window_traffic_lights(image, &neutral_bounds);

    let (window, sidebar, content) = if full_window_detected {
        let window = neutral_bounds.clone();
        let title_h = (window.height * 0.085).clamp(30.0, 56.0);
        let sidebar_w = (window.width * 0.24).clamp(148.0, 235.0);
        let content = Bounds {
            x: (window.x + sidebar_w).max(0.0),
            y: (window.y + title_h).max(0.0),
            width: (window.width - sidebar_w).max(0.0),
            height: (window.height - title_h).max(0.0),
        };
        let sidebar = Bounds {
            x: window.x,
            y: content.y,
            width: (content.x - window.x).max(0.0),
            height: content.height.max(0.0),
        };
        (window, sidebar, content)
    } else {
        let content = neutral_bounds.clone();
        let title_h = (content.height * 0.075).clamp(30.0, 56.0);
        let sidebar_w = (content.width * 0.30).clamp(160.0, 330.0);
        let window_x = (content.x - sidebar_w).max(0.0);
        let window_y = (content.y - title_h).max(0.0);
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

    let add_pair = detect_add_button_pair(image, &content);
    let table = detect_table_bounds_from_borders(image, &content, add_pair)
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
        .or_else(|| fallback_table_bounds(&content));

    SettingsRegions {
        window_bounds: Some(window),
        sidebar_bounds: Some(sidebar),
        content_bounds: Some(content),
        table_bounds: table,
    }
}

fn detect_neutral_content_bounds(image: &RgbaImage) -> Option<Bounds> {
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
            if is_neutral_panel_pixel(px[0], px[1], px[2]) {
                mask[gy * grid_w + gx] = true;
            }
        }
    }

    let mut visited = vec![false; grid_w * grid_h];
    let mut best: Option<(f64, Bounds)> = None;
    let min_box_w = ((grid_w as f64) * 0.16).ceil() as usize;
    let min_box_h = ((grid_h as f64) * 0.22).ceil() as usize;
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

fn has_window_traffic_lights(image: &RgbaImage, bounds: &Bounds) -> bool {
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

fn detect_add_button_pair(image: &RgbaImage, content: &Bounds) -> Option<(f64, f64)> {
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
            if is_symbol_pixel(px[0], px[1], px[2]) {
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
            let bg_penalty = local_background_penalty(image, plus.center_x, plus.center_y);
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
    let bottom_min = (anchor_y + 2).clamp(y0 + 24, y1 - 2);
    let bottom_max = (anchor_y + 20).clamp(bottom_min + 1, y1 - 1);
    let y_bottom = strongest_horizontal_edge(&passes, img_w, x0, x1, bottom_min, bottom_max)?;

    let top_min = (y0 + 6).min(y_bottom - 10);
    let top_max = (y_bottom - 24).clamp(top_min + 1, y_bottom - 1);
    let y_top = strongest_horizontal_edge(&passes, img_w, x0, x1, top_min, top_max)?;
    if y_bottom - y_top < 24 || y_bottom - y_top > 116 {
        return None;
    }

    let (left_min, left_max) = if let Some((add_x, _)) = add_pair {
        let min = (add_x.round() as i32 - 24).clamp(x0, x1 - 2);
        let max = (add_x.round() as i32 + 10).clamp(min + 1, x1 - 1);
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
    let mut best: Option<(f64, i32)> = None;
    for y in y0.max(1)..=y1 {
        let mut score = 0.0;
        let mut samples = 0.0;
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
            }
        }
        if samples < 24.0 {
            continue;
        }
        let avg = score / samples;
        match best {
            Some((best_score, _)) if avg <= best_score => {}
            _ => best = Some((avg, y)),
        }
    }
    best.map(|(_, y)| y)
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
    let mut best: Option<(f64, i32)> = None;
    for x in x0.max(1)..=x1 {
        let mut score = 0.0;
        let mut samples = 0.0;
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
            }
        }
        if samples < 14.0 {
            continue;
        }
        let avg = score / samples;
        match best {
            Some((best_score, _)) if avg <= best_score => {}
            _ => best = Some((avg, x)),
        }
    }
    best.map(|(_, x)| x)
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

fn is_symbol_pixel(r: u8, g: u8, b: u8) -> bool {
    let max = r.max(g).max(b) as i16;
    let min = r.min(g).min(b) as i16;
    let chroma = max - min;
    let luma = (r as u16 + g as u16 + b as u16) / 3;
    (70..=200).contains(&luma) && chroma <= 30
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

use desktop_core::protocol::Bounds;
use image::RgbaImage;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, Default)]
pub struct SettingsRegions {
    pub window_bounds: Option<Bounds>,
    pub sidebar_bounds: Option<Bounds>,
    pub content_bounds: Option<Bounds>,
    pub table_bounds: Option<Bounds>,
}

pub fn detect_settings_regions(image: &RgbaImage) -> SettingsRegions {
    let Some(window) = detect_neutral_window_bounds(image) else {
        return SettingsRegions::default();
    };
    let title_h = (window.height * 0.085).clamp(30.0, 58.0);
    let sidebar_w = (window.width * 0.31).clamp(150.0, 360.0);
    let sidebar = Bounds {
        x: window.x,
        y: (window.y + title_h).max(0.0),
        width: sidebar_w.min(window.width).max(0.0),
        height: (window.height - title_h).max(0.0),
    };
    let content = Bounds {
        x: (window.x + sidebar.width).max(0.0),
        y: (window.y + title_h).max(0.0),
        width: (window.width - sidebar.width).max(0.0),
        height: (window.height - title_h).max(0.0),
    };
    let table = Some(Bounds {
        x: (content.x + 16.0).max(0.0),
        y: (content.y + 62.0).max(0.0),
        width: (content.width - 32.0).max(120.0),
        height: (content.height * 0.16).clamp(52.0, 100.0),
    });
    SettingsRegions {
        window_bounds: Some(window),
        sidebar_bounds: Some(sidebar),
        content_bounds: Some(content),
        table_bounds: table,
    }
}

fn detect_neutral_window_bounds(image: &RgbaImage) -> Option<Bounds> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width < 320 || height < 200 {
        return None;
    }

    let row_step = 1usize;
    let col_step = 1usize;

    let mut row_mask = vec![false; height];
    for y in (0..height).step_by(row_step) {
        let mut neutral = 0usize;
        let mut samples = 0usize;
        for x in (0..width).step_by(col_step) {
            let px = image.get_pixel(x as u32, y as u32).0;
            if is_neutral_panel_pixel(px[0], px[1], px[2]) {
                neutral += 1;
            }
            samples += 1;
        }
        if samples > 0 && (neutral as f64 / samples as f64) > 0.24 {
            row_mask[y] = true;
        }
    }
    let (row_start, row_end) = longest_true_run(&row_mask)?;
    if row_end <= row_start || row_end - row_start < 120 {
        return None;
    }

    let mut col_mask = vec![false; width];
    for x in (0..width).step_by(col_step) {
        let mut neutral = 0usize;
        let mut samples = 0usize;
        for y in (row_start..=row_end).step_by(row_step) {
            let px = image.get_pixel(x as u32, y as u32).0;
            if is_neutral_panel_pixel(px[0], px[1], px[2]) {
                neutral += 1;
            }
            samples += 1;
        }
        if samples > 0 && (neutral as f64 / samples as f64) > 0.28 {
            col_mask[x] = true;
        }
    }
    let (col_start, col_end) = longest_true_run(&col_mask)?;
    if col_end <= col_start || col_end - col_start < 260 {
        return None;
    }

    Some(Bounds {
        x: col_start as f64,
        y: row_start as f64,
        width: (col_end - col_start + 1) as f64,
        height: (row_end - row_start + 1) as f64,
    })
}

fn longest_true_run(values: &[bool]) -> Option<(usize, usize)> {
    let mut best_start = 0usize;
    let mut best_end = 0usize;
    let mut best_len = 0usize;

    let mut i = 0usize;
    while i < values.len() {
        if !values[i] {
            i += 1;
            continue;
        }
        let start = i;
        while i < values.len() && values[i] {
            i += 1;
        }
        let end = i.saturating_sub(1);
        let len = end.saturating_sub(start) + 1;
        if len > best_len {
            best_len = len;
            best_start = start;
            best_end = end;
        }
    }

    if best_len == 0 {
        None
    } else {
        Some((best_start, best_end))
    }
}

fn is_neutral_panel_pixel(r: u8, g: u8, b: u8) -> bool {
    let max = r.max(g).max(b) as i16;
    let min = r.min(g).min(b) as i16;
    let chroma = max - min;
    let luma = (r as u16 + g as u16 + b as u16) / 3;
    chroma <= 28 && (130..=246).contains(&luma)
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

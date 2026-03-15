use std::{env, process};

use desktop_core::protocol::Bounds;
use image::{ImageFormat, Rgba, RgbaImage};

#[path = "../src/vision/regions.rs"]
mod regions;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err}");
        process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        return Err("usage: render_settings_regions <input.png> <output.png>".to_string());
    }
    let input = &args[1];
    let output = &args[2];

    let mut image = image::open(input)
        .map_err(|err| format!("failed to open input image: {err}"))?
        .to_rgba8();

    let detected = regions::detect_settings_regions(&image);

    if let Some(bounds) = detected.window_bounds.as_ref() {
        draw_rect_outline(&mut image, bounds, Rgba([245, 179, 34, 255]), 2);
    }
    if let Some(bounds) = detected.sidebar_bounds.as_ref() {
        draw_rect_outline(&mut image, bounds, Rgba([170, 170, 170, 255]), 2);
    }
    if let Some(bounds) = detected.content_bounds.as_ref() {
        draw_rect_outline(&mut image, bounds, Rgba([45, 199, 124, 255]), 2);
    }
    if let Some(bounds) = detected.table_bounds.as_ref() {
        draw_rect_outline(&mut image, bounds, Rgba([255, 111, 97, 255]), 2);
        let add_x = (bounds.x + 12.0).round().max(0.0) as u32;
        let add_y = (bounds.y + bounds.height - 8.0).round().max(0.0) as u32;
        draw_crosshair(&mut image, add_x, add_y, Rgba([90, 255, 90, 255]));
    }

    image
        .save_with_format(output, ImageFormat::Png)
        .map_err(|err| format!("failed to write output image: {err}"))?;

    println!(
        "{{\"input\":\"{}\",\"output\":\"{}\",\"window\":{},\"sidebar\":{},\"content\":{},\"table\":{}}}",
        input,
        output,
        bounds_json(detected.window_bounds.as_ref()),
        bounds_json(detected.sidebar_bounds.as_ref()),
        bounds_json(detected.content_bounds.as_ref()),
        bounds_json(detected.table_bounds.as_ref()),
    );

    Ok(())
}

fn bounds_json(bounds: Option<&Bounds>) -> String {
    match bounds {
        Some(b) => format!(
            "{{\"x\":{:.2},\"y\":{:.2},\"width\":{:.2},\"height\":{:.2}}}",
            b.x, b.y, b.width, b.height
        ),
        None => "null".to_string(),
    }
}

fn draw_rect_outline(image: &mut RgbaImage, bounds: &Bounds, color: Rgba<u8>, thickness: u32) {
    if image.width() == 0 || image.height() == 0 || bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    let max_x = image.width() - 1;
    let max_y = image.height() - 1;

    let x0 = bounds.x.floor().max(0.0).min(max_x as f64) as u32;
    let y0 = bounds.y.floor().max(0.0).min(max_y as f64) as u32;
    let x1 = (bounds.x + bounds.width)
        .ceil()
        .max(0.0)
        .min(image.width() as f64) as u32;
    let y1 = (bounds.y + bounds.height)
        .ceil()
        .max(0.0)
        .min(image.height() as f64) as u32;
    if x1 <= x0 || y1 <= y0 {
        return;
    }
    let x1 = x1.saturating_sub(1);
    let y1 = y1.saturating_sub(1);

    let t = thickness.max(1);
    for offset in 0..t {
        let top = y0.saturating_add(offset).min(y1);
        let bottom = y1.saturating_sub(offset).max(y0);
        for x in x0..=x1 {
            image.put_pixel(x, top, color);
            image.put_pixel(x, bottom, color);
        }
        let left = x0.saturating_add(offset).min(x1);
        let right = x1.saturating_sub(offset).max(x0);
        for y in y0..=y1 {
            image.put_pixel(left, y, color);
            image.put_pixel(right, y, color);
        }
    }
}

fn draw_crosshair(image: &mut RgbaImage, x: u32, y: u32, color: Rgba<u8>) {
    if image.width() == 0 || image.height() == 0 {
        return;
    }
    let x = x.min(image.width() - 1);
    let y = y.min(image.height() - 1);
    let radius = 6_i32;
    for dx in -radius..=radius {
        let px = x as i32 + dx;
        if (0..image.width() as i32).contains(&px) {
            image.put_pixel(px as u32, y, color);
        }
    }
    for dy in -radius..=radius {
        let py = y as i32 + dy;
        if (0..image.height() as i32).contains(&py) {
            image.put_pixel(x, py as u32, color);
        }
    }
}

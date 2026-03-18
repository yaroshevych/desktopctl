use std::path::PathBuf;

use desktop_core::protocol::Bounds;
use image::{Rgba, RgbaImage};
use serde_json::json;

#[path = "../trace.rs"]
mod trace;
#[path = "../vision/ocr.rs"]
mod ocr;
#[path = "../vision/tokenize_boxes.rs"]
mod tokenize_boxes;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut input: Option<PathBuf> = None;
    let mut overlay_out: Option<PathBuf> = None;
    let mut json_out: Option<PathBuf> = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--input" => {
                input = args.next().map(PathBuf::from);
            }
            "--overlay" => {
                overlay_out = args.next().map(PathBuf::from);
            }
            "--json" => {
                json_out = args.next().map(PathBuf::from);
            }
            _ => {}
        }
    }

    let input = input.ok_or("missing --input <image.png>")?;
    let image = image::open(&input)?.to_rgba8();
    let width = image.width();
    let height = image.height();

    let texts = match ocr::recognize_text_from_image(&input, width, height) {
        Ok(texts) => texts,
        Err(err) => {
            eprintln!("warn: OCR unavailable for {}: {}", input.display(), err.message);
            Vec::new()
        }
    };
    let boxes = tokenize_boxes::detect_ui_boxes(&image);
    let text_bounds: Vec<Bounds> = texts.iter().map(|t| t.bounds.clone()).collect();
    let glyphs = tokenize_boxes::detect_glyphs(&image, &text_bounds);

    let payload = json!({
        "image": {
            "path": input,
            "width": width,
            "height": height
        },
        "windows": [{
            "id": "win_0001",
            "title": "screenshot",
            "bounds": {"x": 0.0, "y": 0.0, "width": width as f64, "height": height as f64},
            "elements": build_elements_json(&texts, &boxes, &glyphs)
        }]
    });

    if let Some(path) = json_out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&payload)?)?;
    }

    if let Some(path) = overlay_out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut canvas = image.clone();
        draw_bounds_outline(
            &mut canvas,
            &Bounds {
                x: 0.0,
                y: 0.0,
                width: width as f64,
                height: height as f64,
            },
            Rgba([255, 255, 255, 255]),
            2,
        );
        for text in &texts {
            draw_bounds_outline(&mut canvas, &text.bounds, Rgba([0, 190, 0, 255]), 1);
        }
        for bounds in &boxes {
            draw_bounds_outline(&mut canvas, bounds, Rgba([40, 120, 255, 255]), 1);
        }
        for bounds in &glyphs {
            draw_bounds_outline(&mut canvas, bounds, Rgba([255, 220, 0, 255]), 1);
        }
        canvas.save(path)?;
    }

    println!(
        "{}",
        json!({
            "image": input,
            "text_count": texts.len(),
            "box_count": boxes.len(),
            "glyph_count": glyphs.len()
        })
    );

    Ok(())
}

fn build_elements_json(
    texts: &[desktop_core::protocol::SnapshotText],
    boxes: &[Bounds],
    glyphs: &[Bounds],
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for (idx, text) in texts.iter().enumerate() {
        out.push(json!({
            "id": format!("text_{:04}", idx + 1),
            "type": "text",
            "bbox": [text.bounds.x, text.bounds.y, text.bounds.width, text.bounds.height],
            "text": text.text,
            "confidence": text.confidence,
            "source": "vision_ocr"
        }));
    }
    for (idx, bounds) in boxes.iter().enumerate() {
        out.push(json!({
            "id": format!("box_{:04}", idx + 1),
            "type": "box",
            "bbox": [bounds.x, bounds.y, bounds.width, bounds.height],
            "source": "rust_edge_grid_v1"
        }));
    }
    for (idx, bounds) in glyphs.iter().enumerate() {
        out.push(json!({
            "id": format!("glyph_{:04}", idx + 1),
            "type": "glyph",
            "bbox": [bounds.x, bounds.y, bounds.width, bounds.height],
            "source": "rust_cc_glyph_v1"
        }));
    }
    out.sort_by(|a, b| {
        let ay = a["bbox"][1].as_f64().unwrap_or(0.0);
        let by = b["bbox"][1].as_f64().unwrap_or(0.0);
        let ax = a["bbox"][0].as_f64().unwrap_or(0.0);
        let bx = b["bbox"][0].as_f64().unwrap_or(0.0);
        ay.partial_cmp(&by)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(ax.partial_cmp(&bx).unwrap_or(std::cmp::Ordering::Equal))
    });
    out
}

fn draw_bounds_outline(image: &mut RgbaImage, bounds: &Bounds, color: Rgba<u8>, thickness: u32) {
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return;
    }
    let w = image.width() as i32;
    let h = image.height() as i32;
    let x0 = bounds.x.floor() as i32;
    let y0 = bounds.y.floor() as i32;
    let x1 = (bounds.x + bounds.width).ceil() as i32 - 1;
    let y1 = (bounds.y + bounds.height).ceil() as i32 - 1;
    if x1 < 0 || y1 < 0 || x0 >= w || y0 >= h {
        return;
    }

    let t = thickness.max(1) as i32;
    for offset in 0..t {
        let lx = (x0 + offset).clamp(0, w - 1);
        let rx = (x1 - offset).clamp(0, w - 1);
        let ty = (y0 + offset).clamp(0, h - 1);
        let by = (y1 - offset).clamp(0, h - 1);
        if lx > rx || ty > by {
            continue;
        }
        for x in lx..=rx {
            image.put_pixel(x as u32, ty as u32, color);
            image.put_pixel(x as u32, by as u32, color);
        }
        for y in ty..=by {
            image.put_pixel(lx as u32, y as u32, color);
            image.put_pixel(rx as u32, y as u32, color);
        }
    }
}

use std::path::PathBuf;

use desktop_core::protocol::{Bounds, SnapshotText};
use image::{Rgba, RgbaImage};
use serde::Deserialize;
use serde_json::json;

// Dev-only threshold for external label files used by tokenize_dump batch runs.
const TEXT_LABEL_CONFIDENCE_MIN: f32 = 0.50;

#[path = "../vision/metal_pipeline.rs"]
#[allow(dead_code)]
mod metal_pipeline;
#[path = "../vision/ocr.rs"]
mod ocr;
#[path = "../vision/text_group.rs"]
#[allow(dead_code)]
mod text_group;
#[path = "../vision/tokenize_boxes.rs"]
mod tokenize_boxes;
#[path = "../trace.rs"]
mod trace;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let mut input: Option<PathBuf> = None;
    let mut overlay_out: Option<PathBuf> = None;
    let mut json_out: Option<PathBuf> = None;
    let mut text_labels: Option<PathBuf> = None;
    let mut skip_ocr = false;
    let mut timings = false;

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
            "--text-labels" => {
                text_labels = args.next().map(PathBuf::from);
            }
            "--skip-ocr" => {
                skip_ocr = true;
            }
            "--timings" => {
                timings = true;
            }
            _ => {}
        }
    }

    let t_total = std::time::Instant::now();
    let input = input.ok_or("missing --input <image.png>")?;
    let t_image = std::time::Instant::now();
    let image = image::open(&input)?.to_rgba8();
    let width = image.width();
    let height = image.height();
    let image_ms = t_image.elapsed().as_secs_f64() * 1000.0;

    let t_ocr = std::time::Instant::now();
    let texts = if let Some(labels_path) = text_labels.as_ref() {
        load_texts_from_labels(labels_path, width as f64, height as f64)?
    } else if skip_ocr {
        Vec::new()
    } else {
        match ocr::recognize_text_from_image(&input, width, height) {
            Ok(texts) => texts,
            Err(err) => {
                eprintln!(
                    "warn: OCR unavailable for {}: {}",
                    input.display(),
                    err.message
                );
                Vec::new()
            }
        }
    };
    let ocr_ms = t_ocr.elapsed().as_secs_f64() * 1000.0;

    let t_detect = std::time::Instant::now();
    let frame = metal_pipeline::process_cpu(&image);

    // Text grouping pipeline: OCR words -> split -> tighten -> lines -> paragraphs -> final fields.
    let words: Vec<text_group::TextBox> = text_group::build_words_from_ocr(&texts, &frame);
    let lines = text_group::group_words_into_lines(&words);
    let paragraphs = text_group::group_lines_into_paragraphs(&lines);
    let final_fields = text_group::final_text_fields(&lines, &paragraphs);
    let line_bounds: Vec<Bounds> = lines.iter().map(|l| l.bounds.clone()).collect();

    let controls = tokenize_boxes::detect_controls(&frame, &line_bounds);
    let detect_ms = t_detect.elapsed().as_secs_f64() * 1000.0;

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
            "elements": build_elements_json(&lines, &final_fields, &controls)
        }]
    });

    let t_json = std::time::Instant::now();
    if let Some(path) = json_out {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&path, serde_json::to_vec_pretty(&payload)?)?;
    }
    let json_ms = t_json.elapsed().as_secs_f64() * 1000.0;

    let t_overlay = std::time::Instant::now();
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
        for text in &final_fields {
            draw_bounds_outline(&mut canvas, &text.bounds, Rgba([0, 190, 0, 255]), 1);
        }
        for para in &paragraphs {
            draw_bounds_outline(&mut canvas, &para.bounds, Rgba([180, 80, 255, 255]), 2);
        }
        for control in &controls {
            let color = match control.kind {
                tokenize_boxes::ControlKind::TextField => Rgba([40, 120, 255, 255]),
                tokenize_boxes::ControlKind::Button => Rgba([255, 220, 0, 255]),
            };
            draw_bounds_outline(&mut canvas, &control.bounds, color, 1);
        }
        canvas.save(path)?;
    }
    let overlay_ms = t_overlay.elapsed().as_secs_f64() * 1000.0;
    let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;

    println!(
        "{}",
        json!({
            "image": input,
            "text_count": final_fields.len(),
            "control_count": controls.len(),
            "timings_ms": {
                "image_load": image_ms,
                "ocr": ocr_ms,
                "detect_controls": detect_ms,
                "write_json": json_ms,
                "write_overlay": overlay_ms,
                "total": total_ms
            }
        })
    );
    if timings {
        eprintln!(
            "timings_ms image_load={:.2} ocr={:.2} detect={:.2} json={:.2} overlay={:.2} total={:.2}",
            image_ms, ocr_ms, detect_ms, json_ms, overlay_ms, total_ms
        );
    }

    Ok(())
}

fn build_elements_json(
    lines: &[text_group::TextBox],
    texts: &[text_group::TextBox],
    controls: &[tokenize_boxes::DetectedControl],
) -> Vec<serde_json::Value> {
    let mut used_line = vec![false; lines.len()];
    let mut out: Vec<serde_json::Value> = Vec::new();

    for control in controls {
        let mut best_idx: Option<usize> = None;
        let mut best_score = 0.0f64;
        for (idx, line) in lines.iter().enumerate() {
            if used_line[idx] || line.text.trim().is_empty() {
                continue;
            }
            let line_area = (line.bounds.width * line.bounds.height).max(1.0);
            let overlap_ratio = overlap_area(&line.bounds, &control.bounds) / line_area;
            let in_box = center_inside(&line.bounds, &control.bounds);
            let score = overlap_ratio + if in_box { 0.35 } else { 0.0 };
            if score > best_score {
                best_score = score;
                best_idx = Some(idx);
            }
        }
        if let Some(idx) = best_idx {
            if best_score < 0.30 {
                continue;
            }
            used_line[idx] = true;
            let text = lines[idx].text.trim();
            if text.is_empty() {
                continue;
            }
            out.push(json!({
                "id": "",
                "type": "text",
                "bbox": [control.bounds.x, control.bounds.y, control.bounds.width, control.bounds.height],
                "has_border": true,
                "text": text,
                "source": "sat_control_v1"
            }));
        }
    }

    for text in texts {
        let mut overlaps_consumed = false;
        for (idx, line) in lines.iter().enumerate() {
            if !used_line[idx] {
                continue;
            }
            let area = (line.bounds.width * line.bounds.height).max(1.0);
            let ratio = overlap_area(&text.bounds, &line.bounds) / area;
            if ratio >= 0.60 || center_inside(&line.bounds, &text.bounds) {
                overlaps_consumed = true;
                break;
            }
        }
        if overlaps_consumed {
            continue;
        }
        out.push(json!({
            "id": "",
            "type": "text",
            "bbox": [text.bounds.x, text.bounds.y, text.bounds.width, text.bounds.height],
            "text": text.text,
            "source": "vision_ocr"
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
    for (idx, entry) in out.iter_mut().enumerate() {
        if let Some(obj) = entry.as_object_mut() {
            let source = obj
                .get("source")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unknown");
            let id = if source == "vision_ocr" {
                let text = obj
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("")
                    .trim()
                    .to_ascii_lowercase();
                let bbox_key = quantized_bbox_key(obj.get("bbox").and_then(|v| v.as_array()));
                format!("ocr_{:08x}", stable_hash32(&format!("{text}|{bbox_key}")))
            } else {
                format!("text_{:04}", idx + 1)
            };
            obj.insert("id".to_string(), serde_json::Value::String(id));
        }
    }
    out
}

fn quantized_bbox_key(bbox: Option<&Vec<serde_json::Value>>) -> String {
    let Some(bbox) = bbox else {
        return "[]".to_string();
    };
    if bbox.len() != 4 {
        return "[]".to_string();
    }
    let q = |v: Option<f64>| -> i64 { (v.unwrap_or(0.0).max(0.0) / 8.0).round() as i64 };
    format!(
        "{},{},{},{}",
        q(bbox[0].as_f64()),
        q(bbox[1].as_f64()),
        q(bbox[2].as_f64()),
        q(bbox[3].as_f64())
    )
}

fn stable_hash32(input: &str) -> u32 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in input.as_bytes() {
        hash ^= u64::from(*b);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    (hash & 0xffff_ffff) as u32
}

fn overlap_area(a: &Bounds, b: &Bounds) -> f64 {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    iw * ih
}

fn center_inside(inner: &Bounds, outer: &Bounds) -> bool {
    let cx = inner.x + inner.width * 0.5;
    let cy = inner.y + inner.height * 0.5;
    cx >= outer.x && cx <= outer.x + outer.width && cy >= outer.y && cy <= outer.y + outer.height
}

#[derive(Debug, Deserialize)]
struct LabelFile {
    #[serde(default)]
    bbox_format: Option<String>,
    windows: Vec<LabelWindow>,
}

#[derive(Debug, Deserialize)]
struct LabelWindow {
    elements: Vec<LabelElement>,
}

#[derive(Debug, Deserialize)]
struct LabelElement {
    #[serde(rename = "type")]
    kind: String,
    bbox: [f64; 4],
    text: Option<String>,
    confidence: Option<f32>,
}

fn load_texts_from_labels(
    labels_path: &std::path::Path,
    image_w: f64,
    image_h: f64,
) -> Result<Vec<SnapshotText>, Box<dyn std::error::Error>> {
    let raw = std::fs::read_to_string(labels_path)?;
    let parsed: LabelFile = serde_json::from_str(&raw)?;
    if let Some(format) = parsed.bbox_format.as_deref() {
        if format != "xywh" {
            return Err(format!(
                "unsupported bbox format in {}: {format}",
                labels_path.display()
            )
            .into());
        }
    }
    let mut texts = Vec::new();
    for window in parsed.windows {
        for element in window.elements {
            if element.kind != "text" {
                continue;
            }
            let text = element.text.unwrap_or_default();
            if text.trim().is_empty() {
                continue;
            }
            let confidence = element.confidence.unwrap_or(0.75);
            if confidence < TEXT_LABEL_CONFIDENCE_MIN {
                continue;
            }
            // Label bbox is interpreted as [x, y, width, height] (xywh).
            let x1 = element.bbox[0].clamp(0.0, image_w.max(1.0));
            let y1 = element.bbox[1].clamp(0.0, image_h.max(1.0));
            let x2 = (element.bbox[0] + element.bbox[2]).clamp(0.0, image_w.max(1.0));
            let y2 = (element.bbox[1] + element.bbox[3]).clamp(0.0, image_h.max(1.0));
            let bounds = Bounds {
                x: x1,
                y: y1,
                width: (x2 - x1).max(0.0),
                height: (y2 - y1).max(0.0),
            };
            if bounds.width < 2.0 || bounds.height < 2.0 {
                continue;
            }
            texts.push(SnapshotText {
                text,
                bounds,
                confidence,
            });
        }
    }
    Ok(texts)
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

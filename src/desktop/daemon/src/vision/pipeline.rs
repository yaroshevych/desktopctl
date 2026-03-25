use std::{
    path::{Path, PathBuf},
    process::Command as ProcessCommand,
};

use desktop_core::{
    error::AppError,
    protocol::{
        Bounds, SnapshotDisplay, SnapshotPayload, TokenEntry, TokenizeElement, TokenizeImage,
        TokenizePayload, TokenizeWindow, now_millis,
    },
};
use image::{ImageFormat, Rgba, imageops::crop_imm};

use crate::trace;

use super::{
    capture::capture_screen_png,
    diff::{diff_region, thumbnail_from_rgba, upscale_region},
    ocr::recognize_text,
    state::with_state,
};

#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub snapshot: SnapshotPayload,
    pub image_path: Option<PathBuf>,
    pub image: image::RgbaImage,
    pub event_ids: Vec<u64>,
}

#[derive(Debug, Clone)]
pub struct TokenizeWindowMeta {
    pub id: String,
    pub title: String,
    pub app: Option<String>,
    pub bounds: Bounds,
}

pub fn capture_and_update(out_path: Option<PathBuf>) -> Result<CaptureResult, AppError> {
    capture_and_update_internal(out_path, None, None, true)
}

pub fn capture_and_update_active_window(
    out_path: Option<PathBuf>,
    bounds: Bounds,
    focused_app_override: Option<String>,
    lookup_focused_app: bool,
) -> Result<CaptureResult, AppError> {
    capture_and_update_internal(
        out_path,
        Some(bounds),
        focused_app_override,
        lookup_focused_app,
    )
}

fn capture_and_update_internal(
    out_path: Option<PathBuf>,
    crop_bounds: Option<Bounds>,
    focused_app_override: Option<String>,
    lookup_focused_app: bool,
) -> Result<CaptureResult, AppError> {
    trace::log("pipeline:capture_and_update:start");
    let mut captured = capture_screen_png(out_path)?;
    if let Some(bounds) = crop_bounds.as_ref() {
        crop_capture_to_bounds(&mut captured, bounds)?;
        trace::log(format!(
            "pipeline:capture_and_update:active_window_crop_ok size={}x{}",
            captured.frame.width, captured.frame.height
        ));
    }
    if let Some(path) = captured.frame.image_path.as_ref() {
        trace::log(format!(
            "pipeline:capture_and_update:capture_ok path={} size={}x{}",
            path.display(),
            captured.frame.width,
            captured.frame.height
        ));
    } else {
        trace::log(format!(
            "pipeline:capture_and_update:capture_ok path=<memory> size={}x{}",
            captured.frame.width, captured.frame.height
        ));
    }
    let thumb = thumbnail_from_rgba(&captured.image, 96, 54);
    trace::log("pipeline:capture_and_update:thumb_ok");
    let texts = recognize_text(&captured.image)?;
    trace::log(format!(
        "pipeline:capture_and_update:ocr_ok texts={}",
        texts.len()
    ));
    let focused_app = if lookup_focused_app {
        focused_app_override.or_else(focused_app_name)
    } else {
        focused_app_override
    };
    let frame = captured.frame;
    let image_path = frame.image_path.clone();
    let image = captured.image;

    with_state(move |state| {
        let roi = state
            .latest_thumbnail()
            .and_then(|prev| diff_region(prev, &thumb, 8))
            .map(|region| {
                upscale_region(region, frame.width, frame.height, thumb.width, thumb.height)
            });

        let update = state.record_capture(frame, thumb, focused_app, texts, roi);
        trace::log(format!(
            "pipeline:capture_and_update:recorded snapshot_id={} event_id={}",
            update.snapshot.snapshot_id, update.event_id
        ));
        let event_ids = state.event_ids(update.snapshot.snapshot_id);

        CaptureResult {
            snapshot: update.snapshot,
            image_path,
            image,
            event_ids,
        }
    })
}

fn crop_capture_to_bounds(
    captured: &mut super::types::CapturedImage,
    bounds: &Bounds,
) -> Result<(), AppError> {
    let image_width = captured.image.width();
    let image_height = captured.image.height();
    let (x, y, width, height) = window_crop_rect(
        image_width,
        image_height,
        captured.frame.width,
        captured.frame.height,
        bounds,
    )
    .ok_or_else(|| {
        AppError::target_not_found("active window bounds are outside the captured display area")
    })?;
    let cropped = crop_imm(&captured.image, x, y, width, height).to_image();
    captured.image = cropped;
    captured.frame.width = width;
    captured.frame.height = height;
    if let Some(path) = captured.frame.image_path.as_ref() {
        captured
            .image
            .save_with_format(path, ImageFormat::Png)
            .map_err(|err| {
                AppError::backend_unavailable(format!(
                    "failed to write active-window capture image: {err}"
                ))
            })?;
    }
    Ok(())
}

fn window_crop_rect(
    image_width: u32,
    image_height: u32,
    logical_width: u32,
    logical_height: u32,
    bounds: &Bounds,
) -> Option<(u32, u32, u32, u32)> {
    if image_width == 0 || image_height == 0 {
        return None;
    }
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return None;
    }

    let sx = if logical_width > 0 {
        image_width as f64 / logical_width as f64
    } else {
        1.0
    };
    let sy = if logical_height > 0 {
        image_height as f64 / logical_height as f64
    } else {
        1.0
    };

    let x = (bounds.x.max(0.0) * sx).floor() as i64;
    let y = (bounds.y.max(0.0) * sy).floor() as i64;
    let width = (bounds.width * sx).ceil().max(1.0) as i64;
    let height = (bounds.height * sy).ceil().max(1.0) as i64;

    let x1 = x.clamp(0, image_width as i64);
    let y1 = y.clamp(0, image_height as i64);
    let x2 = (x1 + width).clamp(0, image_width as i64);
    let y2 = (y1 + height).clamp(0, image_height as i64);

    if x2 <= x1 || y2 <= y1 {
        return None;
    }

    Some((x1 as u32, y1 as u32, (x2 - x1) as u32, (y2 - y1) as u32))
}

pub fn latest_snapshot() -> Result<Option<SnapshotPayload>, AppError> {
    with_state(|state| state.latest_snapshot())
}

pub fn tokenize_window(window_meta: TokenizeWindowMeta) -> Result<TokenizePayload, AppError> {
    trace::log("pipeline:tokenize:window_mode");
    let capture = capture_and_update_active_window(
        None,
        window_meta.bounds.clone(),
        window_meta.app.clone(),
        false,
    )?;
    tokenize_from_snapshot(
        capture.snapshot,
        &capture.image,
        capture.image_path.as_deref(),
        Some(window_meta),
    )
}

pub fn tokenize_screenshot(
    screenshot_path: &Path,
    window_meta: Option<TokenizeWindowMeta>,
) -> Result<TokenizePayload, AppError> {
    let rgba = image::open(screenshot_path)
        .map_err(|err| {
            AppError::invalid_argument(format!(
                "failed to open screenshot {}: {err}",
                screenshot_path.display()
            ))
        })?
        .to_rgba8();
    let width = rgba.width();
    let height = rgba.height();
    let texts = recognize_text(&rgba)?;
    let snapshot = SnapshotPayload {
        snapshot_id: now_millis() as u64,
        timestamp: now_millis().to_string(),
        display: SnapshotDisplay {
            id: 1,
            width,
            height,
            scale: 1.0,
        },
        focused_app: window_meta.as_ref().and_then(|meta| meta.app.clone()),
        texts,
    };
    tokenize_from_snapshot(snapshot, &rgba, Some(screenshot_path), window_meta)
}

fn tokenize_from_snapshot(
    snapshot: SnapshotPayload,
    rgba: &image::RgbaImage,
    image_path: Option<&Path>,
    window_meta: Option<TokenizeWindowMeta>,
) -> Result<TokenizePayload, AppError> {
    let raw_tokens: Vec<TokenEntry> = snapshot
        .texts
        .iter()
        .enumerate()
        .map(|(idx, text)| TokenEntry {
            n: (idx + 1) as u32,
            text: text.text.clone(),
            bounds: text.bounds.clone(),
            confidence: text.confidence,
        })
        .collect();
    let snapshot_id = snapshot.snapshot_id;
    let timestamp = snapshot.timestamp.clone();
    let (image_meta, windows) = build_window_elements(&snapshot, rgba, image_path, window_meta)?;
    with_state(|state| state.replace_token_map(raw_tokens.clone()))?;
    trace::log(format!(
        "pipeline:tokenize:ok snapshot_id={} tokens={}",
        snapshot_id,
        raw_tokens.len()
    ));
    Ok(TokenizePayload {
        snapshot_id,
        timestamp,
        tokens: Vec::new(),
        image: Some(image_meta),
        windows,
    })
}

fn build_window_elements(
    snapshot: &SnapshotPayload,
    rgba: &image::RgbaImage,
    image_path: Option<&Path>,
    window_meta: Option<TokenizeWindowMeta>,
) -> Result<(TokenizeImage, Vec<TokenizeWindow>), AppError> {
    let width = rgba.width();
    let height = rgba.height();
    let frame = super::metal_pipeline::process_cpu(&rgba);
    let words = super::text_group::build_words_from_ocr(&snapshot.texts, &frame);
    let lines = super::text_group::group_words_into_lines(&words);
    let paragraphs = super::text_group::group_lines_into_paragraphs(&lines);
    let final_fields = super::text_group::final_text_fields(&lines, &paragraphs);
    let line_bounds: Vec<Bounds> = lines.iter().map(|line| line.bounds.clone()).collect();
    let detected_controls = super::tokenize_boxes::detect_controls(&frame, &line_bounds);

    let elements = build_unified_text_elements(&lines, &final_fields, &detected_controls);

    let title = window_meta
        .as_ref()
        .and_then(|meta| {
            if meta.title.trim().is_empty() {
                None
            } else {
                Some(meta.title.clone())
            }
        })
        .or_else(|| snapshot.focused_app.clone())
        .unwrap_or_else(|| "active_window".to_string());
    let app = window_meta
        .as_ref()
        .and_then(|meta| meta.app.clone())
        .or_else(|| snapshot.focused_app.clone());
    let os_bounds = window_meta.as_ref().map(|meta| meta.bounds.clone());
    let window = TokenizeWindow {
        id: window_meta
            .as_ref()
            .map(|meta| meta.id.clone())
            .unwrap_or_else(|| "win_0001".to_string()),
        title,
        app,
        bounds: Bounds {
            x: 0.0,
            y: 0.0,
            width: width as f64,
            height: height as f64,
        },
        os_bounds,
        elements,
    };
    let image_meta = TokenizeImage {
        path: image_path
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "<memory>".to_string()),
        width,
        height,
    };
    Ok((image_meta, vec![window]))
}

fn bounds_to_bbox(bounds: &Bounds) -> [f64; 4] {
    [bounds.x, bounds.y, bounds.width, bounds.height]
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

fn build_unified_text_elements(
    lines: &[super::text_group::TextBox],
    final_fields: &[super::text_group::TextBox],
    controls: &[super::tokenize_boxes::DetectedControl],
) -> Vec<TokenizeElement> {
    let mut used_line = vec![false; lines.len()];
    let mut elements: Vec<TokenizeElement> = Vec::new();

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
            elements.push(TokenizeElement {
                id: String::new(),
                kind: String::new(),
                bbox: bounds_to_bbox(&control.bounds),
                has_border: Some(true),
                text: Some(text.to_string()),
                confidence: None,
                source: "sat_control_v1".to_string(),
            });
        }
    }

    for field in final_fields {
        let mut overlaps_consumed = false;
        for (idx, line) in lines.iter().enumerate() {
            if !used_line[idx] {
                continue;
            }
            let area = (line.bounds.width * line.bounds.height).max(1.0);
            let ratio = overlap_area(&field.bounds, &line.bounds) / area;
            if ratio >= 0.60 || center_inside(&line.bounds, &field.bounds) {
                overlaps_consumed = true;
                break;
            }
        }
        if overlaps_consumed {
            continue;
        }
        elements.push(TokenizeElement {
            id: String::new(),
            kind: String::new(),
            bbox: bounds_to_bbox(&field.bounds),
            has_border: None,
            text: Some(field.text.clone()),
            confidence: None,
            source: "vision_ocr".to_string(),
        });
    }

    elements.sort_by(|a, b| {
        a.bbox[1]
            .partial_cmp(&b.bbox[1])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.bbox[0]
                    .partial_cmp(&b.bbox[0])
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });
    for (idx, element) in elements.iter_mut().enumerate() {
        element.id = format!("text_{:04}", idx + 1);
    }
    elements
}

pub fn token(n: u32) -> Result<Option<TokenEntry>, AppError> {
    with_state(|state| state.token(n))
}

pub fn write_tokenize_overlay(
    payload: &TokenizePayload,
    out_path: &std::path::Path,
) -> Result<(), AppError> {
    let image_meta = payload.image.as_ref().ok_or_else(|| {
        AppError::invalid_argument("token payload does not include image metadata")
    })?;
    if image_meta.path == "<memory>" {
        return Err(AppError::invalid_argument(
            "token payload image is in-memory; re-run with --screenshot or --overlay source path",
        ));
    }
    let source_path = std::path::Path::new(&image_meta.path);
    let base = image::open(source_path).map_err(|err| {
        AppError::backend_unavailable(format!(
            "failed to open tokenize source image {}: {err}",
            source_path.display()
        ))
    })?;
    let mut canvas = base.to_rgba8();

    for window in &payload.windows {
        draw_bounds_outline(&mut canvas, &window.bounds, Rgba([255, 255, 255, 255]), 2);
        let bordered: Vec<Bounds> = window
            .elements
            .iter()
            .filter(|e| e.has_border.unwrap_or(false))
            .map(|e| Bounds {
                x: e.bbox[0],
                y: e.bbox[1],
                width: e.bbox[2],
                height: e.bbox[3],
            })
            .collect();
        for element in &window.elements {
            let bounds = Bounds {
                x: element.bbox[0],
                y: element.bbox[1],
                width: element.bbox[2],
                height: element.bbox[3],
            };
            if !element.has_border.unwrap_or(false)
                && (element.kind.is_empty() || element.kind == "text" || element.text.is_some())
                && bordered
                    .iter()
                    .any(|outer| should_suppress_inner_text_overlay(&bounds, outer))
            {
                continue;
            }
            let color = if element.has_border.unwrap_or(false) {
                Rgba([255, 0, 0, 255])
            } else {
                match element.kind.as_str() {
                    "" | "text" => Rgba([0, 190, 0, 255]),
                    "box" => Rgba([40, 120, 255, 255]),
                    "glyph" => Rgba([255, 220, 0, 255]),
                    _ if element.text.is_some() => Rgba([0, 190, 0, 255]),
                    _ => Rgba([220, 220, 220, 255]),
                }
            };
            draw_bounds_outline(&mut canvas, &bounds, color, 1);
        }
    }

    if let Some(parent) = out_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to create tokenize overlay dir {}: {err}",
                parent.display()
            ))
        })?;
    }
    canvas
        .save_with_format(out_path, ImageFormat::Png)
        .map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to write tokenize overlay {}: {err}",
                out_path.display()
            ))
        })?;
    Ok(())
}

fn draw_bounds_outline(
    image: &mut image::RgbaImage,
    bounds: &Bounds,
    color: Rgba<u8>,
    thickness: u32,
) {
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

fn should_suppress_inner_text_overlay(inner: &Bounds, outer: &Bounds) -> bool {
    let inner_area = (inner.width * inner.height).max(1.0);
    let overlap = overlap_area(inner, outer);
    let center_in = center_inside(inner, outer);
    let mostly_inside = overlap / inner_area >= 0.75;
    let clearly_smaller = outer.width >= inner.width + 8.0 && outer.height >= inner.height + 8.0;
    center_in && mostly_inside && clearly_smaller
}

fn focused_app_name() -> Option<String> {
    let script =
        r#"tell application "System Events" to get name of first process whose frontmost is true"#;
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() { None } else { Some(value) }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use desktop_core::protocol::{
        Bounds, SnapshotDisplay, SnapshotPayload, SnapshotText, TokenizeElement, TokenizeImage,
        TokenizePayload, TokenizeWindow,
    };
    use image::{Rgba, RgbaImage};

    use super::{
        TokenizeWindowMeta, build_window_elements, window_crop_rect, write_tokenize_overlay,
    };

    fn golden_fixture_path(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/golden")
            .join(name)
    }

    #[test]
    fn window_crop_rect_scales_from_logical_to_pixels() {
        let bounds = Bounds {
            x: 50.0,
            y: 30.0,
            width: 200.0,
            height: 100.0,
        };
        // Image is 2x logical dimensions.
        let rect = window_crop_rect(2000, 1200, 1000, 600, &bounds).expect("rect");
        assert_eq!(rect, (100, 60, 400, 200));
    }

    #[test]
    fn window_crop_rect_clamps_to_image_edges() {
        let bounds = Bounds {
            x: 900.0,
            y: 500.0,
            width: 300.0,
            height: 200.0,
        };
        let rect = window_crop_rect(1000, 600, 1000, 600, &bounds).expect("rect");
        assert_eq!(rect, (900, 500, 100, 100));
    }

    #[test]
    fn build_window_elements_emits_bordered_text_entries() {
        let image_path = std::env::temp_dir().join(format!(
            "desktopctl-tokenize-test-{}.png",
            std::process::id()
        ));
        let mut image = RgbaImage::from_pixel(220, 140, Rgba([240, 240, 240, 255]));
        for y in 40..100 {
            for x in 40..180 {
                if x == 40 || x == 179 || y == 40 || y == 99 {
                    image.put_pixel(x, y, Rgba([60, 60, 60, 255]));
                }
            }
        }
        image.save(&image_path).expect("write test image");

        let snapshot = SnapshotPayload {
            snapshot_id: 1,
            timestamp: "t".to_string(),
            display: SnapshotDisplay {
                id: 1,
                width: 220,
                height: 140,
                scale: 1.0,
            },
            focused_app: Some("TestApp".to_string()),
            texts: vec![SnapshotText {
                text: "Hello".to_string(),
                bounds: Bounds {
                    x: 56.0,
                    y: 56.0,
                    width: 48.0,
                    height: 18.0,
                },
                confidence: 0.93,
            }],
        };

        let window_meta = TokenizeWindowMeta {
            id: "pid:7".to_string(),
            title: "Sample".to_string(),
            app: Some("TestApp".to_string()),
            bounds: Bounds {
                x: 150.0,
                y: 90.0,
                width: 220.0,
                height: 140.0,
            },
        };
        let (meta, windows) =
            build_window_elements(&snapshot, &image, Some(&image_path), Some(window_meta))
                .expect("build windows");
        assert_eq!(meta.width, 220);
        assert_eq!(meta.height, 140);
        assert_eq!(windows.len(), 1);
        assert_eq!(windows[0].id, "pid:7");
        assert_eq!(windows[0].title, "Sample");
        assert_eq!(windows[0].app.as_deref(), Some("TestApp"));
        assert!(windows[0].os_bounds.is_some());
        let elements = &windows[0].elements;
        assert!(elements.iter().any(|e| e.text.is_some()));
        assert!(
            elements
                .iter()
                .any(|e| e.has_border == Some(true) && e.text.is_some())
        );

        let _ = std::fs::remove_file(&image_path);
    }

    #[test]
    fn build_window_elements_is_deterministic_for_same_input() {
        let image_path = std::env::temp_dir().join(format!(
            "desktopctl-tokenize-determinism-{}.png",
            std::process::id()
        ));
        let mut image = RgbaImage::from_pixel(240, 150, Rgba([22, 22, 24, 255]));
        for y in 22..120 {
            for x in 32..208 {
                if x == 32 || x == 207 || y == 22 || y == 119 {
                    image.put_pixel(x, y, Rgba([228, 228, 228, 255]));
                }
            }
        }
        image.save(&image_path).expect("write test image");

        let snapshot = SnapshotPayload {
            snapshot_id: 5,
            timestamp: "t".to_string(),
            display: SnapshotDisplay {
                id: 1,
                width: 240,
                height: 150,
                scale: 1.0,
            },
            focused_app: Some("Determinism".to_string()),
            texts: vec![SnapshotText {
                text: "Allow".to_string(),
                bounds: Bounds {
                    x: 60.0,
                    y: 58.0,
                    width: 46.0,
                    height: 16.0,
                },
                confidence: 0.97,
            }],
        };
        let window_meta = TokenizeWindowMeta {
            id: "abc:1".to_string(),
            title: "Determinism".to_string(),
            app: Some("Determinism".to_string()),
            bounds: Bounds {
                x: 400.0,
                y: 200.0,
                width: 240.0,
                height: 150.0,
            },
        };
        let (_, run_a) = build_window_elements(
            &snapshot,
            &image,
            Some(&image_path),
            Some(window_meta.clone()),
        )
        .expect("run a");
        let (_, run_b) =
            build_window_elements(&snapshot, &image, Some(&image_path), Some(window_meta))
                .expect("run b");
        let a = serde_json::to_value(&run_a).expect("json a");
        let b = serde_json::to_value(&run_b).expect("json b");
        assert_eq!(a, b, "window elements must be deterministic across runs");

        let _ = std::fs::remove_file(&image_path);
    }

    #[test]
    fn write_tokenize_overlay_writes_png() {
        let source_path = std::env::temp_dir().join(format!(
            "desktopctl-tokenize-overlay-source-{}.png",
            std::process::id()
        ));
        let overlay_path = std::env::temp_dir().join(format!(
            "desktopctl-tokenize-overlay-out-{}.png",
            std::process::id()
        ));

        let mut image = RgbaImage::from_pixel(180, 120, Rgba([240, 240, 240, 255]));
        for y in 28..92 {
            for x in 24..156 {
                if x == 24 || x == 155 || y == 28 || y == 91 {
                    image.put_pixel(x, y, Rgba([30, 30, 30, 255]));
                }
            }
        }
        image.save(&source_path).expect("write source");

        let payload = TokenizePayload {
            snapshot_id: 1,
            timestamp: "1".to_string(),
            tokens: vec![],
            image: Some(TokenizeImage {
                path: source_path.display().to_string(),
                width: 180,
                height: 120,
            }),
            windows: vec![TokenizeWindow {
                id: "win_0001".to_string(),
                title: "Sample".to_string(),
                app: Some("Sample".to_string()),
                bounds: Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: 180.0,
                    height: 120.0,
                },
                os_bounds: None,
                elements: vec![
                    TokenizeElement {
                        id: "text_0001".to_string(),
                        kind: "text".to_string(),
                        bbox: [40.0, 40.0, 40.0, 16.0],
                        has_border: None,
                        text: Some("Hello".to_string()),
                        confidence: Some(0.99),
                        source: "vision_ocr".to_string(),
                    },
                    TokenizeElement {
                        id: "text_0002".to_string(),
                        kind: "text".to_string(),
                        bbox: [30.0, 34.0, 120.0, 56.0],
                        has_border: Some(true),
                        text: Some("Allow".to_string()),
                        confidence: Some(1.0),
                        source: "sat_control_v1".to_string(),
                    },
                ],
            }],
        };

        write_tokenize_overlay(&payload, &overlay_path).expect("write overlay");
        assert!(overlay_path.exists(), "overlay file should exist");
        let overlay = image::open(&overlay_path).expect("open overlay");
        assert_eq!(overlay.width(), 180);
        assert_eq!(overlay.height(), 120);

        let _ = std::fs::remove_file(&source_path);
        let _ = std::fs::remove_file(&overlay_path);
    }

    #[test]
    fn write_tokenize_overlay_suppresses_inner_text_box_when_bordered_box_exists() {
        let source_path = std::env::temp_dir().join(format!(
            "desktopctl-tokenize-overlay-suppress-source-{}.png",
            std::process::id()
        ));
        let overlay_path = std::env::temp_dir().join(format!(
            "desktopctl-tokenize-overlay-suppress-out-{}.png",
            std::process::id()
        ));
        let image = RgbaImage::from_pixel(180, 120, Rgba([240, 240, 240, 255]));
        image.save(&source_path).expect("write source");

        let payload = TokenizePayload {
            snapshot_id: 1,
            timestamp: "1".to_string(),
            tokens: vec![],
            image: Some(TokenizeImage {
                path: source_path.display().to_string(),
                width: 180,
                height: 120,
            }),
            windows: vec![TokenizeWindow {
                id: "win_0001".to_string(),
                title: "Sample".to_string(),
                app: Some("Sample".to_string()),
                bounds: Bounds {
                    x: 0.0,
                    y: 0.0,
                    width: 180.0,
                    height: 120.0,
                },
                os_bounds: None,
                elements: vec![
                    TokenizeElement {
                        id: "text_0001".to_string(),
                        kind: "text".to_string(),
                        bbox: [30.0, 34.0, 120.0, 56.0],
                        has_border: Some(true),
                        text: Some("Allow".to_string()),
                        confidence: Some(1.0),
                        source: "sat_control_v1".to_string(),
                    },
                    TokenizeElement {
                        id: "text_0002".to_string(),
                        kind: "text".to_string(),
                        bbox: [40.0, 40.0, 40.0, 16.0],
                        has_border: None,
                        text: Some("Allow".to_string()),
                        confidence: Some(1.0),
                        source: "vision_ocr".to_string(),
                    },
                ],
            }],
        };

        write_tokenize_overlay(&payload, &overlay_path).expect("write overlay");
        let overlay = image::open(&overlay_path).expect("open overlay").to_rgba8();
        // Inner OCR box top-left would be green if drawn. It should stay background.
        let px = overlay.get_pixel(40, 40);
        assert_eq!(*px, Rgba([240, 240, 240, 255]));
        // Bordered element should be red at its top-left corner.
        let red = overlay.get_pixel(30, 34);
        assert_eq!(*red, Rgba([255, 0, 0, 255]));

        let _ = std::fs::remove_file(&source_path);
        let _ = std::fs::remove_file(&overlay_path);
    }

    #[test]
    fn golden_dictionary_dark_emits_bordered_dictionary_tab() {
        let image_path = golden_fixture_path("dictionary_default_dark.png");
        let image = image::open(&image_path)
            .expect("open golden image")
            .to_rgba8();
        let width = image.width();
        let height = image.height();
        let texts =
            crate::vision::ocr::recognize_text_from_image(&image_path, width, height).expect("ocr");
        let snapshot = SnapshotPayload {
            snapshot_id: 1,
            timestamp: "1".to_string(),
            display: SnapshotDisplay {
                id: 1,
                width,
                height,
                scale: 1.0,
            },
            focused_app: Some("Dictionary".to_string()),
            texts,
        };
        let (_, windows) =
            build_window_elements(&snapshot, &image, Some(&image_path), None).expect("tokenize");
        let elements = &windows[0].elements;
        assert!(elements.iter().all(|e| e.text.is_some()));
        let tab: Vec<_> = elements
            .iter()
            .filter(|e| e.text.as_deref() == Some("Dictionary"))
            .collect();
        assert!(
            tab.iter().any(|e| e.has_border == Some(true)),
            "Dictionary tab should be bordered"
        );
    }

    #[test]
    fn golden_dictionary_light_emits_bordered_q_search() {
        let image_path = golden_fixture_path("dictionary_default_light.png");
        let image = image::open(&image_path)
            .expect("open golden image")
            .to_rgba8();
        let width = image.width();
        let height = image.height();
        let texts =
            crate::vision::ocr::recognize_text_from_image(&image_path, width, height).expect("ocr");
        let snapshot = SnapshotPayload {
            snapshot_id: 1,
            timestamp: "1".to_string(),
            display: SnapshotDisplay {
                id: 1,
                width,
                height,
                scale: 1.0,
            },
            focused_app: Some("Dictionary".to_string()),
            texts,
        };
        let (_, windows) =
            build_window_elements(&snapshot, &image, Some(&image_path), None).expect("tokenize");
        let elements = &windows[0].elements;
        let search = elements
            .iter()
            .find(|e| e.text.as_deref() == Some("Q Search"))
            .expect("Q Search element");
        assert_eq!(search.has_border, Some(true), "Q Search should be bordered");
    }
}

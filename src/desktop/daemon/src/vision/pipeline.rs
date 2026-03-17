use std::{path::PathBuf, process::Command as ProcessCommand};

use desktop_core::{
    error::AppError,
    protocol::{
        Bounds, SnapshotPayload, TokenEntry, TokenizeElement, TokenizeImage, TokenizePayload,
        TokenizeWindow,
    },
};
use image::{ImageFormat, imageops::crop_imm};

use crate::trace;

use super::{
    capture::capture_screen_png,
    diff::{diff_region, thumbnail_from_png, upscale_region},
    ocr::recognize_text_from_image,
    state::with_state,
};

#[derive(Debug, Clone)]
pub struct CaptureResult {
    pub snapshot: SnapshotPayload,
    pub image_path: PathBuf,
    pub event_ids: Vec<u64>,
}

pub fn capture_and_update(out_path: Option<PathBuf>) -> Result<CaptureResult, AppError> {
    capture_and_update_internal(out_path, None)
}

pub fn capture_and_update_active_window(
    out_path: Option<PathBuf>,
    bounds: Bounds,
) -> Result<CaptureResult, AppError> {
    capture_and_update_internal(out_path, Some(bounds))
}

fn capture_and_update_internal(
    out_path: Option<PathBuf>,
    crop_bounds: Option<Bounds>,
) -> Result<CaptureResult, AppError> {
    trace::log("pipeline:capture_and_update:start");
    let mut capture = capture_screen_png(out_path)?;
    if let Some(bounds) = crop_bounds.as_ref() {
        crop_capture_to_bounds(&mut capture, bounds)?;
        trace::log(format!(
            "pipeline:capture_and_update:active_window_crop_ok size={}x{}",
            capture.width, capture.height
        ));
    }
    trace::log(format!(
        "pipeline:capture_and_update:capture_ok path={} size={}x{}",
        capture.image_path.display(),
        capture.width,
        capture.height
    ));
    let thumb = thumbnail_from_png(&capture.image_path, 96, 54)?;
    trace::log("pipeline:capture_and_update:thumb_ok");
    let texts = recognize_text_from_image(&capture.image_path, capture.width, capture.height)?;
    trace::log(format!(
        "pipeline:capture_and_update:ocr_ok texts={}",
        texts.len()
    ));
    let focused_app = focused_app_name();
    let image_path = capture.image_path.clone();

    with_state(move |state| {
        let roi = state
            .latest_thumbnail()
            .and_then(|prev| diff_region(prev, &thumb, 8))
            .map(|region| {
                upscale_region(
                    region,
                    capture.width,
                    capture.height,
                    thumb.width,
                    thumb.height,
                )
            });

        let update = state.record_capture(capture, thumb, focused_app, texts, roi);
        trace::log(format!(
            "pipeline:capture_and_update:recorded snapshot_id={} event_id={}",
            update.snapshot.snapshot_id, update.event_id
        ));
        let event_ids = state.event_ids(update.snapshot.snapshot_id);

        CaptureResult {
            snapshot: update.snapshot,
            image_path,
            event_ids,
        }
    })
}

fn crop_capture_to_bounds(
    capture: &mut super::types::CapturedFrame,
    bounds: &Bounds,
) -> Result<(), AppError> {
    let image = image::open(&capture.image_path).map_err(|err| {
        AppError::backend_unavailable(format!(
            "failed to open capture image for active-window crop: {err}"
        ))
    })?;
    let rgba = image.to_rgba8();
    let image_width = rgba.width();
    let image_height = rgba.height();
    let (x, y, width, height) = window_crop_rect(
        image_width,
        image_height,
        capture.width,
        capture.height,
        bounds,
    )
    .ok_or_else(|| {
        AppError::target_not_found("active window bounds are outside the captured display area")
    })?;
    let cropped = crop_imm(&rgba, x, y, width, height).to_image();
    cropped
        .save_with_format(&capture.image_path, ImageFormat::Png)
        .map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to write active-window capture image: {err}"
            ))
        })?;
    capture.width = width;
    capture.height = height;
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

pub fn tokenize() -> Result<TokenizePayload, AppError> {
    trace::log("pipeline:tokenize:start");
    let capture = capture_and_update(None)?;
    let tokens: Vec<TokenEntry> = capture
        .snapshot
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
    let snapshot_id = capture.snapshot.snapshot_id;
    let timestamp = capture.snapshot.timestamp.clone();
    let (image_meta, windows) = build_window_elements(&capture.snapshot, &capture.image_path)?;
    with_state(|state| state.replace_token_map(tokens.clone()))?;
    trace::log(format!(
        "pipeline:tokenize:ok snapshot_id={} tokens={}",
        snapshot_id,
        tokens.len()
    ));
    Ok(TokenizePayload {
        snapshot_id,
        timestamp,
        tokens,
        image: Some(image_meta),
        windows,
    })
}

fn build_window_elements(
    snapshot: &SnapshotPayload,
    image_path: &std::path::Path,
) -> Result<(TokenizeImage, Vec<TokenizeWindow>), AppError> {
    let image = image::open(image_path).map_err(|err| {
        AppError::backend_unavailable(format!(
            "failed to load capture image for tokenize boxes: {err}"
        ))
    })?;
    let rgba = image.to_rgba8();
    let width = rgba.width();
    let height = rgba.height();
    let box_bounds = super::tokenize_boxes::detect_ui_boxes(&rgba);

    let mut elements = Vec::new();
    for (idx, text) in snapshot.texts.iter().enumerate() {
        elements.push(TokenizeElement {
            id: format!("text_{:04}", idx + 1),
            kind: "text".to_string(),
            bbox: bounds_to_bbox(&text.bounds),
            text: Some(text.text.clone()),
            confidence: Some(text.confidence),
            source: "vision_ocr".to_string(),
        });
    }
    for (idx, bounds) in box_bounds.iter().enumerate() {
        elements.push(TokenizeElement {
            id: format!("box_{:04}", idx + 1),
            kind: "box".to_string(),
            bbox: bounds_to_bbox(bounds),
            text: None,
            confidence: None,
            source: "rust_edge_grid_v1".to_string(),
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

    let window = TokenizeWindow {
        id: "win_0001".to_string(),
        title: snapshot
            .focused_app
            .clone()
            .unwrap_or_else(|| "active_window".to_string()),
        bounds: Bounds {
            x: 0.0,
            y: 0.0,
            width: width as f64,
            height: height as f64,
        },
        elements,
    };
    let image_meta = TokenizeImage {
        path: image_path.display().to_string(),
        width,
        height,
    };
    Ok((image_meta, vec![window]))
}

fn bounds_to_bbox(bounds: &Bounds) -> [f64; 4] {
    [bounds.x, bounds.y, bounds.width, bounds.height]
}

pub fn token(n: u32) -> Result<Option<TokenEntry>, AppError> {
    with_state(|state| state.token(n))
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
    use desktop_core::protocol::{Bounds, SnapshotDisplay, SnapshotPayload, SnapshotText};
    use image::{Rgba, RgbaImage};

    use super::{build_window_elements, window_crop_rect};

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
    fn build_window_elements_emits_text_and_box_entries() {
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

        let (meta, windows) = build_window_elements(&snapshot, &image_path).expect("build windows");
        assert_eq!(meta.width, 220);
        assert_eq!(meta.height, 140);
        assert_eq!(windows.len(), 1);
        let elements = &windows[0].elements;
        assert!(elements.iter().any(|e| e.kind == "text"));
        assert!(elements.iter().any(|e| e.kind == "box"));

        let _ = std::fs::remove_file(&image_path);
    }
}

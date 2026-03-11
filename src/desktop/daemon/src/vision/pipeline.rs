use std::{path::PathBuf, process::Command as ProcessCommand};

use desktop_core::{error::AppError, protocol::SnapshotPayload};

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
    let capture = capture_screen_png(out_path)?;
    let thumb = thumbnail_from_png(&capture.image_path, 96, 54)?;
    let texts = recognize_text_from_image(&capture.image_path, capture.width, capture.height)?;
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
        let event_ids = state.event_ids(update.snapshot.snapshot_id);

        CaptureResult {
            snapshot: update.snapshot,
            image_path,
            event_ids,
        }
    })
}

pub fn latest_snapshot() -> Result<Option<SnapshotPayload>, AppError> {
    with_state(|state| state.latest_snapshot())
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

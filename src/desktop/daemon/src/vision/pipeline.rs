use std::{path::PathBuf, process::Command as ProcessCommand};

use desktop_core::{
    error::AppError,
    protocol::{SnapshotPayload, TokenEntry, TokenizePayload},
};

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
    trace::log("pipeline:capture_and_update:start");
    let capture = capture_screen_png(out_path)?;
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
    })
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

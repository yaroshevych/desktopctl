use std::{fs, path::PathBuf};

use desktop_core::error::AppError;
use serde_json::json;

use super::{pipeline, state::with_state};

pub fn write_debug_snapshot() -> Result<serde_json::Value, AppError> {
    if pipeline::latest_snapshot()?.is_none() {
        let _ = pipeline::capture_and_update(Some(super::capture::default_capture_path()))?;
    }

    let (snapshot, frame_path, tokens) = with_state(|state| {
        let snapshot = state.latest_snapshot();
        let frame_path = state.latest_frame_path();
        let mut tokens: Vec<_> = state.token_map().values().cloned().collect();
        tokens.sort_by_key(|token| token.n);
        (snapshot, frame_path, tokens)
    })?;

    let snapshot =
        snapshot.ok_or_else(|| AppError::internal("no snapshot available for debug output"))?;
    let frame_path = if let Some(path) = frame_path {
        path
    } else {
        let capture = pipeline::capture_and_update(Some(super::capture::default_capture_path()))?;
        capture
            .image_path
            .ok_or_else(|| AppError::internal("no captured frame available for debug output"))?
    };

    let base_dir = PathBuf::from("/tmp/desktopctl-debug");
    fs::create_dir_all(&base_dir).map_err(|err| {
        AppError::backend_unavailable(format!("failed to create debug dir: {err}"))
    })?;
    let png_path = base_dir.join(format!("snapshot-{}.png", snapshot.snapshot_id));
    let json_path = base_dir.join(format!("snapshot-{}.json", snapshot.snapshot_id));

    fs::copy(&frame_path, &png_path).map_err(|err| {
        AppError::backend_unavailable(format!(
            "failed to copy debug frame {}: {err}",
            frame_path.display()
        ))
    })?;
    let payload = json!({
        "snapshot": snapshot,
        "tokens": tokens
    });
    let encoded = serde_json::to_vec_pretty(&payload)
        .map_err(|err| AppError::internal(format!("failed to encode debug payload: {err}")))?;
    fs::write(&json_path, encoded).map_err(|err| {
        AppError::backend_unavailable(format!("failed to write debug json: {err}"))
    })?;

    Ok(json!({
        "snapshot_id": payload["snapshot"]["snapshot_id"],
        "frame_path": png_path,
        "json_path": json_path
    }))
}

use desktop_core::{
    error::AppError,
    protocol::{PermissionState, PermissionsPayload},
};
use serde_json::{Value, json};

use crate::{clipboard, permissions, recording, replay, request_store, vision};

pub(crate) fn clipboard_read() -> Result<Value, AppError> {
    let text = clipboard::read_clipboard()?;
    Ok(json!({ "text": text }))
}

pub(crate) fn clipboard_write(text: String) -> Result<Value, AppError> {
    clipboard::write_clipboard(&text)?;
    Ok(json!({ "written": true }))
}

pub(crate) fn permissions_check() -> Result<Value, AppError> {
    let payload = PermissionsPayload {
        accessibility: PermissionState {
            granted: permissions::accessibility_granted(),
            remediation: (!permissions::accessibility_granted())
                .then(|| permissions::accessibility_remediation().to_string()),
        },
        screen_recording: PermissionState {
            granted: permissions::screen_recording_granted(),
            remediation: (!permissions::screen_recording_granted())
                .then(|| permissions::screen_recording_remediation().to_string()),
        },
    };
    serde_json::to_value(payload)
        .map_err(|err| AppError::internal(format!("failed to encode permissions payload: {err}")))
}

pub(crate) fn debug_snapshot() -> Result<Value, AppError> {
    vision::debug::write_debug_snapshot()
}

pub(crate) fn request_show(request_id: String) -> Result<Value, AppError> {
    request_store::show(&request_id)
}

pub(crate) fn request_list(limit: Option<u64>) -> Result<Value, AppError> {
    request_store::list(limit)
}

pub(crate) fn request_screenshot(
    request_id: String,
    out_path: Option<String>,
) -> Result<Value, AppError> {
    request_store::screenshot(&request_id, out_path)
}

pub(crate) fn request_response(request_id: String) -> Result<Value, AppError> {
    request_store::response(&request_id)
}

pub(crate) fn request_search(
    text: String,
    limit: Option<u64>,
    command: Option<String>,
) -> Result<Value, AppError> {
    request_store::search(&text, limit, command.as_deref())
}

pub(crate) fn replay_record(duration_ms: u64, stop: bool) -> Result<Value, AppError> {
    if stop {
        recording::stop_recording()
    } else {
        recording::start_recording(duration_ms)
    }
}

pub(crate) fn replay_load(session_dir: String) -> Result<Value, AppError> {
    let session_dir = replay::parse_session_dir(&session_dir)?;
    replay::load_session(&session_dir)
}

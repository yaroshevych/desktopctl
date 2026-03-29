use std::{
    collections::VecDeque,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use desktop_core::{
    error::AppError,
    protocol::{RequestEnvelope, ResponseEnvelope, now_millis},
};
use serde_json::{Value, json};

use crate::vision::pipeline;

const MAX_REQUEST_ENTRIES: usize = 10;
const MAX_REQUEST_AGE_MS: u128 = 10 * 60 * 1000;

#[derive(Clone)]
struct RequestEntry {
    request_id: String,
    command: String,
    timestamp: String,
    timestamp_ms: u128,
    response: Value,
    screenshot_png: Option<Vec<u8>>,
}

#[derive(Default)]
struct RequestStore {
    entries: VecDeque<RequestEntry>,
}

static STORE: OnceLock<Mutex<RequestStore>> = OnceLock::new();

pub fn record(request: &RequestEnvelope, response: &ResponseEnvelope) -> Result<(), AppError> {
    let now_ms = now_millis();
    let response_value = serde_json::to_value(response)
        .map_err(|err| AppError::internal(format!("failed to serialize response: {err}")))?;
    let screenshot_png = extract_screenshot_png(response);
    let entry = RequestEntry {
        request_id: request.request_id.clone(),
        command: request.command.name().to_string(),
        timestamp: now_ms.to_string(),
        timestamp_ms: now_ms,
        response: response_value,
        screenshot_png,
    };

    let mut store = request_store()
        .lock()
        .map_err(|_| AppError::internal("request store lock poisoned"))?;
    prune_expired(&mut store, now_ms);
    store.entries.push_back(entry);
    while store.entries.len() > MAX_REQUEST_ENTRIES {
        let _ = store.entries.pop_front();
    }
    Ok(())
}

pub fn show(request_id: &str) -> Result<Value, AppError> {
    let store = request_store()
        .lock()
        .map_err(|_| AppError::internal("request store lock poisoned"))?;
    let now_ms = now_millis();
    let Some(entry) = store
        .entries
        .iter()
        .find(|e| e.request_id == request_id && !is_expired(e, now_ms))
    else {
        return Err(AppError::target_not_found(format!(
            "request \"{request_id}\" not found in artifact buffer"
        )));
    };

    let ok = entry
        .response
        .get("ok")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    Ok(json!({
        "request_id": entry.request_id,
        "command": entry.command,
        "timestamp": entry.timestamp,
        "ok": ok,
        "has_screenshot": entry.screenshot_png.is_some()
    }))
}

pub fn list(limit: Option<u64>) -> Result<Value, AppError> {
    let mut store = request_store()
        .lock()
        .map_err(|_| AppError::internal("request store lock poisoned"))?;
    let now_ms = now_millis();
    prune_expired(&mut store, now_ms);
    let max_entries = limit.unwrap_or(MAX_REQUEST_ENTRIES as u64).max(1) as usize;
    let entries: Vec<Value> = store
        .entries
        .iter()
        .rev()
        .take(max_entries)
        .map(|entry| {
            let ok = entry
                .response
                .get("ok")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            json!({
                "request_id": entry.request_id,
                "command": entry.command,
                "timestamp": entry.timestamp,
                "ok": ok,
                "has_screenshot": entry.screenshot_png.is_some()
            })
        })
        .collect();
    Ok(json!({
        "entries": entries
    }))
}

pub fn response(request_id: &str) -> Result<Value, AppError> {
    let store = request_store()
        .lock()
        .map_err(|_| AppError::internal("request store lock poisoned"))?;
    let now_ms = now_millis();
    let Some(entry) = store
        .entries
        .iter()
        .find(|e| e.request_id == request_id && !is_expired(e, now_ms))
    else {
        return Err(AppError::target_not_found(format!(
            "request \"{request_id}\" not found in artifact buffer"
        )));
    };
    Ok(entry.response.clone())
}

pub fn screenshot(request_id: &str, out_path: Option<String>) -> Result<Value, AppError> {
    let store = request_store()
        .lock()
        .map_err(|_| AppError::internal("request store lock poisoned"))?;
    let now_ms = now_millis();
    let Some(entry) = store
        .entries
        .iter()
        .find(|e| e.request_id == request_id && !is_expired(e, now_ms))
    else {
        return Err(AppError::target_not_found(format!(
            "request \"{request_id}\" not found in artifact buffer"
        )));
    };
    let png = entry.screenshot_png.as_ref().ok_or_else(|| {
        AppError::target_not_found(format!(
            "request \"{request_id}\" has no screenshot artifact"
        ))
    })?;

    let path = out_path
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/desktopctl-request-{request_id}.png")));
    fs::write(&path, png).map_err(|err| {
        AppError::backend_unavailable(format!("failed to write screenshot: {err}"))
    })?;
    Ok(json!({
        "request_id": request_id,
        "path": path,
        "bytes": png.len()
    }))
}

fn request_store() -> &'static Mutex<RequestStore> {
    STORE.get_or_init(|| Mutex::new(RequestStore::default()))
}

fn is_expired(entry: &RequestEntry, now_ms: u128) -> bool {
    now_ms.saturating_sub(entry.timestamp_ms) > MAX_REQUEST_AGE_MS
}

fn prune_expired(store: &mut RequestStore, now_ms: u128) {
    while let Some(front) = store.entries.front() {
        if !is_expired(front, now_ms) {
            break;
        }
        let _ = store.entries.pop_front();
    }
}

fn extract_screenshot_png(response: &ResponseEnvelope) -> Option<Vec<u8>> {
    let ResponseEnvelope::Success(success) = response else {
        return None;
    };
    let result = &success.result;
    let mut candidates: Vec<String> = Vec::new();
    if let Some(path) = result.get("path").and_then(|v| v.as_str()) {
        candidates.push(path.to_string());
    }
    if let Some(path) = result
        .get("image")
        .and_then(|v| v.get("path"))
        .and_then(|v| v.as_str())
    {
        candidates.push(path.to_string());
    }

    for candidate in candidates {
        if candidate == "<memory>" {
            if let Ok(Some(bytes)) = pipeline::latest_frame_png() {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
            continue;
        }
        let p = Path::new(&candidate);
        if p.exists() {
            if let Ok(bytes) = fs::read(p) {
                if !bytes.is_empty() {
                    return Some(bytes);
                }
            }
        }
    }
    if let Ok(Some(bytes)) = pipeline::latest_frame_png() {
        if !bytes.is_empty() {
            return Some(bytes);
        }
    }
    None
}

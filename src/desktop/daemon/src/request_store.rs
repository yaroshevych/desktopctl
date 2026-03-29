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

pub fn search(
    text: &str,
    limit: Option<u64>,
    command_filter: Option<&str>,
) -> Result<Value, AppError> {
    let query = text.trim();
    if query.is_empty() {
        return Err(AppError::invalid_argument(
            "request search query must not be empty",
        ));
    }

    let mut store = request_store()
        .lock()
        .map_err(|_| AppError::internal("request store lock poisoned"))?;
    let now_ms = now_millis();
    prune_expired(&mut store, now_ms);

    let command_filter_norm = command_filter
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|v| v.to_ascii_lowercase());
    let scan_limit = limit.unwrap_or(MAX_REQUEST_ENTRIES as u64).max(1) as usize;
    let mut scanned = 0usize;
    let mut results: Vec<Value> = Vec::new();

    for entry in store.entries.iter().rev() {
        if let Some(filter) = command_filter_norm.as_deref() {
            if !entry.command.eq_ignore_ascii_case(filter) {
                continue;
            }
        }
        if scanned >= scan_limit {
            break;
        }
        scanned += 1;
        let Some(result) = entry.response.get("result").and_then(Value::as_object) else {
            continue;
        };
        let Some(windows) = result.get("windows").and_then(Value::as_array) else {
            continue;
        };
        let mut snapshot = result.clone();
        let mut snapshot_windows: Vec<Value> = Vec::new();
        for window in windows {
            let Some(window_obj) = window.as_object() else {
                continue;
            };
            let Some(elements) = window_obj.get("elements").and_then(Value::as_array) else {
                continue;
            };
            let mut matched: Vec<Value> = Vec::new();
            for element in elements {
                if element_matches_query(element, query) {
                    matched.push(element.clone());
                }
            }
            if matched.is_empty() {
                continue;
            }
            let mut out_window = window_obj.clone();
            out_window.insert("elements".to_string(), Value::Array(matched));
            snapshot_windows.push(Value::Object(out_window));
        }
        if snapshot_windows.is_empty() {
            continue;
        }
        snapshot.insert("windows".to_string(), Value::Array(snapshot_windows));
        snapshot.insert(
            "request_id".to_string(),
            Value::String(entry.request_id.clone()),
        );
        results.push(Value::Object(snapshot));
    }

    Ok(json!({
        "results": results
    }))
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

fn element_matches_query(element: &Value, query: &str) -> bool {
    let normalized_query = normalize_for_match(query);
    if normalized_query.is_empty() {
        return false;
    }
    let query_terms: Vec<&str> = normalized_query.split_whitespace().collect();
    if query_terms.is_empty() {
        return false;
    }

    let mut haystack_terms: Vec<String> = Vec::new();
    for value in [
        element.get("id").and_then(Value::as_str),
        element.get("text").and_then(Value::as_str),
        element.get("source").and_then(Value::as_str),
        element.get("type").and_then(Value::as_str),
    ]
    .into_iter()
    .flatten()
    {
        let norm = normalize_for_match(value);
        if !norm.is_empty() {
            haystack_terms.extend(norm.split_whitespace().map(str::to_string));
        }
    }
    haystack_terms.extend(element_kind_aliases(element));
    haystack_terms.sort();
    haystack_terms.dedup();

    query_terms.iter().all(|term| {
        haystack_terms
            .iter()
            .any(|candidate| fuzzy_term_match(term, candidate))
    })
}

fn element_kind_aliases(element: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let source = element
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let kind = element
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let id = element
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();

    let has =
        |needle: &str| source.contains(needle) || kind.contains(needle) || id.contains(needle);

    if has("axbutton") || has("button") || has("btn") || has("menubutton") || has("popupbutton") {
        out.extend(["button".to_string(), "btn".to_string()]);
    }
    if has("textfield") || has("textarea") || has("input") {
        out.extend([
            "input".to_string(),
            "field".to_string(),
            "text".to_string(),
            "textbox".to_string(),
        ]);
    }
    if has("checkbox") {
        out.push("checkbox".to_string());
    }
    if has("radiobutton") {
        out.push("radio".to_string());
    }
    if has("scrollarea") || has("scrollbar") {
        out.extend(["scroll".to_string(), "scrollable".to_string()]);
    }
    if has("menu") {
        out.push("menu".to_string());
    }
    if has("tab") {
        out.push("tab".to_string());
    }
    if out.is_empty() {
        out.push("element".to_string());
    }
    out
}

fn fuzzy_term_match(query: &str, candidate: &str) -> bool {
    if query.is_empty() || candidate.is_empty() {
        return false;
    }
    if candidate.contains(query) {
        return true;
    }
    if query.contains(candidate) && candidate.len() >= 4 {
        return true;
    }
    let max_dist = if query.len() <= 4 { 1 } else { 2 };
    within_edit_distance(query, candidate, max_dist)
}

fn within_edit_distance(a: &str, b: &str, max_dist: usize) -> bool {
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a_bytes.is_empty() || b_bytes.is_empty() {
        return a_bytes.len().abs_diff(b_bytes.len()) <= max_dist;
    }
    if a_bytes.len().abs_diff(b_bytes.len()) > max_dist {
        return false;
    }

    let mut prev: Vec<usize> = (0..=b_bytes.len()).collect();
    let mut curr = vec![0usize; b_bytes.len() + 1];
    for (i, &ca) in a_bytes.iter().enumerate() {
        curr[0] = i + 1;
        let mut row_min = curr[0];
        for (j, &cb) in b_bytes.iter().enumerate() {
            let cost = usize::from(ca != cb);
            let del = prev[j + 1] + 1;
            let ins = curr[j] + 1;
            let sub = prev[j] + cost;
            let cell = del.min(ins).min(sub);
            curr[j + 1] = cell;
            row_min = row_min.min(cell);
        }
        if row_min > max_dist {
            return false;
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b_bytes.len()] <= max_dist
}

fn normalize_for_match(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_space = false;
    for ch in input.chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            last_space = false;
        } else if !last_space {
            out.push(' ');
            last_space = true;
        }
    }
    out.trim().to_string()
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

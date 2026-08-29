use desktop_core::{error::AppError, protocol::now_millis};
use serde_json::{Value, json};

use super::{MAX_REQUEST_ENTRIES, service};

pub(super) fn search(
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

    let mut store = service::request_store()
        .lock()
        .map_err(|_| AppError::internal("request store lock poisoned"))?;
    let now_ms = now_millis();
    service::prune_expired(&mut store, now_ms);

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

    Ok(json!({ "results": results }))
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

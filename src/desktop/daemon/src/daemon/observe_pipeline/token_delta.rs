use super::*;

pub(super) fn diff_observe_tokens(before: &[Value], after: &[Value]) -> Value {
    use std::collections::{HashMap, HashSet};
    let mut before_map: HashMap<String, &Value> = HashMap::new();
    let mut after_map: HashMap<String, &Value> = HashMap::new();
    for token in before {
        before_map.insert(observe_token_key(token), token);
    }
    for token in after {
        after_map.insert(observe_token_key(token), token);
    }

    let mut added: Vec<Value> = Vec::new();
    let mut removed: Vec<Value> = Vec::new();
    let mut changed: Vec<Value> = Vec::new();
    let before_keys: HashSet<String> = before_map.keys().cloned().collect();
    let after_keys: HashSet<String> = after_map.keys().cloned().collect();

    for key in after_keys.difference(&before_keys) {
        if let Some(token) = after_map.get(key) {
            added.push((*token).clone());
        }
    }
    for key in before_keys.difference(&after_keys) {
        if let Some(token) = before_map.get(key) {
            removed.push((*token).clone());
        }
    }
    for key in before_keys.intersection(&after_keys) {
        let Some(before_token) = before_map.get(key) else {
            continue;
        };
        let Some(after_token) = after_map.get(key) else {
            continue;
        };
        if !observe_token_semantic_equal(before_token, after_token) {
            changed.push(json!({
                "before": (*before_token).clone(),
                "after": (*after_token).clone()
            }));
        }
    }

    json!({
        "added": added,
        "removed": removed,
        "changed": changed
    })
}

pub(super) fn normalize_observe_regions(
    regions: &[desktop_core::protocol::Bounds],
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Vec<Value> {
    regions
        .iter()
        .map(|bounds| relative_bounds_json(bounds, origin))
        .collect()
}

pub(super) fn normalize_observe_tokens_delta(
    mut delta: Value,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Value {
    for key in ["added", "removed"] {
        if let Some(items) = delta.get_mut(key).and_then(Value::as_array_mut) {
            for token in items {
                ensure_observe_token_id(token);
                rewrite_token_bbox_relative(token, origin);
            }
        }
    }
    if let Some(items) = delta.get_mut("changed").and_then(Value::as_array_mut) {
        for entry in items {
            if let Some(before) = entry.get_mut("before") {
                ensure_observe_token_id(before);
                rewrite_token_bbox_relative(before, origin);
            }
            if let Some(after) = entry.get_mut("after") {
                ensure_observe_token_id(after);
                rewrite_token_bbox_relative(after, origin);
            }
        }
    }
    remap_observe_ocr_ids_to_tokenize_ids(&mut delta, origin);
    reconcile_added_removed_pairs(&mut delta);
    dedupe_observe_tokens_delta(&mut delta);
    sort_observe_tokens_delta(&mut delta);
    delta
}

#[derive(Debug, Clone)]
pub(super) struct OcrIdCandidate {
    pub(super) id: String,
    pub(super) text_norm: String,
    pub(super) bounds: desktop_core::protocol::Bounds,
}

fn remap_observe_ocr_ids_to_tokenize_ids(
    delta: &mut Value,
    origin: Option<&desktop_core::protocol::Bounds>,
) {
    let Some(window_bounds) = origin.cloned() else {
        return;
    };
    let app = window_target::frontmost_app_name();
    let window_meta = vision::pipeline::TokenizeWindowMeta {
        id: "frontmost:1".to_string(),
        title: app.clone().unwrap_or_else(|| "active_window".to_string()),
        app,
        bounds: window_bounds,
    };
    let payload = match vision::pipeline::tokenize_window(window_meta) {
        Ok(payload) => payload,
        Err(err) => {
            trace::log(format!(
                "observe:id_remap:tokenize_window_warn {}",
                err.message
            ));
            return;
        }
    };
    let candidates = collect_ocr_id_candidates(&payload);
    if candidates.is_empty() {
        return;
    }
    if let Some(items) = delta.get_mut("added").and_then(Value::as_array_mut) {
        for token in items {
            remap_single_observe_ocr_id(token, &candidates);
        }
    }
    if let Some(items) = delta.get_mut("changed").and_then(Value::as_array_mut) {
        for entry in items {
            if let Some(before) = entry.get_mut("before") {
                remap_single_observe_ocr_id(before, &candidates);
            }
            if let Some(after) = entry.get_mut("after") {
                remap_single_observe_ocr_id(after, &candidates);
            }
        }
    }
}

fn collect_ocr_id_candidates(
    payload: &desktop_core::protocol::TokenizePayload,
) -> Vec<OcrIdCandidate> {
    let mut out = Vec::new();
    for window in &payload.windows {
        for element in &window.elements {
            if element.source != "vision_ocr" {
                continue;
            }
            let id = element.id.trim();
            if id.is_empty() {
                continue;
            }
            let text = element.text.as_deref().unwrap_or("").trim();
            if text.is_empty() {
                continue;
            }
            out.push(OcrIdCandidate {
                id: id.to_string(),
                text_norm: normalize_observe_text(text),
                bounds: desktop_core::protocol::Bounds {
                    x: element.bbox[0],
                    y: element.bbox[1],
                    width: element.bbox[2],
                    height: element.bbox[3],
                },
            });
        }
    }
    out
}

pub(super) fn remap_single_observe_ocr_id(token: &mut Value, candidates: &[OcrIdCandidate]) {
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if source != "vision_ocr" {
        return;
    }
    let text = token
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim();
    if text.is_empty() {
        return;
    }
    let Some(bounds) = token_bbox_bounds(token) else {
        return;
    };
    let text_norm = normalize_observe_text(text);
    let mut best_idx: Option<usize> = None;
    let mut best_score = -1.0_f64;
    for (idx, candidate) in candidates.iter().enumerate() {
        if candidate.text_norm != text_norm {
            continue;
        }
        let score = iou(&candidate.bounds, &bounds);
        if score > best_score {
            best_score = score;
            best_idx = Some(idx);
        }
    }
    let Some(idx) = best_idx else {
        return;
    };
    if best_score < 0.10 {
        return;
    }
    if let Some(obj) = token.as_object_mut() {
        obj.insert("id".to_string(), Value::String(candidates[idx].id.clone()));
    }
}

fn reconcile_added_removed_pairs(delta: &mut Value) {
    use std::collections::{HashMap, VecDeque};

    let added = delta
        .get("added")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let removed = delta
        .get("removed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let changed = delta
        .get("changed")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut removed_by_id: HashMap<String, VecDeque<Value>> = HashMap::new();
    let mut removed_unkeyed: Vec<Value> = Vec::new();
    for token in removed {
        if let Some(id) = token
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            removed_by_id
                .entry(id.to_string())
                .or_default()
                .push_back(token);
        } else {
            removed_unkeyed.push(token);
        }
    }

    let mut added_out: Vec<Value> = Vec::new();
    let mut removed_retain: Vec<Value> = Vec::new();
    for token in added {
        let Some(id) = token
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        else {
            added_out.push(token);
            continue;
        };

        let Some(queue) = removed_by_id.get_mut(id) else {
            added_out.push(token);
            continue;
        };
        let Some(before) = queue.pop_front() else {
            added_out.push(token);
            continue;
        };

        if observe_token_semantic_equal(&before, &token) {
            continue;
        }
        // Same ID but different semantic content: preserve both sides so deletions stay visible.
        removed_retain.push(before);
        added_out.push(token);
    }

    let mut removed_out: Vec<Value> = removed_unkeyed;
    removed_out.extend(removed_retain);
    for queue in removed_by_id.into_values() {
        removed_out.extend(queue);
    }

    if let Some(obj) = delta.as_object_mut() {
        obj.insert("added".to_string(), Value::Array(added_out));
        obj.insert("removed".to_string(), Value::Array(removed_out));
        obj.insert("changed".to_string(), Value::Array(changed));
    }
}

fn dedupe_observe_tokens_delta(delta: &mut Value) {
    if let Some(items) = delta.get_mut("added").and_then(Value::as_array_mut) {
        dedupe_tokens_in_place(items);
    }
    if let Some(items) = delta.get_mut("removed").and_then(Value::as_array_mut) {
        dedupe_tokens_in_place(items);
    }
    if let Some(items) = delta.get_mut("changed").and_then(Value::as_array_mut) {
        dedupe_changed_in_place(items);
    }
}

fn dedupe_tokens_in_place(items: &mut Vec<Value>) {
    use std::collections::HashMap;
    let mut best_by_key: HashMap<String, Value> = HashMap::new();
    for token in items.drain(..) {
        let key = token_dedupe_key(&token);
        if let Some(existing) = best_by_key.get_mut(&key) {
            if token_confidence(&token) > token_confidence(existing) {
                *existing = token;
            }
        } else {
            best_by_key.insert(key, token);
        }
    }
    items.extend(best_by_key.into_values());
}

fn dedupe_changed_in_place(items: &mut Vec<Value>) {
    use std::collections::HashMap;
    let mut best_by_key: HashMap<String, Value> = HashMap::new();
    for entry in items.drain(..) {
        let key = changed_dedupe_key(&entry);
        if let Some(existing) = best_by_key.get_mut(&key) {
            let existing_score = entry_confidence(existing);
            let next_score = entry_confidence(&entry);
            if next_score > existing_score {
                *existing = entry;
            }
        } else {
            best_by_key.insert(key, entry);
        }
    }
    items.extend(best_by_key.into_values());
}

fn token_dedupe_key(token: &Value) -> String {
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .trim()
        .to_string();
    let text = token
        .get("text")
        .and_then(Value::as_str)
        .map(normalize_observe_text)
        .unwrap_or_default();
    let bbox_key = quantized_bbox_key_with_step(token.get("bbox").and_then(Value::as_array), 16.0);
    let checked = token
        .get("checked")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string());
    format!("{source}|{text}|{bbox_key}|{checked}")
}

fn changed_dedupe_key(entry: &Value) -> String {
    let before = entry
        .get("before")
        .map(token_dedupe_key)
        .unwrap_or_else(|| "before:missing".to_string());
    let after = entry
        .get("after")
        .map(token_dedupe_key)
        .unwrap_or_else(|| "after:missing".to_string());
    format!("{before}=>{after}")
}

fn token_confidence(token: &Value) -> f64 {
    token
        .get("confidence")
        .and_then(Value::as_f64)
        .unwrap_or(0.0)
}

fn entry_confidence(entry: &Value) -> f64 {
    let before = entry.get("before").map(token_confidence).unwrap_or(0.0);
    let after = entry.get("after").map(token_confidence).unwrap_or(0.0);
    before.max(after)
}

fn sort_observe_tokens_delta(delta: &mut Value) {
    if let Some(items) = delta.get_mut("added").and_then(Value::as_array_mut) {
        items.sort_by(token_position_compare);
    }
    if let Some(items) = delta.get_mut("removed").and_then(Value::as_array_mut) {
        items.sort_by(token_position_compare);
    }
    if let Some(items) = delta.get_mut("changed").and_then(Value::as_array_mut) {
        items.sort_by(changed_position_compare);
    }
}

fn token_position_compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    let ka = token_position_key(a);
    let kb = token_position_key(b);
    ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
}

fn changed_position_compare(a: &Value, b: &Value) -> std::cmp::Ordering {
    let ka = a
        .get("after")
        .map(token_position_key)
        .unwrap_or_else(|| token_position_key(a));
    let kb = b
        .get("after")
        .map(token_position_key)
        .unwrap_or_else(|| token_position_key(b));
    ka.partial_cmp(&kb).unwrap_or(std::cmp::Ordering::Equal)
}

fn token_position_key(token: &Value) -> (f64, f64, f64, f64) {
    let b = token_bbox_bounds(token).unwrap_or(desktop_core::protocol::Bounds {
        x: f64::MAX,
        y: f64::MAX,
        width: 0.0,
        height: 0.0,
    });
    (b.y, b.x, b.height, b.width)
}

pub(super) fn token_bbox_bounds(token: &Value) -> Option<desktop_core::protocol::Bounds> {
    let bbox = token.get("bbox")?.as_array()?;
    if bbox.len() != 4 {
        return None;
    }
    Some(desktop_core::protocol::Bounds {
        x: bbox[0].as_f64().unwrap_or(0.0),
        y: bbox[1].as_f64().unwrap_or(0.0),
        width: bbox[2].as_f64().unwrap_or(0.0),
        height: bbox[3].as_f64().unwrap_or(0.0),
    })
}

fn normalize_observe_text(input: &str) -> String {
    input
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
        .trim()
        .to_ascii_lowercase()
}

fn rewrite_token_bbox_relative(token: &mut Value, origin: Option<&desktop_core::protocol::Bounds>) {
    let Some(bbox) = token.get("bbox").and_then(Value::as_array) else {
        return;
    };
    if bbox.len() != 4 {
        return;
    }
    let x = bbox[0].as_f64().unwrap_or(0.0);
    let y = bbox[1].as_f64().unwrap_or(0.0);
    let w = bbox[2].as_f64().unwrap_or(0.0);
    let h = bbox[3].as_f64().unwrap_or(0.0);
    let rel = relative_bounds(
        &desktop_core::protocol::Bounds {
            x,
            y,
            width: w,
            height: h,
        },
        origin,
    );
    if let Some(obj) = token.as_object_mut() {
        obj.insert(
            "bbox".to_string(),
            json!([
                round_nonnegative_i64(rel.x),
                round_nonnegative_i64(rel.y),
                round_nonnegative_i64(rel.width),
                round_nonnegative_i64(rel.height)
            ]),
        );
    }
}

fn ensure_observe_token_id(token: &mut Value) {
    if let Some(ax_id) = canonical_ax_token_id(token) {
        if let Some(obj) = token.as_object_mut() {
            obj.insert("id".to_string(), Value::String(ax_id));
        }
        return;
    }
    if token
        .get("id")
        .and_then(Value::as_str)
        .is_some_and(|id| !id.trim().is_empty())
    {
        return;
    }
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let text = token
        .get("text")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let bbox_key = quantized_bbox_key(token.get("bbox").and_then(Value::as_array));
    let material = format!("{source}|{text}|{bbox_key}");
    let prefix = if source == "vision_ocr" {
        "ocr"
    } else if source.starts_with("accessibility_ax:") {
        "ax"
    } else {
        "tok"
    };
    if let Some(obj) = token.as_object_mut() {
        obj.insert(
            "id".to_string(),
            Value::String(format!("{prefix}_{:08x}", stable_hash32(&material))),
        );
    }
}

fn relative_bounds_json(
    bounds: &desktop_core::protocol::Bounds,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> Value {
    let rel = relative_bounds(bounds, origin);
    json!({
        "x": round_nonnegative_i64(rel.x),
        "y": round_nonnegative_i64(rel.y),
        "width": round_nonnegative_i64(rel.width),
        "height": round_nonnegative_i64(rel.height)
    })
}

fn relative_bounds(
    bounds: &desktop_core::protocol::Bounds,
    origin: Option<&desktop_core::protocol::Bounds>,
) -> desktop_core::protocol::Bounds {
    let mut out = bounds.clone();
    if let Some(window) = origin {
        out.x -= window.x;
        out.y -= window.y;
    }
    out.x = out.x.max(0.0);
    out.y = out.y.max(0.0);
    out.width = out.width.max(0.0);
    out.height = out.height.max(0.0);
    out
}

pub(super) fn round_nonnegative_i64(value: f64) -> i64 {
    value.round().max(0.0) as i64
}

pub(super) fn observe_ocr_token_id(text: &str, bounds: &desktop_core::protocol::Bounds) -> String {
    let (x, y, w, h) = quantized_observe_bbox(bounds);
    let material = format!("{}|{x},{y},{w},{h}", text.trim().to_ascii_lowercase());
    format!("ocr_{:08x}", stable_hash32(&material))
}

fn quantized_observe_bbox(bounds: &desktop_core::protocol::Bounds) -> (i64, i64, i64, i64) {
    let q = |v: f64| -> i64 { (v / 8.0).round() as i64 };
    (
        q(bounds.x.max(0.0)),
        q(bounds.y.max(0.0)),
        q(bounds.width.max(0.0)),
        q(bounds.height.max(0.0)),
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

fn observe_token_key(token: &Value) -> String {
    if let Some(ax_key) = canonical_ax_match_key(token) {
        return format!("ax:{ax_key}");
    }
    if let Some(id) = token.get("id").and_then(Value::as_str) {
        if !id.trim().is_empty() {
            return format!("id:{id}");
        }
    }
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let text = token.get("text").and_then(Value::as_str).unwrap_or("");
    let bbox_key = quantized_bbox_key(token.get("bbox").and_then(Value::as_array));
    format!("fallback:{source}:{text}:{bbox_key}")
}

fn canonical_ax_token_id(token: &Value) -> Option<String> {
    let key = canonical_ax_match_key(token)?;
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("accessibility_ax:ax")
        .trim()
        .to_ascii_lowercase();
    let role_prefix = source
        .split(':')
        .nth(1)
        .map(|s| {
            s.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_ascii_lowercase()
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "ax".to_string());
    let suffix = if token_is_scrollable(token) {
        "_scrollable"
    } else {
        ""
    };
    Some(format!(
        "{}_{}{}",
        role_prefix,
        format!("{:08x}", stable_hash32(&key)),
        suffix
    ))
}

fn canonical_ax_match_key(token: &Value) -> Option<String> {
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if !source.starts_with("accessibility_ax:") {
        return None;
    }
    let bbox_key = quantized_bbox_key_with_step(token.get("bbox").and_then(Value::as_array), 16.0);
    let scrollable = token_is_scrollable(token);
    let checked = token
        .get("checked")
        .map(|v| v.to_string())
        .unwrap_or_else(|| "null".to_string());
    Some(format!(
        "{}|{}|scrollable={}|checked={}",
        source.to_ascii_lowercase(),
        bbox_key,
        scrollable,
        checked
    ))
}

fn token_is_scrollable(token: &Value) -> bool {
    if token
        .get("scrollable")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        return true;
    }
    let source = token
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    source.contains("axscrollbar")
        || source.contains("axscrollarea")
        || source.contains("axvalueindicator")
}

fn quantized_bbox_key(bbox: Option<&Vec<Value>>) -> String {
    quantized_bbox_key_with_step(bbox, 8.0)
}

fn quantized_bbox_key_with_step(bbox: Option<&Vec<Value>>, step: f64) -> String {
    let Some(bbox) = bbox else {
        return "[]".to_string();
    };
    if bbox.len() != 4 {
        return "[]".to_string();
    }
    let q = |v: Option<f64>| -> i64 {
        let n = v.unwrap_or(0.0);
        (n / step).round() as i64
    };
    let x = q(bbox[0].as_f64());
    let y = q(bbox[1].as_f64());
    let w = q(bbox[2].as_f64());
    let h = q(bbox[3].as_f64());
    format!("{x},{y},{w},{h}")
}

fn observe_token_semantic_equal(a: &Value, b: &Value) -> bool {
    let a_text = a.get("text").cloned().unwrap_or(Value::Null);
    let b_text = b.get("text").cloned().unwrap_or(Value::Null);
    let a_bbox = a.get("bbox").cloned().unwrap_or_else(|| json!([]));
    let b_bbox = b.get("bbox").cloned().unwrap_or_else(|| json!([]));
    let a_source = a.get("source").cloned().unwrap_or(Value::Null);
    let b_source = b.get("source").cloned().unwrap_or(Value::Null);
    let a_checked = a.get("checked").cloned().unwrap_or(Value::Null);
    let b_checked = b.get("checked").cloned().unwrap_or(Value::Null);
    a_text == b_text && a_bbox == b_bbox && a_source == b_source && a_checked == b_checked
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(id: &str, text: &str, x: f64, y: f64) -> Value {
        json!({
            "id": id,
            "source": "vision_ocr",
            "text": text,
            "bbox": [x, y, 20.0, 10.0]
        })
    }

    #[test]
    fn diff_observe_tokens_reports_add_remove_and_change() {
        let before = vec![tok("a", "Save", 10.0, 10.0), tok("b", "Cancel", 40.0, 10.0)];
        let after = vec![
            tok("a", "Save As", 10.0, 10.0),
            tok("c", "Done", 70.0, 10.0),
        ];
        let delta = diff_observe_tokens(&before, &after);
        assert_eq!(
            delta["added"].as_array().expect("added").len(),
            1,
            "expected one add"
        );
        assert_eq!(
            delta["removed"].as_array().expect("removed").len(),
            1,
            "expected one remove"
        );
        assert_eq!(
            delta["changed"].as_array().expect("changed").len(),
            1,
            "expected one changed"
        );
    }

    #[test]
    fn normalize_observe_tokens_delta_assigns_ids_and_relative_bboxes() {
        let mut delta = json!({
            "added": [{
                "source": "vision_ocr",
                "text": "Open",
                "bbox": [110.0, 210.0, 20.0, 10.0]
            }],
            "removed": [],
            "changed": []
        });
        let origin = desktop_core::protocol::Bounds {
            x: 100.0,
            y: 200.0,
            width: 500.0,
            height: 400.0,
        };
        delta = normalize_observe_tokens_delta(delta, Some(&origin));
        let added = delta["added"].as_array().expect("added");
        assert_eq!(added.len(), 1);
        let id = added[0]["id"].as_str().unwrap_or("");
        assert!(
            !id.trim().is_empty(),
            "expected ID to be filled during normalization"
        );
        assert_eq!(added[0]["bbox"], json!([10, 10, 20, 10]));
    }

    #[test]
    fn normalize_observe_tokens_delta_sorts_tokens_by_position() {
        let mut delta = json!({
            "added": [
                tok("b", "B", 50.0, 40.0),
                tok("a", "A", 10.0, 20.0),
                tok("c", "C", 10.0, 10.0)
            ],
            "removed": [],
            "changed": []
        });
        delta = normalize_observe_tokens_delta(delta, None);
        let added = delta["added"].as_array().expect("added");
        let ids: Vec<&str> = added
            .iter()
            .map(|v| v["id"].as_str().unwrap_or(""))
            .collect();
        assert_eq!(ids, vec!["c", "a", "b"]);
    }

    #[test]
    fn normalize_observe_tokens_delta_cancels_identical_add_remove_pairs() {
        let mut delta = json!({
            "added": [tok("same", "Open", 10.0, 10.0)],
            "removed": [tok("same", "Open", 10.0, 10.0)],
            "changed": []
        });
        delta = normalize_observe_tokens_delta(delta, None);
        assert_eq!(delta["added"], json!([]));
        assert_eq!(delta["removed"], json!([]));
    }

    #[test]
    fn normalize_observe_tokens_delta_keeps_non_identical_add_remove_pairs() {
        let mut delta = json!({
            "added": [tok("same", "Open Now", 10.0, 10.0)],
            "removed": [tok("same", "Open", 10.0, 10.0)],
            "changed": []
        });
        delta = normalize_observe_tokens_delta(delta, None);
        assert_eq!(
            delta["added"].as_array().expect("added").len(),
            1,
            "changed semantic content should remain as add"
        );
        assert_eq!(
            delta["removed"].as_array().expect("removed").len(),
            1,
            "changed semantic content should retain remove"
        );
    }

    #[test]
    fn normalize_observe_tokens_delta_dedupes_near_identical_added_tokens() {
        let mut delta = json!({
            "added": [
                {
                    "id": "a1",
                    "source": "vision_ocr",
                    "text": "Shopping list",
                    "confidence": 0.8,
                    "bbox": [100.0, 200.0, 80.0, 20.0]
                },
                {
                    "id": "a2",
                    "source": "vision_ocr",
                    "text": " shopping   list ",
                    "confidence": 0.9,
                    "bbox": [103.0, 204.0, 81.0, 21.0]
                }
            ],
            "removed": [],
            "changed": []
        });
        delta = normalize_observe_tokens_delta(delta, None);
        let added = delta["added"].as_array().expect("added");
        assert_eq!(
            added.len(),
            1,
            "expected duplicate OCR additions to be deduped"
        );
    }

    #[test]
    fn diff_observe_tokens_matches_ax_by_role_and_bbox_not_raw_id() {
        let before = vec![json!({
            "id": "element_3",
            "source": "accessibility_ax:AXScrollBar",
            "scrollable": true,
            "text": "Old content",
            "bbox": [1031, 51, 17, 629]
        })];
        let after = vec![json!({
            "id": "element",
            "source": "accessibility_ax:AXScrollBar",
            "scrollable": true,
            "text": "New content",
            "bbox": [1031, 51, 17, 629]
        })];
        let mut delta = diff_observe_tokens(&before, &after);
        delta = normalize_observe_tokens_delta(delta, None);
        assert_eq!(
            delta["added"].as_array().expect("added").len(),
            0,
            "should not emit AX add for same geometric AX element"
        );
        assert_eq!(
            delta["removed"].as_array().expect("removed").len(),
            0,
            "should not emit AX remove for same geometric AX element"
        );
        assert_eq!(
            delta["changed"].as_array().expect("changed").len(),
            1,
            "should emit AX changed for text mutation"
        );
    }
}

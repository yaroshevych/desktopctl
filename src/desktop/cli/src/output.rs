use crate::transport::{next_request_id, send_request_with_autostart};
use desktop_core::protocol::{Command, RequestEnvelope, ResponseEnvelope};
use std::time::{SystemTime, UNIX_EPOCH};

pub(crate) fn render_response(
    command: &Command,
    response: &ResponseEnvelope,
    passthrough_stored_response: bool,
) -> serde_json::Value {
    let supports_active_window = command_supports_active_window(command);
    let has_explicit_active_window_id = command_has_explicit_active_window_id(command);
    let json_hints = command_json_hints(command);

    let mut prefix_fields: Vec<(String, String)> = Vec::new();
    let mut eligible_hints: Vec<String> = Vec::new();
    if supports_active_window && !has_explicit_active_window_id {
        eligible_hints.push(active_window_tip_message());
    }
    for hint in &json_hints {
        eligible_hints.push((*hint).to_string());
    }
    if response_contains_unknown_checked(response) {
        eligible_hints.push(
            "toggle control state (checkbox/radio/switch) is unknown; verify with a small-area screenshot around that control".to_string(),
        );
    }
    if let Some(hint) = pick_random_hint(&eligible_hints) {
        prefix_fields.push(("hint".to_string(), hint));
    }
    render_response_with_prefix_fields(response, &prefix_fields, passthrough_stored_response)
}

fn command_supports_active_window(command: &Command) -> bool {
    matches!(
        command,
        Command::PointerMove { .. }
            | Command::PointerDown { .. }
            | Command::PointerUp { .. }
            | Command::PointerClick { .. }
            | Command::PointerClickText { .. }
            | Command::PointerClickId { .. }
            | Command::PointerScroll { .. }
            | Command::PointerDrag { .. }
            | Command::UiType { .. }
            | Command::KeyHotkey { .. }
            | Command::KeyEnter { .. }
            | Command::KeyEscape { .. }
            | Command::ScreenCapture { .. }
            | Command::ScreenTokenize { .. }
    )
}

fn command_has_explicit_active_window_id(command: &Command) -> bool {
    match command {
        Command::PointerMove {
            active_window_id, ..
        }
        | Command::PointerDown {
            active_window_id, ..
        }
        | Command::PointerUp {
            active_window_id, ..
        }
        | Command::PointerClick {
            active_window_id, ..
        }
        | Command::PointerClickText {
            active_window_id, ..
        }
        | Command::PointerClickId {
            active_window_id, ..
        }
        | Command::PointerScroll {
            active_window_id, ..
        }
        | Command::PointerDrag {
            active_window_id, ..
        }
        | Command::UiType {
            active_window_id, ..
        }
        | Command::KeyHotkey {
            active_window_id, ..
        }
        | Command::KeyEnter {
            active_window_id, ..
        }
        | Command::KeyEscape {
            active_window_id, ..
        }
        | Command::ScreenCapture {
            active_window_id, ..
        }
        | Command::ScreenTokenize {
            active_window_id, ..
        } => active_window_id
            .as_deref()
            .map(|id| !id.trim().is_empty())
            .unwrap_or(false),
        _ => false,
    }
}

fn active_window_tip_message() -> String {
    let id = resolve_frontmost_window_id().unwrap_or_else(|| "unknown".to_string());
    format!("use --active-window {id} to avoid acting in the wrong window")
}

fn command_json_hints(command: &Command) -> Vec<&'static str> {
    match command {
        Command::WindowList => {
            vec![
                "compact output with | jq '.result.windows[] | \"\\\\(.id) \\\\(.visible) \\\\(.title)\"'",
            ]
        }
        Command::ScreenCapture { .. } => vec![
            "prefer `screen tokenize` for automation flows; use screenshot as last resort for visual artifacts/debug",
        ],
        Command::ScreenTokenize { .. } => {
            const TOKENIZE_HINTS: [&str; 2] = [
                "tokenize response includes request_id in JSON output; reuse it with `desktopctl request response <request_id>`",
                "compact output with | jq -r '.result.text_dump'",
            ];
            let idx = (SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as usize)
                % TOKENIZE_HINTS.len();
            vec![TOKENIZE_HINTS[idx]]
        }
        Command::PointerScroll { .. } => {
            vec!["before scroll, move pointer into the target scroll area"]
        }
        Command::UiType { .. } => vec![
            "to replace existing field content, send `desktopctl keyboard press cmd+a` before typing",
        ],
        _ => Vec::new(),
    }
}

fn resolve_frontmost_window_id() -> Option<String> {
    let list_request_id = next_request_id();
    let request = RequestEnvelope::new(list_request_id, Command::WindowList);
    let response = send_request_with_autostart(&request).ok()?;
    let success = match response {
        ResponseEnvelope::Success(success) => success,
        ResponseEnvelope::Error(_) => return None,
    };
    let windows = success.result.get("windows")?.as_array()?;
    for window in windows {
        if !window
            .get("frontmost")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            continue;
        }
        if let Some(id) = window.get("id").and_then(serde_json::Value::as_str) {
            let trimmed = id.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

fn render_response_with_prefix_fields(
    response: &ResponseEnvelope,
    prefix_fields: &[(String, String)],
    passthrough_stored_response: bool,
) -> serde_json::Value {
    let mut out = serde_json::Map::new();
    for (key, value) in prefix_fields {
        out.insert(key.clone(), serde_json::Value::String(value.clone()));
    }
    let raw = if passthrough_stored_response {
        match response {
            ResponseEnvelope::Success(success) if is_response_envelope_shape(&success.result) => {
                success.result.clone()
            }
            _ => serde_json::to_value(response).unwrap_or_else(|_| serde_json::json!({})),
        }
    } else {
        serde_json::to_value(response).unwrap_or_else(|_| serde_json::json!({}))
    };
    if let Some(obj) = raw.as_object() {
        for (k, v) in obj {
            out.insert(k.clone(), v.clone());
        }
    }
    serde_json::Value::Object(out)
}

fn response_contains_unknown_checked(response: &ResponseEnvelope) -> bool {
    let value = match response {
        ResponseEnvelope::Success(success) => &success.result,
        ResponseEnvelope::Error(error) => &error.error.details.clone().unwrap_or_default(),
    };
    value_contains_unknown_checked(value)
}

fn pick_random_hint(hints: &[String]) -> Option<String> {
    if hints.is_empty() {
        return None;
    }
    let idx = (SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos() as usize)
        % hints.len();
    Some(hints[idx].clone())
}

fn value_contains_unknown_checked(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(map) => {
            if map
                .get("checked")
                .and_then(serde_json::Value::as_str)
                .is_some_and(|s| s.eq_ignore_ascii_case("unknown"))
            {
                return true;
            }
            map.values().any(value_contains_unknown_checked)
        }
        serde_json::Value::Array(items) => items.iter().any(value_contains_unknown_checked),
        _ => false,
    }
}

fn is_response_envelope_shape(value: &serde_json::Value) -> bool {
    let Some(obj) = value.as_object() else {
        return false;
    };
    obj.get("ok").and_then(serde_json::Value::as_bool).is_some()
        && (obj.contains_key("result") || obj.contains_key("error"))
}

#[cfg(test)]
mod tests {
    use super::render_response;
    use desktop_core::protocol::{Command, ResponseEnvelope};
    use serde_json::json;

    #[test]
    fn request_response_passthrough_uses_embedded_envelope_shape() {
        let command = Command::RequestResponse {
            request_id: "stored-1".to_string(),
        };
        let response = ResponseEnvelope::success(
            "outer-req",
            json!({
                "ok": true,
                "api_version": "1",
                "request_id": "inner-req",
                "result": { "message": "from-store" }
            }),
        );

        let rendered = render_response(&command, &response, true);
        assert_eq!(rendered["request_id"], "inner-req");
        assert_eq!(rendered["result"]["message"], "from-store");
        assert!(rendered.get("hint").is_none());
    }

    #[test]
    fn window_list_renders_static_hint() {
        let command = Command::WindowList;
        let response = ResponseEnvelope::success("r1", json!({ "windows": [] }));

        let rendered = render_response(&command, &response, false);
        assert_eq!(
            rendered["hint"],
            "compact output with | jq '.result.windows[] | \"\\\\(.id) \\\\(.visible) \\\\(.title)\"'"
        );
        assert_eq!(rendered["ok"], true);
        assert_eq!(rendered["result"]["windows"], json!([]));
    }
}

use desktop_core::{
    error::AppError,
    protocol::{Command, ResponseEnvelope},
};
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
        eligible_hints.push(active_window_tip_message(response));
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

pub(crate) fn render_markdown_response(
    command: &Command,
    response: &ResponseEnvelope,
    passthrough_stored_response: bool,
) -> String {
    let rendered = render_response(command, response, passthrough_stored_response);
    if matches!(command, Command::ScreenTokenize { .. }) {
        return render_tokenize_markdown(&rendered);
    }
    render_generic_markdown(command, &rendered)
}

pub(crate) fn render_markdown_error(request_id: &str, err: &AppError) -> String {
    let mut lines: Vec<String> = vec![
        "# Error".to_string(),
        String::new(),
        format!("- Request ID: `{request_id}`"),
        format!("- Code: `{:?}`", err.code),
        format!("- Message: {}", err.message),
        format!("- Retryable: `{}`", err.retryable),
    ];
    if let Some(command) = err.command.as_deref().filter(|v| !v.trim().is_empty()) {
        lines.push(format!("- Command: `{command}`"));
    }
    if let Some(debug_ref) = err.debug_ref.as_deref().filter(|v| !v.trim().is_empty()) {
        lines.push(format!("- Debug Ref: `{debug_ref}`"));
    }
    lines.join("\n")
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

fn active_window_tip_message(response: &ResponseEnvelope) -> String {
    let id = resolve_window_id_from_response(response).unwrap_or_else(|| "unknown".to_string());
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

fn resolve_window_id_from_response(response: &ResponseEnvelope) -> Option<String> {
    let success = match response {
        ResponseEnvelope::Success(success) => success,
        ResponseEnvelope::Error(_) => return None,
    };
    let windows = success.result.get("windows")?.as_array()?;
    for window in windows {
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

fn render_tokenize_markdown(value: &serde_json::Value) -> String {
    let ok = value
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !ok {
        return render_error_markdown_from_value("Screen Tokenize", value);
    }
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let hint = value.get("hint").and_then(serde_json::Value::as_str);
    let windows = value
        .get("result")
        .and_then(|v| v.get("windows"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let top_app = windows
        .first()
        .and_then(|w| w.get("app"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let top_size = windows.first().and_then(|w| {
        let bounds = w.get("bounds")?;
        let width = bounds.get("width")?.as_f64()?;
        let height = bounds.get("height")?.as_f64()?;
        Some(format!("{:.0}x{:.0}", width, height))
    });
    let top_window_title = windows
        .first()
        .and_then(|w| w.get("title"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let top_window_id = windows
        .first()
        .and_then(|w| w.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);

    let mut lines: Vec<String> = vec!["# Screen Tokenize".to_string(), String::new()];
    lines.push(format!("- Request ID: `{request_id}`"));
    if let Some(app) = top_app {
        lines.push(format!("- App: {}", app));
    }
    if let Some(size) = top_size {
        lines.push(format!("- Window Size: {}", size));
    }
    if let Some(title) = top_window_title {
        lines.push(format!("- Window Title: {}", title));
    }
    if let Some(id) = top_window_id {
        lines.push(format!("- Window ID: {}", id));
    }
    if let Some(hint_text) = hint.filter(|v| !v.trim().is_empty()) {
        lines.push(format!("- Hint: {}", hint_text));
    }
    if windows.is_empty() {
        lines.push(String::new());
        lines.push("## Window (unknown)".to_string());
        lines.push("None".to_string());
    }
    let single_window = windows.len() == 1;
    for (window_idx, window) in windows.into_iter().enumerate() {
        let id = window
            .get("id")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("unknown");
        let title = window
            .get("title")
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .unwrap_or("untitled");
        lines.push(String::new());
        if single_window {
            lines.push("## Window".to_string());
        } else {
            lines.push(format!("## Window {}", window_idx + 1));
            lines.push(format!("Title: {}", title));
            lines.push(format!("ID: {}", id));
        }
        let elements = window
            .get("elements")
            .and_then(serde_json::Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut entries: Vec<MarkdownEntry> = elements
            .iter()
            .filter_map(markdown_entry_from_element)
            .collect();
        if entries.is_empty() {
            lines.push("No elements".to_string());
            continue;
        }
        entries.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));

        let mut columns: Vec<Vec<MarkdownEntry>> = Vec::new();
        let mut column_scrollable: Vec<bool> = Vec::new();
        let column_split = 140.0_f64;
        for entry in entries {
            if !columns.is_empty() {
                let idx = columns.len() - 1;
                let last_x = columns[idx].last().map(|e| e.x).unwrap_or(entry.x);
                if (entry.x - last_x).abs() <= column_split {
                    column_scrollable[idx] = column_scrollable[idx] || entry.scrollable;
                    columns[idx].push(entry);
                    continue;
                }
            }
            column_scrollable.push(entry.scrollable);
            columns.push(vec![entry]);
        }

        for (idx, column) in columns.into_iter().enumerate() {
            let mut column = column;
            column.sort_by(|a, b| a.y.total_cmp(&b.y).then_with(|| a.x.total_cmp(&b.x)));
            let title = match idx {
                0 => "Left Column".to_string(),
                1 => "Right Column".to_string(),
                _ => format!("Column {}", idx + 1),
            };
            if column_scrollable.get(idx).copied().unwrap_or(false) {
                lines.push(format!("### {} (Scrollable)", title));
            } else {
                lines.push(format!("### {}", title));
            }
            let mut last_render_key: Option<String> = None;
            let mut last_y: f64 = f64::NEG_INFINITY;
            for entry in column {
                if !entry.visible {
                    continue;
                }
                let dedupe_key = format!(
                    "{}|{}|{}",
                    normalize_single_line(&entry.label).to_ascii_lowercase(),
                    entry.id.as_deref().unwrap_or_default(),
                    entry.checked.as_deref().unwrap_or_default()
                );
                if last_render_key.as_deref() == Some(dedupe_key.as_str())
                    && (entry.y - last_y).abs() < 14.0
                {
                    continue;
                }
                let mut line = entry.label;
                if let Some(id) = entry.id.as_deref().filter(|v| !v.trim().is_empty()) {
                    line.push_str(&format!(" #{id}"));
                }
                if let Some(checked) = entry.checked.as_deref().filter(|v| !v.trim().is_empty()) {
                    line.push_str(&format!(" [checked={checked}]"));
                }
                lines.push(line);
                last_render_key = Some(dedupe_key);
                last_y = entry.y;
            }
        }
    }
    lines.join("\n")
}

fn render_generic_markdown(command: &Command, value: &serde_json::Value) -> String {
    let title = command.name().replace('_', " ");
    let ok = value
        .get("ok")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    if !ok {
        return render_error_markdown_from_value(&title, value);
    }
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let mut lines = vec![format!("# {}", to_title_case(&title)), String::new()];
    lines.push(format!("- Request ID: `{request_id}`"));
    if let Some(hint) = value
        .get("hint")
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.trim().is_empty())
    {
        lines.push(format!("- Hint: {}", hint));
    }

    let result = value.get("result").cloned().unwrap_or_default();
    if let Some(message) = result.get("message").and_then(serde_json::Value::as_str) {
        lines.push(String::new());
        lines.push("## Result".to_string());
        lines.push(message.to_string());
        return lines.join("\n");
    }

    if let Some(obj) = result.as_object() {
        let mut scalar_lines: Vec<String> = Vec::new();
        for (k, v) in obj {
            if let Some(summary) = compact_value_summary(v) {
                scalar_lines.push(format!("- {}: {}", k, summary));
            }
        }
        if !scalar_lines.is_empty() {
            lines.push(String::new());
            lines.push("## Result".to_string());
            lines.extend(scalar_lines);
        }
    }

    lines.join("\n")
}

fn render_error_markdown_from_value(title: &str, value: &serde_json::Value) -> String {
    let request_id = value
        .get("request_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown");
    let error = value.get("error").cloned().unwrap_or_default();
    let code = error
        .get("code")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("internal");
    let message = error
        .get("message")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("unknown error");
    let retryable = error
        .get("retryable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    format!(
        "# {}\n\n- Request ID: `{}`\n- Code: `{}`\n- Message: {}\n- Retryable: `{}`",
        to_title_case(title),
        request_id,
        code,
        message,
        retryable
    )
}

fn element_label(element: &serde_json::Value) -> String {
    for key in ["text", "label", "name", "value"] {
        if let Some(v) = element.get(key).and_then(serde_json::Value::as_str) {
            let cleaned = normalize_single_line(v);
            if !cleaned.is_empty() {
                return cleaned;
            }
        }
    }
    "element".to_string()
}

fn normalize_single_line(value: &str) -> String {
    let normalized = value.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.contains('\n') {
        return normalized
            .split('\n')
            .map(|line| line.split_whitespace().collect::<Vec<&str>>().join(" "))
            .map(|line| line.trim().to_string())
            .filter(|line| !line.is_empty())
            .collect::<Vec<String>>()
            .join("\\n");
    }
    normalized
        .split_whitespace()
        .collect::<Vec<&str>>()
        .join(" ")
        .trim()
        .to_string()
}

fn element_role(element: &serde_json::Value) -> Option<String> {
    element
        .get("role")
        .or_else(|| element.get("kind"))
        .or_else(|| element.get("type"))
        .and_then(serde_json::Value::as_str)
        .map(|v| v.trim().to_ascii_lowercase())
        .filter(|v| !v.is_empty())
}

#[derive(Debug, Clone)]
struct MarkdownEntry {
    label: String,
    id: Option<String>,
    x: f64,
    y: f64,
    scrollable: bool,
    checked: Option<String>,
    visible: bool,
}

fn markdown_entry_from_element(element: &serde_json::Value) -> Option<MarkdownEntry> {
    let label = element_label(element);
    if label.trim().is_empty() {
        return None;
    }
    let bbox = element.get("bbox")?.as_array()?;
    if bbox.len() != 4 {
        return None;
    }
    let x = bbox[0].as_f64().unwrap_or(0.0);
    let y = bbox[1].as_f64().unwrap_or(0.0);
    let role = element_role(element);
    let id = element
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let checked = element
        .get("checked")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    let mut scrollable = element
        .get("scrollable")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let source = element
        .get("source")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let role_text = role.as_deref().unwrap_or_default();
    if source.contains("axvalueindicator")
        || source.contains("axscrollbar")
        || source.contains("axscrollarea")
        || role_text.contains("scroll")
    {
        scrollable = true;
    }
    let visible = !(label.eq_ignore_ascii_case("element") && checked.is_none());
    Some(MarkdownEntry {
        label,
        id,
        x,
        y,
        scrollable,
        checked,
        visible,
    })
}

fn compact_value_summary(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(v) => Some(format!("`{v}`")),
        serde_json::Value::Number(v) => Some(format!("`{v}`")),
        serde_json::Value::String(v) => {
            if v.trim().is_empty() {
                None
            } else {
                Some(v.clone())
            }
        }
        serde_json::Value::Array(items) => Some(format!("`{} items`", items.len())),
        serde_json::Value::Object(map) => Some(format!("`{} fields`", map.len())),
    }
}

fn to_title_case(value: &str) -> String {
    value
        .split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => format!("{}{}", first.to_ascii_uppercase(), chars.as_str()),
                None => String::new(),
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::{render_markdown_response, render_response};
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

    #[test]
    fn tokenize_markdown_omits_positions_and_keeps_ids() {
        let command = Command::ScreenTokenize {
            overlay_out_path: None,
            window_query: None,
            screenshot_path: None,
            active_window: false,
            active_window_id: None,
            region: None,
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "windows": [{
                    "id": "system_settings_1aca92",
                    "title": "Permissions",
                    "app": "System Settings",
                    "elements": [
                        {
                            "id": "button_search",
                            "role": "button",
                            "text": "Search",
                            "bbox": [12, 34, 56, 20]
                        },
                        {
                            "id": "text_sidebar",
                            "role": "text",
                            "text": "Control Centre\nDesktop & Dock\nDisplays",
                            "bbox": [20, 80, 200, 40]
                        },
                        {
                            "id": "axid_toggle_recording",
                            "role": "checkbox",
                            "text": "Recording",
                            "checked": "true",
                            "bbox": [260, 120, 100, 20]
                        }
                    ]
                }]
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("- Window Title: Permissions"));
        assert!(markdown.contains("- Window ID: system_settings_1aca92"));
        assert!(markdown.contains("## Window"));
        assert!(markdown.contains("### Left Column"));
        assert!(markdown.contains("### Right Column"));
        assert!(markdown.contains("Search #button_search"));
        assert!(markdown.contains("Control Centre\\nDesktop & Dock\\nDisplays"));
        assert!(markdown.contains("Recording #axid_toggle_recording [checked=true]"));
        assert!(!markdown.contains("12"));
        assert!(!markdown.contains("34"));
        assert!(!markdown.contains("bbox"));
        assert!(!markdown.contains("## Windows"));
        assert!(!markdown.lines().any(|line| line == "## Text"));
        assert!(!markdown.contains("```text"));
    }
}

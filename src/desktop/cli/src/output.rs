use desktop_core::{
    error::AppError,
    protocol::{Command, ResponseEnvelope},
};
use std::time::{SystemTime, UNIX_EPOCH};

fn is_snake_case_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_')
}

fn push_kv_line(lines: &mut Vec<String>, key: &str, value: impl AsRef<str>) {
    debug_assert!(
        is_snake_case_key(key),
        "markdown key must be snake_case: {key}"
    );
    lines.push(format!("- {key}: {}", value.as_ref()));
}

fn push_plain_field_line(lines: &mut Vec<String>, key: &str, value: impl AsRef<str>) {
    debug_assert!(
        is_snake_case_key(key),
        "markdown key must be snake_case: {key}"
    );
    lines.push(format!("{key}: {}", value.as_ref()));
}

fn push_section(lines: &mut Vec<String>, title: &str) {
    lines.push(String::new());
    lines.push(format!("## {title}"));
}

fn push_subsection(lines: &mut Vec<String>, title: &str) {
    lines.push(format!("### {title}"));
}

pub(crate) fn render_response(
    command: &Command,
    response: &ResponseEnvelope,
    passthrough_stored_response: bool,
) -> serde_json::Value {
    let is_journal_tokenize = is_journal_tokenize(command);
    let supports_active_window = command_supports_active_window(command);
    let has_explicit_active_window_id = command_has_explicit_active_window_id(command);
    let json_hints = if is_journal_tokenize {
        Vec::new()
    } else {
        command_json_hints(command)
    };

    let mut prefix_fields: Vec<(String, String)> = Vec::new();
    let mut eligible_hints: Vec<String> = Vec::new();
    if !is_journal_tokenize && supports_active_window && !has_explicit_active_window_id {
        if let Some(hint) = active_window_tip_message(response) {
            eligible_hints.push(hint);
        }
    }
    if !is_journal_tokenize && matches!(command, Command::OpenApp { .. }) {
        eligible_hints.push(open_app_hint_message(response));
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
    let mut rendered =
        render_response_with_prefix_fields(response, &prefix_fields, passthrough_stored_response);
    if is_journal_tokenize {
        apply_journal_render_redaction(&mut rendered);
    }
    rendered
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
    let mut lines: Vec<String> = vec!["# Error".to_string(), String::new()];
    push_kv_line(&mut lines, "request_id", request_id);
    push_kv_line(&mut lines, "code", format!("{:?}", err.code));
    push_kv_line(&mut lines, "message", &err.message);
    push_kv_line(&mut lines, "retryable", err.retryable.to_string());
    if let Some(command) = err.command.as_deref().filter(|v| !v.trim().is_empty()) {
        push_kv_line(&mut lines, "command", command);
    }
    if let Some(debug_ref) = err.debug_ref.as_deref().filter(|v| !v.trim().is_empty()) {
        push_kv_line(&mut lines, "debug_ref", debug_ref);
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

fn is_journal_tokenize(command: &Command) -> bool {
    matches!(command, Command::ScreenTokenize { journal: true, .. })
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

fn apply_journal_render_redaction(value: &mut serde_json::Value) {
    let Some(obj) = value.as_object_mut() else {
        return;
    };
    obj.remove("request_id");
    obj.remove("hint");
    if let Some(result) = obj
        .get_mut("result")
        .and_then(serde_json::Value::as_object_mut)
    {
        result.remove("window_id");
        result.remove("hint");
        if let Some(windows) = result
            .get_mut("windows")
            .and_then(serde_json::Value::as_array_mut)
        {
            for window in windows {
                let Some(window_obj) = window.as_object_mut() else {
                    continue;
                };
                window_obj.remove("id");
                if let Some(elements) = window_obj
                    .get_mut("elements")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    for element in elements {
                        if let Some(element_obj) = element.as_object_mut() {
                            element_obj.remove("id");
                        }
                    }
                }
            }
        }
        if let Some(windows) = result
            .get_mut("all_windows")
            .and_then(serde_json::Value::as_array_mut)
        {
            for window in windows {
                if let Some(window_obj) = window.as_object_mut() {
                    window_obj.remove("id");
                }
            }
        }
    }
}

fn active_window_tip_message(response: &ResponseEnvelope) -> Option<String> {
    let id = resolve_window_id_from_response(response)?;
    Some(format!(
        "use --active-window {id} to avoid acting in the wrong window"
    ))
}

fn command_json_hints(command: &Command) -> Vec<&'static str> {
    match command {
        Command::WindowList => {
            vec!["window list markdown prints one section per window titled by window_title"]
        }
        Command::ScreenCapture { .. } => vec![
            "prefer screen tokenize for automation flows; use screenshot as last resort for visual artifacts/debug",
        ],
        Command::ScreenTokenize { .. } => {
            const TOKENIZE_HINTS: [&str; 1] = [
                "tokenize response includes request_id in JSON output; reuse it with desktopctl request response <request_id>",
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
            "to replace existing field content, send desktopctl keyboard press cmd+a before typing",
        ],
        _ => Vec::new(),
    }
}

fn open_app_hint_message(response: &ResponseEnvelope) -> String {
    let window_id = match response {
        ResponseEnvelope::Success(success) => success
            .result
            .get("window_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("unknown"),
        ResponseEnvelope::Error(_) => "unknown",
    };
    format!("use --active-window {window_id} in follow-up commands to target this app window")
}

fn resolve_window_id_from_response(response: &ResponseEnvelope) -> Option<String> {
    let success = match response {
        ResponseEnvelope::Success(success) => success,
        ResponseEnvelope::Error(_) => return None,
    };
    if let Some(id) = success
        .result
        .get("observe")
        .and_then(|v| v.get("active_window_id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(id.to_string());
    }
    if let Some(id) = success
        .result
        .get("window_id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(id.to_string());
    }
    if let Some(id) = success
        .result
        .get("window")
        .and_then(|v| v.get("id"))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(id.to_string());
    }
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
    let result = value.get("result");
    let windows = result
        .and_then(|v| v.get("windows"))
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_all_windows = result.and_then(|v| v.get("all_windows")).is_some();
    let all_windows = result
        .and_then(|v| v.get("all_windows"))
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
    push_kv_line(&mut lines, "request_id", request_id);
    if let Some(app) = top_app {
        push_kv_line(&mut lines, "app", app);
    }
    if let Some(size) = top_size {
        push_kv_line(&mut lines, "window_size", size);
    }
    if let Some(title) = top_window_title {
        push_kv_line(&mut lines, "window_title", title);
    }
    if let Some(id) = top_window_id {
        push_kv_line(&mut lines, "window_id", id);
    }
    if let Some(hint_text) = hint.filter(|v| !v.trim().is_empty()) {
        push_kv_line(&mut lines, "hint", hint_text);
    }
    if windows.is_empty() {
        push_section(&mut lines, "Window (unknown)");
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
        if single_window {
            push_section(&mut lines, "Window");
        } else {
            push_section(&mut lines, &format!("Window {}", window_idx + 1));
            push_kv_line(&mut lines, "window_title", title);
            push_kv_line(&mut lines, "window_id", id);
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
            column.sort_by(|a, b| {
                a.y.total_cmp(&b.y)
                    .then_with(|| a.x.total_cmp(&b.x))
                    // Prefer non-OCR ids when duplicate labels overlap.
                    .then_with(|| is_ocr_id(a.id.as_deref()).cmp(&is_ocr_id(b.id.as_deref())))
            });
            let title = match idx {
                0 => "Left Column".to_string(),
                1 => "Right Column".to_string(),
                _ => format!("Column {}", idx + 1),
            };
            if column_scrollable.get(idx).copied().unwrap_or(false) {
                push_subsection(&mut lines, &format!("{} (Scrollable)", title));
            } else {
                push_subsection(&mut lines, &title);
            }
            let mut last_render_entry: Option<MarkdownEntry> = None;
            let mut last_line_index: Option<usize> = None;
            for entry in column {
                if !entry.visible {
                    continue;
                }
                if last_render_entry
                    .as_ref()
                    .is_some_and(|prev| entries_dedupe_match(prev, &entry))
                {
                    if last_render_entry
                        .as_ref()
                        .is_some_and(|prev| is_ocr_id(prev.id.as_deref()))
                        && !is_ocr_id(entry.id.as_deref())
                    {
                        let mut replacement = entry.label.clone();
                        if let Some(id) = entry.id.as_deref().filter(|v| !v.trim().is_empty()) {
                            replacement.push_str(&format!(" #{id}"));
                        }
                        if let Some(checked) =
                            entry.checked.as_deref().filter(|v| !v.trim().is_empty())
                        {
                            replacement.push_str(&format!(" [checked={checked}]"));
                        }
                        if let Some(idx) = last_line_index {
                            if let Some(line) = lines.get_mut(idx) {
                                *line = replacement;
                            }
                            last_render_entry = Some(entry.clone());
                        }
                    }
                    continue;
                }
                let mut line = entry.label.clone();
                if let Some(id) = entry.id.as_deref().filter(|v| !v.trim().is_empty()) {
                    line.push_str(&format!(" #{id}"));
                }
                if let Some(checked) = entry.checked.as_deref().filter(|v| !v.trim().is_empty()) {
                    line.push_str(&format!(" [checked={checked}]"));
                }
                lines.push(line);
                last_render_entry = Some(entry);
                last_line_index = Some(lines.len().saturating_sub(1));
            }
        }
    }
    if has_all_windows {
        append_compact_windows_section_with_title(&mut lines, "All Windows", &all_windows);
    }
    lines.join("\n")
}

fn render_generic_markdown(command: &Command, value: &serde_json::Value) -> String {
    let profile = GenericMarkdownProfile::for_command(command);
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
    push_kv_line(&mut lines, "request_id", request_id);
    if let Some(hint) = value
        .get("hint")
        .and_then(serde_json::Value::as_str)
        .filter(|v| !v.trim().is_empty())
    {
        push_kv_line(&mut lines, "hint", hint);
    }

    let result = value.get("result").cloned().unwrap_or_default();
    if profile.promote_window_id {
        if let Some(window_id) = result
            .get("window_id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            push_kv_line(&mut lines, "window_id", window_id);
        }
    }
    if profile.promote_clipboard_text {
        if let Some(text) = result
            .get("text")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
        {
            push_kv_line(&mut lines, "text", text);
        }
    }
    if let Some(message) = result.get("message").and_then(serde_json::Value::as_str) {
        push_section(&mut lines, "Result");
        lines.push(message.to_string());
        return lines.join("\n");
    }

    if let Some(obj) = result.as_object() {
        let mut scalar_lines: Vec<String> = Vec::new();
        let mut windows_for_section: Option<Vec<serde_json::Value>> = None;
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        for k in keys {
            let Some(v) = obj.get(k) else {
                continue;
            };
            if k == "observe" || k == "click_target" {
                continue;
            }
            if profile.promote_hidden_apps && k == "hidden_apps" {
                if let Some(summary) = compact_value_summary(v) {
                    push_kv_line(&mut lines, "hidden_apps", summary);
                }
                continue;
            }
            if profile.skip_result_key(k) {
                continue;
            }
            if k == "windows" {
                if let Some(items) = v.as_array() {
                    windows_for_section = Some(items.clone());
                }
                continue;
            }
            if k == "window" {
                if append_window_result_lines(v, &mut scalar_lines) {
                    continue;
                }
            }
            if let Some(bounds_summary) = summarize_bounds_value(v) {
                push_kv_line(&mut scalar_lines, k, bounds_summary);
                continue;
            }
            if let Some(summary) = permission_state_summary(v).or_else(|| compact_value_summary(v))
            {
                push_kv_line(&mut scalar_lines, k, summary);
            }
        }
        if !scalar_lines.is_empty() {
            if profile.promote_result_to_top {
                lines.extend(scalar_lines);
            } else {
                push_section(&mut lines, "Result");
                lines.extend(scalar_lines);
            }
        }
        if let Some(windows) = windows_for_section {
            append_windows_section(&mut lines, &windows);
        }
    }

    if let Some(target) = result
        .get("click_target")
        .and_then(serde_json::Value::as_object)
    {
        push_section(&mut lines, "Click Target");
        if let Some(id) = target
            .get("id")
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.trim().is_empty())
        {
            push_kv_line(&mut lines, "id", id);
        }
        if let Some(text) = target
            .get("text")
            .and_then(serde_json::Value::as_str)
            .filter(|v| !v.trim().is_empty())
        {
            push_kv_line(&mut lines, "text", text);
        }
    }

    append_observe_sections(&mut lines, &result);
    lines.join("\n")
}

#[derive(Debug, Clone, Copy, Default)]
struct GenericMarkdownProfile {
    promote_result_to_top: bool,
    promote_window_id: bool,
    promote_clipboard_text: bool,
    promote_hidden_apps: bool,
    suppress_app_state: bool,
    suppress_focused: bool,
}

impl GenericMarkdownProfile {
    fn for_command(command: &Command) -> Self {
        let mut profile = Self::default();
        match command {
            Command::WindowFocus { .. } => {
                profile.promote_result_to_top = true;
                profile.suppress_focused = true;
            }
            Command::WindowBounds { .. } => {
                profile.promote_result_to_top = true;
            }
            Command::OpenApp { .. } => {
                profile.promote_window_id = true;
            }
            Command::AppShow { .. } => {
                profile.promote_window_id = true;
                profile.suppress_app_state = true;
            }
            Command::AppHide { .. } => {
                profile.suppress_app_state = true;
            }
            Command::AppIsolate { .. } => {
                profile.promote_window_id = true;
                profile.promote_hidden_apps = true;
                profile.suppress_app_state = true;
            }
            Command::ClipboardRead => {
                profile.promote_clipboard_text = true;
            }
            _ => {}
        }
        profile
    }

    fn skip_result_key(&self, key: &str) -> bool {
        if self.suppress_app_state && (key == "app" || key == "state") {
            return true;
        }
        if self.promote_window_id && key == "window_id" {
            return true;
        }
        if self.promote_clipboard_text && key == "text" {
            return true;
        }
        if self.suppress_focused && key == "focused" {
            return true;
        }
        false
    }
}

fn append_observe_sections(lines: &mut Vec<String>, result: &serde_json::Value) {
    let Some(observe) = result.get("observe").and_then(serde_json::Value::as_object) else {
        return;
    };
    push_section(lines, "Observe");
    for key in [
        "stability",
        "changed",
        "elapsed_ms",
        "settle_ms",
        "active_window_id",
        "active_window_changed",
        "focus_changed",
        "focused_element_id",
    ] {
        if let Some(summary) = observe.get(key).and_then(compact_value_summary) {
            push_kv_line(lines, key, summary);
        }
    }
    let Some(tokens_delta) = observe
        .get("tokens_delta")
        .and_then(serde_json::Value::as_object)
    else {
        return;
    };
    append_tokens_delta_section(lines, "Added", tokens_delta.get("added"));
    append_tokens_delta_section(lines, "Changed", tokens_delta.get("changed"));
    append_tokens_delta_section(lines, "Removed", tokens_delta.get("removed"));
}

fn append_tokens_delta_section(
    lines: &mut Vec<String>,
    title: &str,
    tokens: Option<&serde_json::Value>,
) {
    let Some(items) = tokens.and_then(serde_json::Value::as_array) else {
        return;
    };
    if items.is_empty() {
        return;
    }
    push_section(lines, title);
    if title == "Removed" || title == "Added" {
        append_tokens_delta_columns(lines, items);
        return;
    }
    for item in items {
        if title == "Changed" {
            let before = item.get("before").unwrap_or(item);
            let after = item.get("after").unwrap_or(item);
            lines.push(format!(
                "{} -> {}",
                format_token_delta_side(before),
                format_token_delta_side(after)
            ));
            continue;
        }
        lines.push(format_token_delta_side(item));
    }
}

fn append_tokens_delta_columns(lines: &mut Vec<String>, items: &[serde_json::Value]) {
    #[derive(Debug, Default)]
    struct DeltaColumn {
        anchor_x: f64,
        entries: Vec<MarkdownEntry>,
    }

    let mut entries: Vec<MarkdownEntry> = items
        .iter()
        .filter_map(markdown_entry_from_token_delta)
        .collect();
    if entries.is_empty() {
        for item in items {
            lines.push(format_token_delta_side(item));
        }
        return;
    }
    entries.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
    let mut columns: Vec<DeltaColumn> = Vec::new();
    let column_split = 140.0_f64;
    for entry in entries {
        if !columns.is_empty() {
            let idx = columns.len() - 1;
            let anchor_x = columns[idx].anchor_x;
            if (entry.x - anchor_x).abs() <= column_split {
                columns[idx].entries.push(entry);
                continue;
            }
        }
        columns.push(DeltaColumn {
            anchor_x: entry.x,
            entries: vec![entry],
        });
    }
    for (idx, column) in columns.into_iter().enumerate() {
        let mut column = column.entries;
        column.sort_by(|a, b| a.y.total_cmp(&b.y).then_with(|| a.x.total_cmp(&b.x)));
        let title = match idx {
            0 => "Left Column".to_string(),
            1 => "Right Column".to_string(),
            _ => format!("Column {}", idx + 1),
        };
        push_subsection(lines, &title);
        let mut last_render_entry: Option<MarkdownEntry> = None;
        for entry in column {
            if last_render_entry
                .as_ref()
                .is_some_and(|prev| entries_dedupe_match(prev, &entry))
            {
                continue;
            }
            let mut line = entry.label.clone();
            if let Some(id) = entry.id.as_deref().filter(|v| !v.trim().is_empty()) {
                line.push_str(&format!(" #{id}"));
            }
            lines.push(line);
            last_render_entry = Some(entry);
        }
    }
}

fn format_token_delta_side(token: &serde_json::Value) -> String {
    let text = token
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .unwrap_or("element");
    let id_suffix = token
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(|id| format!(" #{id}"))
        .unwrap_or_default();
    format!("{text}{id_suffix}")
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
    let mut lines = vec![format!("# {}", to_title_case(title)), String::new()];
    push_kv_line(&mut lines, "request_id", request_id);
    push_kv_line(&mut lines, "code", code);
    push_kv_line(&mut lines, "message", message);
    push_kv_line(&mut lines, "retryable", retryable.to_string());
    lines.join("\n")
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
    width: f64,
    height: f64,
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
    let width = bbox[2].as_f64().unwrap_or(0.0);
    let height = bbox[3].as_f64().unwrap_or(0.0);
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
        width,
        height,
        scrollable,
        checked,
        visible,
    })
}

fn markdown_entry_from_token_delta(token: &serde_json::Value) -> Option<MarkdownEntry> {
    let text = token
        .get("text")
        .and_then(serde_json::Value::as_str)
        .map(normalize_single_line)
        .filter(|v| !v.trim().is_empty())?;
    let bbox = token.get("bbox")?.as_array()?;
    if bbox.len() != 4 {
        return None;
    }
    let x = bbox[0].as_f64().unwrap_or(0.0);
    let y = bbox[1].as_f64().unwrap_or(0.0);
    let width = bbox[2].as_f64().unwrap_or(0.0);
    let height = bbox[3].as_f64().unwrap_or(0.0);
    let id = token
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(str::to_string);
    Some(MarkdownEntry {
        label: text,
        id,
        x,
        y,
        width,
        height,
        scrollable: false,
        checked: None,
        visible: true,
    })
}

fn compact_value_summary(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::Bool(v) => Some(v.to_string()),
        serde_json::Value::Number(v) => Some(v.to_string()),
        serde_json::Value::String(v) => {
            if v.trim().is_empty() {
                None
            } else {
                Some(v.clone())
            }
        }
        serde_json::Value::Array(items) => Some(format!("{} items", items.len())),
        serde_json::Value::Object(map) => Some(format!("{} fields", map.len())),
    }
}

fn is_ocr_id(id: Option<&str>) -> bool {
    id.map(str::trim)
        .filter(|v| !v.is_empty())
        .is_some_and(|v| v.starts_with("ocr_"))
}

fn entries_dedupe_match(a: &MarkdownEntry, b: &MarkdownEntry) -> bool {
    if normalize_single_line(&a.label).to_ascii_lowercase()
        != normalize_single_line(&b.label).to_ascii_lowercase()
    {
        return false;
    }
    if a.checked.as_deref().unwrap_or_default() != b.checked.as_deref().unwrap_or_default() {
        return false;
    }
    bounds_overlap_enough(a, b)
}

fn bounds_overlap_enough(a: &MarkdownEntry, b: &MarkdownEntry) -> bool {
    let ax2 = a.x + a.width.max(0.0);
    let ay2 = a.y + a.height.max(0.0);
    let bx2 = b.x + b.width.max(0.0);
    let by2 = b.y + b.height.max(0.0);
    let ix = (ax2.min(bx2) - a.x.max(b.x)).max(0.0);
    let iy = (ay2.min(by2) - a.y.max(b.y)).max(0.0);
    if ix <= 0.0 || iy <= 0.0 {
        return false;
    }
    let intersection = ix * iy;
    let area_a = (a.width.max(0.0) * a.height.max(0.0)).max(1.0);
    let area_b = (b.width.max(0.0) * b.height.max(0.0)).max(1.0);
    let overlap_vs_smaller = intersection / area_a.min(area_b);
    overlap_vs_smaller >= 0.5
}

fn permission_state_summary(value: &serde_json::Value) -> Option<String> {
    let obj = value.as_object()?;
    let granted = obj.get("granted")?.as_bool()?;
    Some(granted.to_string())
}

fn append_window_result_lines(value: &serde_json::Value, lines: &mut Vec<String>) -> bool {
    let Some(window) = value.as_object() else {
        return false;
    };
    let mut added = false;
    if let Some(v) = window
        .get("title")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        push_kv_line(lines, "window_title", v);
        added = true;
    }
    if let Some(v) = window
        .get("id")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        push_kv_line(lines, "window_id", v);
        added = true;
    }
    if let Some(v) = window
        .get("app")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        push_kv_line(lines, "window_app", v);
        added = true;
    }
    if let Some(size_summary) = window
        .get("bounds")
        .and_then(summarize_bounds_size)
        .filter(|v| !v.is_empty())
    {
        push_kv_line(lines, "window_size", size_summary);
        added = true;
    }
    added
}

fn summarize_bounds_value(value: &serde_json::Value) -> Option<String> {
    let bounds = value.as_object()?;
    let x = bounds.get("x").and_then(serde_json::Value::as_f64)?;
    let y = bounds.get("y").and_then(serde_json::Value::as_f64)?;
    let width = bounds.get("width").and_then(serde_json::Value::as_f64)?;
    let height = bounds.get("height").and_then(serde_json::Value::as_f64)?;
    Some(format!("{width:.0}x{height:.0} @ {x:.0},{y:.0}"))
}

fn summarize_bounds_size(value: &serde_json::Value) -> Option<String> {
    let bounds = value.as_object()?;
    let width = bounds.get("width").and_then(serde_json::Value::as_f64)?;
    let height = bounds.get("height").and_then(serde_json::Value::as_f64)?;
    Some(format!("{width:.0}x{height:.0}"))
}

fn append_windows_section(lines: &mut Vec<String>, windows: &[serde_json::Value]) {
    append_windows_section_with_title(lines, "Windows", windows);
}

fn append_windows_section_with_title(
    lines: &mut Vec<String>,
    title: &str,
    windows: &[serde_json::Value],
) {
    push_section(lines, title);
    if windows.is_empty() {
        lines.push("None".to_string());
        return;
    }
    for window in windows {
        let Some(obj) = window.as_object() else {
            continue;
        };
        let app = obj
            .get("app")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("unknown");
        let id = obj
            .get("id")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("unknown");
        let title = obj
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("(untitled)");
        let size = obj
            .get("bounds")
            .and_then(summarize_bounds_size)
            .unwrap_or_else(|| "unknown".to_string());
        push_subsection(lines, title);
        push_plain_field_line(lines, "window_id", id);
        push_plain_field_line(lines, "app", app);
        push_plain_field_line(lines, "window_size", size);
    }
}

fn append_compact_windows_section_with_title(
    lines: &mut Vec<String>,
    title: &str,
    windows: &[serde_json::Value],
) {
    push_section(lines, title);
    if windows.is_empty() {
        lines.push("None".to_string());
        return;
    }
    for window in windows {
        let Some(obj) = window.as_object() else {
            continue;
        };
        let app = obj
            .get("app")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("unknown");
        let title = obj
            .get("title")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("(untitled)");
        lines.push(format!("- {app}: {title}"));
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
    use desktop_core::protocol::{Command, ObserveOptions, PointerButton, ResponseEnvelope};
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
            "window list markdown prints one section per window titled by window_title"
        );
        assert_eq!(rendered["ok"], true);
        assert_eq!(rendered["result"]["windows"], json!([]));
    }

    #[test]
    fn active_window_hint_uses_observe_active_window_id_when_available() {
        let command = Command::PointerClick {
            x: 1,
            y: 1,
            absolute: false,
            button: PointerButton::Left,
            observe: ObserveOptions::default(),
            active_window: false,
            active_window_id: None,
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "observe": {
                    "active_window_id": "notes_123abc"
                }
            }),
        );

        let rendered = render_response(&command, &response, false);
        assert_eq!(
            rendered["hint"],
            "use --active-window notes_123abc to avoid acting in the wrong window"
        );
    }

    #[test]
    fn active_window_hint_omitted_when_window_id_unavailable() {
        let command = Command::PointerClick {
            x: 1,
            y: 1,
            absolute: false,
            button: PointerButton::Left,
            observe: ObserveOptions::default(),
            active_window: false,
            active_window_id: None,
        };
        let response = ResponseEnvelope::success("r1", json!({}));

        let rendered = render_response(&command, &response, false);
        assert!(rendered.get("hint").is_none());
    }

    #[test]
    fn open_app_renders_window_hint_with_window_id() {
        let command = Command::OpenApp {
            name: "Notes".to_string(),
            args: vec![],
            wait: false,
            timeout_ms: None,
            background: false,
        };
        let response = ResponseEnvelope::success("r1", json!({ "window_id": "notes_859606" }));

        let rendered = render_response(&command, &response, false);
        assert_eq!(
            rendered["hint"],
            "use --active-window notes_859606 in follow-up commands to target this app window"
        );
    }

    #[test]
    fn open_app_markdown_promotes_window_id_to_top_section() {
        let command = Command::OpenApp {
            name: "Notes".to_string(),
            args: vec![],
            wait: false,
            timeout_ms: None,
            background: false,
        };
        let response = ResponseEnvelope::success("r1", json!({ "window_id": "notes_859606" }));

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("- request_id: r1"));
        assert!(markdown.contains("- hint: use --active-window notes_859606"));
        assert!(markdown.contains("- window_id: notes_859606"));
        assert!(!markdown.contains("## Result"));
    }

    #[test]
    fn app_hide_markdown_omits_app_state_result_block() {
        let command = Command::AppHide {
            name: "Notes".to_string(),
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "app": "Notes",
                "state": "hidden"
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("- request_id: r1"));
        assert!(!markdown.contains("## Result"));
        assert!(!markdown.contains("- app: Notes"));
        assert!(!markdown.contains("- state: hidden"));
    }

    #[test]
    fn app_show_markdown_omits_app_state_result_block() {
        let command = Command::AppShow {
            name: "Notes".to_string(),
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "app": "Notes",
                "state": "shown",
                "window_id": "notes_859606"
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("- request_id: r1"));
        assert!(markdown.contains("- window_id: notes_859606"));
        assert!(!markdown.contains("## Result"));
        assert!(!markdown.contains("- app: Notes"));
        assert!(!markdown.contains("- state: shown"));
    }

    #[test]
    fn app_isolate_promotes_hidden_apps_and_omits_app_state_result_block() {
        let command = Command::AppIsolate {
            name: "Notes".to_string(),
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "app": "Notes",
                "hidden_apps": 1,
                "state": "isolated",
                "window_id": "notes_859606"
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("- request_id: r1"));
        assert!(markdown.contains("- window_id: notes_859606"));
        assert!(markdown.contains("- hidden_apps: 1"));
        assert!(!markdown.contains("## Result"));
        assert!(!markdown.contains("- app: Notes"));
        assert!(!markdown.contains("- state: isolated"));
    }

    #[test]
    fn clipboard_read_promotes_text_and_omits_result_block() {
        let command = Command::ClipboardRead;
        let response = ResponseEnvelope::success("r1", json!({ "text": "clp" }));

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("- request_id: r1"));
        assert!(markdown.contains("- text: clp"));
        assert!(!markdown.contains("## Result"));
    }

    #[test]
    fn tokenize_markdown_omits_positions_and_keeps_ids() {
        let command = Command::ScreenTokenize {
            overlay_out_path: None,
            window_query: None,
            screenshot_path: None,
            journal: false,
            list_all_windows: false,
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
        assert!(markdown.contains("- window_title: Permissions"));
        assert!(markdown.contains("- window_id: system_settings_1aca92"));
        assert!(markdown.contains("## Window"));
        assert!(markdown.contains("### Left Column"));
        assert!(markdown.contains("### Right Column"));
        assert!(markdown.contains("Search #button_search"));
        assert!(markdown.contains("Control Centre\\nDesktop & Dock\\nDisplays #text_sidebar"));
        assert!(markdown.contains("Recording #axid_toggle_recording [checked=true]"));
        assert!(!markdown.contains("12"));
        assert!(!markdown.contains("34"));
        assert!(!markdown.contains("bbox"));
        assert!(!markdown.contains("## Windows"));
        assert!(!markdown.lines().any(|line| line == "## Text"));
        assert!(!markdown.contains("```text"));
    }

    #[test]
    fn tokenize_markdown_merges_overlapping_element_and_ocr_entries() {
        let command = Command::ScreenTokenize {
            overlay_out_path: None,
            window_query: None,
            screenshot_path: None,
            journal: false,
            list_all_windows: false,
            active_window: false,
            active_window_id: None,
            region: None,
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "windows": [{
                    "id": "settings_01",
                    "title": "Settings",
                    "app": "System Settings",
                    "elements": [
                        { "id": "ocr_wifi", "role": "text", "text": "Wi-Fi", "bbox": [10, 10, 80, 16] },
                        { "id": "element_wifi", "role": "button", "text": "Wi-Fi", "bbox": [11, 12, 82, 18] },
                        { "id": "element_bluetooth", "role": "button", "text": "Bluetooth", "bbox": [10, 40, 100, 16] }
                    ]
                }]
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("Wi-Fi #element_wifi"));
        assert!(!markdown.contains("Wi-Fi #ocr_wifi"));
        assert!(markdown.contains("Bluetooth #element_bluetooth"));
    }

    #[test]
    fn journal_tokenize_redacts_ids_and_request_metadata() {
        let command = Command::ScreenTokenize {
            overlay_out_path: None,
            window_query: None,
            screenshot_path: None,
            journal: true,
            list_all_windows: false,
            active_window: false,
            active_window_id: None,
            region: None,
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "window_id": "settings_01",
                "hint": "sample hint",
                "windows": [{
                    "id": "settings_01",
                    "title": "Permissions",
                    "app": "System Settings",
                    "elements": [{
                        "id": "axid_toggle_recording",
                        "role": "checkbox",
                        "text": "Recording"
                    }]
                }],
                "all_windows": [{
                    "id": "settings_01",
                    "title": "Permissions",
                    "app": "System Settings"
                }]
            }),
        );

        let rendered = render_response(&command, &response, false);
        assert!(rendered.get("request_id").is_none());
        assert!(rendered.get("hint").is_none());
        let result = rendered.get("result").expect("result");
        assert!(result.get("window_id").is_none());
        assert!(result.get("hint").is_none());
        let windows = result
            .get("windows")
            .and_then(serde_json::Value::as_array)
            .expect("windows array");
        let first = windows.first().expect("first window");
        assert!(first.get("id").is_none());
        let first_element = first
            .get("elements")
            .and_then(serde_json::Value::as_array)
            .and_then(|v| v.first())
            .expect("first element");
        assert!(first_element.get("id").is_none());
        let all_windows = result
            .get("all_windows")
            .and_then(serde_json::Value::as_array)
            .expect("all_windows array");
        let listed = all_windows.first().expect("first listed window");
        assert!(listed.get("id").is_none());
    }

    #[test]
    fn tokenize_markdown_includes_all_windows_section_when_present() {
        let command = Command::ScreenTokenize {
            overlay_out_path: None,
            window_query: None,
            screenshot_path: None,
            journal: false,
            list_all_windows: true,
            active_window: false,
            active_window_id: None,
            region: None,
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "windows": [{
                    "id": "notes_01",
                    "title": "Notes",
                    "app": "Notes",
                    "elements": []
                }],
                "all_windows": [
                    {
                        "id": "notes_01",
                        "title": "Notes",
                        "app": "Notes",
                        "bounds": {"x": 10, "y": 20, "width": 1200, "height": 800}
                    },
                    {
                        "id": "term_02",
                        "title": "Terminal",
                        "app": "Terminal",
                        "bounds": {"x": 30, "y": 40, "width": 900, "height": 700}
                    }
                ]
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("## All Windows"));
        assert!(markdown.contains("- Notes: Notes"));
        assert!(markdown.contains("- Terminal: Terminal"));
    }

    #[test]
    fn permissions_markdown_shows_granted_bools() {
        let command = Command::PermissionsCheck;
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "accessibility": { "granted": true },
                "screen_recording": { "granted": false }
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("- accessibility: true"));
        assert!(markdown.contains("- screen_recording: false"));
        assert!(!markdown.contains("fields"));
    }

    #[test]
    fn window_focus_markdown_summarizes_window_and_plain_bool() {
        let command = Command::WindowFocus {
            title: "Notes".to_string(),
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "focused": true,
                "window": {
                    "id": "notes_8bec33",
                    "app": "Notes",
                    "title": "Shopping list",
                    "bounds": {
                        "x": 1430,
                        "y": 175,
                        "width": 1048,
                        "height": 680
                    },
                    "frontmost": true,
                    "visible": true
                }
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("- window_app: Notes"));
        assert!(markdown.contains("- window_id: notes_8bec33"));
        assert!(markdown.contains("- window_title: Shopping list"));
        assert!(markdown.contains("- window_size: 1048x680"));
        assert!(!markdown.contains("`true`"));
        assert!(!markdown.contains("- window.app:"));
        assert!(!markdown.contains("## Result"));
    }

    #[test]
    fn window_bounds_markdown_summarizes_window_and_bounds() {
        let command = Command::WindowBounds {
            title: "Notes".to_string(),
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "window": {
                    "id": "notes_8bec33",
                    "app": "Notes",
                    "title": "Shopping list",
                    "bounds": {
                        "x": 1430,
                        "y": 175,
                        "width": 1048,
                        "height": 680
                    },
                    "frontmost": true,
                    "visible": true
                }
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("- window_id: notes_8bec33"));
        assert!(markdown.contains("- window_title: Shopping list"));
        assert!(markdown.contains("- window_size: 1048x680"));
        assert!(!markdown.contains("## Result"));
    }

    #[test]
    fn window_list_markdown_renders_windows_section() {
        let command = Command::WindowList;
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "windows": [
                    {
                        "id": "notes_95b2a1",
                        "app": "Notes",
                        "title": "Notes",
                        "bounds": { "x": 1430, "y": 175, "width": 1048, "height": 680 },
                        "frontmost": true,
                        "visible": true
                    },
                    {
                        "id": "settings_01",
                        "app": "System Settings",
                        "title": "Screen & System Audio Recording",
                        "bounds": { "x": 120, "y": 70, "width": 1100, "height": 780 },
                        "frontmost": false,
                        "visible": true
                    }
                ]
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("## Windows"));
        assert!(markdown.contains("### Notes"));
        assert!(markdown.contains("window_id: notes_95b2a1"));
        assert!(markdown.contains("app: Notes"));
        assert!(markdown.contains("window_size: 1048x680"));
        assert!(markdown.contains("### Screen & System Audio Recording"));
        assert!(markdown.contains("window_id: settings_01"));
        assert!(markdown.contains("app: System Settings"));
        assert!(markdown.contains("window_size: 1100x780"));
        assert!(!markdown.contains("- windows: 2 items"));
    }

    #[test]
    fn pointer_click_markdown_includes_observe_delta_sections() {
        let command = Command::PointerClickId {
            id: "ocr_13".to_string(),
            button: PointerButton::Left,
            active_window: false,
            active_window_id: Some("news_bb9921".to_string()),
            observe: ObserveOptions::default(),
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "click_target": {
                    "id": "ocr_13",
                    "source": "vision_ocr",
                    "text": "House ballroom project",
                    "x": 765,
                    "y": 387
                },
                "observe": {
                    "stability": "timeout",
                    "changed": true,
                    "elapsed_ms": 3487,
                    "settle_ms": 3487,
                    "active_window_id": "news_bb9921",
                    "active_window_changed": false,
                    "focus_changed": false,
                    "focused_element_id": "ax_axstatictext",
                    "tokens_delta": {
                        "added": [
                            {
                                "source": "vision_ocr",
                                "text": "halt to White House ballroom project",
                                "confidence": 1.0,
                                "bbox": [361, 122, 725, 46]
                            }
                        ],
                        "changed": [],
                        "removed": [
                            {
                                "source": "vision_ocr",
                                "text": "House ballroom project",
                                "confidence": 1.0,
                                "bbox": [451, 283, 145, 14]
                            }
                        ]
                    }
                }
            }),
        );

        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("## Click Target"));
        assert!(markdown.contains("- request_id: r1"));
        assert!(markdown.contains("- id: ocr_13"));
        assert!(markdown.contains("## Observe"));
        assert!(markdown.contains("- stability: timeout"));
        assert!(markdown.contains("## Added"));
        assert!(markdown.contains("halt to White House ballroom project"));
        assert!(markdown.contains("## Removed"));
        assert!(markdown.contains("House ballroom project"));
    }

    #[test]
    fn pointer_click_markdown_renders_changed_before_after() {
        let command = Command::PointerClickId {
            id: "ocr_13".to_string(),
            button: PointerButton::Left,
            active_window: false,
            active_window_id: Some("news_bb9921".to_string()),
            observe: ObserveOptions::default(),
        };
        let response = ResponseEnvelope::success(
            "r1",
            json!({
                "click_target": { "id": "ocr_13" },
                "observe": {
                    "tokens_delta": {
                        "added": [],
                        "removed": [],
                        "changed": [
                            {
                                "before": {
                                    "source": "vision_ocr",
                                    "text": "old value",
                                    "bbox": [10, 20, 30, 40]
                                },
                                "after": {
                                    "source": "vision_ocr",
                                    "text": "new value",
                                    "bbox": [11, 21, 30, 40]
                                }
                            }
                        ]
                    }
                }
            }),
        );
        let markdown = render_markdown_response(&command, &response, false);
        assert!(markdown.contains("## Changed"));
        assert!(markdown.contains("old value"));
        assert!(markdown.contains("new value"));
        assert!(markdown.contains("->"));
    }
}

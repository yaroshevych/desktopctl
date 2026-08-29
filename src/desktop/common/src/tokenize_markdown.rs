use serde_json::Value;

fn push_kv(lines: &mut Vec<String>, key: &str, value: impl AsRef<str>) {
    lines.push(format!("- {key}: {}", value.as_ref()));
}

fn push_section(lines: &mut Vec<String>, title: &str) {
    lines.push(String::new());
    lines.push(format!("## {title}"));
}

fn push_subsection(lines: &mut Vec<String>, title: &str) {
    lines.push(format!("### {title}"));
}

pub fn render_tokenize_markdown(value: &Value, include_all_hint: bool) -> String {
    let ok = value.get("ok").and_then(Value::as_bool).unwrap_or(false);
    if !ok {
        return render_error(value);
    }
    let request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let hint = value.get("hint").and_then(Value::as_str);
    let result = value.get("result");
    let truncated = include_all_hint
        && result
            .and_then(|v| v.get("truncated"))
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let windows = result
        .and_then(|v| v.get("windows"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_all_windows = result.and_then(|v| v.get("all_windows")).is_some();
    let all_windows = result
        .and_then(|v| v.get("all_windows"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let has_offscreen_elements = include_all_hint
        && windows.iter().any(|window| {
            let Some(bounds) = window.get("bounds") else {
                return false;
            };
            let width = bounds.get("width").and_then(Value::as_f64).unwrap_or(0.0);
            let height = bounds.get("height").and_then(Value::as_f64).unwrap_or(0.0);
            window
                .get("elements")
                .and_then(Value::as_array)
                .is_some_and(|elements| {
                    elements.iter().any(|element| {
                        let source = element.get("source").and_then(Value::as_str).unwrap_or("");
                        let Some(bbox) = element.get("bbox").and_then(Value::as_array) else {
                            return false;
                        };
                        if !source.starts_with("accessibility_ax:") || bbox.len() != 4 {
                            return false;
                        }
                        let x = bbox[0].as_f64().unwrap_or(0.0);
                        let y = bbox[1].as_f64().unwrap_or(0.0);
                        let w = bbox[2].as_f64().unwrap_or(0.0);
                        let h = bbox[3].as_f64().unwrap_or(0.0);
                        !(x < width && 0.0 < x + w && y < height && 0.0 < y + h)
                    })
                })
        });

    let mut lines = vec!["# Screen Tokenize".to_string(), String::new()];
    push_kv(&mut lines, "request_id", request_id);
    if let Some(window) = windows.first() {
        if let Some(text) = window
            .get("app")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
        {
            push_kv(&mut lines, "app", text.trim());
        }
        if let Some(bounds) = window.get("bounds") {
            if let (Some(width), Some(height)) = (
                bounds.get("width").and_then(Value::as_f64),
                bounds.get("height").and_then(Value::as_f64),
            ) {
                push_kv(&mut lines, "window_size", format!("{width:.0}x{height:.0}"));
            }
        }
        if let Some(text) = window
            .get("title")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
        {
            push_kv(&mut lines, "window_title", text.trim());
        }
        if let Some(text) = window
            .get("id")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
        {
            push_kv(&mut lines, "window_id", text.trim());
        }
    }
    if truncated {
        push_kv(
            &mut lines,
            "warning",
            "--all result was truncated by AX traversal limits; some elements may be missing",
        );
    } else if has_offscreen_elements {
        push_kv(
            &mut lines,
            "hint",
            "off-screen element IDs came from --all; use pointer scroll deltas with the cursor in the target scroll area, then re-run screen tokenize --all",
        );
    } else if let Some(hint) = hint.filter(|v| !v.trim().is_empty()) {
        push_kv(&mut lines, "hint", hint);
    }
    if windows.is_empty() {
        push_section(&mut lines, "Window (unknown)");
        lines.push("None".to_string());
    }

    let single_window = windows.len() == 1;
    for (window_idx, window) in windows.into_iter().enumerate() {
        let id = window
            .get("id")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        let title = window
            .get("title")
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
            .unwrap_or("untitled");
        if single_window {
            push_section(&mut lines, "Window");
        } else {
            push_section(&mut lines, &format!("Window {}", window_idx + 1));
            push_kv(&mut lines, "window_title", title);
            push_kv(&mut lines, "window_id", id);
        }
        let mut entries: Vec<Entry> = window
            .get("elements")
            .and_then(Value::as_array)
            .map(|items| items.iter().filter_map(entry_from_value).collect())
            .unwrap_or_default();
        if entries.is_empty() {
            lines.push("No elements".to_string());
            continue;
        }
        entries.sort_by(|a, b| a.x.total_cmp(&b.x).then_with(|| a.y.total_cmp(&b.y)));
        let mut columns: Vec<Vec<Entry>> = Vec::new();
        let mut scrollable: Vec<bool> = Vec::new();
        for entry in entries {
            if let Some(last_column) = columns.last() {
                let last_x = last_column.last().map(|e| e.x).unwrap_or(entry.x);
                if (entry.x - last_x).abs() <= 140.0 {
                    let idx = columns.len() - 1;
                    scrollable[idx] |= entry.scrollable;
                    columns[idx].push(entry);
                    continue;
                }
            }
            scrollable.push(entry.scrollable);
            columns.push(vec![entry]);
        }
        for (idx, mut column) in columns.into_iter().enumerate() {
            column.sort_by(|a, b| {
                a.y.total_cmp(&b.y)
                    .then_with(|| a.x.total_cmp(&b.x))
                    .then_with(|| is_ocr(a.id.as_deref()).cmp(&is_ocr(b.id.as_deref())))
            });
            let name = match idx {
                0 => "Left Column".to_string(),
                1 => "Right Column".to_string(),
                _ => format!("Column {}", idx + 1),
            };
            let subsection = if scrollable.get(idx).copied().unwrap_or(false) {
                format!("{name} (Scrollable)")
            } else {
                name
            };
            push_subsection(&mut lines, &subsection);
            let mut previous: Option<Entry> = None;
            let mut previous_line = None;
            for entry in column {
                if !entry.visible {
                    continue;
                }
                if let Some(prev) = previous.as_ref().filter(|prev| duplicate(prev, &entry)) {
                    if is_ocr(prev.id.as_deref()) && !is_ocr(entry.id.as_deref()) {
                        if let Some(line) = previous_line.and_then(|i| lines.get_mut(i)) {
                            *line = entry.render();
                        }
                        previous = Some(entry);
                    }
                    continue;
                }
                lines.push(entry.render());
                previous_line = Some(lines.len() - 1);
                previous = Some(entry);
            }
        }
    }
    if has_all_windows {
        push_section(&mut lines, "All Windows");
        if all_windows.is_empty() {
            lines.push("None".to_string());
        } else {
            for window in all_windows {
                let app = window
                    .get("app")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown");
                let title = window
                    .get("title")
                    .and_then(Value::as_str)
                    .unwrap_or("(untitled)");
                lines.push(format!("- {app}: {title}"));
            }
        }
    }
    lines.join("\n")
}

fn render_error(value: &Value) -> String {
    let error = value.get("error").cloned().unwrap_or_default();
    let request_id = value
        .get("request_id")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let code = error
        .get("code")
        .and_then(Value::as_str)
        .unwrap_or("internal");
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("unknown error");
    let retryable = error
        .get("retryable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    format!(
        "# Screen Tokenize\n\n- request_id: {request_id}\n- code: {code}\n- message: {message}\n- retryable: {retryable}"
    )
}

#[derive(Clone)]
struct Entry {
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

impl Entry {
    fn render(&self) -> String {
        let mut line = self.label.clone();
        if let Some(id) = self.id.as_deref().filter(|v| !v.is_empty()) {
            line.push_str(&format!(" #{id}"));
        }
        if let Some(checked) = self.checked.as_deref().filter(|v| !v.is_empty()) {
            line.push_str(&format!(" [checked={checked}]"));
        }
        line
    }
}

fn entry_from_value(element: &Value) -> Option<Entry> {
    let label = ["text", "label", "name", "value"]
        .iter()
        .find_map(|key| {
            element
                .get(*key)
                .and_then(Value::as_str)
                .map(normalize)
                .filter(|v| !v.is_empty())
        })
        .unwrap_or_else(|| "element".to_string());
    let bbox = element.get("bbox")?.as_array()?;
    if bbox.len() != 4 {
        return None;
    }
    let role = element
        .get("role")
        .or_else(|| element.get("kind"))
        .or_else(|| element.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let source = element
        .get("source")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let visible = !(label.eq_ignore_ascii_case("element") && element.get("checked").is_none());
    Some(Entry {
        label,
        id: element
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        x: bbox[0].as_f64().unwrap_or(0.0),
        y: bbox[1].as_f64().unwrap_or(0.0),
        width: bbox[2].as_f64().unwrap_or(0.0),
        height: bbox[3].as_f64().unwrap_or(0.0),
        scrollable: element
            .get("scrollable")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || source.contains("axscroll")
            || role.contains("scroll"),
        checked: element
            .get("checked")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        visible,
    })
}

fn normalize(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .split('\n')
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\\n")
}

fn is_ocr(id: Option<&str>) -> bool {
    id.is_some_and(|id| id.trim().starts_with("ocr_"))
}

fn duplicate(a: &Entry, b: &Entry) -> bool {
    normalize(&a.label).eq_ignore_ascii_case(&normalize(&b.label))
        && a.checked.as_deref().unwrap_or_default() == b.checked.as_deref().unwrap_or_default()
        && overlap(a, b)
}

fn overlap(a: &Entry, b: &Entry) -> bool {
    let ix = ((a.x + a.width).min(b.x + b.width) - a.x.max(b.x)).max(0.0);
    let iy = ((a.y + a.height).min(b.y + b.height) - a.y.max(b.y)).max(0.0);
    ix > 0.0
        && iy > 0.0
        && ix * iy
            / (a.width.max(0.0) * a.height.max(0.0))
                .max(1.0)
                .min((b.width.max(0.0) * b.height.max(0.0)).max(1.0))
            >= 0.5
}

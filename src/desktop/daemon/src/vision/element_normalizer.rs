use desktop_core::protocol::{Bounds, ToggleState, TokenizeElement};
use std::collections::HashMap;

const MAX_ELEMENT_TEXT_CHARS: usize = 8192;
const MAX_ELEMENT_SLUG_CHARS: usize = 48;
const MAX_ELEMENT_ID_CHARS: usize = 72;
const POSITIONAL_ID_MARKER: &str = "__ord__";

pub struct ElementBuilder {
    element: TokenizeElement,
}

impl ElementBuilder {
    pub fn new() -> Self {
        Self {
            element: TokenizeElement {
                id: String::new(),
                kind: String::new(),
                bbox: [0.0, 0.0, 0.0, 0.0],
                has_border: None,
                text: None,
                text_truncated: None,
                confidence: None,
                scrollable: None,
                checked: None,
                source: String::new(),
            },
        }
    }

    pub fn bbox(mut self, bounds: Bounds) -> Self {
        self.element.bbox = [bounds.x, bounds.y, bounds.width, bounds.height];
        self
    }

    pub fn id(mut self, id: Option<String>) -> Self {
        self.element.id = id.unwrap_or_default();
        self
    }

    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.element.kind = kind.into();
        self
    }

    pub fn text(mut self, text: Option<String>) -> Self {
        match text {
            Some(v) => {
                let (cleaned, truncated) = sanitize_text(&v);
                self.element.text_truncated = truncated.then_some(true);
                self.element.text = (!cleaned.is_empty()).then_some(cleaned);
                if self.element.text.is_none() {
                    self.element.text_truncated = None;
                }
            }
            None => {
                self.element.text = None;
                self.element.text_truncated = None;
            }
        }
        self
    }

    pub fn source(mut self, source: impl Into<String>) -> Self {
        self.element.source = source.into();
        self
    }

    pub fn has_border(mut self, has_border: Option<bool>) -> Self {
        self.element.has_border = has_border;
        self
    }

    pub fn confidence(mut self, confidence: Option<f32>) -> Self {
        self.element.confidence = confidence;
        self
    }

    pub fn checked(mut self, checked: Option<ToggleState>) -> Self {
        self.element.checked = checked;
        self
    }

    pub fn build(self) -> TokenizeElement {
        self.element
    }
}

pub fn finalize_elements(elements: &mut Vec<TokenizeElement>) {
    split_multiline_ocr_elements(elements);
    elements.sort_by(|a, b| {
        a.bbox[1]
            .partial_cmp(&b.bbox[1])
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(
                a.bbox[0]
                    .partial_cmp(&b.bbox[0])
                    .unwrap_or(std::cmp::Ordering::Equal),
            )
    });

    let mut dedupe_counts: HashMap<String, usize> = HashMap::new();
    for element in elements.iter_mut() {
        if let Some(text) = element.text.as_mut() {
            let (sanitized, truncated) = sanitize_text(text);
            *text = sanitized;
            if text.is_empty() {
                element.text = None;
                element.text_truncated = None;
            } else {
                let already = element.text_truncated == Some(true);
                element.text_truncated = (already || truncated).then_some(true);
            }
        }

        let base_id = element_id_base(element);
        let base_id = if element.id.trim().is_empty() {
            base_id
        } else {
            normalize_existing_id(&element.id).unwrap_or(base_id)
        };
        let next_idx = dedupe_counts.get(&base_id).copied().unwrap_or(0) + 1;
        dedupe_counts.insert(base_id.clone(), next_idx);
        element.id = deduped_id(&base_id, next_idx);
        if element.source.trim().is_empty() {
            element.source = "unknown".to_string();
        }
        if element.scrollable.is_none()
            && element
                .source
                .strip_prefix("accessibility_ax:")
                .is_some_and(is_ax_scrollable_role)
        {
            element.scrollable = Some(true);
        }
    }
}

fn split_multiline_ocr_elements(elements: &mut Vec<TokenizeElement>) {
    let mut out: Vec<TokenizeElement> = Vec::with_capacity(elements.len());
    for element in elements.drain(..) {
        let Some(raw_text) = element.text.as_deref() else {
            out.push(element);
            continue;
        };
        if element.source != "vision_ocr" || !raw_text.contains('\n') {
            out.push(element);
            continue;
        }
        let lines: Vec<String> = raw_text
            .split('\n')
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_string)
            .collect();
        if lines.len() <= 1 {
            out.push(element);
            continue;
        }
        let line_count = lines.len() as f64;
        let base_y = element.bbox[1];
        let total_h = element.bbox[3].max(line_count);
        let line_h = (total_h / line_count).max(1.0);
        for (idx, line) in lines.into_iter().enumerate() {
            let mut split = element.clone();
            split.id.clear();
            split.text = Some(line);
            split.text_truncated = None;
            split.bbox[1] = base_y + line_h * idx as f64;
            split.bbox[3] = if idx + 1 == line_count as usize {
                let used_h = line_h * idx as f64;
                (total_h - used_h).max(1.0)
            } else {
                line_h
            };
            out.push(split);
        }
    }
    *elements = out;
}

fn is_ax_scrollable_role(role: &str) -> bool {
    matches!(
        role,
        "AXScrollArea"
            | "AXScrollBar"
            | "AXList"
            | "AXTable"
            | "AXOutline"
            | "AXWebArea"
            | "AXCollectionView"
    )
}

fn sanitize_text(input: &str) -> (String, bool) {
    let stripped = input
        .chars()
        .filter(|&ch| !is_invisible_text_mark(ch))
        .collect::<String>();
    let normalized_newlines = stripped
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .replace('\u{2028}', "\n")
        .replace('\u{2029}', "\n");
    let unescaped = normalize_escaped_newlines(&normalized_newlines);
    let compact = if unescaped.contains('\n') {
        unescaped
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        unescaped.trim().to_string()
    };
    let cleaned_len = compact.chars().count();
    let truncated = cleaned_len > MAX_ELEMENT_TEXT_CHARS;
    (truncate_chars(&compact, MAX_ELEMENT_TEXT_CHARS), truncated)
}

fn normalize_escaped_newlines(input: &str) -> String {
    if !(input.contains("\\n") || input.contains("\\r")) {
        return input.to_string();
    }
    input
        .replace("\\r\\n", "\n")
        .replace("\\n", "\n")
        .replace("\\r", "\n")
}

fn is_invisible_text_mark(ch: char) -> bool {
    matches!(
        ch,
        '\u{200B}' // zero-width space
            | '\u{200C}' // zero-width non-joiner
            | '\u{200D}' // zero-width joiner
            | '\u{200E}' // left-to-right mark
            | '\u{200F}' // right-to-left mark
            | '\u{202A}' // left-to-right embedding
            | '\u{202B}' // right-to-left embedding
            | '\u{202C}' // pop directional formatting
            | '\u{202D}' // left-to-right override
            | '\u{202E}' // right-to-left override
            | '\u{2060}' // word joiner
            | '\u{2066}' // left-to-right isolate
            | '\u{2067}' // right-to-left isolate
            | '\u{2068}' // first strong isolate
            | '\u{2069}' // pop directional isolate
            | '\u{FEFF}' // byte-order mark / zero-width no-break space
    )
}

fn element_id_base(element: &TokenizeElement) -> String {
    let prefix = element_kind_prefix(element);
    if let Some(suffix) = element_stable_text_suffix(element) {
        return truncate_id_to_limit(&format!("{prefix}_{suffix}"), MAX_ELEMENT_ID_CHARS);
    }
    format!("{prefix}_{POSITIONAL_ID_MARKER}")
}

fn element_kind_prefix(element: &TokenizeElement) -> &'static str {
    if let Some(role) = element.source.strip_prefix("accessibility_ax:") {
        return ax_role_prefix(role);
    }
    if element.source == "vision_ocr" {
        return "ocr";
    }
    match element.kind.as_str() {
        "glyph" => "glyph",
        "box" => "box",
        "text" | "" => {
            if element.has_border == Some(true) {
                "button"
            } else {
                "text"
            }
        }
        _ => "element",
    }
}

fn ax_role_prefix(role: &str) -> &'static str {
    match role {
        "AXButton" | "AXPopUpButton" | "AXMenuButton" => "button",
        "AXCheckBox" => "checkbox",
        "AXRadioButton" => "radio",
        "AXTextField" | "AXComboBox" => "field",
        "AXSlider" => "slider",
        "AXScrollBar" => "scrollbar",
        "AXScrollArea" => "scrollarea",
        "AXValueIndicator" => "indicator",
        "AXIncrementor" => "stepper",
        "AXSplitter" => "splitter",
        _ => "element",
    }
}

fn element_text_slug(text: &str) -> Option<String> {
    let normalized = text.trim();
    if normalized.is_empty() {
        return None;
    }

    let symbol_name = match normalized {
        "+" => Some("add"),
        "-" | "−" => Some("minus"),
        "*" | "×" => Some("multiply"),
        "/" | "÷" => Some("divide"),
        "=" => Some("equals"),
        "%" => Some("percent"),
        "." => Some("dot"),
        "," => Some("comma"),
        _ => None,
    };
    if let Some(name) = symbol_name {
        return Some(name.to_string());
    }

    let mut out = String::new();
    let mut prev_is_sep = false;
    for ch in normalized.chars() {
        if out.len() >= MAX_ELEMENT_SLUG_CHARS {
            break;
        }
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_is_sep = false;
            continue;
        }
        if !prev_is_sep {
            out.push('_');
            prev_is_sep = true;
        }
    }

    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        None
    } else {
        Some(truncate_chars(&trimmed, MAX_ELEMENT_SLUG_CHARS))
    }
}

fn element_stable_text_suffix(element: &TokenizeElement) -> Option<String> {
    let text = element.text.as_deref()?;
    if !can_use_text_for_id(element, text) {
        return None;
    }
    element_text_slug(text)
}

fn can_use_text_for_id(element: &TokenizeElement, text: &str) -> bool {
    if !is_control_like_element(element) {
        return false;
    }
    is_stable_id_text(text)
}

fn is_control_like_element(element: &TokenizeElement) -> bool {
    if element.has_border == Some(true) {
        return true;
    }
    match element.source.strip_prefix("accessibility_ax:") {
        Some("AXButton" | "AXPopUpButton" | "AXMenuButton" | "AXCheckBox" | "AXRadioButton") => {
            true
        }
        _ => false,
    }
}

fn is_stable_id_text(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return false;
    }
    if trimmed.chars().count() > 24 {
        return false;
    }
    if trimmed.split_whitespace().count() > 4 {
        return false;
    }
    trimmed.chars().all(|ch| {
        ch.is_ascii_alphanumeric()
            || matches!(
                ch,
                ' ' | '_' | '-' | '+' | '*' | '/' | '=' | '.' | ',' | '%' | '−' | '×' | '÷'
            )
    })
}

fn truncate_chars(input: &str, max_chars: usize) -> String {
    input.chars().take(max_chars).collect::<String>()
}

fn truncate_id_to_limit(input: &str, max_chars: usize) -> String {
    let mut out = truncate_chars(input, max_chars);
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "element_1".to_string()
    } else {
        out
    }
}

fn deduped_id(base_id: &str, next_idx: usize) -> String {
    let positional_suffix = format!("_{POSITIONAL_ID_MARKER}");
    if let Some(prefix) = base_id
        .strip_suffix(&positional_suffix)
        .or_else(|| base_id.strip_suffix("___ord"))
    {
        return truncate_id_to_limit(&format!("{prefix}_{next_idx}"), MAX_ELEMENT_ID_CHARS);
    }
    if next_idx == 1 {
        return truncate_id_to_limit(base_id, MAX_ELEMENT_ID_CHARS);
    }
    let suffix = format!("_{next_idx}");
    let suffix_len = suffix.chars().count();
    let base_budget = MAX_ELEMENT_ID_CHARS.saturating_sub(suffix_len);
    let truncated_base = truncate_id_to_limit(base_id, base_budget.max(1));
    truncate_id_to_limit(&format!("{truncated_base}{suffix}"), MAX_ELEMENT_ID_CHARS)
}

fn normalize_existing_id(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_sep = false;
    for ch in raw.trim().chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            prev_sep = false;
        } else if !prev_sep {
            out.push('_');
            prev_sep = true;
        }
        if out.chars().count() >= MAX_ELEMENT_ID_CHARS {
            break;
        }
    }
    let normalized = out.trim_matches('_').to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn el(
        source: &str,
        kind: &str,
        text: Option<&str>,
        has_border: Option<bool>,
        x: f64,
    ) -> TokenizeElement {
        TokenizeElement {
            id: String::new(),
            kind: kind.to_string(),
            bbox: [x, 10.0, 20.0, 20.0],
            has_border,
            text: text.map(ToString::to_string),
            text_truncated: None,
            confidence: None,
            scrollable: None,
            checked: None,
            source: source.to_string(),
        }
    }

    #[test]
    fn finalize_elements_builds_semantic_ids() {
        let mut elements = vec![
            el("accessibility_ax:AXButton", "", Some("7"), None, 30.0),
            el("accessibility_ax:AXButton", "", Some("+"), None, 60.0),
            el("vision_ocr", "", Some("Hello World"), None, 10.0),
            el("sat_control_v1", "", Some("OK"), Some(true), 80.0),
        ];
        finalize_elements(&mut elements);
        assert_eq!(elements[0].id, "ocr_1");
        assert_eq!(elements[1].id, "button_7");
        assert_eq!(elements[2].id, "button_add");
        assert_eq!(elements[3].id, "button_ok");
    }

    #[test]
    fn finalize_elements_dedupes_duplicate_ids_with_suffix() {
        let mut elements = vec![
            el("accessibility_ax:AXButton", "", Some("7"), None, 10.0),
            el("accessibility_ax:AXButton", "", Some("7"), None, 20.0),
            el("accessibility_ax:AXButton", "", Some("7"), None, 30.0),
        ];
        finalize_elements(&mut elements);
        assert_eq!(elements[0].id, "button_7");
        assert_eq!(elements[1].id, "button_7_2");
        assert_eq!(elements[2].id, "button_7_3");
    }

    #[test]
    fn finalize_elements_marks_ax_scroll_roles_as_scrollable() {
        let mut elements = vec![
            el("accessibility_ax:AXScrollArea", "", None, None, 10.0),
            el("accessibility_ax:AXButton", "", Some("OK"), None, 20.0),
        ];
        finalize_elements(&mut elements);
        assert_eq!(elements[0].scrollable, Some(true));
        assert_eq!(elements[1].scrollable, None);
    }

    #[test]
    fn finalize_elements_strips_invisible_bidi_marks_from_text() {
        let mut elements = vec![el(
            "accessibility_ax:AXScrollArea",
            "",
            Some("\u{200E}777,787,878"),
            None,
            10.0,
        )];
        finalize_elements(&mut elements);
        assert_eq!(elements[0].text.as_deref(), Some("777,787,878"));
    }

    #[test]
    fn finalize_elements_caps_text_and_id_lengths() {
        let long_text = "a".repeat(50_000);
        let mut elements = vec![el(
            "accessibility_ax:AXTextArea",
            "",
            Some(&long_text),
            None,
            10.0,
        )];
        finalize_elements(&mut elements);
        let text = elements[0].text.as_deref().expect("text");
        assert_eq!(text.chars().count(), MAX_ELEMENT_TEXT_CHARS);
        assert_eq!(elements[0].text_truncated, Some(true));
        assert!(elements[0].id.chars().count() <= MAX_ELEMENT_ID_CHARS);
    }

    #[test]
    fn finalize_elements_decodes_escaped_newlines_and_trims_indent() {
        let mut elements = vec![el(
            "accessibility_ax:AXTextArea",
            "",
            Some("\\n          {\\n            \\\"a\\\": 1\\n          }\\n"),
            None,
            10.0,
        )];
        finalize_elements(&mut elements);
        assert_eq!(elements[0].text.as_deref(), Some("{\n\\\"a\\\": 1\n}"));
        assert_eq!(elements[0].text_truncated, None);
    }

    #[test]
    fn finalize_elements_caps_deduped_ids() {
        let base = "x".repeat(200);
        let id = deduped_id(&base, 2);
        assert!(id.chars().count() <= MAX_ELEMENT_ID_CHARS);
        assert!(id.ends_with("_2"));
    }

    #[test]
    fn finalize_elements_uses_bbox_id_for_fluid_text_area_content() {
        let mut elements = vec![el(
            "accessibility_ax:AXTextArea",
            "",
            Some("{ huge json payload that changes every frame }"),
            None,
            123.0,
        )];
        finalize_elements(&mut elements);
        assert_eq!(elements[0].id, "element_1");
    }

    #[test]
    fn finalize_elements_splits_multiline_ocr_into_separate_elements() {
        let mut elements = vec![el(
            "vision_ocr",
            "",
            Some("Control Centre\nDesktop & Dock\nDisplays"),
            None,
            10.0,
        )];
        elements[0].bbox = [10.0, 100.0, 120.0, 60.0];
        finalize_elements(&mut elements);
        assert_eq!(elements.len(), 3);
        assert_eq!(elements[0].text.as_deref(), Some("Control Centre"));
        assert_eq!(elements[1].text.as_deref(), Some("Desktop & Dock"));
        assert_eq!(elements[2].text.as_deref(), Some("Displays"));
        assert!(elements.iter().all(|el| el.id.starts_with("ocr_")));
        assert_eq!(elements[0].bbox[1], 100.0);
        assert_eq!(elements[1].bbox[1], 120.0);
        assert_eq!(elements[2].bbox[1], 140.0);
    }
}

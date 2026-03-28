use desktop_core::protocol::{Bounds, TokenizeElement};
use std::collections::HashMap;

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
                confidence: None,
                source: String::new(),
            },
        }
    }

    pub fn bbox(mut self, bounds: Bounds) -> Self {
        self.element.bbox = [bounds.x, bounds.y, bounds.width, bounds.height];
        self
    }

    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.element.kind = kind.into();
        self
    }

    pub fn text(mut self, text: Option<String>) -> Self {
        self.element.text = text.and_then(|v| {
            let trimmed = v.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        });
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

    pub fn build(self) -> TokenizeElement {
        self.element
    }
}

pub fn finalize_elements(elements: &mut [TokenizeElement]) {
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
        let base_id = element_id_base(element);
        let next_idx = dedupe_counts.get(&base_id).copied().unwrap_or(0) + 1;
        dedupe_counts.insert(base_id.clone(), next_idx);
        element.id = if next_idx == 1 {
            base_id
        } else {
            format!("{base_id}_{next_idx}")
        };
        if element.source.trim().is_empty() {
            element.source = "unknown".to_string();
        }
        if let Some(text) = element.text.as_mut() {
            *text = text.trim().to_string();
            if text.is_empty() {
                element.text = None;
            }
        }
    }
}

fn element_id_base(element: &TokenizeElement) -> String {
    let prefix = element_kind_prefix(element);
    let suffix = element
        .text
        .as_deref()
        .and_then(element_text_slug)
        .unwrap_or_else(|| "1".to_string());
    format!("{prefix}_{suffix}")
}

fn element_kind_prefix(element: &TokenizeElement) -> &'static str {
    if let Some(role) = element.source.strip_prefix("accessibility_ax:") {
        return ax_role_prefix(role);
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
        Some(trimmed)
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
            confidence: None,
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
        assert_eq!(elements[0].id, "text_hello_world");
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
}

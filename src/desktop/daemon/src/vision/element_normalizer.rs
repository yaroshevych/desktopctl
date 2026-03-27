use desktop_core::protocol::{Bounds, TokenizeElement};

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

    for (idx, element) in elements.iter_mut().enumerate() {
        element.id = format!("text_{:04}", idx + 1);
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

use desktop_core::{error::AppError, protocol::Bounds};

#[derive(Debug, Clone)]
pub struct AxElement {
    pub role: String,
    pub text: Option<String>,
    pub bounds: Bounds,
    pub ax_identifier: Option<String>,
}

#[cfg(target_os = "macos")]
mod macos {
    use accessibility::{AXAttribute, AXUIElement, AXUIElementAttributes};
    use accessibility_sys::{
        AXValueGetType, AXValueGetValue, AXValueRef, kAXFocusedApplicationAttribute,
        kAXFocusedUIElementAttribute, kAXPositionAttribute, kAXSizeAttribute, kAXValueTypeCGPoint,
        kAXValueTypeCGSize,
    };
    use core_foundation::{
        attributed_string::{CFAttributedString, CFAttributedStringGetString},
        base::{CFType, TCFType},
        string::CFString,
    };
    use desktop_core::{error::AppError, protocol::Bounds};
    use std::ffi::c_void;

    use super::AxElement;

    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    pub(super) fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
        let system = AXUIElement::system_wide();
        let focused_app_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(
            kAXFocusedApplicationAttribute,
        ));
        let app_cf = system.attribute(&focused_app_attr).map_err(ax_err)?;
        if !app_cf.instance_of::<AXUIElement>() {
            return Err(AppError::backend_unavailable(
                "focused application AX value had unexpected type",
            ));
        }
        let app = unsafe { AXUIElement::wrap_under_get_rule(app_cf.as_CFTypeRef() as _) };
        let window = app
            .focused_window()
            .or_else(|_| app.main_window())
            .map_err(ax_err)?;
        let mut elements = Vec::new();
        collect_elements_recursive(&window, &mut elements);
        Ok(elements)
    }

    pub(super) fn focused_frontmost_element() -> Result<Option<AxElement>, AppError> {
        let system = AXUIElement::system_wide();
        let focused_app_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(
            kAXFocusedApplicationAttribute,
        ));
        let app_cf = system.attribute(&focused_app_attr).map_err(ax_err)?;
        if !app_cf.instance_of::<AXUIElement>() {
            return Ok(None);
        }
        let app = unsafe { AXUIElement::wrap_under_get_rule(app_cf.as_CFTypeRef() as _) };
        let focused_element_attr =
            AXAttribute::<CFType>::new(&CFString::from_static_string(kAXFocusedUIElementAttribute));
        let focused_cf = match app.attribute(&focused_element_attr) {
            Ok(value) => value,
            Err(_) => return Ok(None),
        };
        if !focused_cf.instance_of::<AXUIElement>() {
            return Ok(None);
        }
        let focused = unsafe { AXUIElement::wrap_under_get_rule(focused_cf.as_CFTypeRef() as _) };
        let role = focused
            .role()
            .map(|value| value.to_string())
            .unwrap_or_else(|_| "AXUnknown".to_string());
        let bounds = element_bounds(&focused).unwrap_or(Bounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        });
        let text = element_label(&focused, &role);
        let ax_identifier = element_identifier(&focused);
        Ok(Some(AxElement {
            role,
            text,
            bounds,
            ax_identifier,
        }))
    }

    fn collect_elements_recursive(element: &AXUIElement, out: &mut Vec<AxElement>) {
        if let Some(role) = element.role().ok().map(|v| v.to_string()) {
            if is_interactive_role(&role) {
                if let Some(bounds) = element_bounds(element) {
                    let text = element_label(element, &role);
                    let ax_identifier = element_identifier(element);
                    out.push(AxElement {
                        role,
                        text,
                        bounds,
                        ax_identifier,
                    });
                }
            }
        }

        if let Ok(children) = element.children() {
            for child in children.iter() {
                collect_elements_recursive(&child, out);
            }
        }
    }

    fn is_interactive_role(role: &str) -> bool {
        matches!(
            role,
            "AXButton"
                | "AXCheckBox"
                | "AXRadioButton"
                | "AXPopUpButton"
                | "AXTextField"
                | "AXTextArea"
                | "AXComboBox"
                | "AXSlider"
                | "AXMenuButton"
                | "AXScrollBar"
                | "AXScrollArea"
                | "AXWebArea"
                | "AXValueIndicator"
                | "AXIncrementor"
                | "AXSplitter"
        )
    }

    fn element_label(element: &AXUIElement, role: &str) -> Option<String> {
        let mut candidates: Vec<String> = Vec::new();
        if let Ok(v) = element.title() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.description() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.label_value() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.value_description() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.placeholder_value() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.help() {
            candidates.push(v.to_string());
        }
        if let Ok(v) = element.attribute(&AXAttribute::value()) {
            if let Some(value_text) = cf_type_text(&v) {
                candidates.push(value_text);
            }
        }
        // Some editors expose selected/visible text through AXSelectedText.
        if is_text_container_role(role) {
            if let Some(selected) = attribute_text_by_name(element, "AXSelectedText") {
                candidates.push(selected);
            }
        }
        // Calculator display commonly appears as AXScrollArea with dynamic value in descendants.
        if role == "AXScrollArea" {
            collect_descendant_text_candidates(element, 6, &mut candidates);
        }

        let primary = best_label_candidate(role, candidates);
        if primary.is_some() {
            return primary;
        }

        // Some controls (notably calculator-style buttons) expose their visible
        // label in child static text instead of the button's own title/value.
        if role == "AXButton" || role == "AXPopUpButton" || role == "AXMenuButton" {
            return element.children().ok().and_then(|children| {
                children.iter().find_map(|child| {
                    child
                        .title()
                        .ok()
                        .map(|v| v.to_string())
                        .or_else(|| {
                            child.attribute(&AXAttribute::value()).ok().and_then(|v| {
                                if v.instance_of::<CFString>() {
                                    Some(
                                        unsafe {
                                            CFString::wrap_under_get_rule(v.as_CFTypeRef() as _)
                                        }
                                        .to_string(),
                                    )
                                } else {
                                    None
                                }
                            })
                        })
                        .map(|s| s.trim().to_string())
                        .filter(|s| !s.is_empty())
                })
            });
        }
        None
    }

    fn element_identifier(element: &AXUIElement) -> Option<String> {
        const IDENTIFIER_ATTRS: [&str; 3] = ["AXIdentifier", "AXDOMIdentifier", "AXUniqueId"];
        for name in IDENTIFIER_ATTRS {
            if let Some(value) = attribute_text_by_name(element, name) {
                let trimmed = value.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
        None
    }

    fn is_text_container_role(role: &str) -> bool {
        matches!(role, "AXTextField" | "AXTextArea" | "AXWebArea")
    }

    fn best_label_candidate(role: &str, candidates: Vec<String>) -> Option<String> {
        let mut best: Option<(u8, String)> = None;
        for raw in candidates {
            let text = raw.trim().to_string();
            if text.is_empty() {
                continue;
            }
            let score = label_candidate_score(role, &text);
            match &best {
                None => best = Some((score, text)),
                Some((best_score, best_text)) => {
                    if score > *best_score || (score == *best_score && text.len() > best_text.len())
                    {
                        best = Some((score, text));
                    }
                }
            }
        }
        best.map(|(_, text)| text)
    }

    fn label_candidate_score(role: &str, text: &str) -> u8 {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return 0;
        }
        if role == "AXScrollArea" {
            let lower = trimmed.to_lowercase();
            if lower == "input" || lower == "output" || lower == "calculator" {
                return 1;
            }
            if trimmed.chars().any(|ch| ch.is_ascii_digit()) {
                return 5;
            }
            if trimmed.contains('+')
                || trimmed.contains('-')
                || trimmed.contains('*')
                || trimmed.contains('×')
                || trimmed.contains('÷')
                || trimmed.contains('=')
            {
                return 4;
            }
            return 2;
        }
        2
    }

    fn collect_descendant_text_candidates(element: &AXUIElement, depth: u8, out: &mut Vec<String>) {
        if depth == 0 {
            return;
        }
        let Ok(children) = element.children() else {
            return;
        };
        for child in children.iter() {
            push_direct_text_candidates(&child, out);
            if depth > 1 {
                collect_descendant_text_candidates(&child, depth - 1, out);
            }
        }
    }

    fn push_direct_text_candidates(element: &AXUIElement, out: &mut Vec<String>) {
        if let Ok(v) = element.title() {
            out.push(v.to_string());
        }
        if let Ok(v) = element.description() {
            out.push(v.to_string());
        }
        if let Ok(v) = element.label_value() {
            out.push(v.to_string());
        }
        if let Ok(v) = element.value_description() {
            out.push(v.to_string());
        }
        if let Ok(v) = element.placeholder_value() {
            out.push(v.to_string());
        }
        if let Ok(v) = element.help() {
            out.push(v.to_string());
        }
        if let Some(selected) = attribute_text_by_name(element, "AXSelectedText") {
            out.push(selected);
        }
        if let Ok(v) = element.attribute(&AXAttribute::value()) {
            if let Some(text) = cf_type_text(&v) {
                out.push(text);
            }
        }
    }

    fn cf_type_text(value: &CFType) -> Option<String> {
        if value.instance_of::<CFString>() {
            let text =
                unsafe { CFString::wrap_under_get_rule(value.as_CFTypeRef() as _) }.to_string();
            let trimmed = text.trim().to_string();
            return (!trimmed.is_empty()).then_some(trimmed);
        }
        if value.instance_of::<CFAttributedString>() {
            let attributed =
                unsafe { CFAttributedString::wrap_under_get_rule(value.as_CFTypeRef() as _) };
            let string_ref =
                unsafe { CFAttributedStringGetString(attributed.as_concrete_TypeRef()) };
            if !string_ref.is_null() {
                let text = unsafe { CFString::wrap_under_get_rule(string_ref) }.to_string();
                let trimmed = text.trim().to_string();
                return (!trimmed.is_empty()).then_some(trimmed);
            }
        }
        None
    }

    fn attribute_text_by_name(element: &AXUIElement, name: &'static str) -> Option<String> {
        let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(name));
        element
            .attribute(&attr)
            .ok()
            .and_then(|value| cf_type_text(&value))
    }

    fn element_bounds(element: &AXUIElement) -> Option<Bounds> {
        let pos_attr =
            AXAttribute::<CFType>::new(&CFString::from_static_string(kAXPositionAttribute));
        let size_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXSizeAttribute));
        let pos = element.attribute(&pos_attr).ok()?;
        let size = element.attribute(&size_attr).ok()?;

        let (x, y) = decode_point(&pos)?;
        let (w, h) = decode_size(&size)?;
        if w <= 1.0 || h <= 1.0 {
            return None;
        }
        Some(Bounds {
            x: x.max(0.0),
            y: y.max(0.0),
            width: w.max(0.0),
            height: h.max(0.0),
        })
    }

    fn decode_point(value: &CFType) -> Option<(f64, f64)> {
        let mut point = CGPoint { x: 0.0, y: 0.0 };
        let ax_value = value.as_CFTypeRef() as AXValueRef;
        let ok = unsafe {
            AXValueGetType(ax_value) == kAXValueTypeCGPoint
                && AXValueGetValue(
                    ax_value,
                    kAXValueTypeCGPoint,
                    (&mut point as *mut CGPoint).cast::<c_void>(),
                )
        };
        ok.then_some((point.x, point.y))
    }

    fn decode_size(value: &CFType) -> Option<(f64, f64)> {
        let mut size = CGSize {
            width: 0.0,
            height: 0.0,
        };
        let ax_value = value.as_CFTypeRef() as AXValueRef;
        let ok = unsafe {
            AXValueGetType(ax_value) == kAXValueTypeCGSize
                && AXValueGetValue(
                    ax_value,
                    kAXValueTypeCGSize,
                    (&mut size as *mut CGSize).cast::<c_void>(),
                )
        };
        ok.then_some((size.width, size.height))
    }

    fn ax_err(err: accessibility::Error) -> AppError {
        AppError::backend_unavailable(format!("failed to query AX tree: {err}"))
    }
}

#[cfg(target_os = "macos")]
pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
    macos::collect_frontmost_window_elements()
}

#[cfg(target_os = "macos")]
pub fn focused_frontmost_element() -> Result<Option<AxElement>, AppError> {
    macos::focused_frontmost_element()
}

#[cfg(not(target_os = "macos"))]
pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
    Ok(Vec::new())
}

#[cfg(not(target_os = "macos"))]
pub fn focused_frontmost_element() -> Result<Option<AxElement>, AppError> {
    Ok(None)
}

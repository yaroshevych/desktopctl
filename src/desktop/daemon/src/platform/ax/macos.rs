use desktop_core::{
    error::AppError,
    protocol::{Bounds, ToggleState},
};

#[derive(Debug, Clone)]
pub struct AxElement {
    pub role: String,
    pub text: Option<String>,
    pub bounds: Bounds,
    pub ax_identifier: Option<String>,
    pub checked: Option<ToggleState>,
}

use crate::trace;
use accessibility::{AXAttribute, AXUIElement, AXUIElementAttributes};
use accessibility_sys::{
    AXUIElementCopyMultipleAttributeValues, AXValueGetType, AXValueGetValue, AXValueRef,
    kAXChildrenAttribute, kAXDescriptionAttribute, kAXErrorSuccess, kAXFocusedApplicationAttribute,
    kAXFocusedUIElementAttribute, kAXIdentifierAttribute, kAXLabelValueAttribute,
    kAXPositionAttribute, kAXRoleAttribute, kAXSizeAttribute, kAXTitleAttribute, kAXValueAttribute,
    kAXValueTypeAXError, kAXValueTypeCGPoint, kAXValueTypeCGSize,
};
use core_foundation::{
    array::{CFArray, CFArrayRef},
    attributed_string::{CFAttributedString, CFAttributedStringGetString},
    base::{CFGetTypeID, CFType, TCFType},
    boolean::CFBoolean,
    number::CFNumber,
    string::CFString,
};
use std::cell::OnceCell;
use std::ffi::c_void;

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

pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
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
    let Some(batch) = fetch_batch(&window) else {
        return Ok(elements);
    };
    let role = batch
        .get(IDX_ROLE)
        .and_then(|v| v.as_ref())
        .and_then(cf_to_string);
    let viewport = batch_bounds(&batch);
    if let Some(role) = role.as_ref() {
        emit_element_from_batch(&window, role, &batch, viewport.as_ref(), &mut elements);
    }
    if let Some(children_value) = batch.get(IDX_CHILDREN).and_then(|v| v.as_ref()) {
        for child in cf_to_children(children_value) {
            collect_elements_recursive(&child, viewport.as_ref(), &mut elements);
        }
    }
    Ok(elements)
}

pub fn focused_frontmost_element() -> Result<Option<AxElement>, AppError> {
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
    dump_all_attributes_compact(&focused, &role, Some(&bounds));
    let text = element_label(&focused, &role);
    let ax_identifier = element_identifier(&focused);
    let checked = element_toggle_state(&focused, &role);
    Ok(Some(AxElement {
        role,
        text,
        bounds,
        ax_identifier,
        checked,
    }))
}

pub fn frontmost_app_pid() -> Option<i64> {
    use accessibility_sys::{AXUIElementGetPid, pid_t};
    let system = AXUIElement::system_wide();
    let focused_app_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(
        kAXFocusedApplicationAttribute,
    ));
    let app_cf = system.attribute(&focused_app_attr).ok()?;
    if !app_cf.instance_of::<AXUIElement>() {
        return None;
    }
    let app = unsafe { AXUIElement::wrap_under_get_rule(app_cf.as_CFTypeRef() as _) };
    let mut pid: pid_t = 0;
    let err = unsafe { AXUIElementGetPid(app.as_concrete_TypeRef(), &mut pid) };
    if err != kAXErrorSuccess || pid <= 0 {
        return None;
    }
    Some(pid as i64)
}

pub fn focused_frontmost_window_bounds() -> Result<Option<Bounds>, AppError> {
    let system = AXUIElement::system_wide();
    let focused_app_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(
        kAXFocusedApplicationAttribute,
    ));
    let app_cf = system.attribute(&focused_app_attr).map_err(ax_err)?;
    if !app_cf.instance_of::<AXUIElement>() {
        return Ok(None);
    }
    let app = unsafe { AXUIElement::wrap_under_get_rule(app_cf.as_CFTypeRef() as _) };
    let window = match app.focused_window().or_else(|_| app.main_window()) {
        Ok(window) => window,
        Err(_) => return Ok(None),
    };
    Ok(element_bounds(&window))
}

const BATCH_ATTRS: &[&str] = &[
    kAXRoleAttribute,
    kAXChildrenAttribute,
    kAXPositionAttribute,
    kAXSizeAttribute,
    kAXTitleAttribute,
    kAXValueAttribute,
    kAXDescriptionAttribute,
    kAXLabelValueAttribute,
    kAXIdentifierAttribute,
];
const IDX_ROLE: usize = 0;
const IDX_CHILDREN: usize = 1;
const IDX_POSITION: usize = 2;
const IDX_SIZE: usize = 3;
const IDX_TITLE: usize = 4;
const IDX_VALUE: usize = 5;
const IDX_DESCRIPTION: usize = 6;
const IDX_LABEL_VALUE: usize = 7;
const IDX_IDENTIFIER: usize = 8;

thread_local! {
    static BATCH_ATTRS_CF: OnceCell<CFArray<CFString>> = const { OnceCell::new() };
}

fn batch_attrs_array() -> CFArrayRef {
    BATCH_ATTRS_CF.with(|cell| {
        let arr = cell.get_or_init(|| {
            let strings: Vec<CFString> = BATCH_ATTRS
                .iter()
                .map(|s| CFString::from_static_string(s))
                .collect();
            CFArray::from_CFTypes(&strings)
        });
        arr.as_concrete_TypeRef()
    })
}

fn fetch_batch(element: &AXUIElement) -> Option<Vec<Option<CFType>>> {
    let attrs = batch_attrs_array();
    let mut out: CFArrayRef = std::ptr::null();
    let err = unsafe {
        AXUIElementCopyMultipleAttributeValues(element.as_concrete_TypeRef(), attrs, 0, &mut out)
    };
    if err != kAXErrorSuccess || out.is_null() {
        return None;
    }
    let arr = unsafe { CFArray::<CFType>::wrap_under_create_rule(out) };
    let len = arr.len();
    let mut values = Vec::with_capacity(len.max(0) as usize);
    for i in 0..len {
        let item = arr.get(i).map(|item| (*item).clone());
        values.push(item.filter(|v| !is_ax_error_value(v)));
    }
    Some(values)
}

fn is_ax_error_value(value: &CFType) -> bool {
    let type_id = value.as_CFTypeRef();
    if type_id.is_null() {
        return false;
    }
    if unsafe { CFGetTypeID(type_id) } != unsafe { accessibility_sys::AXValueGetTypeID() } {
        return false;
    }
    let av = type_id as AXValueRef;
    unsafe { AXValueGetType(av) == kAXValueTypeAXError }
}

fn cf_to_string(value: &CFType) -> Option<String> {
    cf_type_text(value)
}

fn cf_to_children(value: &CFType) -> Vec<AXUIElement> {
    if !value.instance_of::<CFArray<CFType>>() {
        return Vec::new();
    }
    let arr = unsafe { CFArray::<CFType>::wrap_under_get_rule(value.as_CFTypeRef() as CFArrayRef) };
    let mut out = Vec::with_capacity(arr.len().max(0) as usize);
    for i in 0..arr.len() {
        if let Some(item) = arr.get(i) {
            if item.instance_of::<AXUIElement>() {
                let child = unsafe { AXUIElement::wrap_under_get_rule(item.as_CFTypeRef() as _) };
                out.push(child);
            }
        }
    }
    out
}

fn collect_elements_recursive(
    element: &AXUIElement,
    viewport: Option<&Bounds>,
    out: &mut Vec<AxElement>,
) {
    let Some(batch) = fetch_batch(element) else {
        return;
    };

    let role = batch
        .get(IDX_ROLE)
        .and_then(|v| v.as_ref())
        .and_then(cf_to_string);
    let Some(role) = role else {
        return;
    };

    let bounds = batch_bounds(&batch);
    if let (Some(b), Some(vp)) = (bounds.as_ref(), viewport) {
        if !bounds_intersect(b, vp) {
            return;
        }
    }

    emit_element_from_batch(element, &role, &batch, bounds.as_ref(), out);

    if let Some(children_value) = batch.get(IDX_CHILDREN).and_then(|v| v.as_ref()) {
        for child in cf_to_children(children_value) {
            collect_elements_recursive(&child, viewport, out);
        }
    }
}

fn emit_element_from_batch(
    element: &AXUIElement,
    role: &str,
    batch: &[Option<CFType>],
    bounds: Option<&Bounds>,
    out: &mut Vec<AxElement>,
) {
    let interactive = is_interactive_role(role);
    let text_bearing = !interactive && is_text_bearing_role(role);
    if !interactive && !text_bearing {
        return;
    }
    if trace::is_enabled() {
        dump_all_attributes_compact(element, role, bounds);
    }
    let Some(bounds) = bounds else {
        return;
    };
    if interactive {
        let text = batch_interactive_label(batch, role).or_else(|| element_label(element, role));
        let ax_identifier = if should_collect_identifier(role) {
            batch_text_at(batch, IDX_IDENTIFIER).or_else(|| element_identifier(element))
        } else {
            None
        };
        let checked = element_toggle_state_from_batch(batch, role)
            .or_else(|| element_toggle_state(element, role));
        out.push(AxElement {
            role: role.to_string(),
            text,
            bounds: bounds.clone(),
            ax_identifier,
            checked,
        });
    } else {
        let text = batch_text_bearing_label(batch, role);
        if text
            .as_deref()
            .map(str::trim)
            .is_some_and(|t| !t.is_empty())
        {
            out.push(AxElement {
                role: role.to_string(),
                text,
                bounds: bounds.clone(),
                ax_identifier: None,
                checked: None,
            });
        }
    }
}

fn bounds_intersect(a: &Bounds, b: &Bounds) -> bool {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;
    a.x < bx2 && b.x < ax2 && a.y < by2 && b.y < ay2
}

fn batch_bounds(batch: &[Option<CFType>]) -> Option<Bounds> {
    let pos = batch.get(IDX_POSITION).and_then(|v| v.as_ref())?;
    let size = batch.get(IDX_SIZE).and_then(|v| v.as_ref())?;
    let (x, y) = decode_point(pos)?;
    let (w, h) = decode_size(size)?;
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

fn batch_text_at(batch: &[Option<CFType>], idx: usize) -> Option<String> {
    let value = batch.get(idx).and_then(|v| v.as_ref())?;
    cf_to_string(value).and_then(|s| {
        let trimmed = s.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    })
}

fn batch_text_bearing_label(batch: &[Option<CFType>], role: &str) -> Option<String> {
    match role {
        "AXStaticText" | "AXText" => {
            batch_text_at(batch, IDX_VALUE).or_else(|| batch_text_at(batch, IDX_TITLE))
        }
        "AXLink" | "AXHeading" => {
            batch_text_at(batch, IDX_TITLE).or_else(|| batch_text_at(batch, IDX_VALUE))
        }
        "AXImage" => batch_text_at(batch, IDX_DESCRIPTION),
        _ => None,
    }
}

fn batch_interactive_label(batch: &[Option<CFType>], role: &str) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    let push = |candidates: &mut Vec<String>, val: Option<String>| {
        if let Some(v) = val {
            candidates.push(v);
        }
    };
    match role {
        "AXTextField" | "AXTextArea" | "AXWebArea" => {
            push(&mut candidates, batch_text_at(batch, IDX_VALUE));
            push(&mut candidates, batch_text_at(batch, IDX_TITLE));
            push(&mut candidates, batch_text_at(batch, IDX_LABEL_VALUE));
        }
        "AXButton" | "AXPopUpButton" | "AXMenuButton" | "AXRadioButton" | "AXCheckBox"
        | "AXSwitch" => {
            push(&mut candidates, batch_text_at(batch, IDX_TITLE));
            push(&mut candidates, batch_text_at(batch, IDX_LABEL_VALUE));
            push(&mut candidates, batch_text_at(batch, IDX_VALUE));
            push(&mut candidates, batch_text_at(batch, IDX_DESCRIPTION));
        }
        _ => {
            push(&mut candidates, batch_text_at(batch, IDX_TITLE));
            push(&mut candidates, batch_text_at(batch, IDX_LABEL_VALUE));
            push(&mut candidates, batch_text_at(batch, IDX_VALUE));
            push(&mut candidates, batch_text_at(batch, IDX_DESCRIPTION));
        }
    }
    best_label_candidate(role, candidates)
}

fn element_toggle_state_from_batch(batch: &[Option<CFType>], role: &str) -> Option<ToggleState> {
    if !matches!(role, "AXCheckBox" | "AXRadioButton" | "AXSwitch") {
        return None;
    }
    let value = batch.get(IDX_VALUE).and_then(|v| v.as_ref())?;
    if let Some(b) = value.downcast::<CFBoolean>() {
        return Some(if bool::from(b) {
            ToggleState::True
        } else {
            ToggleState::False
        });
    }
    if let Some(n) = value.downcast::<CFNumber>() {
        let raw = n
            .to_i64()
            .or_else(|| n.to_f64().map(|v| v.round() as i64))?;
        return Some(if raw <= 0 {
            ToggleState::False
        } else if raw == 1 {
            ToggleState::True
        } else {
            ToggleState::Mixed
        });
    }
    if let Some(text) = cf_type_text_raw(value) {
        return parse_toggle_text_state(&text);
    }
    Some(ToggleState::Unknown)
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
            | "AXSwitch"
    )
}

fn is_text_bearing_role(role: &str) -> bool {
    matches!(
        role,
        "AXStaticText" | "AXText" | "AXLink" | "AXHeading" | "AXImage"
    )
}

fn element_toggle_state(element: &AXUIElement, role: &str) -> Option<ToggleState> {
    if !matches!(role, "AXCheckBox" | "AXRadioButton" | "AXSwitch") {
        return None;
    }
    let Ok(value) = element.attribute(&AXAttribute::value()) else {
        trace::log(format!("ax:toggle_state role={role} value=missing"));
        return Some(ToggleState::Unknown);
    };
    if let Some(v) = value.downcast::<CFBoolean>() {
        let checked = bool::from(v);
        trace::log(format!(
            "ax:toggle_state role={role} value_kind=bool checked={checked}"
        ));
        return Some(if checked {
            ToggleState::True
        } else {
            ToggleState::False
        });
    }
    if let Some(v) = value.downcast::<CFNumber>() {
        let raw = v.to_i64().or_else(|| v.to_f64().map(|n| n.round() as i64));
        if let Some(n) = raw {
            if n <= 0 {
                trace::log(format!(
                    "ax:toggle_state role={role} value_kind=number raw={n} checked=false"
                ));
                return Some(ToggleState::False);
            }
            if n == 1 {
                trace::log(format!(
                    "ax:toggle_state role={role} value_kind=number raw={n} checked=true"
                ));
                return Some(ToggleState::True);
            }
            trace::log(format!(
                "ax:toggle_state role={role} value_kind=number raw={n} mixed=true"
            ));
            return Some(ToggleState::Mixed);
        }
    }
    if let Some(raw_text) = cf_type_text_raw(&value) {
        if let Some(state) = parse_toggle_text_state(&raw_text) {
            trace::log(format!(
                "ax:toggle_state role={role} value_kind=text raw=\"{}\" raw_dbg={:?} checked={:?}",
                raw_text.replace('\n', " "),
                raw_text,
                state
            ));
            return Some(state);
        }
        trace::log(format!(
            "ax:toggle_state role={role} value_kind=text raw=\"{}\" raw_dbg={:?} parsed=none",
            raw_text.replace('\n', " "),
            raw_text
        ));
    }
    trace::log(format!(
        "ax:toggle_state role={role} value_kind={} parsed=none",
        cf_type_kind(&value)
    ));
    Some(ToggleState::Unknown)
}

fn cf_type_kind(value: &CFType) -> &'static str {
    if value.instance_of::<CFBoolean>() {
        "bool"
    } else if value.instance_of::<CFNumber>() {
        "number"
    } else if value.instance_of::<CFString>() {
        "string"
    } else if value.instance_of::<CFAttributedString>() {
        "attributed_string"
    } else {
        "other"
    }
}

fn parse_toggle_text_state(raw: &str) -> Option<ToggleState> {
    let normalized = raw.trim().to_ascii_lowercase();
    if normalized.is_empty() {
        return None;
    }
    match normalized.as_str() {
        "0" | "false" | "off" | "unchecked" | "no" => Some(ToggleState::False),
        "1" | "true" | "on" | "checked" | "yes" => Some(ToggleState::True),
        "2" | "mixed" | "indeterminate" | "partially checked" => Some(ToggleState::Mixed),
        _ => None,
    }
}

fn element_label(element: &AXUIElement, role: &str) -> Option<String> {
    let mut candidates: Vec<String> = Vec::new();
    match role {
        // Prefer value-like fields first for editable content; this keeps AX calls small.
        "AXTextField" | "AXTextArea" | "AXWebArea" => {
            push_value_text(element, &mut candidates);
            if let Some(selected) = attribute_text_by_name(element, "AXSelectedText") {
                candidates.push(selected);
            }
            if let Ok(v) = element.placeholder_value() {
                candidates.push(v.to_string());
            }
            if let Ok(v) = element.title() {
                candidates.push(v.to_string());
            }
            if let Ok(v) = element.label_value() {
                candidates.push(v.to_string());
            }
        }
        // Buttons frequently expose title/value/label only.
        "AXButton" | "AXPopUpButton" | "AXMenuButton" | "AXRadioButton" | "AXCheckBox"
        | "AXSwitch" => {
            if let Ok(v) = element.title() {
                candidates.push(v.to_string());
            }
            if let Ok(v) = element.label_value() {
                candidates.push(v.to_string());
            }
            push_value_text(element, &mut candidates);
            if let Ok(v) = element.description() {
                candidates.push(v.to_string());
            }
        }
        // Calculator display commonly appears as AXScrollArea with dynamic value in descendants.
        "AXScrollArea" => {
            if let Ok(v) = element.title() {
                candidates.push(v.to_string());
            }
            if let Ok(v) = element.label_value() {
                candidates.push(v.to_string());
            }
            push_value_text(element, &mut candidates);
            let mut scan_budget: usize = 64;
            collect_descendant_text_candidates(element, 3, &mut scan_budget, &mut candidates);
        }
        _ => {
            if let Ok(v) = element.title() {
                candidates.push(v.to_string());
            }
            if let Ok(v) = element.label_value() {
                candidates.push(v.to_string());
            }
            push_value_text(element, &mut candidates);
            if let Ok(v) = element.description() {
                candidates.push(v.to_string());
            }
        }
    }
    if let Some(primary) = best_label_candidate(role, candidates) {
        return Some(primary);
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
                                    unsafe { CFString::wrap_under_get_rule(v.as_CFTypeRef() as _) }
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

fn push_value_text(element: &AXUIElement, out: &mut Vec<String>) {
    if let Ok(v) = element.attribute(&AXAttribute::value()) {
        if let Some(value_text) = cf_type_text(&v) {
            out.push(value_text);
        }
    }
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

fn should_collect_identifier(role: &str) -> bool {
    matches!(
        role,
        "AXButton"
            | "AXCheckBox"
            | "AXRadioButton"
            | "AXPopUpButton"
            | "AXTextField"
            | "AXTextArea"
            | "AXComboBox"
            | "AXMenuButton"
            | "AXSwitch"
    )
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
                if score > *best_score || (score == *best_score && text.len() > best_text.len()) {
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

fn collect_descendant_text_candidates(
    element: &AXUIElement,
    depth: u8,
    scan_budget: &mut usize,
    out: &mut Vec<String>,
) {
    if depth == 0 || *scan_budget == 0 {
        return;
    }
    let Ok(children) = element.children() else {
        return;
    };
    for child in children.iter() {
        if *scan_budget == 0 {
            break;
        }
        *scan_budget = scan_budget.saturating_sub(1);
        push_direct_text_candidates(&child, out);
        if depth > 1 {
            collect_descendant_text_candidates(&child, depth - 1, scan_budget, out);
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
        let text = unsafe { CFString::wrap_under_get_rule(value.as_CFTypeRef() as _) }.to_string();
        let trimmed = text.trim().to_string();
        return (!trimmed.is_empty()).then_some(trimmed);
    }
    if value.instance_of::<CFAttributedString>() {
        let attributed =
            unsafe { CFAttributedString::wrap_under_get_rule(value.as_CFTypeRef() as _) };
        let string_ref = unsafe { CFAttributedStringGetString(attributed.as_concrete_TypeRef()) };
        if !string_ref.is_null() {
            let text = unsafe { CFString::wrap_under_get_rule(string_ref) }.to_string();
            let trimmed = text.trim().to_string();
            return (!trimmed.is_empty()).then_some(trimmed);
        }
    }
    None
}

fn cf_type_text_raw(value: &CFType) -> Option<String> {
    if value.instance_of::<CFString>() {
        return Some(
            unsafe { CFString::wrap_under_get_rule(value.as_CFTypeRef() as _) }.to_string(),
        );
    }
    if value.instance_of::<CFAttributedString>() {
        let attributed =
            unsafe { CFAttributedString::wrap_under_get_rule(value.as_CFTypeRef() as _) };
        let string_ref = unsafe { CFAttributedStringGetString(attributed.as_concrete_TypeRef()) };
        if !string_ref.is_null() {
            return Some(unsafe { CFString::wrap_under_get_rule(string_ref) }.to_string());
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

fn dump_all_attributes_compact(element: &AXUIElement, role: &str, bounds: Option<&Bounds>) {
    if !trace::is_enabled() {
        return;
    }
    let names = match element.attribute_names() {
        Ok(names) => names,
        Err(err) => {
            trace::log(format!("ax:attrs role={role} names_err={err}"));
            return;
        }
    };
    let mut parts = Vec::with_capacity(names.len().max(0) as usize);
    for name in names.iter() {
        let key = compact_for_log(&name.to_string(), 64);
        let attr = AXAttribute::<CFType>::new(&name);
        let value = match element.attribute(&attr) {
            Ok(value) => cf_type_compact(&value),
            Err(err) => format!("<err:{err}>"),
        };
        parts.push(format!("{key}={value}"));
    }
    parts.sort_unstable();
    let bounds_text = bounds.map_or_else(
        || "none".to_string(),
        |b| {
            format!(
                "{:.0},{:.0},{:.0},{:.0}",
                b.x.max(0.0),
                b.y.max(0.0),
                b.width.max(0.0),
                b.height.max(0.0)
            )
        },
    );
    trace::log(format!(
        "ax:attrs role={} bounds={} count={} attrs={}",
        compact_for_log(role, 64),
        bounds_text,
        parts.len(),
        compact_for_log(&parts.join(" | "), 8000)
    ));
}

fn cf_type_compact(value: &CFType) -> String {
    if let Some(v) = value.downcast::<CFBoolean>() {
        return format!("bool:{}", bool::from(v));
    }
    if let Some(v) = value.downcast::<CFNumber>() {
        if let Some(n) = v.to_i64() {
            return format!("num:{n}");
        }
        if let Some(n) = v.to_f64() {
            return format!("num:{n:.3}");
        }
        return "num:<unknown>".to_string();
    }
    if let Some(text) = cf_type_text_raw(value) {
        return format!("str:\"{}\"", compact_for_log(&text, 240));
    }
    if let Some((x, y)) = decode_point(value) {
        return format!("point:{x:.1},{y:.1}");
    }
    if let Some((w, h)) = decode_size(value) {
        return format!("size:{w:.1},{h:.1}");
    }
    if value.instance_of::<CFArray<CFType>>() {
        let arr = unsafe { CFArray::<CFType>::wrap_under_get_rule(value.as_CFTypeRef() as _) };
        let len = arr.len().max(0) as usize;
        let mut sample: Vec<String> = Vec::new();
        let limit = len.min(4);
        for idx in 0..limit {
            if let Some(entry) = arr.get(idx as isize) {
                sample.push(compact_for_log(&cf_type_compact(&entry), 64));
            }
        }
        let suffix = if len > limit { ",..." } else { "" };
        return format!("arr:{}:[{}{}]", len, sample.join(","), suffix);
    }
    if value.instance_of::<AXUIElement>() {
        return "ax_element".to_string();
    }
    format!("other:{}", cf_type_kind(value))
}

fn compact_for_log(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let mut out = String::with_capacity(value.len().min(max_chars));
    let mut prev_space = false;
    let mut char_count = 0usize;
    for ch in value.chars() {
        let normalized = if ch.is_whitespace() { ' ' } else { ch };
        if normalized == ' ' {
            if prev_space {
                continue;
            }
            prev_space = true;
            out.push(' ');
        } else {
            prev_space = false;
            out.push(normalized);
        }
        char_count += 1;
        if char_count >= max_chars {
            out.push_str("...");
            break;
        }
    }
    out.trim().to_string()
}

fn element_bounds(element: &AXUIElement) -> Option<Bounds> {
    let pos_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXPositionAttribute));
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

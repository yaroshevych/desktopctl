use desktop_core::{
    error::AppError,
    protocol::{Bounds, ToggleState},
};
use std::collections::HashSet;
use std::{cell::RefCell, thread_local};
use uiautomation::{
    UIAutomation, UIElement,
    patterns::{UITogglePattern, UIValuePattern},
    types::{ControlType, Handle as UiaHandle, ToggleState as UiaToggleState},
};
use windows_sys::Win32::UI::WindowsAndMessaging::GetForegroundWindow;

#[derive(Debug, Clone)]
pub struct AxElement {
    pub role: String,
    pub text: Option<String>,
    pub bounds: Bounds,
    pub ax_identifier: Option<String>,
    pub checked: Option<ToggleState>,
}

const MAX_UIA_DEPTH: usize = 8;
const MAX_UIA_NODES: usize = 512;

thread_local! {
    static UIA_CONTEXT: RefCell<Option<UIAutomation>> = const { RefCell::new(None) };
}

pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
    let hwnd = frontmost_hwnd();
    if hwnd == 0 {
        return Ok(Vec::new());
    }

    let (root, walker) = with_uia_context(|automation| {
        let root = automation
            .element_from_handle(UiaHandle::from(hwnd as isize))
            .map_err(|err| backend_error("failed to resolve frontmost window UIA element", err))?;
        let walker = automation
            .get_control_view_walker()
            .map_err(|err| backend_error("failed to create UIA control walker", err))?;
        Ok((root, walker))
    })?;

    let mut out = Vec::new();
    let mut seen = HashSet::new();
    let mut stack: Vec<(UIElement, usize)> = Vec::new();

    if let Some(focused) = focused_frontmost_element()? {
        push_unique(&mut out, &mut seen, focused);
    }

    if let Some(children) = walker.get_children(&root) {
        for child in children.into_iter().rev() {
            stack.push((child, 1));
        }
    }

    while let Some((element, depth)) = stack.pop() {
        if out.len() >= MAX_UIA_NODES {
            break;
        }

        if let Some(ax) = to_ax_element(&element) {
            push_unique(&mut out, &mut seen, ax);
        }

        if depth >= MAX_UIA_DEPTH {
            continue;
        }

        if let Some(children) = walker.get_children(&element) {
            for child in children.into_iter().rev() {
                stack.push((child, depth + 1));
            }
        }
    }

    Ok(out)
}

pub fn focused_frontmost_element() -> Result<Option<AxElement>, AppError> {
    with_uia_context(|automation| {
        let focused = match automation.get_focused_element() {
            Ok(element) => element,
            Err(_) => return Ok(None),
        };
        Ok(to_ax_element(&focused).or_else(|| to_ax_fallback_element(&focused)))
    })
}

pub fn focused_frontmost_window_bounds() -> Result<Option<Bounds>, AppError> {
    let hwnd = frontmost_hwnd();
    if hwnd == 0 {
        return Ok(None);
    }
    with_uia_context(|automation| {
        let window = match automation.element_from_handle(UiaHandle::from(hwnd as isize)) {
            Ok(element) => element,
            Err(_) => return Ok(None),
        };
        Ok(element_bounds(&window))
    })
}

fn frontmost_hwnd() -> usize {
    // SAFETY: GetForegroundWindow has no preconditions.
    unsafe { GetForegroundWindow() as usize }
}

fn to_ax_element(element: &UIElement) -> Option<AxElement> {
    if element.is_offscreen().ok() == Some(true) {
        return None;
    }

    let control_type = element.get_control_type().ok()?;
    let role = map_role_for_control_type(control_type)?;
    let bounds = element_bounds(element)?;
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return None;
    }

    Some(AxElement {
        role,
        text: map_text_for_element(element, control_type),
        bounds,
        ax_identifier: map_identifier_for_element(element),
        checked: map_toggle_state_for_element(element, control_type),
    })
}

fn to_ax_fallback_element(element: &UIElement) -> Option<AxElement> {
    let bounds = element_bounds(element)?;
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return None;
    }
    let control_type = element.get_control_type().ok();
    Some(AxElement {
        role: control_type
            .and_then(map_role_for_control_type)
            .unwrap_or_else(|| "AXUnknown".to_string()),
        text: control_type
            .and_then(|ty| map_text_for_element(element, ty))
            .or_else(|| normalize_text_candidates(vec![element.get_name().unwrap_or_default()])),
        bounds,
        ax_identifier: map_identifier_for_element(element),
        checked: control_type.and_then(|ty| map_toggle_state_for_element(element, ty)),
    })
}

fn map_role_for_control_type(control_type: ControlType) -> Option<String> {
    let role = match control_type {
        ControlType::Button | ControlType::SplitButton | ControlType::MenuItem => "AXButton",
        ControlType::CheckBox => "AXCheckBox",
        ControlType::RadioButton => "AXRadioButton",
        ControlType::ComboBox => "AXPopUpButton",
        ControlType::Edit => "AXTextField",
        ControlType::Document => "AXTextArea",
        ControlType::Text => "AXStaticText",
        ControlType::Hyperlink => "AXLink",
        ControlType::TabItem => "AXTabItem",
        ControlType::ListItem | ControlType::TreeItem => "AXRow",
        ControlType::Slider => "AXSlider",
        ControlType::Spinner => "AXIncrementor",
        ControlType::ScrollBar => "AXScrollBar",
        ControlType::Tab => "AXTabGroup",
        ControlType::Window | ControlType::Pane | ControlType::Group => "AXGroup",
        _ => return None,
    };
    Some(role.to_string())
}

fn element_bounds(element: &UIElement) -> Option<Bounds> {
    let rect = element.get_bounding_rectangle().ok()?;
    let left = rect.get_left() as f64;
    let top = rect.get_top() as f64;
    let right = rect.get_right() as f64;
    let bottom = rect.get_bottom() as f64;
    let width = (right - left).max(0.0);
    let height = (bottom - top).max(0.0);

    if width <= 0.0 || height <= 0.0 {
        return None;
    }

    Some(Bounds {
        x: left.max(0.0),
        y: top.max(0.0),
        width,
        height,
    })
}

fn map_text_for_element(element: &UIElement, control_type: ControlType) -> Option<String> {
    let mut candidates = Vec::with_capacity(4);

    if let Ok(name) = element.get_name() {
        candidates.push(name);
    }

    if matches!(
        control_type,
        ControlType::Edit
            | ControlType::Document
            | ControlType::Button
            | ControlType::SplitButton
            | ControlType::ComboBox
    ) && let Ok(value_pattern) = element.get_pattern::<UIValuePattern>()
        && let Ok(value) = value_pattern.get_value()
    {
        candidates.push(value);
    }

    if let Ok(help) = element.get_help_text() {
        candidates.push(help);
    }

    normalize_text_candidates(candidates)
}

fn map_identifier_for_element(element: &UIElement) -> Option<String> {
    if let Ok(id) = element.get_automation_id() {
        let id = id.trim();
        if !id.is_empty() {
            return Some(format!("uia-{id}"));
        }
    }

    if let Ok(runtime_id) = element.get_runtime_id()
        && !runtime_id.is_empty()
    {
        let joined = runtime_id
            .iter()
            .map(std::string::ToString::to_string)
            .collect::<Vec<String>>()
            .join("-");
        if !joined.is_empty() {
            return Some(format!("runtime-{joined}"));
        }
    }

    None
}

fn map_toggle_state_for_element(
    element: &UIElement,
    control_type: ControlType,
) -> Option<ToggleState> {
    if !matches!(
        control_type,
        ControlType::CheckBox | ControlType::RadioButton
    ) {
        return None;
    }

    let pattern = element.get_pattern::<UITogglePattern>().ok()?;
    let state = pattern.get_toggle_state().ok()?;
    Some(match state {
        UiaToggleState::On => ToggleState::True,
        UiaToggleState::Off => ToggleState::False,
        UiaToggleState::Indeterminate => ToggleState::Mixed,
    })
}

fn push_unique(out: &mut Vec<AxElement>, seen: &mut HashSet<String>, element: AxElement) {
    let key = dedupe_key(&element);
    if seen.insert(key) {
        out.push(element);
    }
}

fn dedupe_key(element: &AxElement) -> String {
    let id = element
        .ax_identifier
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("<none>");
    let text = element
        .text
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("<none>");
    let role = element.role.trim();
    format!(
        "{id}|{role}|{text}|{:.0},{:.0},{:.0},{:.0}",
        element.bounds.x.round(),
        element.bounds.y.round(),
        element.bounds.width.round(),
        element.bounds.height.round()
    )
}

fn normalize_text_candidates(values: Vec<String>) -> Option<String> {
    for value in values {
        let compact = value.replace('\r', " ").replace('\n', " ");
        let normalized = compact.split_whitespace().collect::<Vec<&str>>().join(" ");
        if !normalized.is_empty() {
            return Some(normalized);
        }
    }
    None
}

fn backend_error(context: &str, err: uiautomation::Error) -> AppError {
    AppError::backend_unavailable(format!("{context}: {err}"))
}

fn with_uia_context<T>(
    f: impl FnOnce(&UIAutomation) -> Result<T, AppError>,
) -> Result<T, AppError> {
    UIA_CONTEXT.with(|slot| {
        if slot.borrow().is_none() {
            let automation = UIAutomation::new()
                .map_err(|err| backend_error("failed to initialize UIAutomation", err))?;
            *slot.borrow_mut() = Some(automation);
        }
        let borrow = slot.borrow();
        let automation = borrow
            .as_ref()
            .ok_or_else(|| AppError::backend_unavailable("UIAutomation context unavailable"))?;
        f(automation)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(bounds: Bounds) -> AxElement {
        AxElement {
            role: "AXButton".to_string(),
            text: Some("Save".to_string()),
            bounds,
            ax_identifier: Some("uia-save-button".to_string()),
            checked: None,
        }
    }

    #[test]
    fn map_role_for_control_type_includes_common_windows_controls() {
        assert_eq!(
            map_role_for_control_type(ControlType::Button).as_deref(),
            Some("AXButton")
        );
        assert_eq!(
            map_role_for_control_type(ControlType::Edit).as_deref(),
            Some("AXTextField")
        );
        assert_eq!(
            map_role_for_control_type(ControlType::Tab).as_deref(),
            Some("AXTabGroup")
        );
        assert_eq!(
            map_role_for_control_type(ControlType::Pane).as_deref(),
            Some("AXGroup")
        );
    }

    #[test]
    fn normalize_text_candidates_trims_and_collapses_whitespace() {
        let normalized =
            normalize_text_candidates(vec!["   ".to_string(), "Line 1\r\n  Line 2".to_string()]);
        assert_eq!(normalized.as_deref(), Some("Line 1 Line 2"));
    }

    #[test]
    fn dedupe_key_matches_same_element_after_rounding() {
        let a = sample(Bounds {
            x: 100.49,
            y: 200.49,
            width: 80.49,
            height: 30.49,
        });
        let b = sample(Bounds {
            x: 100.51,
            y: 200.51,
            width: 80.51,
            height: 30.51,
        });
        assert_eq!(dedupe_key(&a), dedupe_key(&b));
    }

    #[test]
    fn push_unique_skips_duplicates() {
        let mut out = Vec::new();
        let mut seen = HashSet::new();
        let element = sample(Bounds {
            x: 10.0,
            y: 20.0,
            width: 30.0,
            height: 40.0,
        });
        push_unique(&mut out, &mut seen, element.clone());
        push_unique(&mut out, &mut seen, element);
        assert_eq!(out.len(), 1);
    }
}

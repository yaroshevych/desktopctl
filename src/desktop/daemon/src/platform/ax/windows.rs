use desktop_core::{
    error::AppError,
    protocol::{Bounds, ToggleState},
};
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

pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
    let hwnd = frontmost_hwnd();
    if hwnd == 0 {
        return Ok(Vec::new());
    }

    let automation = UIAutomation::new()
        .map_err(|err| backend_error("failed to initialize UIAutomation", err))?;
    let root = automation
        .element_from_handle(UiaHandle::from(hwnd as isize))
        .map_err(|err| backend_error("failed to resolve frontmost window UIA element", err))?;
    let walker = automation
        .get_control_view_walker()
        .map_err(|err| backend_error("failed to create UIA control walker", err))?;

    let mut out = Vec::new();
    let mut stack: Vec<(UIElement, usize)> = Vec::new();

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
            out.push(ax);
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
    let automation = UIAutomation::new()
        .map_err(|err| backend_error("failed to initialize UIAutomation", err))?;
    let focused = match automation.get_focused_element() {
        Ok(element) => element,
        Err(_) => return Ok(None),
    };
    Ok(to_ax_element(&focused))
}

pub fn focused_frontmost_window_bounds() -> Result<Option<Bounds>, AppError> {
    let hwnd = frontmost_hwnd();
    if hwnd == 0 {
        return Ok(None);
    }
    let automation = UIAutomation::new()
        .map_err(|err| backend_error("failed to initialize UIAutomation", err))?;
    let window = match automation.element_from_handle(UiaHandle::from(hwnd as isize)) {
        Ok(element) => element,
        Err(_) => return Ok(None),
    };
    Ok(element_bounds(&window))
}

fn frontmost_hwnd() -> usize {
    // SAFETY: GetForegroundWindow has no preconditions.
    unsafe { GetForegroundWindow() as usize }
}

fn to_ax_element(element: &UIElement) -> Option<AxElement> {
    let control_type = element.get_control_type().ok()?;
    let role = role_for_control_type(control_type)?;
    let bounds = element_bounds(element)?;
    if bounds.width <= 0.0 || bounds.height <= 0.0 {
        return None;
    }

    Some(AxElement {
        role,
        text: element_text(element, control_type),
        bounds,
        ax_identifier: element_identifier(element),
        checked: element_toggle_state(element, control_type),
    })
}

fn role_for_control_type(control_type: ControlType) -> Option<String> {
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
        ControlType::Slider => "AXSlider",
        ControlType::Spinner => "AXIncrementor",
        ControlType::ScrollBar => "AXScrollBar",
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

fn element_text(element: &UIElement, control_type: ControlType) -> Option<String> {
    let mut candidates = Vec::with_capacity(4);

    if let Ok(name) = element.get_name() {
        candidates.push(name);
    }

    if matches!(control_type, ControlType::Edit | ControlType::Document)
        && let Ok(value_pattern) = element.get_pattern::<UIValuePattern>()
        && let Ok(value) = value_pattern.get_value()
    {
        candidates.push(value);
    }

    if let Ok(help) = element.get_help_text() {
        candidates.push(help);
    }

    normalize_text_candidates(candidates)
}

fn element_identifier(element: &UIElement) -> Option<String> {
    if let Ok(id) = element.get_automation_id() {
        let id = id.trim();
        if !id.is_empty() {
            return Some(id.to_string());
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

fn element_toggle_state(element: &UIElement, control_type: ControlType) -> Option<ToggleState> {
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

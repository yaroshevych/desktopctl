use desktop_core::{
    automation::{Point, new_backend},
    error::AppError,
    protocol::{ObserveOptions, PointerButton},
};
use serde_json::{Value, json};

use crate::trace;

pub(crate) fn pointer_move(
    x: u32,
    y: u32,
    absolute: bool,
    active_window: bool,
    active_window_id: Option<String>,
    request_context: &super::super::RequestContext,
) -> Result<Value, AppError> {
    trace::log(format!(
        "pointer_move:start x={x} y={y} absolute={absolute}"
    ));
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let point = super::super::resolve_pointer_click_point(
        x,
        y,
        absolute,
        active_window,
        guard.bound_active_window_id.as_deref(),
        request_context,
    )?;
    super::super::guards::assert_bound_window_matches(guard.bound_active_window_id.as_deref())?;
    backend.move_mouse(point)?;
    trace::log(format!(
        "pointer_move:ok x={} y={} absolute={absolute}",
        point.x, point.y
    ));
    Ok(json!({}))
}

pub(crate) fn pointer_down(
    x: u32,
    y: u32,
    button: PointerButton,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    trace::log(format!("pointer_down:start x={x} y={y}"));
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    super::super::guards::assert_bound_window_matches(guard.bound_active_window_id.as_deref())?;
    let point = Point::new(x, y);
    backend.move_mouse(point)?;
    match button {
        PointerButton::Left => backend.left_down(point)?,
        PointerButton::Right => backend.right_down(point)?,
    }
    trace::log(format!("pointer_down:ok x={x} y={y}"));
    Ok(json!({}))
}

pub(crate) fn pointer_up(
    x: u32,
    y: u32,
    button: PointerButton,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    trace::log(format!("pointer_up:start x={x} y={y}"));
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    super::super::guards::assert_bound_window_matches(guard.bound_active_window_id.as_deref())?;
    let point = Point::new(x, y);
    backend.move_mouse(point)?;
    match button {
        PointerButton::Left => backend.left_up(point)?,
        PointerButton::Right => backend.right_up(point)?,
    }
    trace::log(format!("pointer_up:ok x={x} y={y}"));
    Ok(json!({}))
}

pub(crate) fn pointer_click(
    x: u32,
    y: u32,
    absolute: bool,
    button: PointerButton,
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
    request_context: &super::super::RequestContext,
) -> Result<Value, AppError> {
    trace::log(format!(
        "pointer_click:start x={x} y={y} absolute={absolute}"
    ));
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    let point = super::super::resolve_pointer_click_point(
        x,
        y,
        absolute,
        active_window,
        guard.bound_active_window_id.as_deref(),
        request_context,
    )?;
    super::super::guards::assert_bound_window_matches(guard.bound_active_window_id.as_deref())?;
    backend.move_mouse(point)?;
    match button {
        PointerButton::Left => backend.left_click(point)?,
        PointerButton::Right => backend.right_click(point)?,
    }
    trace::log(format!(
        "pointer_click:ok x={} y={} absolute={absolute}",
        point.x, point.y
    ));
    let mut result = json!({});
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
    )?;
    Ok(result)
}

pub(crate) fn pointer_scroll(
    id: Option<String>,
    dx: i32,
    dy: i32,
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
    request_context: &super::super::RequestContext,
) -> Result<Value, AppError> {
    trace::log(format!(
        "pointer_scroll:start id={:?} dx={dx} dy={dy}",
        id.as_deref()
    ));
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    if let Some(element_id) = id.as_deref() {
        let target = super::super::resolve_element_id_target(
            element_id,
            active_window,
            guard.bound_active_window_id.as_deref(),
            request_context,
        )?;
        let center = super::super::center_point(&target.bounds);
        backend.move_mouse(center)?;
    }
    super::super::guards::assert_bound_window_matches(guard.bound_active_window_id.as_deref())?;
    backend.scroll_wheel(dx, dy)?;
    trace::log(format!("pointer_scroll:ok dx={dx} dy={dy}"));
    let mut result = json!({});
    if let Some(element_id) = id {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("id".to_string(), json!(element_id));
        }
    }
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
    )?;
    Ok(result)
}

pub(crate) fn pointer_drag(
    x1: u32,
    y1: u32,
    x2: u32,
    y2: u32,
    hold_ms: u64,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    trace::log(format!(
        "pointer_drag:start from=({}, {}) to=({}, {}) hold_ms={}",
        x1, y1, x2, y2, hold_ms
    ));
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    super::super::guards::assert_bound_window_matches(guard.bound_active_window_id.as_deref())?;
    let start = Point::new(x1, y1);
    let end = Point::new(x2, y2);
    backend.move_mouse(start)?;
    backend.left_down(start)?;
    backend.sleep_ms(hold_ms.max(30));
    backend.left_drag(end)?;
    backend.left_up(end)?;
    trace::log(format!(
        "pointer_drag:ok from=({}, {}) to=({}, {}) hold_ms={}",
        x1, y1, x2, y2, hold_ms
    ));
    Ok(json!({}))
}

pub(crate) fn key_type(
    text: String,
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    backend.type_text(&text)?;
    let mut result = json!({});
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
    )?;
    Ok(result)
}

pub(crate) fn key_hotkey(
    hotkey: String,
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    backend.press_hotkey(&hotkey)?;
    let mut result = json!({});
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
    )?;
    Ok(result)
}

pub(crate) fn key_enter(
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    backend.press_enter()?;
    let mut result = json!({});
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
    )?;
    Ok(result)
}

pub(crate) fn key_escape(
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    backend.press_escape()?;
    let mut result = json!({});
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
    )?;
    Ok(result)
}

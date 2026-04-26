use desktop_core::{
    automation::{BackgroundInputTarget, Point, new_backend, new_background_input_backend},
    error::AppError,
    protocol::{ObserveOptions, PointerButton},
};
use serde_json::{Value, json};

use crate::trace;

struct BackgroundInputAttempt {
    handled: bool,
    verification: Option<super::super::background_verification::BackgroundInputVerification>,
}

impl BackgroundInputAttempt {
    fn not_handled() -> Self {
        Self {
            handled: false,
            verification: None,
        }
    }

    fn handled(
        verification: Option<super::super::background_verification::BackgroundInputVerification>,
    ) -> Self {
        Self {
            handled: true,
            verification,
        }
    }
}

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
    reject_unsupported_background_input("pointer move", active_window_id.as_deref())?;
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
    reject_unsupported_background_input("pointer down", active_window_id.as_deref())?;
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
    reject_unsupported_background_input("pointer up", active_window_id.as_deref())?;
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
    let explicit_active_window_id = active_window_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
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
    let background_attempt = try_background_left_click(
        explicit_active_window_id,
        guard.bound_active_window.as_ref(),
        point,
        button,
    )?;
    let background_verification = if background_attempt.handled {
        background_attempt.verification
    } else {
        super::super::guards::assert_bound_window_matches(guard.bound_active_window_id.as_deref())?;
        backend.move_mouse(point)?;
        match button {
            PointerButton::Left => backend.left_click(point)?,
            PointerButton::Right => backend.right_click(point)?,
        }
        None
    };
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
        None,
    )?;
    super::super::background_verification::append(&mut result, background_verification);
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
    let explicit_active_window_id = active_window_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    let mut target_point = None;
    if let Some(element_id) = id.as_deref() {
        let target = super::super::resolve_element_id_target(
            element_id,
            active_window,
            guard.bound_active_window_id.as_deref(),
            request_context,
        )?;
        let center = super::super::center_point(&target.bounds);
        target_point = Some(center);
        if !explicit_active_window_id || !super::super::background_input_enabled() {
            backend.move_mouse(center)?;
        }
    }
    let background_attempt = try_background_scroll(
        explicit_active_window_id,
        guard.bound_active_window.as_ref(),
        target_point,
        dx,
        dy,
    )?;
    let background_verification = if background_attempt.handled {
        background_attempt.verification
    } else {
        super::super::guards::assert_bound_window_matches(guard.bound_active_window_id.as_deref())?;
        backend.scroll_wheel(dx, dy)?;
        None
    };
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
        None,
    )?;
    super::super::background_verification::append(&mut result, background_verification);
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
    let explicit_active_window_id = active_window_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let start = Point::new(x1, y1);
    let end = Point::new(x2, y2);
    let background_attempt = try_background_drag(
        explicit_active_window_id,
        guard.bound_active_window.as_ref(),
        start,
        end,
        hold_ms,
    )?;
    let background_verification = if background_attempt.handled {
        background_attempt.verification
    } else {
        super::super::guards::assert_bound_window_matches(guard.bound_active_window_id.as_deref())?;
        backend.move_mouse(start)?;
        backend.left_down(start)?;
        backend.sleep_ms(hold_ms.max(30));
        let dx = i64::from(x2) - i64::from(x1);
        let dy = i64::from(y2) - i64::from(y1);
        let max_axis = dx.unsigned_abs().max(dy.unsigned_abs());
        let steps = ((max_axis / 40).max(1)).min(24) as u32;
        for step in 1..=steps {
            let x = i64::from(x1) + (dx * i64::from(step)) / i64::from(steps);
            let y = i64::from(y1) + (dy * i64::from(step)) / i64::from(steps);
            let px = x.clamp(0, u32::MAX as i64) as u32;
            let py = y.clamp(0, u32::MAX as i64) as u32;
            backend.left_drag(Point::new(px, py))?;
        }
        backend.left_up(end)?;
        None
    };
    trace::log(format!(
        "pointer_drag:ok from=({}, {}) to=({}, {}) hold_ms={}",
        x1, y1, x2, y2, hold_ms
    ));
    let mut result = json!({});
    super::super::background_verification::append(&mut result, background_verification);
    Ok(result)
}

pub(crate) fn key_type(
    text: String,
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let explicit_active_window_id = active_window_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    let background_attempt = try_background_type_text(
        explicit_active_window_id,
        guard.bound_active_window.as_ref(),
        &text,
    )?;
    let background_verification = if background_attempt.handled {
        background_attempt.verification
    } else {
        backend.type_text(&text)?;
        None
    };
    let mut result = json!({});
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
        None,
    )?;
    super::super::background_verification::append(&mut result, background_verification);
    Ok(result)
}

fn try_background_left_click(
    explicit_active_window_id: bool,
    bound_active_window: Option<&crate::platform::windowing::WindowInfo>,
    point: Point,
    button: PointerButton,
) -> Result<BackgroundInputAttempt, AppError> {
    if !explicit_active_window_id || !super::super::background_input_enabled() {
        return Ok(BackgroundInputAttempt::not_handled());
    }
    if !matches!(button, PointerButton::Left) {
        return Err(super::super::background_input_unsupported("right click"));
    }
    let Some(target_window) = bound_active_window else {
        return Ok(BackgroundInputAttempt::not_handled());
    };
    let target = super::super::background_input_target_for_window(target_window)?;
    let backend = new_background_input_backend()?;
    let verification =
        verify_background_action("left_click", &target, || backend.left_click(&target, point))?;
    trace::log(format!(
        "background_input:left_click ok pid={} window_id={} point=({}, {})",
        target.pid, target.window_id, point.x, point.y
    ));
    Ok(BackgroundInputAttempt::handled(verification))
}

fn try_background_type_text(
    explicit_active_window_id: bool,
    bound_active_window: Option<&crate::platform::windowing::WindowInfo>,
    text: &str,
) -> Result<BackgroundInputAttempt, AppError> {
    if !explicit_active_window_id || !super::super::background_input_enabled() {
        return Ok(BackgroundInputAttempt::not_handled());
    }
    let Some(target_window) = bound_active_window else {
        return Ok(BackgroundInputAttempt::not_handled());
    };
    let target = super::super::background_input_target_for_window(target_window)?;
    let backend = new_background_input_backend()?;
    let verification =
        verify_background_action("type_text", &target, || backend.type_text(&target, text))?;
    trace::log(format!(
        "background_input:type_text ok pid={} window_id={} chars={}",
        target.pid,
        target.window_id,
        text.chars().count()
    ));
    Ok(BackgroundInputAttempt::handled(verification))
}

fn try_background_scroll(
    explicit_active_window_id: bool,
    bound_active_window: Option<&crate::platform::windowing::WindowInfo>,
    point: Option<Point>,
    dx: i32,
    dy: i32,
) -> Result<BackgroundInputAttempt, AppError> {
    if !explicit_active_window_id || !super::super::background_input_enabled() {
        return Ok(BackgroundInputAttempt::not_handled());
    }
    let Some(target_window) = bound_active_window else {
        return Err(super::super::background_input_unsupported("pointer scroll"));
    };
    let target = super::super::background_input_target_for_window(target_window)?;
    let point = point.unwrap_or_else(|| super::super::center_point(&target_window.bounds));
    let backend = new_background_input_backend()?;
    let verification = verify_background_action("scroll", &target, || {
        backend.scroll_wheel(&target, point, dx, dy)
    })?;
    trace::log(format!(
        "background_input:scroll ok pid={} window_id={} point=({}, {}) dx={} dy={}",
        target.pid, target.window_id, point.x, point.y, dx, dy
    ));
    Ok(BackgroundInputAttempt::handled(verification))
}

fn try_background_drag(
    explicit_active_window_id: bool,
    bound_active_window: Option<&crate::platform::windowing::WindowInfo>,
    start: Point,
    end: Point,
    hold_ms: u64,
) -> Result<BackgroundInputAttempt, AppError> {
    if !explicit_active_window_id || !super::super::background_input_enabled() {
        return Ok(BackgroundInputAttempt::not_handled());
    }
    let Some(target_window) = bound_active_window else {
        return Err(super::super::background_input_unsupported("pointer drag"));
    };
    let target = super::super::background_input_target_for_window(target_window)?;
    let backend = new_background_input_backend()?;
    let verification = verify_background_action("drag", &target, || {
        backend.left_drag(&target, start, end, hold_ms)
    })?;
    trace::log(format!(
        "background_input:drag ok pid={} window_id={} start=({}, {}) end=({}, {}) hold_ms={}",
        target.pid, target.window_id, start.x, start.y, end.x, end.y, hold_ms
    ));
    Ok(BackgroundInputAttempt::handled(verification))
}

fn try_background_hotkey(
    explicit_active_window_id: bool,
    bound_active_window: Option<&crate::platform::windowing::WindowInfo>,
    hotkey: &str,
) -> Result<BackgroundInputAttempt, AppError> {
    if !explicit_active_window_id || !super::super::background_input_enabled() {
        return Ok(BackgroundInputAttempt::not_handled());
    }
    let Some(target_window) = bound_active_window else {
        return Err(super::super::background_input_unsupported("key hotkey"));
    };
    let target = super::super::background_input_target_for_window(target_window)?;
    let backend = new_background_input_backend()?;
    let verification =
        verify_background_action("hotkey", &target, || backend.press_hotkey(&target, hotkey))?;
    trace::log(format!(
        "background_input:hotkey ok pid={} window_id={} hotkey={:?}",
        target.pid, target.window_id, hotkey
    ));
    Ok(BackgroundInputAttempt::handled(verification))
}

fn try_background_enter(
    explicit_active_window_id: bool,
    bound_active_window: Option<&crate::platform::windowing::WindowInfo>,
) -> Result<BackgroundInputAttempt, AppError> {
    if !explicit_active_window_id || !super::super::background_input_enabled() {
        return Ok(BackgroundInputAttempt::not_handled());
    }
    let Some(target_window) = bound_active_window else {
        return Err(super::super::background_input_unsupported("key enter"));
    };
    let target = super::super::background_input_target_for_window(target_window)?;
    let backend = new_background_input_backend()?;
    let verification = verify_background_action("enter", &target, || backend.press_enter(&target))?;
    trace::log(format!(
        "background_input:enter ok pid={} window_id={}",
        target.pid, target.window_id
    ));
    Ok(BackgroundInputAttempt::handled(verification))
}

fn try_background_escape(
    explicit_active_window_id: bool,
    bound_active_window: Option<&crate::platform::windowing::WindowInfo>,
) -> Result<BackgroundInputAttempt, AppError> {
    if !explicit_active_window_id || !super::super::background_input_enabled() {
        return Ok(BackgroundInputAttempt::not_handled());
    }
    let Some(target_window) = bound_active_window else {
        return Err(super::super::background_input_unsupported("key escape"));
    };
    let target = super::super::background_input_target_for_window(target_window)?;
    let backend = new_background_input_backend()?;
    let verification =
        verify_background_action("escape", &target, || backend.press_escape(&target))?;
    trace::log(format!(
        "background_input:escape ok pid={} window_id={}",
        target.pid, target.window_id
    ));
    Ok(BackgroundInputAttempt::handled(verification))
}

fn verify_background_action(
    action: &str,
    target: &BackgroundInputTarget,
    action_fn: impl FnOnce() -> Result<(), AppError>,
) -> Result<Option<super::super::background_verification::BackgroundInputVerification>, AppError> {
    super::super::background_verification::verify_after_action(action, target, action_fn)
}

fn reject_unsupported_background_input(
    command_name: &str,
    active_window_id: Option<&str>,
) -> Result<(), AppError> {
    if super::super::background_input_enabled()
        && active_window_id
            .map(str::trim)
            .is_some_and(|value| !value.is_empty())
    {
        return Err(super::super::background_input_unsupported(command_name));
    }
    Ok(())
}

pub(crate) fn key_hotkey(
    hotkey: String,
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let explicit_active_window_id = active_window_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    let background_attempt = try_background_hotkey(
        explicit_active_window_id,
        guard.bound_active_window.as_ref(),
        &hotkey,
    )?;
    let background_verification = if background_attempt.handled {
        background_attempt.verification
    } else {
        backend.press_hotkey(&hotkey)?;
        None
    };
    let mut result = json!({});
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
        None,
    )?;
    super::super::background_verification::append(&mut result, background_verification);
    Ok(result)
}

pub(crate) fn key_enter(
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let explicit_active_window_id = active_window_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    let background_attempt = try_background_enter(
        explicit_active_window_id,
        guard.bound_active_window.as_ref(),
    )?;
    let background_verification = if background_attempt.handled {
        background_attempt.verification
    } else {
        backend.press_enter()?;
        None
    };
    let mut result = json!({});
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
        None,
    )?;
    super::super::background_verification::append(&mut result, background_verification);
    Ok(result)
}

pub(crate) fn key_escape(
    observe: ObserveOptions,
    active_window: bool,
    active_window_id: Option<String>,
) -> Result<Value, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let explicit_active_window_id = active_window_id
        .as_deref()
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    let background_attempt = try_background_escape(
        explicit_active_window_id,
        guard.bound_active_window.as_ref(),
    )?;
    let background_verification = if background_attempt.handled {
        background_attempt.verification
    } else {
        backend.press_escape()?;
        None
    };
    let mut result = json!({});
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        guard.observe_scope.as_ref(),
        None,
    )?;
    super::super::background_verification::append(&mut result, background_verification);
    Ok(result)
}

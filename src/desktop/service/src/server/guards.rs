use desktop_core::{error::AppError, protocol::ObserveOptions};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct ActiveWindowGuard {
    pub(crate) bound_active_window_id: Option<String>,
    pub(crate) bound_active_window: Option<crate::platform::windowing::WindowInfo>,
    pub(crate) observe_scope: Option<desktop_core::protocol::Bounds>,
}

pub(crate) fn prepare_active_window(
    active_window: bool,
    active_window_id: Option<&str>,
) -> Result<ActiveWindowGuard, AppError> {
    let bound_active_window =
        super::resolve_active_window_for_guard(active_window, active_window_id)?;
    let bound_active_window_id = bound_active_window
        .as_ref()
        .and_then(|window| window.window_ref.clone());
    let observe_scope = bound_active_window
        .as_ref()
        .map(|window| window.bounds.clone());
    Ok(ActiveWindowGuard {
        bound_active_window_id,
        bound_active_window,
        observe_scope,
    })
}

pub(crate) fn assert_bound_window_matches(
    bound_active_window_id: Option<&str>,
) -> Result<(), AppError> {
    if let Some(reference) = bound_active_window_id {
        let _ = super::assert_active_window_id_matches(reference)?;
    }
    Ok(())
}

pub(crate) fn capture_observe_start(
    observe: &ObserveOptions,
    observe_target: Option<&crate::platform::windowing::WindowInfo>,
) -> super::ObserveStartState {
    super::capture_observe_start_state(observe, observe_target)
}

pub(crate) fn explicit_observe_target<'a>(
    guard: &'a ActiveWindowGuard,
    active_window_id: Option<&str>,
) -> Option<&'a crate::platform::windowing::WindowInfo> {
    active_window_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .and_then(|_| guard.bound_active_window.as_ref())
}

pub(crate) fn append_observe(
    result: &mut Value,
    observe: &ObserveOptions,
    observe_start: &super::ObserveStartState,
    observe_scope: Option<&desktop_core::protocol::Bounds>,
    pre_click_tokens: Option<&[Value]>,
) -> Result<(), AppError> {
    super::append_observe_payload(
        result,
        super::observe_after_action(observe, observe_start, observe_scope, pre_click_tokens)?,
    );
    Ok(())
}

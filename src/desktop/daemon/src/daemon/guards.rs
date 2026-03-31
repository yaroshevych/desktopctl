use desktop_core::{error::AppError, protocol::ObserveOptions};
use serde_json::Value;

#[derive(Debug, Clone)]
pub(crate) struct ActiveWindowGuard {
    pub(crate) bound_active_window_id: Option<String>,
    pub(crate) observe_scope: Option<desktop_core::protocol::Bounds>,
}

pub(crate) fn prepare_active_window(
    active_window: bool,
    active_window_id: Option<&str>,
) -> Result<ActiveWindowGuard, AppError> {
    let bound_active_window_id =
        super::bind_active_window_reference(active_window, active_window_id)?;
    let observe_scope =
        super::resolve_observe_scope_bounds(active_window, bound_active_window_id.as_deref())?;
    Ok(ActiveWindowGuard {
        bound_active_window_id,
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

pub(crate) fn capture_observe_start(observe: &ObserveOptions) -> super::ObserveStartState {
    super::capture_observe_start_state(observe)
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

use desktop_core::{error::AppError, protocol::PointerButton};
use serde_json::Value;

pub(crate) fn click_text(
    text: String,
    button: PointerButton,
    active_window: bool,
    active_window_id: Option<String>,
    observe: desktop_core::protocol::ObserveOptions,
    request_context: &super::super::RequestContext,
) -> Result<Value, AppError> {
    let bound_active_window_id =
        super::super::bind_active_window_reference(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::capture_observe_start_state(&observe);
    let mut result = super::super::click_text_target(
        &text,
        button,
        active_window,
        bound_active_window_id.as_deref(),
        request_context,
    )?;
    super::super::append_observe_payload(
        &mut result,
        super::super::observe_after_action(&observe, &observe_start, None)?,
    );
    Ok(result)
}

pub(crate) fn click_id(
    id: String,
    button: PointerButton,
    active_window: bool,
    active_window_id: Option<String>,
    observe: desktop_core::protocol::ObserveOptions,
    request_context: &super::super::RequestContext,
) -> Result<Value, AppError> {
    let bound_active_window_id =
        super::super::bind_active_window_reference(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::capture_observe_start_state(&observe);
    let mut result = super::super::click_element_id_target(
        &id,
        button,
        active_window,
        bound_active_window_id.as_deref(),
        request_context,
    )?;
    super::super::append_observe_payload(
        &mut result,
        super::super::observe_after_action(&observe, &observe_start, None)?,
    );
    Ok(result)
}

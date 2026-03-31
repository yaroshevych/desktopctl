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
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    let (mut result, pre_click_tokens) = super::super::click_text_target(
        &text,
        button,
        active_window,
        guard.bound_active_window_id.as_deref(),
        request_context,
    )?;
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        None,
        pre_click_tokens.as_deref(),
    )?;
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
    let guard =
        super::super::guards::prepare_active_window(active_window, active_window_id.as_deref())?;
    let observe_start = super::super::guards::capture_observe_start(&observe);
    let (mut result, pre_click_tokens) = super::super::click_element_id_target(
        &id,
        button,
        active_window,
        guard.bound_active_window_id.as_deref(),
        request_context,
    )?;
    super::super::guards::append_observe(
        &mut result,
        &observe,
        &observe_start,
        None,
        pre_click_tokens.as_deref(),
    )?;
    Ok(result)
}

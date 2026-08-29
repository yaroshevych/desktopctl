use desktop_core::{error::AppError, protocol::Bounds};

pub(super) fn tracked_window_bounds() -> Option<Bounds> {
    None
}

pub(super) fn update_from_tokenize(
    _payload: &desktop_core::protocol::TokenizePayload,
) -> Result<(), AppError> {
    Ok(())
}

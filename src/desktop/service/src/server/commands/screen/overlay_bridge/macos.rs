use desktop_core::{error::AppError, protocol::Bounds};

use crate::overlay;

pub(super) fn tracked_window_bounds() -> Option<Bounds> {
    if overlay::is_active() {
        overlay::tracked_window_bounds()
    } else {
        None
    }
}

pub(super) fn update_from_tokenize(
    payload: &desktop_core::protocol::TokenizePayload,
) -> Result<(), AppError> {
    overlay::update_from_tokenize(payload)
}

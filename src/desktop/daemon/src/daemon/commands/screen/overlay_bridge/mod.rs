#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

use desktop_core::{error::AppError, protocol::Bounds};

pub(super) fn tracked_window_bounds() -> Option<Bounds> {
    imp::tracked_window_bounds()
}

pub(super) fn update_from_tokenize(
    payload: &desktop_core::protocol::TokenizePayload,
) -> Result<(), AppError> {
    imp::update_from_tokenize(payload)
}

#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(not(target_os = "macos"))]
use unsupported as imp;

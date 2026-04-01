use desktop_core::error::AppError;

pub(crate) fn run() -> Result<(), AppError> {
    Err(AppError::backend_unavailable(
        "DesktopCtl.app is currently supported only on macOS",
    ))
}

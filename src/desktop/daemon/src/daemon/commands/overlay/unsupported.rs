use desktop_core::error::AppError;
use serde_json::Value;

pub(crate) fn start(_duration_ms: Option<u64>) -> Result<Value, AppError> {
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

pub(crate) fn stop() -> Result<Value, AppError> {
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

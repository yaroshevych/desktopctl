use std::path::PathBuf;

use desktop_core::{error::AppError, protocol::Bounds};

use crate::vision::pipeline::{self, CaptureResult};

pub fn default_capture_path() -> PathBuf {
    crate::vision::capture::default_capture_path()
}

pub fn capture_display(out_path: Option<PathBuf>) -> Result<CaptureResult, AppError> {
    pipeline::capture_and_update(out_path)
}

pub fn capture_bounds(
    out_path: Option<PathBuf>,
    bounds: Bounds,
    focused_app_override: Option<String>,
    lookup_focused_app: bool,
) -> Result<CaptureResult, AppError> {
    pipeline::capture_and_update_active_window(
        out_path,
        bounds,
        focused_app_override,
        lookup_focused_app,
    )
}

pub fn capture_window(
    out_path: Option<PathBuf>,
    window_id: u32,
    window_bounds: Bounds,
    crop_bounds: Option<Bounds>,
    focused_app_override: Option<String>,
) -> Result<CaptureResult, AppError> {
    pipeline::capture_and_update_window(
        out_path,
        window_id,
        window_bounds,
        crop_bounds,
        focused_app_override,
    )
}

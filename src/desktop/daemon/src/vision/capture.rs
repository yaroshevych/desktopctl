#[cfg(target_os = "macos")]
#[path = "capture/macos.rs"]
mod macos_impl;

#[cfg(target_os = "macos")]
pub use macos_impl::capture_screen_png;
#[cfg(target_os = "macos")]
pub(crate) use macos_impl::default_capture_path;

#[cfg(not(target_os = "macos"))]
use std::{
    path::PathBuf,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(not(target_os = "macos"))]
use desktop_core::{error::AppError, protocol::now_millis};
#[cfg(not(target_os = "macos"))]
use image::RgbaImage;

#[cfg(not(target_os = "macos"))]
use super::types::{CapturedFrame, CapturedImage};

#[cfg(not(target_os = "macos"))]
pub fn capture_screen_png(_out_path: Option<PathBuf>) -> Result<CapturedImage, AppError> {
    Err(AppError::backend_unavailable(
        "screen capture backend not implemented for this platform",
    ))
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn default_capture_path() -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/desktopctl-captures/capture-{ts}.png"))
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
fn empty_capture() -> CapturedImage {
    CapturedImage {
        frame: CapturedFrame {
            snapshot_id: now_millis() as u64,
            timestamp: now_millis().to_string(),
            display_id: 0,
            width: 0,
            height: 0,
            scale: 1.0,
            image_path: None,
        },
        image: RgbaImage::new(0, 0),
    }
}

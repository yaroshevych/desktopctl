#[cfg(target_os = "macos")]
#[path = "ocr/macos.rs"]
mod macos_impl;

use desktop_core::{error::AppError, protocol::SnapshotText};
use image::RgbaImage;

#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub fn recognize_text(image: &RgbaImage) -> Result<Vec<SnapshotText>, AppError> {
    macos_impl::recognize_text(image)
}

#[cfg(target_os = "macos")]
#[allow(dead_code)]
pub fn recognize_text_from_image(
    path: &std::path::Path,
    image_width: u32,
    image_height: u32,
) -> Result<Vec<SnapshotText>, AppError> {
    macos_impl::recognize_text_from_image(path, image_width, image_height)
}

#[cfg(not(target_os = "macos"))]
pub fn recognize_text(_image: &RgbaImage) -> Result<Vec<SnapshotText>, AppError> {
    Ok(Vec::new())
}

#[cfg(not(target_os = "macos"))]
#[allow(dead_code)]
pub fn recognize_text_from_image(
    _path: &std::path::Path,
    _image_width: u32,
    _image_height: u32,
) -> Result<Vec<SnapshotText>, AppError> {
    Ok(Vec::new())
}

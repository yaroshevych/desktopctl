#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

use image::RgbaImage;

use super::GrayThumbnail;

pub(super) fn thumbnail_from_rgba_gpu(
    image: &RgbaImage,
    width: u32,
    height: u32,
) -> Option<GrayThumbnail> {
    imp::thumbnail_from_rgba_gpu(image, width, height)
}

#[cfg(target_os = "macos")]
use macos as imp;
#[cfg(not(target_os = "macos"))]
use unsupported as imp;

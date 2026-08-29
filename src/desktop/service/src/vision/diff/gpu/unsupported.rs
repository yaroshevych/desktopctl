use image::RgbaImage;

use super::GrayThumbnail;

pub(super) fn thumbnail_from_rgba_gpu(
    _image: &RgbaImage,
    _width: u32,
    _height: u32,
) -> Option<GrayThumbnail> {
    None
}

use std::ffi::c_void;
use std::path::Path;

use desktop_core::{
    error::AppError,
    protocol::{Bounds, SnapshotText},
};
use image::RgbaImage;
use objc2::{AnyThread, ClassType, runtime::AnyObject};
use objc2_core_foundation::CGRect;
use objc2_core_graphics::{
    CGBitmapInfo, CGColorRenderingIntent, CGColorSpace, CGDataProvider, CGImage, CGImageAlphaInfo,
    CGImageByteOrderInfo,
};
use objc2_foundation::{NSArray, NSDictionary};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
    VNRequestTextRecognitionLevel,
};

use crate::trace;

/// Recognize text from an in-memory RGBA image.
/// Preprocesses the image (dark-mode inversion, contrast boost) before OCR.
pub fn recognize_text(image: &RgbaImage) -> Result<Vec<SnapshotText>, AppError> {
    let width = image.width();
    let height = image.height();
    trace::log(format!("ocr:start size={}x{}", width, height));

    let preprocessed = preprocess_for_ocr(image);
    let cg_image = build_cgimage_from_rgba(preprocessed)?;

    let options = NSDictionary::<VNImageOption, AnyObject>::from_slices::<VNImageOption>(&[], &[]);
    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            cg_image.as_ref(),
            &options,
        )
    };

    let request = VNRecognizeTextRequest::new();
    request.setRecognitionLevel(VNRequestTextRecognitionLevel::Accurate);

    let request_obj: &VNRequest = request.as_super().as_super();
    let requests = NSArray::from_slice(&[request_obj]);
    handler.performRequests_error(&requests).map_err(|err| {
        trace::log(format!(
            "ocr:perform_failed {}",
            err.localizedDescription().to_string()
        ));
        AppError::backend_unavailable(format!(
            "Vision OCR request failed: {}",
            err.localizedDescription().to_string()
        ))
    })?;

    let mut texts = Vec::new();
    if let Some(observations) = request.results() {
        for observation in observations.iter() {
            let candidates = observation.topCandidates(1);
            if candidates.is_empty() {
                continue;
            }
            let candidate = candidates.objectAtIndex(0);
            let text = candidate.string().to_string();
            if text.trim().is_empty() {
                continue;
            }

            let confidence = candidate.confidence();
            let normalized = unsafe { observation.boundingBox() };
            texts.push(SnapshotText {
                text,
                bounds: normalize_bounding_box(normalized, width as f64, height as f64),
                confidence,
            });
        }
    }

    trace::log(format!("ocr:ok texts={}", texts.len()));
    Ok(texts)
}

/// Legacy wrapper: loads image from path, delegates to `recognize_text`.
#[allow(dead_code)]
pub fn recognize_text_from_image(
    path: &Path,
    _image_width: u32,
    _image_height: u32,
) -> Result<Vec<SnapshotText>, AppError> {
    trace::log(format!("ocr:load path={}", path.display()));
    let img = image::open(path)
        .map_err(|e| {
            AppError::invalid_argument(format!("failed to open image {}: {}", path.display(), e))
        })?
        .to_rgba8();
    recognize_text(&img)
}

// ── preprocessing ───────────────────────────────────────────────────────────

/// Detects dark backgrounds and inverts to light; boosts contrast for OCR.
fn preprocess_for_ocr(image: &RgbaImage) -> RgbaImage {
    let is_dark = is_dark_background(image);
    let dbg = std::env::var("TOKENIZE_DEBUG").is_ok();
    if dbg {
        eprintln!("[ocr] preprocess: dark_bg={}", is_dark);
    }

    let mut out = image.clone();

    if is_dark {
        // Invert RGB channels (keep alpha).
        for pixel in out.pixels_mut() {
            pixel[0] = 255 - pixel[0];
            pixel[1] = 255 - pixel[1];
            pixel[2] = 255 - pixel[2];
        }
    }

    // Contrast stretch: map [lo, hi] percentiles to [0, 255].
    contrast_stretch(&mut out);

    out
}

/// Sample pixels to determine if the image has a predominantly dark background.
fn is_dark_background(image: &RgbaImage) -> bool {
    let w = image.width() as usize;
    let h = image.height() as usize;
    if w == 0 || h == 0 {
        return false;
    }

    // Sample a grid of points (edges + center).
    let mut dark_count = 0u32;
    let mut total = 0u32;
    let step_x = (w / 8).max(1);
    let step_y = (h / 8).max(1);

    for y in (0..h).step_by(step_y) {
        for x in (0..w).step_by(step_x) {
            let p = image.get_pixel(x as u32, y as u32);
            let luma = (p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114) / 1000;
            if luma < 80 {
                dark_count += 1;
            }
            total += 1;
        }
    }

    dark_count > total / 2
}

/// Stretch contrast: find 1st and 99th percentile luminance, remap to full range.
fn contrast_stretch(image: &mut RgbaImage) {
    // Build luminance histogram.
    let mut hist = [0u32; 256];
    for pixel in image.pixels() {
        let luma = (pixel[0] as u32 * 299 + pixel[1] as u32 * 587 + pixel[2] as u32 * 114) / 1000;
        hist[luma as usize] += 1;
    }

    let total = image.width() as u32 * image.height() as u32;
    let lo_target = total / 100; // 1st percentile
    let hi_target = total - total / 100; // 99th percentile

    let mut cumulative = 0u32;
    let mut lo = 0u8;
    let mut hi = 255u8;
    for (i, &count) in hist.iter().enumerate() {
        cumulative += count;
        if cumulative <= lo_target {
            lo = i as u8;
        }
        if cumulative < hi_target {
            hi = i as u8;
        }
    }

    let range = (hi as f32 - lo as f32).max(1.0);
    if range > 200.0 {
        // Already good contrast, skip.
        return;
    }

    let scale = 255.0 / range;
    for pixel in image.pixels_mut() {
        for c in 0..3 {
            let v = pixel[c] as f32;
            let stretched = ((v - lo as f32) * scale).clamp(0.0, 255.0);
            pixel[c] = stretched as u8;
        }
    }
}

// ── image bridge ────────────────────────────────────────────────────────────

unsafe extern "C-unwind" fn release_provider_bytes(
    info: *mut c_void,
    _data: std::ptr::NonNull<c_void>,
    _size: usize,
) {
    if !info.is_null() {
        unsafe {
            drop(Box::<Vec<u8>>::from_raw(info as *mut Vec<u8>));
        }
    }
}

fn build_cgimage_from_rgba(
    image: RgbaImage,
) -> Result<objc2_core_foundation::CFRetained<CGImage>, AppError> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width == 0 || height == 0 {
        return Err(AppError::invalid_argument("ocr image must be non-empty"));
    }
    let bytes = Box::new(image.into_raw());
    let data_ptr = bytes.as_ptr() as *const c_void;
    let byte_len = bytes.len();
    let info_ptr = Box::into_raw(bytes) as *mut c_void;
    let provider = unsafe {
        CGDataProvider::with_data(info_ptr, data_ptr, byte_len, Some(release_provider_bytes))
    }
    .ok_or_else(|| AppError::backend_unavailable("failed to build CGDataProvider for OCR image"))?;
    let color_space = CGColorSpace::new_device_rgb()
        .ok_or_else(|| AppError::backend_unavailable("failed to create device RGB color space"))?;
    let bitmap_info = CGBitmapInfo(CGImageAlphaInfo::Last.0 | CGImageByteOrderInfo::OrderDefault.0);
    unsafe {
        CGImage::new(
            width,
            height,
            8,
            32,
            width * 4,
            Some(color_space.as_ref()),
            bitmap_info,
            Some(provider.as_ref()),
            std::ptr::null(),
            false,
            CGColorRenderingIntent::RenderingIntentDefault,
        )
    }
    .ok_or_else(|| AppError::backend_unavailable("failed to build CGImage for OCR"))
}

// ── coordinate conversion ───────────────────────────────────────────────────

fn normalize_bounding_box(rect: CGRect, image_width: f64, image_height: f64) -> Bounds {
    // Vision uses normalized coordinates with origin at bottom-left.
    let x = rect.origin.x * image_width;
    let width = rect.size.width * image_width;
    let height = rect.size.height * image_height;
    let y = (1.0 - rect.origin.y - rect.size.height) * image_height;
    Bounds {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};

    use super::normalize_bounding_box;

    #[test]
    fn normalizes_vision_coordinates_to_screen_space() {
        let rect = CGRect::new(CGPoint::new(0.1, 0.2), CGSize::new(0.3, 0.4));
        let bounds = normalize_bounding_box(rect, 1000.0, 500.0);
        assert_eq!(bounds.x, 100.0);
        assert_eq!(bounds.width, 300.0);
        assert_eq!(bounds.height, 200.0);
        assert_eq!(bounds.y, 200.0);
    }
}

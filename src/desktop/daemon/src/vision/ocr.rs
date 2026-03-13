use std::path::Path;

use desktop_core::{
    error::AppError,
    protocol::{Bounds, SnapshotText},
};
use objc2::{AnyThread, ClassType, runtime::AnyObject};
use objc2_core_foundation::CGRect;
use objc2_foundation::{NSArray, NSDictionary, NSURL};
use objc2_vision::{
    VNImageOption, VNImageRequestHandler, VNRecognizeTextRequest, VNRequest,
    VNRequestTextRecognitionLevel,
};

use crate::trace;

pub fn recognize_text_from_image(
    path: &Path,
    image_width: u32,
    image_height: u32,
) -> Result<Vec<SnapshotText>, AppError> {
    trace::log(format!(
        "ocr:start path={} size={}x{}",
        path.display(),
        image_width,
        image_height
    ));
    let url = NSURL::from_file_path(path).ok_or_else(|| {
        AppError::invalid_argument(format!("invalid image path for OCR: {}", path.display()))
    })?;

    let options = NSDictionary::<VNImageOption, AnyObject>::from_slices::<VNImageOption>(&[], &[]);
    let handler = unsafe {
        VNImageRequestHandler::initWithURL_options(VNImageRequestHandler::alloc(), &url, &options)
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
                bounds: normalize_bounding_box(normalized, image_width as f64, image_height as f64),
                confidence,
            });
        }
    }

    trace::log(format!("ocr:ok texts={}", texts.len()));
    Ok(texts)
}

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

use std::{
    ffi::CString,
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use block2::RcBlock;
use core_graphics::{
    display::CGDisplay,
    geometry::CGRect as CgRect,
    window::{
        create_image, kCGWindowImageBestResolution, kCGWindowImageBoundsIgnoreFraming,
        kCGWindowListOptionIncludingWindow,
    },
};
use desktop_core::{error::AppError, protocol::now_millis};
use image::{ImageFormat, RgbaImage, imageops::FilterType};
use objc2::runtime::AnyClass;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSError, NSURL};
use objc2_screen_capture_kit::{
    SCScreenshotConfiguration, SCScreenshotManager, SCScreenshotOutput,
};

use crate::trace;

use crate::vision::types::{CapturedFrame, CapturedImage};

pub fn capture_screen_png(out_path: Option<PathBuf>) -> Result<CapturedImage, AppError> {
    trace::log("capture:screen_png:start");
    let display = CGDisplay::main();
    let bounds = display.bounds();
    let rect = CGRect::new(
        CGPoint::new(bounds.origin.x, bounds.origin.y),
        CGSize::new(bounds.size.width, bounds.size.height),
    );

    // In-memory-by-default path (no out_path) avoids disk I/O.
    if out_path.is_none() {
        let mut image = capture_with_coregraphics(&display)?;
        let logical_w = bounds.size.width.max(0.0) as u32;
        let logical_h = bounds.size.height.max(0.0) as u32;
        if logical_w > 0 && logical_h > 0 && image.width() > logical_w && image.height() > logical_h
        {
            trace::log(format!(
                "capture:screen_png:downscale_to_logical from={}x{} to={}x{}",
                image.width(),
                image.height(),
                logical_w,
                logical_h
            ));
            image = image::imageops::resize(&image, logical_w, logical_h, FilterType::Triangle);
        }
        trace::log("capture:screen_png:ok path=<memory>");
        return Ok(CapturedImage {
            frame: CapturedFrame {
                snapshot_id: now_millis() as u64,
                timestamp: now_millis().to_string(),
                display_id: display.id,
                // Keep logical display size in frame metadata; crop math uses
                // this with window bounds (also logical units).
                width: bounds.size.width.max(0.0) as u32,
                height: bounds.size.height.max(0.0) as u32,
                scale: 1.0,
                image_path: None,
            },
            image,
        });
    }

    let target_path = out_path.expect("checked is_some");
    let image = if screencapturekit_screenshot_api_available() {
        let sck_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            capture_with_screencapturekit_to_path(rect, &target_path)
        }));
        match sck_result {
            Ok(Ok(())) => image::open(&target_path)
                .map_err(|err| {
                    AppError::backend_unavailable(format!(
                        "failed to open capture image after ScreenCaptureKit write: {err}"
                    ))
                })?
                .to_rgba8(),
            Ok(Err(err)) => {
                trace::log(format!(
                    "capture:screen_png:sck_error fallback=coregraphics err={}",
                    err.message
                ));
                let image = capture_with_coregraphics(&display)?;
                save_capture_png(&image, &target_path)?;
                image
            }
            Err(payload) => {
                let panic_message = if let Some(msg) = payload.downcast_ref::<&str>() {
                    (*msg).to_string()
                } else if let Some(msg) = payload.downcast_ref::<String>() {
                    msg.clone()
                } else {
                    "non-string panic payload".to_string()
                };
                trace::log(format!(
                    "capture:screen_png:sck_panic fallback=coregraphics panic={panic_message}"
                ));
                let image = capture_with_coregraphics(&display)?;
                save_capture_png(&image, &target_path)?;
                image
            }
        }
    } else {
        trace::log("capture:screen_png:fallback=coregraphics");
        let image = capture_with_coregraphics(&display)?;
        save_capture_png(&image, &target_path)?;
        image
    };
    trace::log(format!(
        "capture:screen_png:ok path={}",
        target_path.display()
    ));

    Ok(CapturedImage {
        frame: CapturedFrame {
            snapshot_id: now_millis() as u64,
            timestamp: now_millis().to_string(),
            display_id: display.id,
            // Keep logical display size in frame metadata; crop math uses
            // this with window bounds (also logical units).
            width: bounds.size.width.max(0.0) as u32,
            height: bounds.size.height.max(0.0) as u32,
            scale: 1.0,
            image_path: Some(target_path),
        },
        image,
    })
}

pub(crate) fn capture_window_png(
    out_path: Option<PathBuf>,
    window_id: u32,
) -> Result<CapturedImage, AppError> {
    trace::log(format!("capture:window_png:start window_id={window_id}"));
    let cg_image = create_image(
        CgRect::default(),
        kCGWindowListOptionIncludingWindow,
        window_id,
        kCGWindowImageBoundsIgnoreFraming | kCGWindowImageBestResolution,
    )
    .ok_or_else(|| {
        AppError::backend_unavailable(format!(
            "background window capture failed for window {window_id}; switch to frontmost mode"
        ))
    })?;
    let image = cg_image_to_rgba(&cg_image)?;
    if image.width() == 0 || image.height() == 0 {
        return Err(AppError::backend_unavailable(format!(
            "background window capture returned an empty image for window {window_id}; switch to frontmost mode"
        )));
    }

    let image_path = if let Some(path) = out_path {
        save_capture_png(&image, &path)?;
        Some(path)
    } else {
        None
    };

    trace::log(format!(
        "capture:window_png:ok window_id={} path={} size={}x{}",
        window_id,
        image_path
            .as_ref()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "<memory>".to_string()),
        image.width(),
        image.height()
    ));

    Ok(CapturedImage {
        frame: CapturedFrame {
            snapshot_id: now_millis() as u64,
            timestamp: now_millis().to_string(),
            display_id: window_id,
            width: image.width(),
            height: image.height(),
            scale: 1.0,
            image_path,
        },
        image,
    })
}

pub(crate) fn default_capture_path() -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/desktopctl-captures/capture-{ts}.png"))
}

fn screencapturekit_screenshot_api_available() -> bool {
    // Some systems expose ScreenCaptureKit partially (for example, manager
    // class present but screenshot configuration class unavailable). Require
    // both classes before attempting the API.
    let manager = CString::new("SCScreenshotManager").expect("valid class name");
    let config = CString::new("SCScreenshotConfiguration").expect("valid class name");
    AnyClass::get(&manager).is_some() && AnyClass::get(&config).is_some()
}

fn capture_with_screencapturekit_to_path(
    rect: CGRect,
    target_path: &PathBuf,
) -> Result<(), AppError> {
    let file_url = NSURL::from_file_path(target_path).ok_or_else(|| {
        AppError::invalid_argument(format!(
            "invalid capture output path: {}",
            target_path.display()
        ))
    })?;
    let config = unsafe { SCScreenshotConfiguration::new() };
    unsafe {
        config.setShowsCursor(true);
        config.setFileURL(Some(&file_url));
    }

    let (tx, rx) = mpsc::channel::<Result<(), String>>();
    let callback = RcBlock::new(move |output: *mut SCScreenshotOutput, err: *mut NSError| {
        if !err.is_null() {
            let message = unsafe { (&*err).localizedDescription().to_string() };
            let _ = tx.send(Err(message));
            return;
        }
        if output.is_null() {
            let _ = tx.send(Err("capture returned empty screenshot output".to_string()));
            return;
        }
        let _ = tx.send(Ok(()));
    });

    unsafe {
        SCScreenshotManager::captureScreenshotWithRect_configuration_completionHandler(
            rect,
            &config,
            Some(&*callback),
        );
    }

    match rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
        Ok(Err(message)) => {
            return Err(AppError::backend_unavailable(format!(
                "screencapturekit screenshot failed: {message}"
            )));
        }
        Err(_) => {
            return Err(AppError::timeout(
                "timed out waiting for ScreenCaptureKit screenshot callback",
            ));
        }
    }

    if !target_path.exists() {
        return Err(AppError::backend_unavailable(format!(
            "ScreenCaptureKit completed but no file was written at {}",
            target_path.display()
        )));
    }
    Ok(())
}

fn capture_with_coregraphics(display: &CGDisplay) -> Result<RgbaImage, AppError> {
    let cg_image = display.image().ok_or_else(|| {
        AppError::backend_unavailable("CoreGraphics fallback failed to capture display image")
    })?;
    cg_image_to_rgba(&cg_image)
}

fn cg_image_to_rgba(cg_image: &core_graphics::image::CGImage) -> Result<RgbaImage, AppError> {
    let width = cg_image.width();
    let height = cg_image.height();
    let bytes_per_row = cg_image.bytes_per_row();
    let bits_per_pixel = cg_image.bits_per_pixel();
    if bits_per_pixel < 24 {
        return Err(AppError::backend_unavailable(format!(
            "unsupported CoreGraphics pixel format (bits_per_pixel={bits_per_pixel})"
        )));
    }

    let data = cg_image.data();
    let raw = data.bytes();
    let required = bytes_per_row.saturating_mul(height);
    if raw.len() < required {
        return Err(AppError::backend_unavailable(format!(
            "CoreGraphics image buffer too small: got {}, need at least {required}",
            raw.len()
        )));
    }

    let mut rgba = vec![0u8; width.saturating_mul(height).saturating_mul(4)];
    for y in 0..height {
        let row_start = y.saturating_mul(bytes_per_row);
        for x in 0..width {
            let src = row_start + x.saturating_mul(4);
            let dst = (y.saturating_mul(width) + x).saturating_mul(4);
            if src + 3 >= raw.len() || dst + 3 >= rgba.len() {
                continue;
            }
            // CGDisplay image bytes are typically BGRA on little-endian macOS.
            rgba[dst] = raw[src + 2];
            rgba[dst + 1] = raw[src + 1];
            rgba[dst + 2] = raw[src];
            rgba[dst + 3] = raw[src + 3];
        }
    }

    let image = RgbaImage::from_vec(width as u32, height as u32, rgba).ok_or_else(|| {
        AppError::backend_unavailable("failed to build RGBA image from CoreGraphics buffer")
    })?;
    Ok(image)
}

fn save_capture_png(image: &RgbaImage, target_path: &PathBuf) -> Result<(), AppError> {
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::backend_unavailable(format!("failed to create capture directory: {err}"))
        })?;
    }
    image
        .save_with_format(target_path, ImageFormat::Png)
        .map_err(|err| {
            AppError::backend_unavailable(format!(
                "failed to write PNG capture {}: {err}",
                target_path.display()
            ))
        })
}

#[cfg(test)]
mod tests {
    use super::default_capture_path;

    #[test]
    fn default_capture_path_points_to_tmp_png() {
        let path = default_capture_path();
        let path_s = path.display().to_string();
        assert!(path_s.starts_with("/tmp/desktopctl-captures/capture-"));
        assert!(path_s.ends_with(".png"));
    }
}

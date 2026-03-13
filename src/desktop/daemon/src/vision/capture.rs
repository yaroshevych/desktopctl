use std::{
    ffi::CString,
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use block2::RcBlock;
use core_graphics::display::CGDisplay;
use desktop_core::{error::AppError, protocol::now_millis};
use image::{ImageFormat, RgbaImage};
use objc2::runtime::AnyClass;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSError, NSURL};
use objc2_screen_capture_kit::{
    SCScreenshotConfiguration, SCScreenshotManager, SCScreenshotOutput,
};

use crate::trace;

use super::types::CapturedFrame;

pub fn capture_screen_png(out_path: Option<PathBuf>) -> Result<CapturedFrame, AppError> {
    trace::log("capture:screen_png:start");
    let display = CGDisplay::main();
    let bounds = display.bounds();
    let rect = CGRect::new(
        CGPoint::new(bounds.origin.x, bounds.origin.y),
        CGSize::new(bounds.size.width, bounds.size.height),
    );

    let target_path = out_path.unwrap_or_else(default_capture_path);
    if let Some(parent) = target_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            AppError::backend_unavailable(format!("failed to create capture directory: {err}"))
        })?;
    }

    if !screencapturekit_screenshot_api_available() {
        trace::log("capture:screen_png:fallback=coregraphics");
        capture_with_coregraphics(&display, &target_path)?;
        return Ok(CapturedFrame {
            snapshot_id: now_millis() as u64,
            timestamp: now_millis().to_string(),
            display_id: display.id,
            width: bounds.size.width.max(0.0) as u32,
            height: bounds.size.height.max(0.0) as u32,
            scale: 1.0,
            image_path: target_path,
        });
    }

    let file_url = NSURL::from_file_path(&target_path).ok_or_else(|| {
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
            trace::log(format!("capture:screen_png:sck_error {message}"));
            return Err(AppError::backend_unavailable(format!(
                "screencapturekit screenshot failed: {message}"
            )));
        }
        Err(_) => {
            trace::log("capture:screen_png:sck_timeout");
            return Err(AppError::timeout(
                "timed out waiting for ScreenCaptureKit screenshot callback",
            ));
        }
    }

    if !target_path.exists() {
        trace::log("capture:screen_png:no_file_written");
        return Err(AppError::backend_unavailable(format!(
            "ScreenCaptureKit completed but no file was written at {}",
            target_path.display()
        )));
    }
    trace::log(format!("capture:screen_png:ok path={}", target_path.display()));

    Ok(CapturedFrame {
        snapshot_id: now_millis() as u64,
        timestamp: now_millis().to_string(),
        display_id: display.id,
        width: bounds.size.width.max(0.0) as u32,
        height: bounds.size.height.max(0.0) as u32,
        scale: 1.0,
        image_path: target_path,
    })
}

fn default_capture_path() -> PathBuf {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    PathBuf::from(format!("/tmp/desktopctl-captures/capture-{ts}.png"))
}

fn screencapturekit_screenshot_api_available() -> bool {
    let config = CString::new("SCScreenshotConfiguration").expect("valid class name");
    let manager = CString::new("SCScreenshotManager").expect("valid class name");
    AnyClass::get(&config).is_some() && AnyClass::get(&manager).is_some()
}

fn capture_with_coregraphics(display: &CGDisplay, target_path: &PathBuf) -> Result<(), AppError> {
    let cg_image = display.image().ok_or_else(|| {
        AppError::backend_unavailable("CoreGraphics fallback failed to capture display image")
    })?;
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
    image
        .save_with_format(target_path, ImageFormat::Png)
        .map_err(|err| {
            AppError::backend_unavailable(format!("failed to write PNG from CoreGraphics fallback: {err}"))
        })?;
    Ok(())
}

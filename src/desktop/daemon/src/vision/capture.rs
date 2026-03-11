use std::{
    fs,
    path::PathBuf,
    sync::mpsc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use block2::RcBlock;
use core_graphics::display::CGDisplay;
use desktop_core::{error::AppError, protocol::now_millis};
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_foundation::{NSError, NSURL};
use objc2_screen_capture_kit::{
    SCScreenshotConfiguration, SCScreenshotManager, SCScreenshotOutput,
};

use super::types::CapturedFrame;

pub fn capture_screen_png(out_path: Option<PathBuf>) -> Result<CapturedFrame, AppError> {
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

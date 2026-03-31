use desktop_core::error::AppError;
use serde_json::{Value, json};

#[cfg(target_os = "macos")]
use crate::overlay;
use crate::trace;

const DEFAULT_OVERLAY_AUTO_STOP_MS: u64 = 30_000;

pub(crate) fn start(duration_ms: Option<u64>) -> Result<Value, AppError> {
    #[cfg(target_os = "macos")]
    {
        super::super::PRIVACY_OVERLAY_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
        let started = overlay::start_overlay()?;
        let stop_after = duration_ms.unwrap_or(DEFAULT_OVERLAY_AUTO_STOP_MS).max(1);
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(stop_after));
            if let Err(err) = overlay::stop_overlay() {
                trace::log(format!(
                    "overlay:auto_stop err duration_ms={} error={}",
                    stop_after, err
                ));
            } else {
                trace::log(format!("overlay:auto_stop ok duration_ms={stop_after}"));
            }
        });
        return Ok(json!({
            "overlay_running": true,
            "started": started,
            "duration_ms": stop_after
        }));
    }
    #[allow(unreachable_code)]
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

pub(crate) fn stop() -> Result<Value, AppError> {
    #[cfg(target_os = "macos")]
    {
        super::super::PRIVACY_OVERLAY_ACTIVE.store(false, std::sync::atomic::Ordering::SeqCst);
        let stopped = overlay::stop_overlay()?;
        return Ok(json!({
            "overlay_running": false,
            "stopped": stopped
        }));
    }
    #[allow(unreachable_code)]
    Err(AppError::backend_unavailable(
        "overlay is supported only on macOS",
    ))
}

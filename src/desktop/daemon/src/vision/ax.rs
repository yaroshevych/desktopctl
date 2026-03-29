use std::{thread, time::Duration};

pub use crate::platform::ax::AxElement;

use crate::platform;

const AX_RETRY_ATTEMPTS: usize = 3;
const AX_RETRY_DELAY_MS: u64 = 20;

pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, desktop_core::error::AppError>
{
    let mut last_err: Option<desktop_core::error::AppError> = None;
    for attempt in 1..=AX_RETRY_ATTEMPTS {
        match platform::ax::collect_frontmost_window_elements() {
            Ok(items) => return Ok(items),
            Err(err) => {
                let retryable = err.message.contains("kAXErrorCannotComplete")
                    || err.message.contains("kAXErrorNoValue");
                if !retryable || attempt == AX_RETRY_ATTEMPTS {
                    return Err(err);
                }
                last_err = Some(err);
                thread::sleep(Duration::from_millis(AX_RETRY_DELAY_MS));
            }
        }
    }
    Err(last_err.unwrap_or_else(|| {
        desktop_core::error::AppError::backend_unavailable("failed to query AX tree")
    }))
}

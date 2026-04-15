use desktop_core::error::AppError;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use crate::daemon;

pub(crate) fn run() -> Result<(), AppError> {
    enable_per_monitor_dpi_awareness();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--on-demand") {
        return daemon::run_blocking(daemon::DaemonConfig::on_demand());
    }

    daemon::run_blocking(daemon::DaemonConfig::resident())
}

fn enable_per_monitor_dpi_awareness() {
    // SAFETY: process-wide DPI awareness bootstrap; failure is non-fatal and expected if already set.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

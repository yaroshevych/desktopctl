use desktop_core::error::AppError;

use crate::daemon;

pub(crate) fn run() -> Result<(), AppError> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--on-demand") {
        return daemon::run_blocking(daemon::DaemonConfig::on_demand());
    }

    daemon::run_blocking(daemon::DaemonConfig::resident())
}

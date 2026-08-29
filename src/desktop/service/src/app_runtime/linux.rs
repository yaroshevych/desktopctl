use desktop_core::error::AppError;

use crate::{daemon, journal};

pub(crate) fn run() -> Result<(), AppError> {
    let args: Vec<String> = std::env::args().collect();
    let background = args.iter().any(|a| a == "--background");
    let config = if args.iter().any(|a| a == "--on-demand") {
        daemon::DaemonConfig::on_demand()
    } else {
        daemon::DaemonConfig::resident()
    }
    .with_background_input(background);

    journal::start_from_disk();
    daemon::run_blocking(config)
}

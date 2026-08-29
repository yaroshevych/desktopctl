#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[path = "policy/mod.rs"]
mod app_policy;
mod app_runtime;
mod automation;
mod clipboard;
#[path = "server/mod.rs"]
mod daemon;
mod journal;
mod overlay;
mod platform;
mod request_store;
mod storage;
mod trace;
mod vision;

fn main() {
    storage::initialize();
    #[cfg(target_os = "macos")]
    let result = {
        app_policy::reload_current_from_disk();
        journal::start_from_disk();
        let args: Vec<String> = std::env::args().collect();
        let background = args.iter().any(|arg| arg == "--background");
        let config = if args.iter().any(|arg| arg == "--on-demand") {
            daemon::DaemonConfig::on_demand()
        } else {
            daemon::DaemonConfig::resident()
        };
        daemon::run_blocking(config.with_background_input(background))
    };
    #[cfg(not(target_os = "macos"))]
    let result = app_runtime::run();

    if let Err(err) = result {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

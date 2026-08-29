#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod agent_launcher;
mod app_policy;
mod app_runtime;
mod automation;
mod clipboard;
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
    if let Err(err) = app_runtime::run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

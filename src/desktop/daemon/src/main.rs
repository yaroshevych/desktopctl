#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

#[cfg(target_os = "macos")]
mod agent_launcher;
#[cfg(target_os = "macos")]
mod agent_runner;
#[cfg(target_os = "macos")]
mod agent_sessions;
mod app_policy;
mod app_runtime;
mod clipboard;
mod daemon;
mod journal;
mod overlay;
mod platform;
mod request_store;
mod trace;
mod vision;

fn main() {
    if let Err(err) = app_runtime::run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

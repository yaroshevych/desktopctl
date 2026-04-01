#[cfg(target_os = "macos")]
mod about;
mod app_runtime;
mod clipboard;
mod daemon;
#[cfg(target_os = "macos")]
mod overlay;
mod permissions;
#[cfg(target_os = "macos")]
mod permissions_dialog;
mod platform;
mod recording;
mod replay;
mod request_store;
mod trace;
mod vision;
mod window_refs;
mod window_target;

fn main() {
    if let Err(err) = app_runtime::run() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

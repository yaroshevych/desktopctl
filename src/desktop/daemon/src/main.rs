mod app_runtime;
mod clipboard;
mod daemon;
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

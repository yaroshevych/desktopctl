#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

fn main() {
    #[cfg(target_os = "macos")]
    if let Err(error) = desktop_app::runtime::macos_app::run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }

    #[cfg(not(target_os = "macos"))]
    eprintln!("desktopctl-app process split is currently available only on macOS");
}

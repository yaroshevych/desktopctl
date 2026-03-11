mod daemon;

#[cfg(target_os = "macos")]
fn main() {
    if let Err(err) = run_macos_app() {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("DesktopCtl.app is currently supported only on macOS");
    std::process::exit(1);
}

#[cfg(target_os = "macos")]
fn run_macos_app() -> Result<(), desktop_core::error::AppError> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--on-demand") {
        return daemon::run_blocking(daemon::DaemonConfig::on_demand());
    }

    use objc2::MainThreadMarker;
    use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
    use tray_icon::{
        TrayIconBuilder,
        menu::{Menu, MenuEvent, MenuItem},
    };

    daemon::start_background(daemon::DaemonConfig::resident())?;

    let mtm = MainThreadMarker::new().ok_or_else(|| {
        desktop_core::error::AppError::backend_unavailable("must run on main thread")
    })?;
    let ns_app = NSApplication::sharedApplication(mtm);
    let _ = ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let menu = Menu::new();
    let quit = MenuItem::new("Exit", true, None);
    menu.append(&quit)
        .map_err(|e| desktop_core::error::AppError::backend_unavailable(e.to_string()))?;

    let quit_id = quit.id().clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == quit_id {
            std::process::exit(0);
        }
    }));

    let _tray = TrayIconBuilder::new()
        .with_tooltip("DesktopCtl")
        .with_menu(Box::new(menu))
        .with_icon(default_icon()?)
        .build()
        .map_err(|e| desktop_core::error::AppError::backend_unavailable(e.to_string()))?;

    ns_app.run();
    Ok(())
}

#[cfg(target_os = "macos")]
fn default_icon() -> Result<tray_icon::Icon, desktop_core::error::AppError> {
    let width = 18u32;
    let height = 18u32;
    let mut rgba = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        for x in 0..width {
            let idx = ((y * width + x) * 4) as usize;
            let border = x <= 1 || y <= 1 || x >= width - 2 || y >= height - 2;
            if border {
                rgba[idx] = 0xff;
                rgba[idx + 1] = 0xff;
                rgba[idx + 2] = 0xff;
                rgba[idx + 3] = 0xff;
            } else {
                rgba[idx] = 0x00;
                rgba[idx + 1] = 0xb3;
                rgba[idx + 2] = 0x66;
                rgba[idx + 3] = 0xff;
            }
        }
    }

    tray_icon::Icon::from_rgba(rgba, width, height)
        .map_err(|e| desktop_core::error::AppError::backend_unavailable(e.to_string()))
}

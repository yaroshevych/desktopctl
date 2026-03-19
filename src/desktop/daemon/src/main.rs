mod clipboard;
mod daemon;
#[cfg(target_os = "macos")]
mod overlay;
mod permissions;
mod recording;
mod replay;
mod trace;
mod vision;

#[cfg(target_os = "macos")]
use std::{
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
    time::Duration,
};

#[cfg(target_os = "macos")]
use desktop_core::{
    ipc,
    protocol::{Command, RequestEnvelope, ResponseEnvelope, now_millis},
};

#[cfg(target_os = "macos")]
const OVERLAY_LIVE_INTERVAL_MS: u64 = 200;
#[cfg(target_os = "macos")]
static OVERLAY_LIVE_ENABLED: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static OVERLAY_LIVE_SEQ: AtomicU64 = AtomicU64::new(1);

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

    let mtm = MainThreadMarker::new().ok_or_else(|| {
        desktop_core::error::AppError::backend_unavailable("must run on main thread")
    })?;
    let ns_app = NSApplication::sharedApplication(mtm);
    let _ = ns_app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);

    let permission_requests = permissions::request_startup_permissions();
    if permission_requests.accessibility_requested {
        eprintln!("requested Accessibility permission for DesktopCtl.app");
    }
    if permission_requests.screen_recording_requested {
        eprintln!("requested Screen Recording permission for DesktopCtl.app");
    }

    daemon::start_background(daemon::DaemonConfig::resident())?;

    let menu = Menu::new();
    let toggle_overlay = MenuItem::new("Toggle Overlay", true, None);
    let quit = MenuItem::new("Exit", true, None);
    menu.append(&toggle_overlay)
        .map_err(|e| desktop_core::error::AppError::backend_unavailable(e.to_string()))?;
    menu.append(&quit)
        .map_err(|e| desktop_core::error::AppError::backend_unavailable(e.to_string()))?;

    let toggle_overlay_id = toggle_overlay.id().clone();
    let quit_id = quit.id().clone();
    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == toggle_overlay_id {
            trace::log("menubar:toggle_overlay click");
            let result = if overlay::is_active() {
                let result = overlay::stop_overlay();
                if result.is_ok() {
                    stop_overlay_live_loop();
                }
                result
            } else {
                let result = overlay::start_overlay();
                if result.is_ok() {
                    start_overlay_live_loop();
                }
                result
            };
            if let Err(err) = result {
                trace::log(format!("menubar:toggle_overlay err {err}"));
                eprintln!("overlay toggle failed: {err}");
            } else {
                trace::log(format!(
                    "menubar:toggle_overlay ok active={}",
                    overlay::is_active()
                ));
            }
            return;
        }
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

#[cfg(target_os = "macos")]
fn start_overlay_live_loop() {
    if OVERLAY_LIVE_ENABLED
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        trace::log("overlay:live_loop already_running");
        return;
    }
    trace::log("overlay:live_loop start");
    thread::spawn(|| {
        let mut consecutive_errors: usize = 0;
        while OVERLAY_LIVE_ENABLED.load(Ordering::SeqCst) {
            let request_id = format!(
                "overlay-live-{}-{}",
                now_millis(),
                OVERLAY_LIVE_SEQ.fetch_add(1, Ordering::SeqCst)
            );
            let request = RequestEnvelope::new(
                request_id,
                Command::ScreenTokenize {
                    overlay_out_path: None,
                    window_id: None,
                    screenshot_path: None,
                },
            );
            match ipc::send_request(&request) {
                Ok(ResponseEnvelope::Success(_)) => {
                    if consecutive_errors > 0 {
                        trace::log("overlay:live_loop recovered");
                        consecutive_errors = 0;
                    }
                }
                Ok(ResponseEnvelope::Error(err)) => {
                    consecutive_errors += 1;
                    if consecutive_errors == 1 || consecutive_errors % 25 == 0 {
                        trace::log(format!(
                            "overlay:live_loop tick_error code={:?} msg={}",
                            err.error.code, err.error.message
                        ));
                    }
                }
                Err(err) => {
                    consecutive_errors += 1;
                    if consecutive_errors == 1 || consecutive_errors % 25 == 0 {
                        trace::log(format!("overlay:live_loop ipc_error {err}"));
                    }
                }
            }
            thread::sleep(Duration::from_millis(OVERLAY_LIVE_INTERVAL_MS));
        }
        trace::log("overlay:live_loop stop");
    });
}

#[cfg(target_os = "macos")]
fn stop_overlay_live_loop() {
    if OVERLAY_LIVE_ENABLED.swap(false, Ordering::SeqCst) {
        trace::log("overlay:live_loop stop_requested");
    } else {
        trace::log("overlay:live_loop already_stopped");
    }
}

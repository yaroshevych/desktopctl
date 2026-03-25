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
    sync::{
        OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
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

// TrayIcon is !Send+!Sync; keep it in a thread-local so we never need to
// move it across threads. Icon updates are dispatched to the main thread via
// exec_async, so TRAY is always accessed from the same thread it was created on.
#[cfg(target_os = "macos")]
thread_local! {
    static TRAY: std::cell::RefCell<Option<tray_icon::TrayIcon>> = std::cell::RefCell::new(None);
}

#[cfg(target_os = "macos")]
static ICON_IDLE: OnceLock<tray_icon::Icon> = OnceLock::new();
#[cfg(target_os = "macos")]
static ICON_ACTIVE: OnceLock<tray_icon::Icon> = OnceLock::new();

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
                let is_active = overlay::is_active();
                trace::log(format!("menubar:toggle_overlay ok active={is_active}"));
                // Update icon on the main thread (TrayIcon is !Send).
                dispatch2::DispatchQueue::main().exec_async(move || {
                    let icon = if is_active {
                        ICON_ACTIVE.get().cloned()
                    } else {
                        ICON_IDLE.get().cloned()
                    };
                    TRAY.with(|cell| {
                        if let Some(tray) = cell.borrow().as_ref() {
                            let _ = tray.set_icon_with_as_template(icon, true);
                        }
                    });
                });
            }
            return;
        }
        if event.id == quit_id {
            std::process::exit(0);
        }
    }));

    // Pre-render both icons; fall back gracefully if SF symbol rendering fails.
    let idle = icon_idle().unwrap_or_else(|e| {
        eprintln!("icon_idle failed ({e}), using placeholder");
        placeholder_icon()
    });
    let active = icon_active().unwrap_or_else(|e| {
        eprintln!("icon_active failed ({e}), using placeholder");
        placeholder_icon()
    });
    let _ = ICON_IDLE.set(idle.clone());
    let _ = ICON_ACTIVE.set(active);

    let tray = TrayIconBuilder::new()
        .with_tooltip("DesktopCtl")
        .with_menu(Box::new(menu))
        .with_icon(idle)
        .with_icon_as_template(true)
        .build()
        .map_err(|e| desktop_core::error::AppError::backend_unavailable(e.to_string()))?;
    TRAY.with(|cell| *cell.borrow_mut() = Some(tray));

    ns_app.run();
    Ok(())
}

/// Idle icon: just the aperture shutter, no frame.
#[cfg(target_os = "macos")]
fn icon_idle() -> Result<tray_icon::Icon, desktop_core::error::AppError> {
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    const W: isize = 36;
    // Draw aperture at 90% so it has a little breathing room.
    let scale = 0.90_f64;
    let size = NSSize::new(W as f64 * scale, W as f64 * scale);
    let offset = ((W as f64 - size.width) / 2.0).round();
    let rect = NSRect {
        origin: NSPoint {
            x: offset,
            y: offset,
        },
        size,
    };
    render_sf_icon(W, &[("camera.aperture", rect)])
}

/// Active icon: viewfinder frame + aperture shutter inside it.
#[cfg(target_os = "macos")]
fn icon_active() -> Result<tray_icon::Icon, desktop_core::error::AppError> {
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    const W: isize = 36;
    let full = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize::new(W as f64, W as f64),
    };
    let ap_scale = 0.72_f64;
    let ap_size = NSSize::new(W as f64 * ap_scale, W as f64 * ap_scale);
    let ap_off = ((W as f64 - ap_size.width) / 2.0).round();
    let aperture = NSRect {
        origin: NSPoint {
            x: ap_off,
            y: ap_off,
        },
        size: ap_size,
    };
    render_sf_icon(W, &[("viewfinder", full), ("camera.aperture", aperture)])
}

/// Renders the given SF symbols at the specified rects into a single W×W RGBA icon.
/// Symbols are drawn in order (painter's algorithm: first = bottom).
#[cfg(target_os = "macos")]
fn render_sf_icon(
    w: isize,
    symbols: &[(&str, objc2_foundation::NSRect)],
) -> Result<tray_icon::Icon, desktop_core::error::AppError> {
    use objc2::{class, msg_send, runtime::AnyObject};
    use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};

    let err = |s: &str| desktop_core::error::AppError::backend_unavailable(s);
    const SOURCE_OVER: usize = 2; // NSCompositingOperationSourceOver
    let zero_rect = NSRect {
        origin: NSPoint { x: 0.0, y: 0.0 },
        size: NSSize::new(0.0, 0.0),
    };

    unsafe {
        let device_rgb = NSString::from_str("NSDeviceRGBColorSpace");
        let rep_alloc: *mut AnyObject = msg_send![class!(NSBitmapImageRep), alloc];
        let rep: *mut AnyObject = msg_send![
            rep_alloc,
            initWithBitmapDataPlanes: std::ptr::null_mut::<*mut u8>(),
            pixelsWide: w,
            pixelsHigh: w,
            bitsPerSample: 8isize,
            samplesPerPixel: 4isize,
            hasAlpha: true,
            isPlanar: false,
            colorSpaceName: &*device_rgb,
            bytesPerRow: 0isize,
            bitsPerPixel: 32isize
        ];
        if rep.is_null() {
            return Err(err("NSBitmapImageRep init failed"));
        }

        let ctx: *mut AnyObject = msg_send![
            class!(NSGraphicsContext),
            graphicsContextWithBitmapImageRep: rep
        ];
        if ctx.is_null() {
            return Err(err("NSGraphicsContext from bitmap failed"));
        }

        let _: () = msg_send![class!(NSGraphicsContext), saveGraphicsState];
        let _: () = msg_send![class!(NSGraphicsContext), setCurrentContext: ctx];

        // Black on transparent — template flag inverts to white on dark menu bars.
        let black: *mut AnyObject = msg_send![class!(NSColor), blackColor];
        let _: () = msg_send![black, set];

        for (name, rect) in symbols {
            let ns_name = NSString::from_str(name);
            let sym: *mut AnyObject = msg_send![
                class!(NSImage),
                imageWithSystemSymbolName: &*ns_name,
                accessibilityDescription: std::ptr::null::<AnyObject>()
            ];
            if !sym.is_null() {
                let _: () = msg_send![
                    sym,
                    drawInRect: *rect,
                    fromRect: zero_rect,
                    operation: SOURCE_OVER,
                    fraction: 1.0f64
                ];
            }
        }

        let _: () = msg_send![class!(NSGraphicsContext), restoreGraphicsState];

        let bpr: isize = msg_send![rep, bytesPerRow];
        let data: *const u8 = msg_send![rep, bitmapData];
        if data.is_null() {
            return Err(err("bitmapData is null"));
        }
        let bpr = bpr as usize;
        let mut rgba = Vec::with_capacity(w as usize * w as usize * 4);
        for row in 0..w as usize {
            let src = std::slice::from_raw_parts(data.add(row * bpr), w as usize * 4);
            rgba.extend_from_slice(src);
        }

        tray_icon::Icon::from_rgba(rgba, w as u32, w as u32).map_err(|e| err(&e.to_string()))
    }
}

/// Minimal placeholder used only if SF symbol rendering fails.
#[cfg(target_os = "macos")]
fn placeholder_icon() -> tray_icon::Icon {
    let w = 18u32;
    let rgba = vec![0xffu8; (w * w * 4) as usize]; // solid white square
    tray_icon::Icon::from_rgba(rgba, w, w).expect("placeholder icon")
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
            let tick_start = Instant::now();
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
            let mut tick_status = "ok";
            match ipc::send_request(&request) {
                Ok(ResponseEnvelope::Success(_)) => {
                    if consecutive_errors > 0 {
                        trace::log("overlay:live_loop recovered");
                        consecutive_errors = 0;
                    }
                }
                Ok(ResponseEnvelope::Error(err)) => {
                    tick_status = "tick_error";
                    consecutive_errors += 1;
                    if consecutive_errors == 1 || consecutive_errors % 25 == 0 {
                        trace::log(format!(
                            "overlay:live_loop tick_error code={:?} msg={}",
                            err.error.code, err.error.message
                        ));
                    }
                }
                Err(err) => {
                    tick_status = "ipc_error";
                    consecutive_errors += 1;
                    if consecutive_errors == 1 || consecutive_errors % 25 == 0 {
                        trace::log(format!("overlay:live_loop ipc_error {err}"));
                    }
                }
            }
            trace::log(format!(
                "overlay:live_loop timing_ms={} status={}",
                tick_start.elapsed().as_millis(),
                tick_status
            ));
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

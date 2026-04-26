use std::cell::RefCell;

use desktop_core::error::AppError;
use windows_sys::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext,
};

use crate::{daemon, trace};

thread_local! {
    static TRAY: RefCell<Option<tray_icon::TrayIcon>> = RefCell::new(None);
    static MENU_STATE: RefCell<Option<MenuState>> = RefCell::new(None);
}

#[derive(Clone)]
struct MenuState {
    toggle_cli_gui_ops: tray_icon::menu::MenuItem,
}

fn cli_gui_toggle_menu_label(disabled: bool) -> &'static str {
    if disabled {
        "Enable Agent Access"
    } else {
        "Disable Agent Access"
    }
}

fn on_gui_ops_state_changed(disabled: bool) {
    MENU_STATE.with(|cell| {
        if let Some(state) = cell.borrow().as_ref() {
            state
                .toggle_cli_gui_ops
                .set_text(cli_gui_toggle_menu_label(disabled));
        }
    });
}

pub(crate) fn run() -> Result<(), AppError> {
    enable_per_monitor_dpi_awareness();
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--on-demand") {
        return daemon::run_blocking(daemon::DaemonConfig::on_demand());
    }

    use tray_icon::{
        TrayIconBuilder,
        menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
    };

    daemon::start_background(daemon::DaemonConfig::resident())?;

    let menu = Menu::new();
    let toggle_cli_gui_ops = MenuItem::new(
        cli_gui_toggle_menu_label(daemon::gui_ops_disabled()),
        true,
        None,
    );
    let app_access_policy = MenuItem::new("Agent Permissions", true, None);
    let toggle_overlay = MenuItem::new("Toggle Overlay", true, None);
    let check_permissions = MenuItem::new("Setup Access", true, None);
    let about = MenuItem::new("About", true, None);
    let quit = MenuItem::new("Exit", true, None);

    menu.append(&toggle_cli_gui_ops)
        .map_err(|e| AppError::backend_unavailable(e.to_string()))?;
    menu.append(&app_access_policy)
        .map_err(|e| AppError::backend_unavailable(e.to_string()))?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|e| AppError::backend_unavailable(e.to_string()))?;
    menu.append(&toggle_overlay)
        .map_err(|e| AppError::backend_unavailable(e.to_string()))?;
    menu.append(&check_permissions)
        .map_err(|e| AppError::backend_unavailable(e.to_string()))?;
    menu.append(&about)
        .map_err(|e| AppError::backend_unavailable(e.to_string()))?;
    menu.append(&PredefinedMenuItem::separator())
        .map_err(|e| AppError::backend_unavailable(e.to_string()))?;
    menu.append(&quit)
        .map_err(|e| AppError::backend_unavailable(e.to_string()))?;

    let toggle_cli_gui_ops_id = toggle_cli_gui_ops.id().clone();
    let toggle_overlay_id = toggle_overlay.id().clone();
    let check_permissions_id = check_permissions.id().clone();
    let app_access_policy_id = app_access_policy.id().clone();
    let about_id = about.id().clone();
    let quit_id = quit.id().clone();

    MENU_STATE.with(|cell| {
        *cell.borrow_mut() = Some(MenuState {
            toggle_cli_gui_ops: toggle_cli_gui_ops.clone(),
        });
    });

    daemon::register_gui_ops_state_hook(on_gui_ops_state_changed);

    MenuEvent::set_event_handler(Some(move |event: MenuEvent| {
        if event.id == about_id {
            trace::log("menu:about click");
            return;
        }
        if event.id == check_permissions_id {
            trace::log("menu:check_permissions click");
            return;
        }
        if event.id == app_access_policy_id {
            trace::log("menu:app_access_policy click");
            return;
        }
        if event.id == toggle_cli_gui_ops_id {
            let disabled = !daemon::gui_ops_disabled();
            daemon::set_gui_ops_disabled(disabled);
            trace::log(format!("menu:toggle_cli_gui_ops disabled={disabled}"));
            return;
        }
        if event.id == toggle_overlay_id {
            trace::log("menu:toggle_overlay click");
            return;
        }
        if event.id == quit_id {
            std::process::exit(0);
        }
    }));

    let tray = TrayIconBuilder::new()
        .with_tooltip("DesktopCtl")
        .with_menu(Box::new(menu))
        .with_icon(placeholder_icon())
        .build()
        .map_err(|e| AppError::backend_unavailable(e.to_string()))?;

    TRAY.with(|cell| *cell.borrow_mut() = Some(tray));

    // Run the Win32 message loop for the tray icon
    run_message_loop()
}

fn run_message_loop() -> Result<(), AppError> {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetMessageW, TranslateMessage, DispatchMessageW, MSG,
    };

    let mut msg: MSG = unsafe { std::mem::zeroed() };

    loop {
        let ret = unsafe { GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) };
        if ret == 0 || ret == -1 {
            if ret == -1 {
                return Err(AppError::backend_unavailable("GetMessageW failed"));
            }
            break;
        }
        unsafe {
            TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    Ok(())
}

fn enable_per_monitor_dpi_awareness() {
    // SAFETY: process-wide DPI awareness bootstrap; failure is non-fatal and expected if already set.
    let _ = unsafe { SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
}

/// Minimal placeholder: 18x18 white RGBA square
fn placeholder_icon() -> tray_icon::Icon {
    let w = 18u32;
    let rgba = vec![0xffu8; (w * w * 4) as usize]; // solid white square
    tray_icon::Icon::from_rgba(rgba, w, w).expect("placeholder icon")
}

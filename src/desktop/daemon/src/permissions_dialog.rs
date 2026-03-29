use block2::RcBlock;
use std::{
    cell::RefCell,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use dispatch2::DispatchQueue;
use objc2::{
    ClassType, MainThreadMarker, MainThreadOnly, class, define_class, msg_send, rc::Retained,
    runtime::AnyObject, sel,
};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSFont, NSTextField, NSView, NSWindow,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSURL};

use crate::permissions;

const WEBSITE_URL: &str = "https://desktopctl.com";
const W: f64 = 500.0;
const H: f64 = 340.0;
// Right column: starts 12px past the button right edge (16 + 190 + 12 = 218).
const RX: f64 = 218.0;
const RW: f64 = W - RX - 16.0;
// Vertical centres for the three permission rows (macOS coords, y=0 at bottom).
const CY_CLI: f64 = 286.0;
const CY_AX: f64 = 216.0;
const CY_SR: f64 = 146.0;

// Button action target.
define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    struct PermissionsTarget;

    impl PermissionsTarget {
        #[unsafe(method(installAgentTool:))]
        fn install_agent_tool(&self, _: &AnyObject) {
            open_url(WEBSITE_URL);
        }

        #[unsafe(method(grantAccessibility:))]
        fn grant_accessibility(&self, _: &AnyObject) {
            permissions::open_accessibility_settings();
        }

        #[unsafe(method(grantScreenRecording:))]
        fn grant_screen_recording(&self, _: &AnyObject) {
            permissions::open_screen_recording_settings();
        }

        #[unsafe(method(openWebsite:))]
        fn open_website_action(&self, _: &AnyObject) {
            open_url(WEBSITE_URL);
        }
    }
);

// Raw pointers to updatable UI elements. Valid while DialogState is alive.
struct DialogState {
    window: Retained<NSWindow>,
    /// NSControl holds only a weak ref to its target; we keep the strong ref here.
    _target: Retained<AnyObject>,
    /// Notification center + observer token for NSWindowWillCloseNotification.
    nc: *mut AnyObject,
    observer: *mut AnyObject,
    cli_btn: *mut AnyObject,
    cli_status: *mut AnyObject,
    ax_btn: *mut AnyObject,
    ax_status: *mut AnyObject,
    sr_btn: *mut AnyObject,
    sr_status: *mut AnyObject,
    /// Cleared on Drop to stop the background refresh thread.
    active: Arc<AtomicBool>,
}

impl Drop for DialogState {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Relaxed);
        unsafe {
            if !self.nc.is_null() && !self.observer.is_null() {
                let _: () = msg_send![self.nc, removeObserver: self.observer];
            }
        }
    }
}

thread_local! {
    static DIALOG: RefCell<Option<DialogState>> = RefCell::new(None);
}

pub fn show() {
    DispatchQueue::main().exec_async(show_on_main);
}

fn show_on_main() {
    // Take the previous dialog state out before closing so close callbacks
    // can mutate DIALOG without RefCell reentrancy.
    if let Some(prev) = DIALOG.with(|cell| cell.borrow_mut().take()) {
        prev.window.close();
    }

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };

    let cli = cli_in_path();
    let ax = permissions::accessibility_granted();
    let sr = permissions::screen_recording_granted();

    unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window.setTitle(&NSString::from_str("Permissions"));

        let cv = window.contentView().expect("window has no content view");

        let target_raw: *mut AnyObject = msg_send![PermissionsTarget::class(), new];
        let target: &AnyObject = &*target_raw;
        let nc: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let will_close = NSString::from_str("NSWindowWillCloseNotification");
        let close_block = RcBlock::new(|_notif: *mut AnyObject| {
            DIALOG.with(|cell| {
                let _ = cell.borrow_mut().take();
            });
        });
        let observer: *mut AnyObject = msg_send![
            nc,
            addObserverForName: &*will_close,
            object: &*window,
            queue: std::ptr::null::<AnyObject>(),
            usingBlock: &*close_block
        ];

        // --- Agent Tool row ---
        let (cli_btn, cli_status) = permission_row(
            &cv,
            "Agent Tool",
            "Install",
            "AI agents use this CLI tool to control the desktop.",
            sel!(installAgentTool:),
            target,
            cli,
            CY_CLI,
            mtm,
            "● Installed",
            "● Not Installed",
        );
        cli_btn.setEnabled(!cli);

        // --- Accessibility row ---
        let (ax_btn, ax_status) = permission_row(
            &cv,
            "Accessibility",
            "Grant",
            "Controls mouse, keyboard input, and reads UI element state across all apps.",
            sel!(grantAccessibility:),
            target,
            ax,
            CY_AX,
            mtm,
            "● Granted",
            "● Not Granted",
        );
        ax_btn.setEnabled(!ax);

        // --- Screen Recording row ---
        let (sr_btn, sr_status) = permission_row(
            &cv,
            "Screen Recording",
            "Grant",
            "Captures screen frames for GPU-accelerated text recognition and visual grounding.",
            sel!(grantScreenRecording:),
            target,
            sr,
            CY_SR,
            mtm,
            "● Granted",
            "● Not Granted",
        );
        sr_btn.setEnabled(!sr);

        // --- Website / privacy row ---
        let website_btn = NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Website ↗"),
            Some(target),
            Some(sel!(openWebsite:)),
            mtm,
        );
        website_btn.setFrame(NSRect::new(
            NSPoint::new(16.0, 64.0),
            NSSize::new(130.0, 28.0),
        ));
        cv.addSubview(&website_btn);

        let privacy = NSTextField::wrappingLabelWithString(
            &NSString::from_str("Strictly local. No data is collected or sent to the internet."),
            mtm,
        );
        privacy.setFrame(NSRect::new(
            NSPoint::new(162.0, 54.0),
            NSSize::new(322.0, 44.0),
        ));
        privacy.setFont(Some(&NSFont::systemFontOfSize(11.0)));
        cv.addSubview(&privacy);

        // --- Close button (bottom right) ---
        let close_btn =
            NSButton::buttonWithTitle_target_action(&NSString::from_str("Close"), None, None, mtm);
        close_btn.setFrame(NSRect::new(
            NSPoint::new(W - 106.0, 12.0),
            NSSize::new(90.0, 28.0),
        ));
        let _: () = msg_send![&*close_btn, setTarget: &*window];
        let _: () = msg_send![&*close_btn, setAction: sel!(performClose:)];
        cv.addSubview(&close_btn);

        let cli_btn_raw = &*cli_btn as *const _ as *mut AnyObject;
        let cli_status_raw = &*cli_status as *const _ as *mut AnyObject;
        let ax_btn_raw = &*ax_btn as *const _ as *mut AnyObject;
        let ax_status_raw = &*ax_status as *const _ as *mut AnyObject;
        let sr_btn_raw = &*sr_btn as *const _ as *mut AnyObject;
        let sr_status_raw = &*sr_status as *const _ as *mut AnyObject;

        let active = Arc::new(AtomicBool::new(true));
        let active_thread = Arc::clone(&active);
        thread::spawn(move || {
            while active_thread.load(Ordering::Relaxed) {
                thread::sleep(Duration::from_secs(1));
                if !active_thread.load(Ordering::Relaxed) {
                    break;
                }
                DispatchQueue::main().exec_async(refresh_status);
            }
        });

        let state = DialogState {
            window,
            _target: Retained::from_raw(target_raw).unwrap(),
            nc,
            observer,
            cli_btn: cli_btn_raw,
            cli_status: cli_status_raw,
            ax_btn: ax_btn_raw,
            ax_status: ax_status_raw,
            sr_btn: sr_btn_raw,
            sr_status: sr_status_raw,
            active,
        };

        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![ns_app, activateIgnoringOtherApps: true];
        state.window.center();
        state.window.makeKeyAndOrderFront(None);

        DIALOG.with(|cell| *cell.borrow_mut() = Some(state));
    }
}

fn refresh_status() {
    DIALOG.with(|cell| {
        let borrowed = cell.borrow();
        let Some(ref s) = *borrowed else { return };
        let cli = cli_in_path();
        let ax = permissions::accessibility_granted();
        let sr = permissions::screen_recording_granted();
        unsafe {
            refresh_row(
                s.cli_btn,
                s.cli_status,
                cli,
                "Agent Tool",
                "Install",
                "● Installed",
                "● Not Installed",
            );
            refresh_row(
                s.ax_btn,
                s.ax_status,
                ax,
                "Accessibility",
                "Grant",
                "● Granted",
                "● Not Granted",
            );
            refresh_row(
                s.sr_btn,
                s.sr_status,
                sr,
                "Screen Recording",
                "Grant",
                "● Granted",
                "● Not Granted",
            );
        }
    });
}

unsafe fn refresh_row(
    btn: *mut AnyObject,
    status_lbl: *mut AnyObject,
    granted: bool,
    name: &str,
    verb: &str,
    status_granted: &str,
    status_not_granted: &str,
) {
    let title_str = if granted {
        format!("{name} ✓")
    } else {
        format!("{verb} {name}")
    };
    let title = NSString::from_str(&title_str);
    let _: () = msg_send![btn, setTitle: &*title];
    let _: () = msg_send![btn, setEnabled: !granted];

    let status_text = NSString::from_str(if granted {
        status_granted
    } else {
        status_not_granted
    });
    let _: () = msg_send![status_lbl, setStringValue: &*status_text];

    let color: *mut AnyObject = if granted {
        msg_send![class!(NSColor), systemGreenColor]
    } else {
        msg_send![class!(NSColor), systemOrangeColor]
    };
    let _: () = msg_send![status_lbl, setTextColor: color];
}

// Returns (button, status_label).
unsafe fn permission_row(
    cv: &NSView,
    name: &str,
    verb: &str,
    explanation: &str,
    action: objc2::runtime::Sel,
    target: &AnyObject,
    granted: bool,
    center_y: f64,
    mtm: MainThreadMarker,
    status_granted: &str,
    status_not_granted: &str,
) -> (Retained<NSButton>, Retained<NSTextField>) {
    let btn_label = if granted {
        format!("{name} ✓")
    } else {
        format!("{verb} {name}")
    };
    let btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(&btn_label),
            Some(target),
            Some(action),
            mtm,
        )
    };
    btn.setFrame(NSRect::new(
        NSPoint::new(16.0, center_y - 14.0),
        NSSize::new(190.0, 28.0),
    ));
    cv.addSubview(&btn);

    let status_text = if granted {
        status_granted
    } else {
        status_not_granted
    };
    let status = NSTextField::labelWithString(&NSString::from_str(status_text), mtm);
    status.setFrame(NSRect::new(
        NSPoint::new(RX, center_y + 2.0),
        NSSize::new(RW, 20.0),
    ));
    let color = if granted {
        NSColor::systemGreenColor()
    } else {
        NSColor::systemOrangeColor()
    };
    status.setTextColor(Some(&color));
    cv.addSubview(&status);

    let exp = NSTextField::wrappingLabelWithString(&NSString::from_str(explanation), mtm);
    exp.setFrame(NSRect::new(
        NSPoint::new(RX, center_y - 38.0),
        NSSize::new(RW, 34.0),
    ));
    exp.setFont(Some(&NSFont::systemFontOfSize(11.0)));
    cv.addSubview(&exp);

    (btn, status)
}

fn cli_in_path() -> bool {
    std::process::Command::new("which")
        .arg("desktopctl")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn open_url(url: &str) {
    if let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url)) {
        let workspace = NSWorkspace::sharedWorkspace();
        let _ = workspace.openURL(&nsurl);
    }
}

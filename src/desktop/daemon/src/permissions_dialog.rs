use block2::RcBlock;
use std::{
    cell::RefCell,
    collections::HashSet,
    fs,
    path::PathBuf,
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::Duration,
};

use dispatch2::DispatchQueue;
use objc2::{
    ClassType, MainThreadMarker, MainThreadOnly, Message, class, define_class, msg_send,
    rc::Retained, runtime::AnyObject, sel,
};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSControlSize, NSFont, NSTextField, NSView, NSWindow,
    NSWindowStyleMask, NSWorkspace,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString, NSURL};
use std::os::unix::fs::symlink;

use crate::permissions;

const WEBSITE_URL: &str = "https://desktopctl.com";
const W: f64 = 520.0;
const H: f64 = 380.0;
const MIN_W: f64 = 520.0;
const MIN_H: f64 = 380.0;
const OUTER_MARGIN: f64 = 16.0;
const COLUMN_GAP: f64 = 16.0;
const BUTTON_W: f64 = 190.0;
const BUTTON_H: f64 = 52.0;
const ROW_TOP: f64 = 16.0;
const ROW_GAP: f64 = 14.0;
const CONTENT_TOP_FROM_BUTTON: f64 = 10.0;
const CLOSE_BOTTOM_INSET: f64 = 8.0;

// Button action target.
define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    struct PermissionsTarget;

    impl PermissionsTarget {
        #[unsafe(method(installAgentTool:))]
        fn install_agent_tool(&self, _: &AnyObject) {
            let _ = install_cli_symlink();
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

#[derive(Clone, Copy)]
struct RowStatusSpec {
    granted: bool,
    status_granted: &'static str,
    status_not_granted: &'static str,
}

#[derive(Clone, Copy)]
struct RowSpec {
    name: &'static str,
    verb: &'static str,
    explanation: &'static str,
    action: objc2::runtime::Sel,
    status: Option<RowStatusSpec>,
}

struct BuiltRow {
    btn: Retained<NSButton>,
    status: Option<Retained<NSTextField>>,
    exp: Retained<NSTextField>,
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

fn set_autolayout<T: Message + ?Sized>(view: &T) {
    unsafe {
        let _: () = msg_send![view, setTranslatesAutoresizingMaskIntoConstraints: false];
    }
}

fn set_large_button(button: &NSButton) {
    unsafe {
        let _: () = msg_send![button, setControlSize: NSControlSize::Large];
    }
}

fn set_active(constraint: *mut AnyObject) {
    unsafe {
        let _: () = msg_send![constraint, setActive: true];
    }
}

fn eq_anchor(anchor: *mut AnyObject, to_anchor: *mut AnyObject, constant: f64) {
    let constraint: *mut AnyObject = unsafe {
        if constant == 0.0 {
            msg_send![anchor, constraintEqualToAnchor: to_anchor]
        } else {
            msg_send![anchor, constraintEqualToAnchor: to_anchor, constant: constant]
        }
    };
    set_active(constraint);
}

fn const_anchor(anchor: *mut AnyObject, constant: f64) {
    let constraint: *mut AnyObject =
        unsafe { msg_send![anchor, constraintEqualToConstant: constant] };
    set_active(constraint);
}

fn layout_row(
    cv: &NSView,
    btn: &NSButton,
    status: Option<&NSTextField>,
    exp: &NSTextField,
    prev_exp: Option<&NSTextField>,
    top_constant: f64,
) {
    let cv_top: *mut AnyObject = unsafe { msg_send![cv, topAnchor] };
    let cv_trailing: *mut AnyObject = unsafe { msg_send![cv, trailingAnchor] };
    let cv_leading: *mut AnyObject = unsafe { msg_send![cv, leadingAnchor] };

    let btn_top: *mut AnyObject = unsafe { msg_send![btn, topAnchor] };
    let btn_leading: *mut AnyObject = unsafe { msg_send![btn, leadingAnchor] };
    let btn_width: *mut AnyObject = unsafe { msg_send![btn, widthAnchor] };
    let btn_height: *mut AnyObject = unsafe { msg_send![btn, heightAnchor] };
    let btn_trailing: *mut AnyObject = unsafe { msg_send![btn, trailingAnchor] };

    if let Some(prev) = prev_exp {
        let prev_bottom: *mut AnyObject = unsafe { msg_send![prev, bottomAnchor] };
        eq_anchor(btn_top, prev_bottom, top_constant);
    } else {
        eq_anchor(btn_top, cv_top, top_constant);
    }
    eq_anchor(btn_leading, cv_leading, OUTER_MARGIN);
    const_anchor(btn_width, BUTTON_W);
    const_anchor(btn_height, BUTTON_H);

    let exp_top: *mut AnyObject = unsafe { msg_send![exp, topAnchor] };
    let exp_leading: *mut AnyObject = unsafe { msg_send![exp, leadingAnchor] };
    let exp_trailing: *mut AnyObject = unsafe { msg_send![exp, trailingAnchor] };
    if let Some(status) = status {
        let status_top: *mut AnyObject = unsafe { msg_send![status, topAnchor] };
        let status_leading: *mut AnyObject = unsafe { msg_send![status, leadingAnchor] };
        let status_trailing: *mut AnyObject = unsafe { msg_send![status, trailingAnchor] };
        eq_anchor(status_top, btn_top, CONTENT_TOP_FROM_BUTTON);
        eq_anchor(status_leading, btn_trailing, COLUMN_GAP);
        eq_anchor(status_trailing, cv_trailing, -OUTER_MARGIN);

        let status_bottom: *mut AnyObject = unsafe { msg_send![status, bottomAnchor] };
        eq_anchor(exp_top, status_bottom, 2.0);
        eq_anchor(exp_leading, status_leading, 0.0);
        eq_anchor(exp_trailing, status_trailing, 0.0);
    } else {
        eq_anchor(exp_top, btn_top, CONTENT_TOP_FROM_BUTTON);
        eq_anchor(exp_leading, btn_trailing, COLUMN_GAP);
        eq_anchor(exp_trailing, cv_trailing, -OUTER_MARGIN);
    }
}

fn layout_close_button(cv: &NSView, close_btn: &NSButton, last_content: &NSTextField) {
    let cv_bottom: *mut AnyObject = unsafe { msg_send![cv, bottomAnchor] };
    let cv_trailing: *mut AnyObject = unsafe { msg_send![cv, trailingAnchor] };
    let last_bottom: *mut AnyObject = unsafe { msg_send![last_content, bottomAnchor] };

    let close_top: *mut AnyObject = unsafe { msg_send![close_btn, topAnchor] };
    let close_bottom: *mut AnyObject = unsafe { msg_send![close_btn, bottomAnchor] };
    let close_trailing: *mut AnyObject = unsafe { msg_send![close_btn, trailingAnchor] };
    let close_width: *mut AnyObject = unsafe { msg_send![close_btn, widthAnchor] };
    let close_height: *mut AnyObject = unsafe { msg_send![close_btn, heightAnchor] };
    eq_anchor(close_top, last_bottom, ROW_GAP + 8.0);
    eq_anchor(close_bottom, cv_bottom, -CLOSE_BOTTOM_INSET);
    eq_anchor(close_trailing, cv_trailing, -OUTER_MARGIN);
    const_anchor(close_width, 100.0);
    const_anchor(close_height, BUTTON_H);
}

fn autosize_window_to_content(window: &NSWindow, cv: &NSView) {
    unsafe {
        let _: () = msg_send![cv, layoutSubtreeIfNeeded];
        let fitting: NSSize = msg_send![cv, fittingSize];
        let target = NSSize::new(fitting.width.max(MIN_W), fitting.height.max(MIN_H));
        let _: () = msg_send![window, setContentSize: target];
    }
}

fn build_row(cv: &NSView, target: &AnyObject, mtm: MainThreadMarker, spec: RowSpec) -> BuiltRow {
    let (btn, status, exp) = unsafe {
        dialog_row(
            cv,
            spec.name,
            spec.verb,
            spec.explanation,
            spec.action,
            target,
            spec.status
                .map(|s| (s.granted, s.status_granted, s.status_not_granted)),
            mtm,
        )
    };
    BuiltRow { btn, status, exp }
}

fn set_row_enabled(row: &BuiltRow, enabled: bool) {
    row.btn.setEnabled(enabled);
}

fn set_row_autolayout(row: &BuiltRow) {
    set_autolayout(&*row.btn);
    if let Some(status) = row.status.as_ref() {
        set_autolayout(&**status);
    }
    set_autolayout(&*row.exp);
}

fn row_status_ref(row: &BuiltRow) -> &NSTextField {
    row.status
        .as_deref()
        .expect("row should include a status label")
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

        let cli_row = build_row(
            &cv,
            target,
            mtm,
            RowSpec {
                name: "Agent Tool",
                verb: "Install",
                explanation: "Your AI agent uses this tool to see and control your desktop. Once installed, agents can open apps, click buttons, type text, and wait for results — working through any application on your Mac without manual help.",
                action: sel!(installAgentTool:),
                status: Some(RowStatusSpec {
                    granted: cli,
                    status_granted: "● Installed",
                    status_not_granted: "● Not Installed",
                }),
            },
        );
        set_row_enabled(&cli_row, !cli);

        let ax_row = build_row(
            &cv,
            target,
            mtm,
            RowSpec {
                name: "Accessibility",
                verb: "Grant",
                explanation: "Lets agents read what's on screen and interact with it — buttons, inputs, menus, and more. Without this, agents can see the screen but cannot understand or act on what's in it.\n\nNote: if DesktopCtl is already in the list of allowed apps, remove and add it again.",
                action: sel!(grantAccessibility:),
                status: Some(RowStatusSpec {
                    granted: ax,
                    status_granted: "● Granted",
                    status_not_granted: "● Not Granted",
                }),
            },
        );
        set_row_enabled(&ax_row, !ax);

        let sr_row = build_row(
            &cv,
            target,
            mtm,
            RowSpec {
                name: "Screen Recording",
                verb: "Grant",
                explanation: "Lets agents see your screen so they can navigate apps visually. All processing happens on your Mac. Nothing is uploaded or sent to your AI provider unless you explicitly ask it to.\n\nNote: if DesktopCtl is already in the list of allowed apps, remove and add it again.",
                action: sel!(grantScreenRecording:),
                status: Some(RowStatusSpec {
                    granted: sr,
                    status_granted: "● Granted",
                    status_not_granted: "● Not Granted",
                }),
            },
        );
        set_row_enabled(&sr_row, !sr);

        let website_row = build_row(
            &cv,
            target,
            mtm,
            RowSpec {
                name: "Website ↗",
                verb: "",
                explanation: "Learn more about how DesktopCtl works, what agents can do with it, and how to get started.",
                action: sel!(openWebsite:),
                status: None,
            },
        );

        // --- Close button (bottom right) ---
        let close_btn =
            NSButton::buttonWithTitle_target_action(&NSString::from_str("Close"), None, None, mtm);
        set_large_button(&close_btn);
        let _: () = msg_send![&*close_btn, setTarget: &*window];
        let _: () = msg_send![&*close_btn, setAction: sel!(performClose:)];
        cv.addSubview(&close_btn);

        set_row_autolayout(&cli_row);
        set_row_autolayout(&ax_row);
        set_row_autolayout(&sr_row);
        set_row_autolayout(&website_row);
        set_autolayout(&*close_btn);

        layout_row(
            &cv,
            &cli_row.btn,
            cli_row.status.as_deref(),
            &cli_row.exp,
            None,
            ROW_TOP,
        );
        layout_row(
            &cv,
            &ax_row.btn,
            ax_row.status.as_deref(),
            &ax_row.exp,
            Some(&cli_row.exp),
            ROW_GAP,
        );
        layout_row(
            &cv,
            &sr_row.btn,
            sr_row.status.as_deref(),
            &sr_row.exp,
            Some(&ax_row.exp),
            ROW_GAP,
        );
        layout_row(
            &cv,
            &website_row.btn,
            website_row.status.as_deref(),
            &website_row.exp,
            Some(&sr_row.exp),
            ROW_GAP,
        );
        layout_close_button(&cv, &close_btn, &website_row.exp);
        autosize_window_to_content(&window, &cv);

        let cli_btn_raw = &*cli_row.btn as *const _ as *mut AnyObject;
        let cli_status_raw = row_status_ref(&cli_row) as *const _ as *mut AnyObject;
        let ax_btn_raw = &*ax_row.btn as *const _ as *mut AnyObject;
        let ax_status_raw = row_status_ref(&ax_row) as *const _ as *mut AnyObject;
        let sr_btn_raw = &*sr_row.btn as *const _ as *mut AnyObject;
        let sr_status_raw = row_status_ref(&sr_row) as *const _ as *mut AnyObject;

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
unsafe fn dialog_row(
    cv: &NSView,
    name: &str,
    verb: &str,
    explanation: &str,
    action: objc2::runtime::Sel,
    target: &AnyObject,
    status: Option<(bool, &str, &str)>,
    mtm: MainThreadMarker,
) -> (
    Retained<NSButton>,
    Option<Retained<NSTextField>>,
    Retained<NSTextField>,
) {
    let btn_label = if let Some((granted, _, _)) = status {
        if granted {
            format!("{name} ✓")
        } else {
            format!("{verb} {name}")
        }
    } else {
        name.to_string()
    };
    let btn = unsafe {
        NSButton::buttonWithTitle_target_action(
            &NSString::from_str(&btn_label),
            Some(target),
            Some(action),
            mtm,
        )
    };
    set_large_button(&btn);
    cv.addSubview(&btn);

    let status = if let Some((granted, status_granted, status_not_granted)) = status {
        let status_text = if granted {
            status_granted
        } else {
            status_not_granted
        };
        let status = NSTextField::labelWithString(&NSString::from_str(status_text), mtm);
        status.setFont(Some(&NSFont::boldSystemFontOfSize(17.0)));
        let color = if granted {
            NSColor::systemGreenColor()
        } else {
            NSColor::systemOrangeColor()
        };
        status.setTextColor(Some(&color));
        cv.addSubview(&status);
        Some(status)
    } else {
        None
    };

    let exp = NSTextField::wrappingLabelWithString(&NSString::from_str(explanation), mtm);
    exp.setFont(Some(&NSFont::systemFontOfSize(12.0)));
    cv.addSubview(&exp);

    (btn, status, exp)
}

fn cli_in_path() -> bool {
    Command::new("which")
        .arg("desktopctl")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn discover_cli_binary() -> Option<PathBuf> {
    if let Ok(path) = std::env::var("DESKTOPCTL_CLI_BIN") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Some(candidate);
        }
    }

    let exe = std::env::current_exe().ok()?;
    let exe_dir = exe.parent()?;

    let sibling = exe_dir.join("desktopctl");
    if sibling.exists() {
        return Some(sibling);
    }

    let bundled = exe_dir
        .parent()
        .and_then(|p| p.parent())
        .map(|p| p.join("MacOS/desktopctl"));
    if let Some(candidate) = bundled {
        if candidate.exists() {
            return Some(candidate);
        }
    }

    None
}

fn install_cli_symlink() -> Result<PathBuf, String> {
    let source = discover_cli_binary().ok_or_else(|| "desktopctl binary not found".to_string())?;

    let mut candidate_dirs: Vec<PathBuf> = std::env::var("PATH")
        .ok()
        .map(|path| {
            path.split(':')
                .filter(|segment| !segment.trim().is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();

    if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
        candidate_dirs.push(PathBuf::from("/usr/local/bin"));
        candidate_dirs.push(PathBuf::from("/opt/homebrew/bin"));
        candidate_dirs.push(home.join(".local/bin"));
        candidate_dirs.push(home.join("bin"));
    }

    let mut seen: HashSet<PathBuf> = HashSet::new();
    candidate_dirs.retain(|dir| seen.insert(dir.clone()));

    for dir in candidate_dirs {
        if fs::create_dir_all(&dir).is_err() {
            continue;
        }
        let link_path = dir.join("desktopctl");
        if let Ok(meta) = fs::symlink_metadata(&link_path) {
            if meta.file_type().is_symlink() {
                if let Ok(existing_target) = fs::read_link(&link_path) {
                    if existing_target == source {
                        return Ok(link_path);
                    }
                }
                if fs::remove_file(&link_path).is_err() {
                    continue;
                }
            } else {
                continue;
            }
        }
        if symlink(&source, &link_path).is_ok() {
            return Ok(link_path);
        }
    }

    Err("failed to install desktopctl symlink in writable PATH dir".to_string())
}

fn open_url(url: &str) {
    if let Some(nsurl) = NSURL::URLWithString(&NSString::from_str(url)) {
        let workspace = NSWorkspace::sharedWorkspace();
        let _ = workspace.openURL(&nsurl);
    }
}

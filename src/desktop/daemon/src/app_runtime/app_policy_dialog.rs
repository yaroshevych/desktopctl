use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::{
    ClassType, MainThreadMarker, MainThreadOnly, class, define_class, msg_send, rc::Retained,
    runtime::AnyObject, sel,
};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSFont, NSTextField, NSWindow, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize, NSString};
use std::{
    cell::RefCell,
    ffi::CStr,
    sync::atomic::{AtomicU64, Ordering},
    thread,
    time::Duration,
};

use crate::app_policy::{self, AppPolicyConfig, PolicyMode};

const W: f64 = 448.0;
const H: f64 = 270.0;
const SAVE_DEBOUNCE_MS: u64 = 500;
static SAVE_DEBOUNCE_SEQ: AtomicU64 = AtomicU64::new(0);

// Button/action target.
define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    struct AppPolicyTarget;

    impl AppPolicyTarget {
        #[unsafe(method(appPolicyModeChanged:))]
        fn app_policy_mode_changed(&self, _: &AnyObject) {
            schedule_debounced_save();
        }

        #[unsafe(method(appPolicyAppsChanged:))]
        fn app_policy_apps_changed(&self, _: &AnyObject) {
            schedule_debounced_save();
        }

        #[unsafe(method(closePolicyDialog:))]
        fn close_policy_dialog(&self, sender: &AnyObject) {
            unsafe {
                let window: *mut AnyObject = msg_send![sender, window];
                commit_window_editing(window);
            }
            unsafe {
                let window: *mut AnyObject = msg_send![sender, window];
                if !window.is_null() {
                    let _: () = msg_send![window, performClose: sender];
                }
            }
        }
    }
);

struct DialogState {
    window: Retained<NSWindow>,
    _target: Retained<AnyObject>,
    nc: *mut AnyObject,
    observer: *mut AnyObject,
    text_observer: *mut AnyObject,
    mode_popup: Retained<AnyObject>,
    apps_field: Retained<AnyObject>,
    full_screen_checkbox: Retained<AnyObject>,
    warning_label: Retained<NSTextField>,
}

impl Drop for DialogState {
    fn drop(&mut self) {
        unsafe {
            if !self.nc.is_null() && !self.observer.is_null() {
                let _: () = msg_send![self.nc, removeObserver: self.observer];
            }
            if !self.nc.is_null() && !self.text_observer.is_null() {
                let _: () = msg_send![self.nc, removeObserver: self.text_observer];
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

fn mode_to_index(mode: PolicyMode) -> isize {
    match mode {
        PolicyMode::AllowAll => 0,
        PolicyMode::AllowOnlySelected => 1,
        PolicyMode::AllowAllExcept => 2,
    }
}

fn index_to_mode(index: isize) -> PolicyMode {
    match index {
        1 => PolicyMode::AllowOnlySelected,
        2 => PolicyMode::AllowAllExcept,
        _ => PolicyMode::AllowAll,
    }
}

unsafe fn selected_policy_mode(popup: &AnyObject) -> PolicyMode {
    let index: isize = msg_send![popup, indexOfSelectedItem];
    index_to_mode(index)
}

unsafe fn string_value(control: &AnyObject) -> String {
    let ns_string: *mut AnyObject = msg_send![control, stringValue];
    if ns_string.is_null() {
        return String::new();
    }
    let c_ptr: *const std::ffi::c_char = msg_send![ns_string, UTF8String];
    if c_ptr.is_null() {
        return String::new();
    }
    unsafe { CStr::from_ptr(c_ptr) }
        .to_string_lossy()
        .into_owned()
}

unsafe fn bool_state(control: &AnyObject) -> bool {
    let state: isize = msg_send![control, state];
    state != 0
}

unsafe fn apply_policy_controls_state(
    mode: PolicyMode,
    apps: &[String],
    apps_field: &AnyObject,
    warning_label: &AnyObject,
) {
    let _: () = msg_send![apps_field, setEnabled: mode != PolicyMode::AllowAll];
    let warning = if mode != PolicyMode::AllowAll && apps.is_empty() {
        "Add at least one app for this mode."
    } else {
        ""
    };
    let warning_ns = NSString::from_str(warning);
    let _: () = msg_send![warning_label, setStringValue: &*warning_ns];
}

fn persist_policy_from_dialog() {
    DIALOG.with(|cell| {
        let borrowed = cell.borrow();
        let Some(ref state) = *borrowed else { return };
        unsafe {
            let mode = selected_policy_mode(&state.mode_popup);
            let csv = string_value(&state.apps_field);
            let apps = app_policy::normalize_apps_csv(&csv);
            let cfg = AppPolicyConfig {
                policy_mode: mode,
                apps: apps.clone(),
                allow_full_screen_capture: bool_state(&state.full_screen_checkbox),
                agent_access_disabled: app_policy::current().agent_access_disabled,
            };
            if let Err(err) = app_policy::save(&cfg) {
                eprintln!("failed to save app policy config: {err}");
            } else {
                app_policy::set_current(&cfg);
            }
            apply_policy_controls_state(mode, &apps, &state.apps_field, &state.warning_label);
        }
    });
}

fn save_policy_now() {
    SAVE_DEBOUNCE_SEQ.fetch_add(1, Ordering::SeqCst);
    persist_policy_from_dialog();
}

fn save_policy_if_ticket(ticket: u64) {
    if SAVE_DEBOUNCE_SEQ.load(Ordering::SeqCst) != ticket {
        return;
    }
    persist_policy_from_dialog();
}

fn schedule_debounced_save() {
    let ticket = SAVE_DEBOUNCE_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    thread::spawn(move || {
        thread::sleep(Duration::from_millis(SAVE_DEBOUNCE_MS));
        if SAVE_DEBOUNCE_SEQ.load(Ordering::SeqCst) != ticket {
            return;
        }
        DispatchQueue::main().exec_async(move || save_policy_if_ticket(ticket));
    });
}

unsafe fn commit_window_editing(window: *mut AnyObject) {
    if window.is_null() {
        return;
    }
    let _: bool = unsafe { msg_send![window, makeFirstResponder: std::ptr::null::<AnyObject>()] };
}

fn show_on_main() {
    if let Some(prev) = DIALOG.with(|cell| cell.borrow_mut().take()) {
        prev.window.close();
    }

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };

    let load_outcome = app_policy::load_with_diagnostics();
    let cfg = load_outcome.config;

    unsafe {
        if let Some(warning) = load_outcome.warning.as_ref() {
            let alert: *mut AnyObject = msg_send![class!(NSAlert), new];
            let msg = NSString::from_str("App Access Policy Config Error");
            let _: () = msg_send![alert, setMessageText: &*msg];
            let info = NSString::from_str(&format!(
                "{warning}\n\nDesktopCtl loaded default policy settings."
            ));
            let _: () = msg_send![alert, setInformativeText: &*info];
            let ok = NSString::from_str("OK");
            let _: () = msg_send![alert, addButtonWithTitle: &*ok];
            let _: usize = msg_send![alert, runModal];
        }

        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window.setTitle(&NSString::from_str("Agent Permissions"));

        let cv = window.contentView().expect("window has no content view");

        let target_raw: *mut AnyObject = msg_send![AppPolicyTarget::class(), new];
        let target: &AnyObject = &*target_raw;

        let nc: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let will_close = NSString::from_str("NSWindowWillCloseNotification");
        let close_block = RcBlock::new(|notif: *mut AnyObject| {
            let window: *mut AnyObject = msg_send![notif, object];
            commit_window_editing(window);
            save_policy_now();
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
        let text_changed = NSString::from_str("NSControlTextDidChangeNotification");
        let text_change_block = RcBlock::new(|_notif: *mut AnyObject| {
            schedule_debounced_save();
        });

        let title = NSTextField::wrappingLabelWithString(
            &NSString::from_str("Choose which frontmost apps DesktopCtl can control."),
            mtm,
        );
        title.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        title.setFrame(NSRect::new(
            NSPoint::new(20.0, 218.0),
            NSSize::new(408.0, 28.0),
        ));
        cv.addSubview(&title);

        let mode_popup: *mut AnyObject = msg_send![class!(NSPopUpButton), alloc];
        let mode_popup: *mut AnyObject = msg_send![
            mode_popup,
            initWithFrame: NSRect::new(NSPoint::new(20.0, 176.0), NSSize::new(408.0, 30.0)),
            pullsDown: false
        ];
        let _: () = msg_send![mode_popup, addItemWithTitle: &*NSString::from_str("Allow all")];
        let _: () = msg_send![
            mode_popup,
            addItemWithTitle: &*NSString::from_str("Allow only selected")
        ];
        let _: () = msg_send![
            mode_popup,
            addItemWithTitle: &*NSString::from_str("Allow all, except")
        ];
        let _: () = msg_send![mode_popup, selectItemAtIndex: mode_to_index(cfg.policy_mode)];
        let _: () = msg_send![mode_popup, setTarget: target];
        let _: () = msg_send![mode_popup, setAction: sel!(appPolicyModeChanged:)];
        let _: () = msg_send![&*cv, addSubview: mode_popup];

        let apps_field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let apps_field: *mut AnyObject = msg_send![
            apps_field,
            initWithFrame: NSRect::new(NSPoint::new(20.0, 138.0), NSSize::new(408.0, 24.0))
        ];
        let apps_csv = app_policy::apps_to_csv(&cfg.apps);
        let _: () = msg_send![apps_field, setStringValue: &*NSString::from_str(&apps_csv)];
        let _: () = msg_send![
            apps_field,
            setPlaceholderString: Some(&*NSString::from_str("e.g. Safari, Slack, Terminal"))
        ];
        let _: () = msg_send![apps_field, setTarget: target];
        let _: () = msg_send![apps_field, setAction: sel!(appPolicyAppsChanged:)];
        let _: () = msg_send![&*cv, addSubview: apps_field];
        let text_observer: *mut AnyObject = msg_send![
            nc,
            addObserverForName: &*text_changed,
            object: apps_field,
            queue: std::ptr::null::<AnyObject>(),
            usingBlock: &*text_change_block
        ];

        let full_screen_checkbox: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let full_screen_checkbox: *mut AnyObject = msg_send![
            full_screen_checkbox,
            initWithFrame: NSRect::new(NSPoint::new(20.0, 78.0), NSSize::new(408.0, 18.0))
        ];
        let _: () = msg_send![full_screen_checkbox, setButtonType: 3usize];
        let _: () = msg_send![
            full_screen_checkbox,
            setTitle: &*NSString::from_str("Allow full-screen capture")
        ];
        let _: () = msg_send![
            full_screen_checkbox,
            setState: if cfg.allow_full_screen_capture { 1isize } else { 0isize }
        ];
        let _: () = msg_send![full_screen_checkbox, setTarget: target];
        let _: () = msg_send![full_screen_checkbox, setAction: sel!(appPolicyModeChanged:)];
        let _: () = msg_send![&*cv, addSubview: full_screen_checkbox];

        let helper = NSTextField::wrappingLabelWithString(
            &NSString::from_str("Comma-separated app names. Example: Safari, Slack"),
            mtm,
        );
        helper.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        helper.setFrame(NSRect::new(
            NSPoint::new(20.0, 116.0),
            NSSize::new(408.0, 18.0),
        ));
        cv.addSubview(&helper);

        let warning = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        warning.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        warning.setTextColor(Some(&NSColor::systemOrangeColor()));
        warning.setFrame(NSRect::new(
            NSPoint::new(20.0, 56.0),
            NSSize::new(408.0, 16.0),
        ));
        cv.addSubview(&warning);

        let close_btn =
            NSButton::buttonWithTitle_target_action(&NSString::from_str("Close"), None, None, mtm);
        close_btn.setFrame(NSRect::new(
            NSPoint::new(328.0, 20.0),
            NSSize::new(100.0, 32.0),
        ));
        let _: () = msg_send![&*close_btn, setTarget: target];
        let _: () = msg_send![&*close_btn, setAction: sel!(closePolicyDialog:)];
        cv.addSubview(&close_btn);

        apply_policy_controls_state(cfg.policy_mode, &cfg.apps, &*apps_field, &*warning);

        let state = DialogState {
            window,
            _target: Retained::from_raw(target_raw).unwrap(),
            nc,
            observer,
            text_observer,
            mode_popup: Retained::from_raw(mode_popup).unwrap(),
            apps_field: Retained::from_raw(apps_field).unwrap(),
            full_screen_checkbox: Retained::from_raw(full_screen_checkbox).unwrap(),
            warning_label: warning,
        };

        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![ns_app, activateIgnoringOtherApps: true];
        state.window.center();
        state.window.makeKeyAndOrderFront(None);

        DIALOG.with(|cell| *cell.borrow_mut() = Some(state));
    }
}

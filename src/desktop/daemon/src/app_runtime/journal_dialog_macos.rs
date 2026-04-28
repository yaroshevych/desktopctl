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
use std::{cell::RefCell, ffi::CStr, path::PathBuf};

use crate::journal::{self, JournalConfig};

const W: f64 = 500.0;
const H: f64 = 250.0;

define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    struct JournalTarget;

    impl JournalTarget {
        #[unsafe(method(chooseJournalDirectory:))]
        fn choose_journal_directory(&self, _: &AnyObject) {
            choose_directory();
        }

        #[unsafe(method(saveJournalDialog:))]
        fn save_journal_dialog(&self, sender: &AnyObject) {
            unsafe {
                let window: *mut AnyObject = msg_send![sender, window];
                commit_window_editing(window);
            }
            persist_from_dialog();
            unsafe {
                let window: *mut AnyObject = msg_send![sender, window];
                if !window.is_null() {
                    let _: () = msg_send![window, performClose: sender];
                }
            }
        }

        #[unsafe(method(cancelJournalDialog:))]
        fn cancel_journal_dialog(&self, sender: &AnyObject) {
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
    enabled_checkbox: Retained<AnyObject>,
    interval_field: Retained<AnyObject>,
    output_field: Retained<AnyObject>,
    warning_label: Retained<NSTextField>,
}

impl Drop for DialogState {
    fn drop(&mut self) {
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

unsafe fn commit_window_editing(window: *mut AnyObject) {
    if window.is_null() {
        return;
    }
    let _: bool = unsafe { msg_send![window, makeFirstResponder: std::ptr::null::<AnyObject>()] };
}

fn persist_from_dialog() {
    DIALOG.with(|cell| {
        let borrowed = cell.borrow();
        let Some(ref state) = *borrowed else { return };
        unsafe {
            let interval_raw = string_value(&state.interval_field);
            let interval_seconds = interval_raw.trim().parse::<u64>().unwrap_or(30).max(1);
            let output_dir = PathBuf::from(string_value(&state.output_field));
            let cfg = JournalConfig {
                enabled: bool_state(&state.enabled_checkbox),
                interval_seconds,
                output_dir,
            };
            if let Err(err) = journal::apply(cfg) {
                state
                    .warning_label
                    .setStringValue(&NSString::from_str(&err));
            }
        }
    });
}

fn choose_directory() {
    DIALOG.with(|cell| {
        let borrowed = cell.borrow();
        let Some(ref state) = *borrowed else { return };
        unsafe {
            let panel: *mut AnyObject = msg_send![class!(NSOpenPanel), openPanel];
            let _: () = msg_send![panel, setCanChooseFiles: false];
            let _: () = msg_send![panel, setCanChooseDirectories: true];
            let _: () = msg_send![panel, setAllowsMultipleSelection: false];
            let result: isize = msg_send![panel, runModal];
            if result != 1 {
                return;
            }
            let url: *mut AnyObject = msg_send![panel, URL];
            if url.is_null() {
                return;
            }
            let path: *mut AnyObject = msg_send![url, path];
            if path.is_null() {
                return;
            }
            let _: () = msg_send![&*state.output_field, setStringValue: path];
        }
    });
}

fn show_on_main() {
    if let Some(prev) = DIALOG.with(|cell| cell.borrow_mut().take()) {
        prev.window.close();
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let cfg = journal::load_current_from_disk().config;

    unsafe {
        let window = NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(W, H)),
            NSWindowStyleMask::Titled | NSWindowStyleMask::Closable,
            NSBackingStoreType::Buffered,
            false,
        );
        window.setReleasedWhenClosed(false);
        window.setTitle(&NSString::from_str("Journal"));
        let cv = window.contentView().expect("window has no content view");
        let target_raw: *mut AnyObject = msg_send![JournalTarget::class(), new];
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

        let title = NSTextField::wrappingLabelWithString(
            &NSString::from_str("Capture active-window tokenize journals on an interval."),
            mtm,
        );
        title.setFont(Some(&NSFont::systemFontOfSize(14.0)));
        title.setFrame(NSRect::new(
            NSPoint::new(20.0, 194.0),
            NSSize::new(460.0, 28.0),
        ));
        cv.addSubview(&title);

        let enabled_checkbox: *mut AnyObject = msg_send![class!(NSButton), alloc];
        let enabled_checkbox: *mut AnyObject = msg_send![
            enabled_checkbox,
            initWithFrame: NSRect::new(NSPoint::new(20.0, 160.0), NSSize::new(440.0, 22.0))
        ];
        let _: () = msg_send![enabled_checkbox, setButtonType: 3usize];
        let _: () = msg_send![enabled_checkbox, setTitle: &*NSString::from_str("Enabled")];
        let _: () =
            msg_send![enabled_checkbox, setState: if cfg.enabled { 1isize } else { 0isize }];
        let _: () = msg_send![&*cv, addSubview: enabled_checkbox];

        let interval_label =
            NSTextField::labelWithString(&NSString::from_str("Timeout seconds"), mtm);
        interval_label.setFrame(NSRect::new(
            NSPoint::new(20.0, 122.0),
            NSSize::new(130.0, 20.0),
        ));
        cv.addSubview(&interval_label);
        let interval_field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let interval_field: *mut AnyObject = msg_send![
            interval_field,
            initWithFrame: NSRect::new(NSPoint::new(160.0, 118.0), NSSize::new(90.0, 24.0))
        ];
        let _: () = msg_send![
            interval_field,
            setStringValue: &*NSString::from_str(&cfg.interval_seconds.to_string())
        ];
        let _: () = msg_send![&*cv, addSubview: interval_field];

        let output_label =
            NSTextField::labelWithString(&NSString::from_str("Output directory"), mtm);
        output_label.setFrame(NSRect::new(
            NSPoint::new(20.0, 82.0),
            NSSize::new(130.0, 20.0),
        ));
        cv.addSubview(&output_label);
        let output_field: *mut AnyObject = msg_send![class!(NSTextField), alloc];
        let output_field: *mut AnyObject = msg_send![
            output_field,
            initWithFrame: NSRect::new(NSPoint::new(160.0, 78.0), NSSize::new(230.0, 24.0))
        ];
        let _: () = msg_send![
            output_field,
            setStringValue: &*NSString::from_str(&cfg.output_dir.display().to_string())
        ];
        let _: () = msg_send![&*cv, addSubview: output_field];

        let choose_btn = NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Choose..."),
            Some(target),
            Some(sel!(chooseJournalDirectory:)),
            mtm,
        );
        choose_btn.setFrame(NSRect::new(
            NSPoint::new(400.0, 74.0),
            NSSize::new(80.0, 32.0),
        ));
        cv.addSubview(&choose_btn);

        let warning = NSTextField::labelWithString(&NSString::from_str(""), mtm);
        warning.setFont(Some(&NSFont::systemFontOfSize(12.0)));
        warning.setTextColor(Some(&NSColor::systemOrangeColor()));
        warning.setFrame(NSRect::new(
            NSPoint::new(20.0, 54.0),
            NSSize::new(460.0, 18.0),
        ));
        cv.addSubview(&warning);

        let cancel_btn = NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Cancel"),
            Some(target),
            Some(sel!(cancelJournalDialog:)),
            mtm,
        );
        cancel_btn.setFrame(NSRect::new(
            NSPoint::new(276.0, 18.0),
            NSSize::new(100.0, 32.0),
        ));
        cv.addSubview(&cancel_btn);
        let save_btn = NSButton::buttonWithTitle_target_action(
            &NSString::from_str("Save"),
            Some(target),
            Some(sel!(saveJournalDialog:)),
            mtm,
        );
        save_btn.setFrame(NSRect::new(
            NSPoint::new(384.0, 18.0),
            NSSize::new(96.0, 32.0),
        ));
        cv.addSubview(&save_btn);

        let state = DialogState {
            window,
            _target: Retained::from_raw(target_raw).unwrap(),
            nc,
            observer,
            enabled_checkbox: Retained::from_raw(enabled_checkbox).unwrap(),
            interval_field: Retained::from_raw(interval_field).unwrap(),
            output_field: Retained::from_raw(output_field).unwrap(),
            warning_label: warning,
        };

        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![ns_app, activateIgnoringOtherApps: true];
        state.window.center();
        state.window.makeKeyAndOrderFront(None);
        DIALOG.with(|cell| *cell.borrow_mut() = Some(state));
    }
}

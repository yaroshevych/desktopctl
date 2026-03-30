use std::cell::RefCell;

use dispatch2::DispatchQueue;
use objc2::{ClassType, class, define_class, msg_send, rc::Retained, runtime::AnyObject, sel};
use objc2_app_kit::NSWindowStyleMask;
use objc2_foundation::NSString;

const ABOUT_TEXT: &str = "Computer Vision and mouse/keyboard control utility \
for local AI agents. Bring your own AI and automate UI interactions with \
mouse, keyboard, and GPU-accelerated text recognition.\n\n\
DesktopCtl does not collect your data and does not connect to the internet.\n\n\
Point your AI agent at the GitHub link below to read the docs and get started.";

const WEBSITE_URL: &str = "https://desktopctl.com";
const GITHUB_URL: &str = "https://github.com/yaroshevych/desktopctl";

define_class!(
    #[unsafe(super(objc2::runtime::NSObject))]
    struct AboutTarget;

    impl AboutTarget {
        #[unsafe(method(openWebsite:))]
        fn open_website(&self, _: &AnyObject) {
            open_url(WEBSITE_URL);
        }

        #[unsafe(method(openGitHub:))]
        fn open_github(&self, _: &AnyObject) {
            open_url(GITHUB_URL);
        }
    }
);

struct AboutState {
    /// Keep alert alive — it owns the window.
    _alert: Retained<AnyObject>,
    /// Keep target alive — NSButton holds only a weak ref.
    _target: Retained<AnyObject>,
}

thread_local! {
    static ABOUT: RefCell<Option<AboutState>> = RefCell::new(None);
}

pub fn show() {
    DispatchQueue::main().exec_async(show_on_main);
}

fn show_on_main() {
    // Close any existing About window.
    ABOUT.with(|cell| {
        if let Some(ref s) = *cell.borrow() {
            unsafe {
                let alert_ptr = Retained::as_ptr(&s._alert);
                let window: *mut AnyObject = msg_send![alert_ptr, window];
                let _: () = msg_send![window, close];
            }
        }
        *cell.borrow_mut() = None;
    });

    unsafe {
        let alert: *mut AnyObject = msg_send![class!(NSAlert), new];

        let title = NSString::from_str("Desktop Control");
        let _: () = msg_send![alert, setMessageText: &*title];

        let info = NSString::from_str(ABOUT_TEXT);
        let _: () = msg_send![alert, setInformativeText: &*info];

        let website_lbl = NSString::from_str("Website");
        let _: () = msg_send![alert, addButtonWithTitle: &*website_lbl];

        let github_lbl = NSString::from_str("GitHub");
        let _: () = msg_send![alert, addButtonWithTitle: &*github_lbl];

        // Force layout so buttons and window are fully initialised.
        let _: () = msg_send![alert, layout];

        // Add a close button (red ×) to the alert's underlying window.
        let window: *mut AnyObject = msg_send![alert, window];
        let current_mask: NSWindowStyleMask = msg_send![window, styleMask];
        let _: () = msg_send![window, setStyleMask: current_mask | NSWindowStyleMask::Closable];

        // Wire alert buttons to our target instead of the modal-loop handler.
        let target_raw: *mut AnyObject = msg_send![AboutTarget::class(), new];
        let buttons: *mut AnyObject = msg_send![alert, buttons];
        let website_btn: *mut AnyObject = msg_send![buttons, objectAtIndex: 0usize];
        let github_btn: *mut AnyObject = msg_send![buttons, objectAtIndex: 1usize];
        let _: () = msg_send![website_btn, setTarget: target_raw];
        let _: () = msg_send![website_btn, setAction: sel!(openWebsite:)];
        let _: () = msg_send![github_btn, setTarget: target_raw];
        let _: () = msg_send![github_btn, setAction: sel!(openGitHub:)];

        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![ns_app, activateIgnoringOtherApps: true];
        let _: () = msg_send![window, center];
        let _: () = msg_send![window, makeKeyAndOrderFront: std::ptr::null::<AnyObject>()];

        let alert_retained: Retained<AnyObject> = Retained::from_raw(alert).unwrap();
        let target_retained: Retained<AnyObject> = Retained::from_raw(target_raw).unwrap();
        ABOUT.with(|cell| {
            *cell.borrow_mut() = Some(AboutState {
                _alert: alert_retained,
                _target: target_retained,
            });
        });
    }
}

fn open_url(url: &str) {
    unsafe {
        let ns_str = NSString::from_str(url);
        let nsurl: *mut AnyObject = msg_send![class!(NSURL), URLWithString: &*ns_str];
        if !nsurl.is_null() {
            let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
            let _: () = msg_send![workspace, openURL: nsurl];
        }
    }
}

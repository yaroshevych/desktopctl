use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::{class, msg_send, runtime::AnyObject};
use objc2_app_kit::NSWindowStyleMask;
use objc2_foundation::NSString;

const ABOUT_TEXT: &str = "Computer Vision and mouse/keyboard control utility \
for local AI agents. Bring your own AI and automate UI interactions with \
mouse, keyboard, and GPU-accelerated text recognition.\n\n\
DesktopCtl does not collect your data and does not connect to the internet.\n\n\
Point your AI agent at the GitHub link below to read the docs and get started.";

const WEBSITE_URL: &str = "https://desktopctl.com";
const GITHUB_URL: &str = "https://github.com/yaroshevych/desktopctl";

// NSAlertFirstButtonReturn / Second — added in order: Website (1000), GitHub (1001).
const ALERT_WEBSITE: isize = 1000;
const ALERT_GITHUB: isize = 1001;

pub fn show() {
    DispatchQueue::main().exec_async(show_on_main);
}

fn show_on_main() {
    unsafe {
        let alert: *mut AnyObject = msg_send![class!(NSAlert), new];

        let title = NSString::from_str("Desktop Control");
        let _: () = msg_send![alert, setMessageText: &*title];

        let info = NSString::from_str(ABOUT_TEXT);
        let _: () = msg_send![alert, setInformativeText: &*info];

        // Two action buttons (right-to-left: Website is default/right, GitHub is left).
        let website_lbl = NSString::from_str("Website");
        let _: () = msg_send![alert, addButtonWithTitle: &*website_lbl]; // 1000

        let github_lbl = NSString::from_str("GitHub");
        let _: () = msg_send![alert, addButtonWithTitle: &*github_lbl]; // 1001

        // Add the standard close button (red ×) to the alert's underlying window.
        let window: *mut AnyObject = msg_send![alert, window];
        let current_mask: NSWindowStyleMask = msg_send![window, styleMask];
        let _: () = msg_send![window, setStyleMask: current_mask | NSWindowStyleMask::Closable];

        // When the user clicks ×, NSWindow posts NSWindowWillCloseNotification.
        // Handle it by stopping the modal loop (returns NSModalResponseStop = -1000).
        let nc: *mut AnyObject = msg_send![class!(NSNotificationCenter), defaultCenter];
        let stop_block = RcBlock::new(|_notif: *mut AnyObject| {
            let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
            let _: () = msg_send![ns_app, stopModal];
        });
        let will_close = NSString::from_str("NSWindowWillCloseNotification");
        let observer: *mut AnyObject = msg_send![
            nc,
            addObserverForName: &*will_close,
            object: window,
            queue: std::ptr::null::<AnyObject>(),
            usingBlock: &*stop_block
        ];

        // Bring the app to front so the dialog appears above other windows.
        let ns_app: *mut AnyObject = msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![ns_app, activateIgnoringOtherApps: true];

        let response: isize = msg_send![alert, runModal];

        // Clean up the notification observer now that the modal is done.
        let _: () = msg_send![nc, removeObserver: observer];

        let url = match response {
            ALERT_WEBSITE => Some(WEBSITE_URL),
            ALERT_GITHUB => Some(GITHUB_URL),
            _ => None, // closed via × button (NSModalResponseStop = -1000)
        };
        if let Some(url_str) = url {
            open_url(url_str);
        }
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

use dispatch2::DispatchQueue;
use objc2::{class, msg_send};
use objc2_foundation::NSString;

const ABOUT_TEXT: &str = "Computer Vision and mouse/keyboard control utility \
for local AI agents. Bring your own AI and automate UI interactions with \
mouse, keyboard, and GPU-accelerated text recognition.\n\n\
DesktopCtl does not collect your data and does not connect to the internet.\n\n\
Learn more at desktopctl.com";

pub(crate) fn show() {
    DispatchQueue::main().exec_async(show_on_main);
}

fn show_on_main() {
    unsafe {
        let alert: *mut objc2::runtime::AnyObject = msg_send![class!(NSAlert), new];
        if alert.is_null() {
            return;
        }

        let title = NSString::from_str("DesktopCtl");
        let message = NSString::from_str(ABOUT_TEXT);

        let _: () = msg_send![alert, setMessageText: &*title];
        let _: () = msg_send![alert, setInformativeText: &*message];

        let btn_label = NSString::from_str("Close");
        let _: () = msg_send![alert, addButtonWithTitle: &*btn_label];

        let app: *mut objc2::runtime::AnyObject =
            msg_send![class!(NSApplication), sharedApplication];
        let _: () = msg_send![app, activateIgnoringOtherApps: true];

        let _: isize = msg_send![alert, runModal];
    }
}

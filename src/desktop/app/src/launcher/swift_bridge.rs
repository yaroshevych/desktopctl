use std::ffi::{CString, c_char, c_void};

use super::core::LauncherSnapshot;

type ActionCallback = unsafe extern "C" fn(*const c_char, usize);
type SettingsChangedCallback = unsafe extern "C" fn(i32, i32, i32);
type NotificationActionCallback = unsafe extern "C" fn(*const c_char, usize);

unsafe extern "C" {
    fn desktopctl_launcher_mount(parent: *mut c_void, callback: Option<ActionCallback>) -> bool;
    fn desktopctl_launcher_start_settings_observer(callback: Option<SettingsChangedCallback>);
    fn desktopctl_launcher_set_snapshot(json: *const c_char, length: usize);
    fn desktopctl_launcher_focus_prompt();
    fn desktopctl_launcher_prepare_for_presentation();
    fn desktopctl_launcher_move_selection(delta: isize);
    fn desktopctl_launcher_toggle_actions_menu();
    fn desktopctl_launcher_move_actions_menu_focus(delta: isize, escape_top: bool) -> bool;
    fn desktopctl_launcher_dismiss_actions_menu() -> bool;
    fn desktopctl_launcher_activate_actions_menu() -> bool;
    fn desktopctl_launcher_actions_menu_handles_navigation() -> bool;
    fn desktopctl_launcher_unmount();
    fn desktopctl_launcher_show_completion_notification(
        title: *const c_char,
        body: *const c_char,
        session_id: *const c_char,
    );
    fn desktopctl_launcher_set_notification_action_callback(
        callback: Option<NotificationActionCallback>,
    );
}

pub fn mount(parent: *mut c_void, callback: ActionCallback) -> bool {
    unsafe { desktopctl_launcher_mount(parent, Some(callback)) }
}

pub fn start_settings_observer(callback: SettingsChangedCallback) {
    unsafe { desktopctl_launcher_start_settings_observer(Some(callback)) };
}

pub fn serialize_snapshot(snapshot: &LauncherSnapshot) -> Option<Vec<u8>> {
    serde_json::to_vec(snapshot).ok()
}

pub fn set_snapshot_json(json: &[u8]) {
    unsafe {
        desktopctl_launcher_set_snapshot(json.as_ptr().cast(), json.len());
    }
}

pub fn focus_prompt() {
    unsafe { desktopctl_launcher_focus_prompt() };
}

pub fn prepare_for_presentation() {
    unsafe { desktopctl_launcher_prepare_for_presentation() };
}

pub fn move_selection(delta: isize) {
    unsafe { desktopctl_launcher_move_selection(delta) };
}

pub fn toggle_actions_menu() {
    unsafe { desktopctl_launcher_toggle_actions_menu() };
}

pub fn move_actions_menu_focus(delta: isize, escape_top: bool) -> bool {
    unsafe { desktopctl_launcher_move_actions_menu_focus(delta, escape_top) }
}

pub fn dismiss_actions_menu() -> bool {
    unsafe { desktopctl_launcher_dismiss_actions_menu() }
}

pub fn activate_actions_menu() -> bool {
    unsafe { desktopctl_launcher_activate_actions_menu() }
}

pub fn actions_menu_handles_navigation() -> bool {
    unsafe { desktopctl_launcher_actions_menu_handles_navigation() }
}

#[allow(dead_code)]
pub fn unmount() {
    unsafe { desktopctl_launcher_unmount() }
}

pub fn show_completion_notification_for_session(title: &str, body: &str, session_id: &str) {
    let Ok(title) = CString::new(title) else {
        return;
    };
    let Ok(body) = CString::new(body) else { return };
    let Ok(session_id) = CString::new(session_id) else {
        return;
    };
    unsafe {
        desktopctl_launcher_show_completion_notification(
            title.as_ptr(),
            body.as_ptr(),
            session_id.as_ptr(),
        );
    }
}

pub fn start_notification_action_observer(callback: NotificationActionCallback) {
    unsafe {
        desktopctl_launcher_set_notification_action_callback(Some(callback));
    }
}

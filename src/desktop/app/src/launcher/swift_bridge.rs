use std::ffi::{CString, c_char, c_void};

use super::core::LauncherSnapshot;

type ActionCallback = unsafe extern "C" fn(*const c_char, usize);

unsafe extern "C" {
    fn desktopctl_launcher_mount(parent: *mut c_void, callback: Option<ActionCallback>) -> bool;
    fn desktopctl_launcher_set_snapshot(json: *const c_char, length: usize);
    fn desktopctl_launcher_focus_prompt();
    fn desktopctl_launcher_prepare_for_presentation();
    fn desktopctl_launcher_move_selection(delta: isize);
    fn desktopctl_launcher_toggle_actions_menu();
    fn desktopctl_launcher_dismiss_actions_menu() -> bool;
    fn desktopctl_launcher_activate_actions_menu() -> bool;
    fn desktopctl_launcher_actions_menu_handles_navigation() -> bool;
    fn desktopctl_launcher_unmount();
}

pub fn mount(parent: *mut c_void, callback: ActionCallback) -> bool {
    unsafe { desktopctl_launcher_mount(parent, Some(callback)) }
}

pub fn set_snapshot(snapshot: &LauncherSnapshot) {
    let Ok(json) = serde_json::to_string(snapshot) else {
        return;
    };
    let Ok(json) = CString::new(json) else {
        return;
    };
    unsafe { desktopctl_launcher_set_snapshot(json.as_ptr(), json.as_bytes().len()) };
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

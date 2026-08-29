#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub use linux::{
    AxElement, collect_frontmost_window_elements, collect_window_elements,
    focused_frontmost_element, focused_frontmost_window_bounds, frontmost_app_pid,
};
#[cfg(target_os = "macos")]
pub use macos::{
    AxElement, collect_frontmost_window_elements, collect_window_elements,
    focused_frontmost_element, focused_frontmost_window_bounds, focused_window_bounds_for_pid,
    frontmost_app_pid,
};
#[cfg(target_os = "windows")]
pub use windows::{
    AxElement, collect_frontmost_window_elements, collect_window_elements,
    focused_frontmost_element, focused_frontmost_window_bounds, frontmost_app_pid,
};

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub use unsupported::{
    AxElement, collect_frontmost_window_elements, collect_window_elements,
    focused_frontmost_element, focused_frontmost_window_bounds, frontmost_app_pid,
};

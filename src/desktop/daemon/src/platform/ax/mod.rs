#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{
    AxElement, collect_frontmost_window_elements, focused_frontmost_element,
    focused_frontmost_window_bounds,
};
#[cfg(target_os = "windows")]
pub use windows::{
    AxElement, collect_frontmost_window_elements, focused_frontmost_element,
    focused_frontmost_window_bounds,
};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use unsupported::{
    AxElement, collect_frontmost_window_elements, focused_frontmost_element,
    focused_frontmost_window_bounds,
};

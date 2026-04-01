#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub use macos::{
    AxElement, collect_frontmost_window_elements, focused_frontmost_element,
    focused_frontmost_window_bounds,
};

#[cfg(not(target_os = "macos"))]
pub use unsupported::{
    AxElement, collect_frontmost_window_elements, focused_frontmost_element,
    focused_frontmost_window_bounds,
};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub use macos::{focus_window, hide_application, isolate_application, show_application};
#[cfg(target_os = "windows")]
pub use windows::{focus_window, hide_application, isolate_application, show_application};

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub use unsupported::{focus_window, hide_application, isolate_application, show_application};

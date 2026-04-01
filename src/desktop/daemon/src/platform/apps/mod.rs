#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub use macos::{focus_window, hide_application, isolate_application, show_application};

#[cfg(not(target_os = "macos"))]
pub use unsupported::{focus_window, hide_application, isolate_application, show_application};

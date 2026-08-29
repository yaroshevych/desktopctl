#[cfg(target_os = "macos")]
mod about;
#[cfg(target_os = "windows")]
mod about_windows;
#[cfg(target_os = "windows")]
mod journal_dialog_windows;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::run;
#[cfg(target_os = "macos")]
pub(crate) use macos::run;
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
pub(crate) use unsupported::run;
#[cfg(target_os = "windows")]
pub(crate) use windows::run;

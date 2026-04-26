#[cfg(target_os = "macos")]
mod about;
#[cfg(target_os = "windows")]
mod about;
#[cfg(target_os = "macos")]
mod app_policy_dialog;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod permissions_dialog;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
mod unsupported;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "macos")]
pub(crate) use macos::run;
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
pub(crate) use unsupported::run;
#[cfg(target_os = "windows")]
pub(crate) use windows::run;

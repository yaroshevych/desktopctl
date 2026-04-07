#[cfg(target_os = "macos")]
mod about;
#[cfg(target_os = "macos")]
mod app_policy_dialog;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod permissions_dialog;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub(crate) use macos::run;
#[cfg(not(target_os = "macos"))]
pub(crate) use unsupported::run;

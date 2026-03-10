use thiserror::Error;

#[derive(Debug, Error)]
pub enum AppError {
    #[error(
        "accessibility permission required. enable it for DesktopCtl.app in System Settings -> Privacy & Security -> Accessibility"
    )]
    AccessibilityPermissionMissing,
    #[error("unsupported platform: {0}")]
    UnsupportedPlatform(&'static str),
    #[error("invalid hotkey format: {0}")]
    InvalidHotkey(String),
    #[error("automation command failed: {0}")]
    AutomationCommand(String),
    #[error("automation backend error: {0}")]
    AutomationBackend(String),
    #[error("ipc error: {0}")]
    Ipc(String),
    #[error("invalid cli arguments: {0}")]
    Cli(String),
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

pub use self::backend::*;

#[cfg(target_os = "macos")]
use macos as backend;
#[cfg(not(target_os = "macos"))]
use unsupported as backend;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct MenuNode {
    pub id: String,
    pub title: String,
    pub role: String,
    pub enabled: bool,
    pub action_supported: bool,
    pub shortcut: Option<String>,
    pub mark: Option<String>,
    pub kind: String,
    pub children: Vec<MenuNode>,
    pub truncated: bool,
    pub omitted_count: usize,
}

#[derive(Debug, Clone)]
pub struct MenuSnapshot {
    pub items: Vec<MenuNode>,
}

#[derive(Debug, Clone)]
pub struct MenuActionResult {
    pub id: String,
    pub title: String,
    pub shortcut: Option<String>,
}

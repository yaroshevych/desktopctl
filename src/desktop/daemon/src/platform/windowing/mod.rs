use desktop_core::protocol::Bounds;
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: String,
    pub window_ref: Option<String>,
    pub parent_id: Option<String>,
    pub pid: i64,
    pub index: u32,
    pub app: String,
    pub title: String,
    pub bounds: Bounds,
    pub frontmost: bool,
    pub visible: bool,
    pub modal: Option<bool>,
}

impl WindowInfo {
    pub fn as_json(&self) -> Value {
        let public_id = self.window_ref.as_deref().unwrap_or(self.id.as_str());
        json!({
            "id": public_id,
            "parent_id": self.parent_id,
            "app": self.app,
            "title": self.title,
            "bounds": self.bounds,
            "frontmost": self.frontmost,
            "visible": self.visible,
            "modal": self.modal
        })
    }
}

#[derive(Debug, Clone)]
pub struct FrontmostWindowContext {
    pub app: Option<String>,
    pub bounds: Option<Bounds>,
}

#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(target_os = "macos"))]
mod unsupported;

#[cfg(target_os = "macos")]
pub use macos::{
    frontmost_window_context, list_frontmost_app_windows, list_windows, list_windows_basic,
    main_display_bounds,
};

#[cfg(not(target_os = "macos"))]
pub use unsupported::{
    frontmost_window_context, list_frontmost_app_windows, list_windows, list_windows_basic,
    main_display_bounds,
};

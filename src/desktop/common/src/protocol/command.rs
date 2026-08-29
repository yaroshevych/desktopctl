use serde::{Deserialize, Serialize};

use super::{Bounds, ObserveOptions, PROTOCOL_VERSION};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum PointerButton {
    #[default]
    Left,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub protocol_version: u32,
    pub request_id: String,
    #[serde(default)]
    pub options: RequestOptions,
    pub command: Command,
}

impl RequestEnvelope {
    pub fn new(request_id: String, command: Command) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            options: RequestOptions::default(),
            command,
        }
    }

    pub fn with_background_input(mut self, enabled: bool) -> Self {
        self.options.background_input = enabled;
        self
    }
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct HumanOptions {
    /// Typing speed in words per minute; 0 keeps the current default
    #[serde(default)]
    pub wpm: u32,
    /// Inter-operation jitter range in ms [min, max]; both 0 disables jitter
    #[serde(default)]
    pub jitter_ms: [u32; 2],
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct RequestOptions {
    #[serde(default)]
    pub background_input: bool,
    /// When present, input commands use human-like timing and motion
    #[serde(default)]
    pub human: Option<HumanOptions>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Ping,
    ServiceStatus,
    ActiveWindowDescribe,
    AgentAccessSet {
        enabled: bool,
    },
    DisableGui,
    AppHide {
        name: String,
    },
    AppShow {
        name: String,
    },
    AppIsolate {
        name: String,
    },
    WindowList,
    WindowBounds {
        title: String,
    },
    WindowFocus {
        title: String,
    },
    OpenApp {
        name: String,
        args: Vec<String>,
        wait: bool,
        timeout_ms: Option<u64>,
        #[serde(default)]
        background: bool,
    },
    PointerMove {
        x: u32,
        y: u32,
        #[serde(default)]
        absolute: bool,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    PointerDown {
        x: u32,
        y: u32,
        #[serde(default)]
        button: PointerButton,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    PointerUp {
        x: u32,
        y: u32,
        #[serde(default)]
        button: PointerButton,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    PointerClick {
        x: u32,
        y: u32,
        #[serde(default)]
        absolute: bool,
        #[serde(default)]
        button: PointerButton,
        #[serde(default)]
        observe: ObserveOptions,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    PointerClickText {
        text: String,
        #[serde(default)]
        button: PointerButton,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
        #[serde(default)]
        observe: ObserveOptions,
    },
    PointerClickId {
        id: String,
        #[serde(default)]
        button: PointerButton,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
        #[serde(default)]
        observe: ObserveOptions,
    },
    PointerScroll {
        #[serde(default)]
        id: Option<String>,
        dx: i32,
        dy: i32,
        #[serde(default)]
        observe: ObserveOptions,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    PointerDrag {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        hold_ms: u64,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    UiType {
        text: String,
        #[serde(default)]
        observe: ObserveOptions,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    KeyHotkey {
        hotkey: String,
        #[serde(default)]
        observe: ObserveOptions,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    KeyEnter {
        #[serde(default)]
        observe: ObserveOptions,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    KeyEscape {
        #[serde(default)]
        observe: ObserveOptions,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    WaitText {
        text: String,
        timeout_ms: u64,
        interval_ms: u64,
        disappear: bool,
    },
    ScreenCapture {
        out_path: Option<String>,
        #[serde(default)]
        overlay: bool,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
        #[serde(default)]
        region: Option<Bounds>,
    },
    ScreenTokenize {
        #[serde(default)]
        overlay_out_path: Option<String>,
        #[serde(default)]
        window_query: Option<String>,
        #[serde(default)]
        screenshot_path: Option<String>,
        #[serde(default)]
        journal: bool,
        #[serde(default)]
        list_all_windows: bool,
        #[serde(default)]
        all: bool,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
        #[serde(default)]
        region: Option<Bounds>,
    },
    ScreenFindText {
        text: String,
        all: bool,
    },
    MenuList {
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
        #[serde(default)]
        system: bool,
        #[serde(default)]
        all: bool,
    },
    MenuClick {
        #[serde(default)]
        id: Option<String>,
        #[serde(default)]
        path: Option<String>,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        active_window_id: Option<String>,
    },
    OverlayStart {
        duration_ms: Option<u64>,
    },
    OverlayStop,
    ClipboardRead,
    ClipboardWrite {
        text: String,
    },
    PermissionsCheck,
    DebugSnapshot,
    RequestShow {
        request_id: String,
    },
    RequestList {
        #[serde(default)]
        limit: Option<u64>,
    },
    RequestScreenshot {
        request_id: String,
        out_path: Option<String>,
    },
    RequestResponse {
        request_id: String,
    },
    RequestSearch {
        text: String,
        #[serde(default)]
        limit: Option<u64>,
        #[serde(default)]
        command: Option<String>,
    },
    ReplayRecord {
        duration_ms: u64,
        stop: bool,
    },
    ReplayLoad {
        session_dir: String,
    },
}

impl Command {
    pub fn name(&self) -> &'static str {
        match self {
            Command::Ping => "ping",
            Command::ServiceStatus => "service_status",
            Command::ActiveWindowDescribe => "active_window_describe",
            Command::AgentAccessSet { .. } => "agent_access_set",
            Command::DisableGui => "disable",
            Command::AppHide { .. } => "app_hide",
            Command::AppShow { .. } => "app_show",
            Command::AppIsolate { .. } => "app_isolate",
            Command::WindowList => "window_list",
            Command::WindowBounds { .. } => "window_bounds",
            Command::WindowFocus { .. } => "window_focus",
            Command::OpenApp { .. } => "open",
            Command::PointerMove { .. } => "pointer_move",
            Command::PointerDown { .. } => "pointer_down",
            Command::PointerUp { .. } => "pointer_up",
            Command::PointerClick { .. } => "pointer_click",
            Command::PointerClickText { .. } => "pointer_click_text",
            Command::PointerClickId { .. } => "pointer_click_id",
            Command::PointerScroll { .. } => "pointer_scroll",
            Command::PointerDrag { .. } => "pointer_drag",
            Command::UiType { .. } => "type",
            Command::KeyHotkey { .. } => "key_hotkey",
            Command::KeyEnter { .. } => "key_enter",
            Command::KeyEscape { .. } => "key_escape",
            Command::WaitText { .. } => "wait_text",
            Command::ScreenCapture { .. } => "screen_capture",
            Command::ScreenTokenize { .. } => "screen_tokenize",
            Command::ScreenFindText { .. } => "screen_find_text",
            Command::MenuList { .. } => "menu_list",
            Command::MenuClick { .. } => "menu_click",
            Command::OverlayStart { .. } => "overlay_start",
            Command::OverlayStop => "overlay_stop",
            Command::ClipboardRead => "clipboard_read",
            Command::ClipboardWrite { .. } => "clipboard_write",
            Command::PermissionsCheck => "permissions_check",
            Command::DebugSnapshot => "debug_snapshot",
            Command::RequestShow { .. } => "request_show",
            Command::RequestList { .. } => "request_list",
            Command::RequestScreenshot { .. } => "request_screenshot",
            Command::RequestResponse { .. } => "request_response",
            Command::RequestSearch { .. } => "request_search",
            Command::ReplayRecord { stop: true, .. } => "replay_record_stop",
            Command::ReplayRecord { stop: false, .. } => "replay_record_start",
            Command::ReplayLoad { .. } => "replay_load",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Command;

    #[test]
    fn service_status_has_stable_wire_name() {
        let value = serde_json::to_value(Command::ServiceStatus).expect("serialize command");
        assert_eq!(value, serde_json::json!({ "cmd": "service_status" }));
        assert_eq!(Command::ServiceStatus.name(), "service_status");
    }

    #[test]
    fn active_window_describe_has_stable_wire_name() {
        let value = serde_json::to_value(Command::ActiveWindowDescribe).expect("serialize command");
        assert_eq!(
            value,
            serde_json::json!({ "cmd": "active_window_describe" })
        );
        assert_eq!(
            Command::ActiveWindowDescribe.name(),
            "active_window_describe"
        );
    }
}

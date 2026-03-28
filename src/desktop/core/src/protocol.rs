use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{AppError, ErrorCode};

pub const PROTOCOL_VERSION: u32 = 1;
pub const API_VERSION: &str = "1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestEnvelope {
    pub protocol_version: u32,
    pub request_id: String,
    pub command: Command,
}

impl RequestEnvelope {
    pub fn new(request_id: String, command: Command) -> Self {
        Self {
            protocol_version: PROTOCOL_VERSION,
            request_id,
            command,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Ping,
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
    },
    PointerMove {
        x: u32,
        y: u32,
    },
    PointerDown {
        x: u32,
        y: u32,
    },
    PointerUp {
        x: u32,
        y: u32,
    },
    PointerClick {
        x: u32,
        y: u32,
        #[serde(default)]
        absolute: bool,
    },
    PointerClickText {
        text: String,
    },
    PointerClickId {
        id: String,
    },
    PointerClickToken {
        token: u32,
    },
    PointerScroll {
        dx: i32,
        dy: i32,
    },
    PointerDrag {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        hold_ms: u64,
    },
    UiType {
        text: String,
    },
    KeyHotkey {
        hotkey: String,
    },
    KeyEnter,
    KeyEscape,
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
        region: Option<Bounds>,
    },
    ScreenTokenize {
        #[serde(default)]
        overlay_out_path: Option<String>,
        #[serde(default)]
        window_id: Option<String>,
        #[serde(default)]
        screenshot_path: Option<String>,
        #[serde(default)]
        active_window: bool,
        #[serde(default)]
        region: Option<Bounds>,
    },
    ScreenFindText {
        text: String,
        all: bool,
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
    RequestScreenshot {
        request_id: String,
        out_path: Option<String>,
    },
    RequestResponse {
        request_id: String,
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
            Command::PointerClickToken { .. } => "pointer_click_token",
            Command::PointerScroll { .. } => "pointer_scroll",
            Command::PointerDrag { .. } => "pointer_drag",
            Command::UiType { .. } => "type",
            Command::KeyHotkey { .. } => "key_hotkey",
            Command::KeyEnter => "key_enter",
            Command::KeyEscape => "key_escape",
            Command::WaitText { .. } => "wait_text",
            Command::ScreenCapture { .. } => "screen_capture",
            Command::ScreenTokenize { .. } => "screen_tokenize",
            Command::ScreenFindText { .. } => "screen_find_text",
            Command::OverlayStart { .. } => "overlay_start",
            Command::OverlayStop => "overlay_stop",
            Command::ClipboardRead => "clipboard_read",
            Command::ClipboardWrite { .. } => "clipboard_write",
            Command::PermissionsCheck => "permissions_check",
            Command::DebugSnapshot => "debug_snapshot",
            Command::RequestShow { .. } => "request_show",
            Command::RequestScreenshot { .. } => "request_screenshot",
            Command::RequestResponse { .. } => "request_response",
            Command::ReplayRecord { stop: true, .. } => "replay_record_stop",
            Command::ReplayRecord { stop: false, .. } => "replay_record_start",
            Command::ReplayLoad { .. } => "replay_load",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ResponseEnvelope {
    Success(SuccessResponse),
    Error(ErrorResponse),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SuccessResponse {
    pub ok: bool,
    pub api_version: String,
    pub request_id: String,
    pub result: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub ok: bool,
    pub api_version: String,
    pub request_id: String,
    pub error: ErrorPayload,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorPayload {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub command: String,
    pub debug_ref: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}

impl ResponseEnvelope {
    pub fn success(request_id: impl Into<String>, result: Value) -> Self {
        Self::Success(SuccessResponse {
            ok: true,
            api_version: API_VERSION.to_string(),
            request_id: request_id.into(),
            result,
        })
    }

    pub fn success_message(request_id: impl Into<String>, message: impl Into<String>) -> Self {
        Self::success(request_id, json!({ "message": message.into() }))
    }

    pub fn from_error(
        request_id: impl Into<String>,
        command: impl Into<String>,
        error: AppError,
    ) -> Self {
        let debug_ref = error
            .debug_ref
            .unwrap_or_else(|| format!("dbg-{}", now_millis()));
        Self::Error(ErrorResponse {
            ok: false,
            api_version: API_VERSION.to_string(),
            request_id: request_id.into(),
            error: ErrorPayload {
                code: error.code,
                message: error.message,
                retryable: error.retryable,
                command: error.command.unwrap_or_else(|| command.into()),
                debug_ref,
                details: error.details,
            },
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bounds {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotText {
    pub text: String,
    pub bounds: Bounds,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotDisplay {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotPayload {
    pub snapshot_id: u64,
    pub timestamp: String,
    pub display: SnapshotDisplay,
    pub focused_app: Option<String>,
    pub texts: Vec<SnapshotText>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenEntry {
    pub n: u32,
    pub text: String,
    pub bounds: Bounds,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizePayload {
    pub snapshot_id: u64,
    pub timestamp: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<TokenizeImage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub windows: Vec<TokenizeWindow>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizeImage {
    pub path: String,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizeWindow {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    pub bounds: Bounds,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os_bounds: Option<Bounds>,
    pub elements: Vec<TokenizeElement>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenizeElement {
    pub id: String,
    #[serde(rename = "type", default, skip_serializing_if = "String::is_empty")]
    pub kind: String,
    pub bbox: [f64; 4],
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_border: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
    pub source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionState {
    pub granted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PermissionsPayload {
    pub accessibility: PermissionState,
    pub screen_recording: PermissionState,
}

pub fn now_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}

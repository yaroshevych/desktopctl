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
    OpenApp {
        name: String,
        args: Vec<String>,
        wait: bool,
        timeout_ms: Option<u64>,
    },
    OpenSpotlight,
    OpenLaunchpad,
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
    Wait {
        ms: u64,
    },
    WaitText {
        text: String,
        timeout_ms: u64,
        interval_ms: u64,
    },
    ScreenCapture {
        out_path: Option<String>,
    },
    ScreenSnapshot,
    ScreenTokenize,
    ScreenFindText {
        text: String,
        all: bool,
    },
    ScreenLayout,
    ScreenSettingsMap,
    UiClickText {
        text: String,
        timeout_ms: u64,
    },
    UiClickTextOffset {
        text: String,
        dx: i32,
        dy: i32,
        timeout_ms: u64,
    },
    UiClickSettingsAdd,
    UiClickSettingsRemove,
    UiClickSettingsToggle {
        text: String,
        timeout_ms: u64,
    },
    UiSettingsEnsureEnabled {
        text: String,
        timeout_ms: u64,
    },
    UiSettingsUnlock {
        password: String,
        timeout_ms: u64,
    },
    UiClickToken {
        token: u32,
    },
    UiRead,
    ClipboardRead,
    ClipboardWrite {
        text: String,
    },
    PermissionsCheck,
    DebugSnapshot,
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
            Command::OpenApp { .. } => "open",
            Command::OpenSpotlight => "open_spotlight",
            Command::OpenLaunchpad => "open_launchpad",
            Command::PointerMove { .. } => "pointer_move",
            Command::PointerDown { .. } => "pointer_down",
            Command::PointerUp { .. } => "pointer_up",
            Command::PointerClick { .. } => "pointer_click",
            Command::PointerDrag { .. } => "pointer_drag",
            Command::UiType { .. } => "type",
            Command::KeyHotkey { .. } => "key_hotkey",
            Command::KeyEnter => "key_enter",
            Command::Wait { .. } => "wait",
            Command::WaitText { .. } => "wait_text",
            Command::ScreenCapture { .. } => "screen_capture",
            Command::ScreenSnapshot => "screen_snapshot",
            Command::ScreenTokenize => "screen_tokenize",
            Command::ScreenFindText { .. } => "screen_find_text",
            Command::ScreenLayout => "screen_layout",
            Command::ScreenSettingsMap => "screen_settings_map",
            Command::UiClickText { .. } => "ui_click_text",
            Command::UiClickTextOffset { .. } => "ui_click_text_offset",
            Command::UiClickSettingsAdd => "ui_click_settings_add",
            Command::UiClickSettingsRemove => "ui_click_settings_remove",
            Command::UiClickSettingsToggle { .. } => "ui_click_settings_toggle",
            Command::UiSettingsEnsureEnabled { .. } => "ui_settings_ensure_enabled",
            Command::UiSettingsUnlock { .. } => "ui_settings_unlock",
            Command::UiClickToken { .. } => "ui_click_token",
            Command::UiRead => "ui_read",
            Command::ClipboardRead => "clipboard_read",
            Command::ClipboardWrite { .. } => "clipboard_write",
            Command::PermissionsCheck => "permissions_check",
            Command::DebugSnapshot => "debug_snapshot",
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
    pub tokens: Vec<TokenEntry>,
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

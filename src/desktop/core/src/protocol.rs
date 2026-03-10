use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub command: Command,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "snake_case")]
pub enum Command {
    Ping,
    OpenApp { name: String, args: Vec<String> },
    OpenSpotlight,
    OpenLaunchpad,
    PointerMove { x: u32, y: u32 },
    PointerDown { x: u32, y: u32 },
    PointerUp { x: u32, y: u32 },
    PointerClick { x: u32, y: u32 },
    PointerDrag {
        x1: u32,
        y1: u32,
        x2: u32,
        y2: u32,
        hold_ms: u64,
    },
    UiType { text: String },
    KeyHotkey { hotkey: String },
    KeyEnter,
    Wait { ms: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Response {
    pub ok: bool,
    pub message: Option<String>,
}

impl Response {
    pub fn ok(message: Option<String>) -> Self {
        Self { ok: true, message }
    }

    pub fn err(message: String) -> Self {
        Self {
            ok: false,
            message: Some(message),
        }
    }
}

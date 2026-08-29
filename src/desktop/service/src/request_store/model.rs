use std::collections::VecDeque;

use serde_json::Value;

#[derive(Clone)]
pub(super) struct RequestEntry {
    pub(super) request_id: String,
    pub(super) command: String,
    pub(super) timestamp: String,
    pub(super) timestamp_ms: u128,
    pub(super) response: Value,
    pub(super) screenshot_png: Option<Vec<u8>>,
}

#[derive(Default)]
pub(super) struct RequestStore {
    pub(super) entries: VecDeque<RequestEntry>,
}

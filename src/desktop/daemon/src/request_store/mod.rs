mod model;
mod search;
mod service;

use desktop_core::{
    error::AppError,
    protocol::{RequestEnvelope, ResponseEnvelope},
};
use serde_json::Value;

pub(super) const MAX_REQUEST_ENTRIES: usize = 10;
pub(super) const MAX_REQUEST_AGE_MS: u128 = 10 * 60 * 1000;

pub fn record(request: &RequestEnvelope, response: &ResponseEnvelope) -> Result<(), AppError> {
    service::record(request, response)
}

pub fn show(request_id: &str) -> Result<Value, AppError> {
    service::show(request_id)
}

pub fn list(limit: Option<u64>) -> Result<Value, AppError> {
    service::list(limit)
}

pub fn response(request_id: &str) -> Result<Value, AppError> {
    service::response(request_id)
}

pub fn screenshot(request_id: &str, out_path: Option<String>) -> Result<Value, AppError> {
    service::screenshot(request_id, out_path)
}

pub fn search(
    text: &str,
    limit: Option<u64>,
    command_filter: Option<&str>,
) -> Result<Value, AppError> {
    search::search(text, limit, command_filter)
}

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{AppError, ErrorCode};

use super::{API_VERSION, now_millis};

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

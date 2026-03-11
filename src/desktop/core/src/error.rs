use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ErrorCode {
    TargetNotFound,
    LowConfidence,
    AmbiguousTarget,
    PostconditionFailed,
    PermissionDenied,
    Timeout,
    InvalidArgument,
    DaemonNotRunning,
    BackendUnavailable,
    Internal,
}

#[derive(Debug, Clone, Error, Serialize, Deserialize)]
#[error("{message}")]
pub struct AppError {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
    pub command: Option<String>,
    pub debug_ref: Option<String>,
    pub details: Option<Value>,
}

impl AppError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
            command: None,
            debug_ref: None,
            details: None,
        }
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = retryable;
        self
    }

    pub fn with_command(mut self, command: impl Into<String>) -> Self {
        self.command = Some(command.into());
        self
    }

    pub fn with_debug_ref(mut self, debug_ref: impl Into<String>) -> Self {
        self.debug_ref = Some(debug_ref.into());
        self
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }

    pub fn target_not_found(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::TargetNotFound, message)
    }

    pub fn low_confidence(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::LowConfidence, message)
    }

    pub fn ambiguous_target(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::AmbiguousTarget, message)
    }

    pub fn postcondition_failed(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PostconditionFailed, message)
    }

    pub fn permission_denied(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::PermissionDenied, message)
    }

    pub fn timeout(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Timeout, message).with_retryable(true)
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn daemon_not_running(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::DaemonNotRunning, message).with_retryable(true)
    }

    pub fn backend_unavailable(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BackendUnavailable, message).with_retryable(true)
    }

    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::Internal, message)
    }
}

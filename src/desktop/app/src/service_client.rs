use desktop_core::{
    error::AppError,
    ipc,
    protocol::{
        ActiveWindowPayload, Command, RequestEnvelope, ResponseEnvelope, ServiceStatusPayload,
        WindowListPayload, WindowSummary,
    },
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
pub struct ServiceClient;

impl ServiceClient {
    pub fn send(&self, command: Command) -> Result<Value, AppError> {
        self.send_with(command, ipc::send_request)
    }

    pub fn status(&self) -> Result<ServiceStatusPayload, AppError> {
        self.send_typed(Command::ServiceStatus)
    }

    pub fn active_window(&self) -> Result<ActiveWindowPayload, AppError> {
        self.send_typed(Command::ActiveWindowDescribe)
    }

    pub fn windows(&self) -> Result<Vec<WindowSummary>, AppError> {
        self.send_typed::<WindowListPayload>(Command::WindowList)
            .map(|payload| payload.windows)
    }

    pub fn send_typed<T>(&self, command: Command) -> Result<T, AppError>
    where
        T: DeserializeOwned,
    {
        let command_name = command.name();
        let value = self.send(command)?;
        serde_json::from_value(value).map_err(|error| {
            AppError::internal(format!(
                "decode {command_name} service response failed: {error}"
            ))
        })
    }

    fn send_with<F>(&self, command: Command, send: F) -> Result<Value, AppError>
    where
        F: FnOnce(&RequestEnvelope) -> Result<ResponseEnvelope, AppError>,
    {
        let request = RequestEnvelope::new(format!("app-{}", Uuid::new_v4()), command);
        match send(&request)? {
            ResponseEnvelope::Success(response) => Ok(response.result),
            ResponseEnvelope::Error(response) => Err(AppError {
                code: response.error.code,
                message: response.error.message,
                retryable: response.error.retryable,
                command: Some(response.error.command),
                debug_ref: Some(response.error.debug_ref),
                details: response.error.details,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use desktop_core::protocol::{PROTOCOL_VERSION, ResponseEnvelope};
    use serde_json::json;

    use super::ServiceClient;

    #[test]
    fn status_decodes_typed_service_boundary() {
        let client = ServiceClient;
        let value = client
            .send_with(desktop_core::protocol::Command::ServiceStatus, |request| {
                assert_eq!(request.protocol_version, PROTOCOL_VERSION);
                Ok(ResponseEnvelope::success(
                    request.request_id.clone(),
                    json!({
                        "service_version": "0.2.0",
                        "protocol_min": 1,
                        "protocol_max": 1,
                        "capabilities": ["automation"]
                    }),
                ))
            })
            .expect("service response");
        let status: desktop_core::protocol::ServiceStatusPayload =
            serde_json::from_value(value).expect("typed status");
        assert_eq!(status.service_version, "0.2.0");
    }
}

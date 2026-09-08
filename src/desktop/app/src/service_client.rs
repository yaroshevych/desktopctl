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
use std::time::Instant;
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

    pub fn shutdown(&self) -> Result<(), AppError> {
        self.send(Command::Shutdown).map(|_| ())
    }

    pub fn active_window(&self) -> Result<ActiveWindowPayload, AppError> {
        self.send_typed(Command::ActiveWindowDescribe)
    }

    pub fn active_app_pid(&self) -> Result<Option<i64>, AppError> {
        self.send_typed(Command::ActiveAppPid)
    }

    pub fn window_for_pid(&self, pid: i64) -> Result<WindowSummary, AppError> {
        self.send_typed(Command::WindowDescribeForPid { pid })
    }

    pub fn windows(&self) -> Result<Vec<WindowSummary>, AppError> {
        self.send_typed::<WindowListPayload>(Command::WindowList)
            .map(|payload| payload.windows)
    }

    pub fn set_agent_access(&self, enabled: bool) -> Result<bool, AppError> {
        let value = self.send(Command::AgentAccessSet { enabled })?;
        value
            .get("enabled")
            .and_then(Value::as_bool)
            .ok_or_else(|| AppError::internal("agent access response missing enabled state"))
    }

    pub fn settings(&self) -> Result<Value, AppError> {
        self.send(Command::SettingsGet)
    }

    pub fn update_settings(
        &self,
        journal: Option<Value>,
        app_policy: Option<Value>,
        launcher: Option<Value>,
    ) -> Result<(), AppError> {
        self.send(Command::SettingsUpdate {
            journal,
            app_policy,
            launcher,
        })
        .map(|_| ())
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
        let request = RequestEnvelope::new(format!("app-{}", Uuid::now_v7()), command);
        let request_started = Instant::now();
        let request_id = request.request_id.clone();
        let command_name = request.command.name().to_string();
        crate::trace::log(format!(
            "service_client:request_start request_id={} command={}",
            request_id, command_name
        ));
        let response = match send(&request) {
            Ok(response) => response,
            Err(error) => {
                crate::trace::log(format!(
                    "service_client:request_error request_id={} command={} elapsed_ms={} code={:?} msg={}",
                    request_id,
                    command_name,
                    request_started.elapsed().as_millis(),
                    error.code,
                    error.message
                ));
                return Err(error);
            }
        };
        match response {
            ResponseEnvelope::Success(response) => {
                crate::trace::log(format!(
                    "service_client:request_done request_id={} command={} status=ok elapsed_ms={}",
                    request_id,
                    command_name,
                    request_started.elapsed().as_millis()
                ));
                Ok(response.result)
            }
            ResponseEnvelope::Error(response) => {
                crate::trace::log(format!(
                    "service_client:request_done request_id={} command={} status=service_error elapsed_ms={} code={:?}",
                    request_id,
                    command_name,
                    request_started.elapsed().as_millis(),
                    response.error.code
                ));
                Err(AppError {
                    code: response.error.code,
                    message: response.error.message,
                    retryable: response.error.retryable,
                    command: Some(response.error.command),
                    debug_ref: Some(response.error.debug_ref),
                    details: response.error.details,
                })
            }
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

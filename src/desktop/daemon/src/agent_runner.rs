// Headless agent execution for the launcher.
//
// The runner deliberately owns no UI or persistence. It returns the native
// session identity reported by the adapter, leaving DesktopCtl's session store
// to persist that identity and the short transcript shown by the UI.

#![allow(dead_code)] // Adapter API includes cancellation/configuration seams used by future UI.

use std::{
    env,
    ffi::OsString,
    fmt,
    io::{self, Read},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use serde_json::Value;

/// Adapter-neutral request passed to an agent runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRequest {
    pub prompt: String,
    pub session: Option<AgentSessionRef>,
    pub target_window: Option<TargetWindow>,
}

impl AgentRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            session: None,
            target_window: None,
        }
    }
}

/// The native identity that an adapter can use to continue a conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionRef {
    pub id: Option<String>,
    pub path: Option<PathBuf>,
    pub cwd: Option<PathBuf>,
}

impl AgentSessionRef {
    pub fn id(id: impl Into<String>) -> Self {
        Self {
            id: Some(id.into()),
            path: None,
            cwd: None,
        }
    }

    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self {
            id: None,
            path: Some(path.into()),
            cwd: None,
        }
    }

    fn is_empty(&self) -> bool {
        self.id.as_deref().is_none_or(str::is_empty) && self.path.is_none()
    }
}

/// Enough target metadata for Pi to use DesktopCtl's existing active-window
/// identity mechanism.  `id` is normally the opaque `window_ref` issued by
/// the daemon (for example, `mail_abc123`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetWindow {
    pub id: String,
    pub app: Option<String>,
    pub title: Option<String>,
}

/// Adapter-neutral result.  The UI should display only `final_answer`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentResult {
    pub session: AgentSessionRef,
    pub final_answer: String,
}

/// A small abstraction so another CLI adapter can be added without changing
/// launcher/session UI code.
pub trait AgentRunner: Send + Sync {
    fn spawn(&self, request: AgentRequest) -> Result<AgentProcess, AgentRunnerError>;
}

/// Pi command runner.  The optional executable and cwd are useful for tests
/// and for callers that expose explicit configuration.
#[derive(Debug, Clone, Default)]
pub struct PiRunner {
    executable: Option<PathBuf>,
    current_dir: Option<PathBuf>,
}

impl PiRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_executable(path: impl Into<PathBuf>) -> Self {
        Self {
            executable: Some(path.into()),
            current_dir: None,
        }
    }

    pub fn with_current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    pub fn executable(&self) -> Result<PathBuf, AgentRunnerError> {
        if let Some(path) = self.executable.as_deref() {
            if is_executable_file(path) {
                return Ok(path.to_path_buf());
            }
            return Err(AgentRunnerError::MissingExecutable {
                configured: Some(path.to_path_buf()),
                message: format!(
                    "configured Pi executable does not exist or is not executable: {}",
                    path.display()
                ),
            });
        }
        discover_pi_executable()
    }

    /// Build the exact argv passed to Pi.  This is intentionally separate from
    /// process creation so callers/tests can verify no shell is involved.
    pub fn args_for(request: &AgentRequest) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("--mode"),
            OsString::from("json"),
            OsString::from("--print"),
        ];
        if let Some(session) = request.session.as_ref().filter(|s| !s.is_empty()) {
            if let Some(path) = session.path.as_ref() {
                args.push(OsString::from("--session"));
                args.push(path.as_os_str().to_os_string());
            } else if let Some(id) = session.id.as_deref().filter(|id| !id.trim().is_empty()) {
                args.push(OsString::from("--session-id"));
                args.push(OsString::from(id));
            }
        }
        if request.target_window.is_some() {
            args.push(OsString::from("--append-system-prompt"));
            args.push(OsString::from(
                "The user's desktop target is the current topmost non-DesktopCtl window. Use desktopctl commands with --active-window when inspecting or acting on it.",
            ));
        }
        // `--` protects prompts beginning with a dash while keeping user text
        // an argv element rather than shell source.
        args.push(OsString::from("--"));
        args.push(OsString::from(&request.prompt));
        args
    }

    fn command_for(&self, request: &AgentRequest) -> Result<Command, AgentRunnerError> {
        let executable = self.executable()?;
        let mut command = Command::new(executable);
        command.args(Self::args_for(request));
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        // Drain stderr concurrently in AgentProcess::wait.  It is never
        // merged into the structured stdout stream.
        command.stderr(Stdio::piped());
        if let Some(dir) = self.current_dir.as_deref() {
            command.current_dir(dir);
        }
        Ok(command)
    }
}

impl AgentRunner for PiRunner {
    fn spawn(&self, request: AgentRequest) -> Result<AgentProcess, AgentRunnerError> {
        let mut command = self.command_for(&request)?;
        let child = command.spawn().map_err(|source| AgentRunnerError::Spawn {
            executable: self.executable().ok(),
            source,
        })?;
        Ok(AgentProcess::new(child))
    }
}

/// A running adapter process.  `wait_with_cancellation` drains both output
/// streams while polling the child, allowing cancellation without leaving a
/// Pi process behind.
pub struct AgentProcess {
    child: Option<Child>,
}

impl fmt::Debug for AgentProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentProcess")
            .field("running", &self.child.is_some())
            .finish()
    }
}

impl AgentProcess {
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    pub fn wait(mut self) -> Result<AgentResult, AgentRunnerError> {
        let never_cancelled = AtomicBool::new(false);
        self.wait_with_cancellation(&never_cancelled)
    }

    pub fn wait_with_cancellation(
        &mut self,
        cancellation: &AtomicBool,
    ) -> Result<AgentResult, AgentRunnerError> {
        let mut child = self.child.take().ok_or_else(|| {
            AgentRunnerError::Process("process was already waited or cancelled".into())
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentRunnerError::Process("Pi stdout pipe was unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AgentRunnerError::Process("Pi stderr pipe was unavailable".into()))?;
        let stdout_thread = thread::spawn(move || read_pipe(stdout));
        let stderr_thread = thread::spawn(move || read_pipe(stderr));

        let status = loop {
            if cancellation.load(Ordering::Acquire) {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return Err(AgentRunnerError::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(source) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stdout_thread.join();
                    let _ = stderr_thread.join();
                    return Err(AgentRunnerError::Wait { source });
                }
            }
        };

        let stdout = stdout_thread
            .join()
            .map_err(|_| AgentRunnerError::Process("Pi stdout reader panicked".into()))??;
        let stderr = stderr_thread
            .join()
            .map_err(|_| AgentRunnerError::Process("Pi stderr reader panicked".into()))??;
        if !status.success() {
            let detail = stderr.trim();
            return Err(if detail.is_empty() {
                AgentRunnerError::Process(format!("Pi exited with {status}"))
            } else {
                AgentRunnerError::Process(format!("Pi exited with {status}: {detail}"))
            });
        }
        parse_pi_output(&stdout)
    }

    pub fn cancel(&mut self) -> Result<(), AgentRunnerError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => {
                child
                    .kill()
                    .map_err(|source| AgentRunnerError::Kill { source })?;
                child
                    .wait()
                    .map(|_| ())
                    .map_err(|source| AgentRunnerError::Wait { source })
            }
            Err(source) => Err(AgentRunnerError::Wait { source }),
        }
    }
}

impl Drop for AgentProcess {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            if child.try_wait().ok().flatten().is_none() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }
}

fn read_pipe<R: Read>(mut pipe: R) -> Result<String, AgentRunnerError> {
    let mut bytes = Vec::new();
    pipe.read_to_end(&mut bytes)
        .map_err(|source| AgentRunnerError::Io { source })?;
    String::from_utf8(bytes).map_err(|source| AgentRunnerError::Utf8 { source })
}

/// Resolve Pi without relying on the interactive shell's PATH.  The explicit
/// environment override is suitable for GUI app deployments and diagnostics.
pub fn discover_pi_executable() -> Result<PathBuf, AgentRunnerError> {
    let configured = env::var_os("DESKTOPCTL_PI_PATH");
    let path = env::var_os("PATH");
    let home = env::var_os("HOME").map(PathBuf::from);
    resolve_pi_executable(configured.as_deref(), path.as_deref(), home.as_deref())
}

fn resolve_pi_executable(
    configured: Option<&std::ffi::OsStr>,
    path: Option<&std::ffi::OsStr>,
    home: Option<&Path>,
) -> Result<PathBuf, AgentRunnerError> {
    if let Some(configured) = configured.filter(|value| !value.is_empty()) {
        let configured = PathBuf::from(configured);
        if is_executable_file(&configured) {
            return Ok(configured);
        }
        return Err(AgentRunnerError::MissingExecutable {
            configured: Some(configured.clone()),
            message: format!(
                "Pi was not found at DESKTOPCTL_PI_PATH={}; install Pi or configure DESKTOPCTL_PI_PATH to its executable",
                configured.display()
            ),
        });
    }

    let mut candidates = Vec::new();
    if let Some(path) = path {
        candidates.extend(env::split_paths(path).map(|dir| dir.join("pi")));
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin/pi"),
        PathBuf::from("/usr/local/bin/pi"),
        PathBuf::from("/usr/bin/pi"),
    ]);
    if let Some(home) = home {
        candidates.extend([
            home.join(".local/bin/pi"),
            home.join(".npm-global/bin/pi"),
            home.join(".bun/bin/pi"),
            home.join(".volta/bin/pi"),
            home.join(".asdf/shims/pi"),
            home.join("bin/pi"),
        ]);
    }
    candidates.dedup();
    candidates
        .into_iter()
        .find(|candidate| is_executable_file(candidate))
        .ok_or_else(|| AgentRunnerError::MissingExecutable {
            configured: None,
            message: "Pi executable not found. Install Pi, or set DESKTOPCTL_PI_PATH to the full path of the pi executable (GUI apps may not inherit your shell PATH).".into(),
        })
}

fn is_executable_file(path: &Path) -> bool {
    if !path.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        return path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false);
    }
    #[cfg(not(unix))]
    true
}

/// Parse Pi's `--mode json` JSONL stream.  Only the final assistant text is
/// retained; thinking, tools, usage, and internal events are discarded.
pub fn parse_pi_output(output: &str) -> Result<AgentResult, AgentRunnerError> {
    let mut session_id = None;
    let mut session_path = None;
    let mut session_cwd = None;
    let mut final_answer = None;
    let mut fallback_answer = None;
    let mut saw_event = false;

    for (line_number, line) in output.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line).map_err(|source| {
            AgentRunnerError::Parse(format!(
                "invalid Pi JSON on line {}: {source}",
                line_number + 1
            ))
        })?;
        saw_event = true;
        if let Some(id) = string_field(&event, &["id", "sessionId", "session_id"]).filter(|_| {
            event.get("type").and_then(Value::as_str) == Some("session")
                || event.get("sessionId").is_some()
                || event.get("session_id").is_some()
        }) {
            session_id = Some(id.to_string());
        }
        if let Some(path) = string_field(
            &event,
            &["sessionFile", "session_file", "sessionPath", "session_path"],
        ) {
            session_path = Some(PathBuf::from(path));
        }
        if event.get("type").and_then(Value::as_str) == Some("session") {
            if let Some(cwd) = event.get("cwd").and_then(Value::as_str) {
                session_cwd = Some(PathBuf::from(cwd));
            }
        }

        match event.get("type").and_then(Value::as_str) {
            Some("message_end") => {
                let message = event.get("message").unwrap_or(&Value::Null);
                if message.get("role").and_then(Value::as_str) == Some("assistant") {
                    if is_failed_message(message) {
                        return Err(AgentRunnerError::Parse(
                            message
                                .get("errorMessage")
                                .and_then(Value::as_str)
                                .unwrap_or("Pi assistant turn failed")
                                .to_string(),
                        ));
                    }
                    final_answer = extract_message_text(message);
                }
            }
            Some("agent_end") => {
                if let Some(messages) = event.get("messages").and_then(Value::as_array) {
                    for message in messages {
                        if message.get("role").and_then(Value::as_str) == Some("assistant")
                            && !is_failed_message(message)
                        {
                            fallback_answer = extract_message_text(message);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    if !saw_event {
        return Err(AgentRunnerError::Parse("Pi produced no JSON events".into()));
    }
    let final_answer = final_answer.or(fallback_answer).ok_or_else(|| {
        AgentRunnerError::Parse("Pi output contained no final assistant answer".into())
    })?;
    if final_answer.trim().is_empty() {
        return Err(AgentRunnerError::Parse(
            "Pi final assistant answer was empty".into(),
        ));
    }
    Ok(AgentResult {
        session: AgentSessionRef {
            id: session_id,
            path: session_path,
            cwd: session_cwd,
        },
        final_answer,
    })
}

fn string_field<'a>(value: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .filter(|v| !v.trim().is_empty())
    })
}

fn is_failed_message(message: &Value) -> bool {
    matches!(
        message.get("stopReason").and_then(Value::as_str),
        Some("error" | "aborted")
    )
}

fn extract_message_text(message: &Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let pieces = content.as_array()?.iter().filter_map(|piece| {
        if piece.get("type").and_then(Value::as_str) == Some("text") {
            piece.get("text").and_then(Value::as_str)
        } else {
            None
        }
    });
    let text = pieces.collect::<Vec<_>>().join("");
    (!text.is_empty()).then_some(text)
}

#[derive(Debug)]
pub enum AgentRunnerError {
    MissingExecutable {
        configured: Option<PathBuf>,
        message: String,
    },
    Spawn {
        executable: Option<PathBuf>,
        source: io::Error,
    },
    Io {
        source: io::Error,
    },
    Utf8 {
        source: std::string::FromUtf8Error,
    },
    Wait {
        source: io::Error,
    },
    Kill {
        source: io::Error,
    },
    Parse(String),
    Process(String),
    Cancelled,
}

impl fmt::Display for AgentRunnerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingExecutable { message, .. } => f.write_str(message),
            Self::Spawn { executable, source } => write!(
                f,
                "failed to start Pi{}: {source}",
                executable
                    .as_ref()
                    .map(|path| format!(" ({})", path.display()))
                    .unwrap_or_default()
            ),
            Self::Io { source } => write!(f, "failed reading Pi output: {source}"),
            Self::Utf8 { source } => write!(f, "Pi output was not valid UTF-8: {source}"),
            Self::Wait { source } => write!(f, "failed waiting for Pi: {source}"),
            Self::Kill { source } => write!(f, "failed cancelling Pi: {source}"),
            Self::Parse(message) => write!(f, "invalid Pi output: {message}"),
            Self::Process(message) => f.write_str(message),
            Self::Cancelled => f.write_str("Pi request cancelled"),
        }
    }
}

impl std::error::Error for AgentRunnerError {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        ffi::OsString,
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn parses_final_assistant_text_and_session_header() {
        let output = concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"pi-123\",\"timestamp\":\"now\",\"cwd\":\"/project\"}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"hidden\"},{\"type\":\"text\",\"text\":\"Hello\"}],\"stopReason\":\"stop\"}}\n"
        );
        let result = parse_pi_output(output).expect("valid output");
        assert_eq!(result.session.id.as_deref(), Some("pi-123"));
        assert_eq!(result.session.cwd.as_deref(), Some(Path::new("/project")));
        assert_eq!(result.final_answer, "Hello");
    }

    #[test]
    fn parser_uses_latest_message_end_and_ignores_tools() {
        let output = concat!(
            "{\"type\":\"session\",\"id\":\"s\"}\n",
            "{\"type\":\"tool_execution_end\",\"result\":{\"secret\":true}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":\"first\",\"stopReason\":\"stop\"}}\n",
            "{\"type\":\"message_end\",\"message\":{\"role\":\"assistant\",\"content\":\"last\",\"stopReason\":\"stop\"}}\n"
        );
        assert_eq!(parse_pi_output(output).unwrap().final_answer, "last");
    }

    #[test]
    fn malformed_json_is_rejected() {
        let error = parse_pi_output("{\"type\":\"session\"}\nnot-json\n").unwrap_err();
        assert!(matches!(error, AgentRunnerError::Parse(_)));
    }

    #[test]
    fn failed_assistant_message_is_rejected() {
        let output = r#"{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"aborted"}}"#;
        assert!(parse_pi_output(output).is_err());
    }

    #[test]
    fn args_use_direct_session_arguments_and_target_instruction() {
        let mut request = AgentRequest::new("- summarize this");
        request.session = Some(AgentSessionRef::id("native-id"));
        request.target_window = Some(TargetWindow {
            id: "mail_abc123".into(),
            app: Some("Mail".into()),
            title: Some("Inbox".into()),
        });
        let args = PiRunner::args_for(&request);
        assert_eq!(args[0], OsString::from("--mode"));
        assert!(
            args.windows(2)
                .any(|pair| pair == [OsString::from("--session-id"), OsString::from("native-id")])
        );
        assert!(args.iter().any(|arg| arg == &OsString::from("--")));
        assert_eq!(args.last(), Some(&OsString::from("- summarize this")));
        let context_index = args
            .iter()
            .position(|arg| arg == "--append-system-prompt")
            .expect("system prompt flag");
        assert!(
            args[context_index + 1]
                .to_string_lossy()
                .contains("current topmost non-DesktopCtl window")
        );
    }

    #[test]
    fn path_takes_precedence_over_session_id() {
        let mut request = AgentRequest::new("follow up");
        request.session = Some(AgentSessionRef {
            id: Some("id".into()),
            path: Some(PathBuf::from("/tmp/native.jsonl")),
            cwd: None,
        });
        let args = PiRunner::args_for(&request);
        assert!(args.iter().any(|arg| arg == &OsString::from("--session")));
        assert!(
            !args
                .iter()
                .any(|arg| arg == &OsString::from("--session-id"))
        );
    }

    #[test]
    fn discovery_prefers_explicit_override() {
        let current = env::current_exe().expect("test executable");
        let result = resolve_pi_executable(
            Some(current.as_os_str()),
            Some(OsString::from("/definitely/missing").as_os_str()),
            None,
        )
        .expect("override should win");
        assert_eq!(result, current);
    }

    #[test]
    fn discovery_reports_missing_override_usefully() {
        let error = resolve_pi_executable(
            Some(OsString::from("/definitely/missing/pi").as_os_str()),
            None,
            None,
        )
        .unwrap_err();
        assert!(error.to_string().contains("DESKTOPCTL_PI_PATH"));
    }

    #[test]
    fn configured_runner_rejects_non_executable_path() {
        let path = env::temp_dir().join(format!(
            "desktopctl-pi-runner-test-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, b"not executable").expect("write test file");
        let error = PiRunner::with_executable(&path).executable().unwrap_err();
        let _ = fs::remove_file(path);
        assert!(matches!(error, AgentRunnerError::MissingExecutable { .. }));
    }
}

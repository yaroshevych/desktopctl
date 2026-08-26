//! DesktopCtl-owned metadata for the agent launcher.
//!
//! Pi remains the authority for its complete native transcript.  This module
//! only stores the small amount of data needed to render the launcher and the
//! user/final-answer transcript: prompts, final answers, session identity,
//! state, and the target window bound when a run was started.

#![allow(dead_code)] // Store exposes focused operations used by tests and future adapters.

use std::{
    error::Error,
    fmt,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

const STORE_VERSION: u32 = 1;
const STORE_FILE_NAME: &str = "agent-sessions.json";
const DEFAULT_MAX_RECENT: usize = 12;
const TITLE_MAX_CHARS: usize = 72;
static TEMP_FILE_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// The lifecycle state shown by the launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentSessionStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl AgentSessionStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// The only transcript roles DesktopCtl renders.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionMessageRole {
    User,
    Assistant,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMessage {
    pub role: SessionMessageRole,
    pub text: String,
    pub created_at_ms: u64,
}

impl SessionMessage {
    pub fn user(text: impl Into<String>, created_at_ms: u64) -> Self {
        Self {
            role: SessionMessageRole::User,
            text: text.into(),
            created_at_ms,
        }
    }

    pub fn assistant(text: impl Into<String>, created_at_ms: u64) -> Self {
        Self {
            role: SessionMessageRole::Assistant,
            text: text.into(),
            created_at_ms,
        }
    }
}

/// Useful identifying metadata for the window that was frontmost before the
/// launcher became key.  Native IDs are intentionally strings because the
/// public DesktopCtl window reference is opaque and platform-specific.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetWindowMetadata {
    #[serde(default)]
    pub window_ref: Option<String>,
    #[serde(default)]
    pub native_id: Option<String>,
    #[serde(default)]
    pub pid: Option<i64>,
    #[serde(default)]
    pub app: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSession {
    /// DesktopCtl's stable UUID, distinct from Pi's native session identity.
    pub id: String,
    pub agent: String,
    #[serde(default)]
    pub native_session_id: Option<String>,
    #[serde(default)]
    pub native_session_path: Option<String>,
    #[serde(default)]
    pub native_session_cwd: Option<String>,
    pub title: String,
    pub messages: Vec<SessionMessage>,
    #[serde(default)]
    pub target_window: Option<TargetWindowMetadata>,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub status: AgentSessionStatus,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub unread: bool,
    #[serde(default)]
    pub visited: bool,
    /// Set only while a request is executing.  It prevents a second request
    /// from being started in this session and lets late child-process events
    /// be ignored safely.
    #[serde(default)]
    pub active_request_id: Option<String>,
}

impl AgentSession {
    pub fn first_prompt(&self) -> Option<&str> {
        self.messages
            .iter()
            .find(|message| message.role == SessionMessageRole::User)
            .map(|message| message.text.as_str())
    }

    pub fn final_answer(&self) -> Option<&str> {
        self.messages
            .iter()
            .rev()
            .find(|message| message.role == SessionMessageRole::Assistant)
            .map(|message| message.text.as_str())
    }

    pub fn answer_preview(&self, max_chars: usize) -> Option<String> {
        self.final_answer()
            .map(|answer| truncate_one_line(answer, max_chars))
    }
}

#[derive(Debug)]
pub enum SessionStoreError {
    Io(std::io::Error),
    Json(serde_json::Error),
    Invalid(String),
    NotFound(String),
    AlreadyRunning(String),
    RequestMismatch(String),
}

impl fmt::Display for SessionStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "session store I/O error: {error}"),
            Self::Json(error) => write!(f, "session store JSON error: {error}"),
            Self::Invalid(message) => write!(f, "invalid session state: {message}"),
            Self::NotFound(id) => write!(f, "session not found: {id}"),
            Self::AlreadyRunning(id) => write!(f, "session already has a running request: {id}"),
            Self::RequestMismatch(id) => write!(f, "request does not own running session: {id}"),
        }
    }
}

impl Error for SessionStoreError {}

impl From<std::io::Error> for SessionStoreError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for SessionStoreError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PersistedSessions {
    version: u32,
    sessions: Vec<AgentSession>,
}

/// Persistent collection of DesktopCtl launcher sessions.
#[derive(Debug, Clone)]
pub struct AgentSessionStore {
    path: PathBuf,
    sessions: Vec<AgentSession>,
}

impl AgentSessionStore {
    /// Resolve the normal per-user data directory.  The override makes tests
    /// hermetic and is also useful for a portable/development installation.
    pub fn data_dir() -> Option<PathBuf> {
        if let Some(override_dir) = std::env::var_os("DESKTOPCTL_AGENT_DATA_DIR") {
            let path = PathBuf::from(override_dir);
            if !path.as_os_str().is_empty() {
                return Some(path);
            }
        }

        let home = std::env::var_os("HOME").map(PathBuf::from)?;
        #[cfg(target_os = "macos")]
        {
            return Some(home.join("Library/Application Support/DesktopCtl"));
        }
        #[cfg(not(target_os = "macos"))]
        {
            if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
                let path = PathBuf::from(xdg);
                if !path.as_os_str().is_empty() {
                    return Some(path.join("desktopctl"));
                }
            }
            Some(home.join(".local/share/desktopctl"))
        }
    }

    pub fn default_path() -> Option<PathBuf> {
        Self::data_dir().map(|dir| dir.join(STORE_FILE_NAME))
    }

    /// Load the default store and recover sessions left running by a crashed
    /// DesktopCtl process.
    pub fn load() -> Result<Self, SessionStoreError> {
        let path = Self::default_path()
            .ok_or_else(|| SessionStoreError::Invalid("unable to resolve data directory".into()))?;
        Self::load_at(path, unix_now_ms())
    }

    pub fn load_at(path: impl Into<PathBuf>, now_ms: u64) -> Result<Self, SessionStoreError> {
        let path = path.into();
        if !path.exists() {
            return Ok(Self {
                path,
                sessions: Vec::new(),
            });
        }

        let bytes = fs::read(&path)?;
        let persisted: PersistedSessions = serde_json::from_slice(&bytes)?;
        if persisted.version != STORE_VERSION {
            return Err(SessionStoreError::Invalid(format!(
                "unsupported store version {}; expected {STORE_VERSION}",
                persisted.version
            )));
        }
        let mut store = Self {
            path,
            sessions: persisted.sessions,
        };
        if store.recover_stale_running_at(now_ms) > 0 {
            store.save()?;
        }
        Ok(store)
    }

    /// Best-effort startup loading for the menu-bar process.  A malformed
    /// file is left untouched and reported to the caller; the UI can continue
    /// with an empty store and offer a useful diagnostic.
    pub fn load_or_empty_at(path: impl Into<PathBuf>, now_ms: u64) -> (Self, Option<String>) {
        let path = path.into();
        match Self::load_at(path.clone(), now_ms) {
            Ok(store) => (store, None),
            Err(error) => (
                Self {
                    path,
                    sessions: Vec::new(),
                },
                Some(error.to_string()),
            ),
        }
    }

    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            sessions: Vec::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn sessions(&self) -> &[AgentSession] {
        &self.sessions
    }

    pub fn get(&self, id: &str) -> Option<&AgentSession> {
        self.sessions.iter().find(|session| session.id == id)
    }

    pub fn get_mut(&mut self, id: &str) -> Option<&mut AgentSession> {
        self.sessions.iter_mut().find(|session| session.id == id)
    }

    /// Persist the current state atomically.  The temporary file is created
    /// beside the destination so rename is on the same filesystem.
    pub fn save(&self) -> Result<(), SessionStoreError> {
        let Some(parent) = self.path.parent() else {
            return Err(SessionStoreError::Invalid(
                "session store path has no parent directory".into(),
            ));
        };
        fs::create_dir_all(parent)?;
        set_private_permissions(parent)?;

        let payload = PersistedSessions {
            version: STORE_VERSION,
            sessions: self.sessions.clone(),
        };
        let bytes = serde_json::to_vec_pretty(&payload)?;
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let temp_path = parent.join(format!(
            ".{}.tmp-{}-{}",
            STORE_FILE_NAME,
            std::process::id(),
            sequence
        ));
        let write_result = (|| -> Result<(), SessionStoreError> {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp_path)?;
            set_private_permissions_file(&file)?;
            file.write_all(&bytes)?;
            file.sync_all()?;
            fs::rename(&temp_path, &self.path)?;
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp_path);
        }
        write_result
    }

    /// Start a new Pi request and create its DesktopCtl session metadata.
    pub fn create_running(
        &mut self,
        prompt: impl Into<String>,
        target_window: Option<TargetWindowMetadata>,
        now_ms: u64,
    ) -> Result<(String, String), SessionStoreError> {
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err(SessionStoreError::Invalid(
                "prompt must not be empty".into(),
            ));
        }
        let id = Uuid::new_v4().to_string();
        let request_id = Uuid::new_v4().to_string();
        let session = AgentSession {
            id: id.clone(),
            agent: "pi".to_string(),
            native_session_id: None,
            native_session_path: None,
            native_session_cwd: None,
            title: derive_title(&prompt),
            messages: vec![SessionMessage::user(prompt, now_ms)],
            target_window,
            created_at_ms: now_ms,
            updated_at_ms: now_ms,
            status: AgentSessionStatus::Running,
            error: None,
            unread: false,
            visited: false,
            active_request_id: Some(request_id.clone()),
        };
        self.sessions.push(session);
        self.save()?;
        Ok((id, request_id))
    }

    /// Start a follow-up in an existing terminal session.
    pub fn begin_request(
        &mut self,
        session_id: &str,
        prompt: impl Into<String>,
        now_ms: u64,
    ) -> Result<String, SessionStoreError> {
        let prompt = prompt.into();
        if prompt.trim().is_empty() {
            return Err(SessionStoreError::Invalid(
                "prompt must not be empty".into(),
            ));
        }
        let session = self
            .get_mut(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.to_string()))?;
        if session.status == AgentSessionStatus::Running || session.active_request_id.is_some() {
            return Err(SessionStoreError::AlreadyRunning(session_id.to_string()));
        }
        let request_id = Uuid::new_v4().to_string();
        session.messages.push(SessionMessage::user(prompt, now_ms));
        session.status = AgentSessionStatus::Running;
        session.error = None;
        session.updated_at_ms = now_ms;
        session.active_request_id = Some(request_id.clone());
        self.save()?;
        Ok(request_id)
    }

    pub fn bind_native_session(
        &mut self,
        session_id: &str,
        native_session_id: Option<String>,
        native_session_path: Option<String>,
        native_session_cwd: Option<String>,
    ) -> Result<(), SessionStoreError> {
        let session = self
            .get_mut(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.to_string()))?;
        session.native_session_id = native_session_id;
        session.native_session_path = native_session_path;
        session.native_session_cwd = native_session_cwd;
        self.save()
    }

    pub fn complete_request(
        &mut self,
        session_id: &str,
        request_id: &str,
        answer: impl Into<String>,
        now_ms: u64,
    ) -> Result<(), SessionStoreError> {
        let answer = answer.into();
        if answer.trim().is_empty() {
            return Err(SessionStoreError::Invalid(
                "assistant answer must not be empty".into(),
            ));
        }
        let session = self.running_request(session_id, request_id)?;
        session
            .messages
            .push(SessionMessage::assistant(answer, now_ms));
        session.status = AgentSessionStatus::Completed;
        session.error = None;
        session.updated_at_ms = now_ms;
        session.active_request_id = None;
        session.unread = true;
        session.visited = false;
        self.save()
    }

    pub fn fail_request(
        &mut self,
        session_id: &str,
        request_id: &str,
        error: impl Into<String>,
        now_ms: u64,
    ) -> Result<(), SessionStoreError> {
        self.finish_without_answer(
            session_id,
            request_id,
            AgentSessionStatus::Failed,
            error.into(),
            now_ms,
        )
    }

    pub fn cancel_request(
        &mut self,
        session_id: &str,
        request_id: &str,
        now_ms: u64,
    ) -> Result<(), SessionStoreError> {
        self.finish_without_answer(
            session_id,
            request_id,
            AgentSessionStatus::Cancelled,
            "request cancelled".to_string(),
            now_ms,
        )
    }

    pub fn mark_visited(&mut self, session_id: &str, now_ms: u64) -> Result<(), SessionStoreError> {
        let session = self
            .get_mut(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.to_string()))?;
        session.visited = true;
        session.unread = false;
        session.updated_at_ms = now_ms;
        self.save()
    }

    pub fn mark_unread(&mut self, session_id: &str, unread: bool) -> Result<(), SessionStoreError> {
        let session = self
            .get_mut(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.to_string()))?;
        session.unread = unread;
        self.save()
    }

    pub fn sync_native_transcript(
        &mut self,
        session_id: &str,
        messages: Vec<SessionMessage>,
        native_session_path: Option<String>,
    ) -> Result<bool, SessionStoreError> {
        if messages.is_empty() {
            return Ok(false);
        }
        let session = self
            .get_mut(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.to_string()))?;
        // A native-file refresh is a snapshot taken on a worker thread. Never
        // let a stale snapshot overwrite a prompt just appended for an active
        // launcher request; the next open will sync after that run completes.
        if session.status == AgentSessionStatus::Running || session.active_request_id.is_some() {
            return Ok(false);
        }
        let changed = session.messages != messages
            || (native_session_path.is_some()
                && session.native_session_path != native_session_path);
        if !changed {
            return Ok(false);
        }
        if let Some(last_timestamp) = messages
            .iter()
            .map(|message| message.created_at_ms)
            .filter(|timestamp| *timestamp > 0)
            .max()
        {
            session.updated_at_ms = session.updated_at_ms.max(last_timestamp);
        }
        session.messages = messages;
        if native_session_path.is_some() {
            session.native_session_path = native_session_path;
        }
        self.save()?;
        Ok(true)
    }

    /// Recover requests that were running when the process disappeared.  This
    /// is intentionally explicit and deterministic for tests; `load_at` calls
    /// it automatically during normal startup.
    pub fn recover_stale_running_at(&mut self, now_ms: u64) -> usize {
        let mut recovered = 0;
        for session in &mut self.sessions {
            if session.status == AgentSessionStatus::Running || session.active_request_id.is_some()
            {
                session.status = AgentSessionStatus::Failed;
                session.error = Some("DesktopCtl restarted before this request completed".into());
                session.active_request_id = None;
                session.updated_at_ms = now_ms;
                session.unread = true;
                recovered += 1;
            }
        }
        recovered
    }

    pub fn recent(&self, limit: usize) -> Vec<&AgentSession> {
        let mut sessions: Vec<&AgentSession> = self.sessions.iter().collect();
        sessions.sort_by(|a, b| {
            b.updated_at_ms
                .cmp(&a.updated_at_ms)
                .then_with(|| b.created_at_ms.cmp(&a.created_at_ms))
                .then_with(|| b.id.cmp(&a.id))
        });
        sessions.truncate(limit);
        sessions
    }

    pub fn recent_default(&self) -> Vec<&AgentSession> {
        self.recent(DEFAULT_MAX_RECENT)
    }

    pub fn latest_completed_unvisited(&self) -> Option<&AgentSession> {
        self.sessions
            .iter()
            .filter(|session| session.status == AgentSessionStatus::Completed && !session.visited)
            .max_by(|a, b| {
                a.updated_at_ms
                    .cmp(&b.updated_at_ms)
                    .then_with(|| a.id.cmp(&b.id))
            })
    }

    fn running_request(
        &mut self,
        session_id: &str,
        request_id: &str,
    ) -> Result<&mut AgentSession, SessionStoreError> {
        let session = self
            .get_mut(session_id)
            .ok_or_else(|| SessionStoreError::NotFound(session_id.to_string()))?;
        if session.status != AgentSessionStatus::Running {
            return Err(SessionStoreError::RequestMismatch(session_id.to_string()));
        }
        if session.active_request_id.as_deref() != Some(request_id) {
            return Err(SessionStoreError::RequestMismatch(session_id.to_string()));
        }
        Ok(session)
    }

    fn finish_without_answer(
        &mut self,
        session_id: &str,
        request_id: &str,
        status: AgentSessionStatus,
        error: String,
        now_ms: u64,
    ) -> Result<(), SessionStoreError> {
        let session = self.running_request(session_id, request_id)?;
        session.status = status;
        session.error = Some(error);
        session.updated_at_ms = now_ms;
        session.active_request_id = None;
        session.unread = true;
        self.save()
    }
}

pub fn unix_now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
        .unwrap_or(0)
}

pub fn derive_title(prompt: &str) -> String {
    let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_one_line(&normalized, TITLE_MAX_CHARS)
}

pub fn truncate_one_line(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut chars = normalized.chars();
    let value: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        let suffix = "…";
        if max_chars <= suffix.chars().count() {
            suffix.chars().take(max_chars).collect()
        } else {
            format!(
                "{}{}",
                value.chars().take(max_chars - 1).collect::<String>(),
                suffix
            )
        }
    } else {
        value
    }
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> Result<(), SessionStoreError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path) -> Result<(), SessionStoreError> {
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions_file(file: &File) -> Result<(), SessionStoreError> {
    use std::os::unix::fs::PermissionsExt;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_permissions_file(_file: &File) -> Result<(), SessionStoreError> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn test_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "desktopctl-agent-sessions-test-{}-{name}",
            std::process::id()
        ))
    }

    fn clean(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    #[test]
    fn persists_round_trip_and_uses_only_short_transcript() {
        let path = test_path("round-trip").join("sessions.json");
        clean(&path);
        let mut store = AgentSessionStore::new(&path);
        let target = TargetWindowMetadata {
            window_ref: Some("mail_abc123".into()),
            native_id: Some("42:99".into()),
            pid: Some(42),
            app: Some("Mail".into()),
            title: Some("Inbox".into()),
        };
        let (id, request) = store
            .create_running("  Summarise\n this email ", Some(target.clone()), 100)
            .expect("create");
        store
            .bind_native_session(
                &id,
                Some("pi-session".into()),
                None,
                Some("/Users/test/project".into()),
            )
            .expect("bind native session");
        store
            .complete_request(&id, &request, "A concise answer", 200)
            .expect("complete");

        let loaded = AgentSessionStore::load_at(&path, 300).expect("load");
        let session = loaded.get(&id).expect("session");
        assert_eq!(session.agent, "pi");
        assert_eq!(session.title, "Summarise this email");
        assert_eq!(session.target_window, Some(target));
        assert_eq!(session.status, AgentSessionStatus::Completed);
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.final_answer(), Some("A concise answer"));
        assert_eq!(session.native_session_id.as_deref(), Some("pi-session"));
        assert_eq!(
            session.native_session_cwd.as_deref(),
            Some("/Users/test/project")
        );
        assert!(session.unread);
        clean(&path);
    }

    #[test]
    fn transitions_enforce_one_request_and_owner() {
        let path = test_path("transitions").join("sessions.json");
        clean(&path);
        let mut store = AgentSessionStore::new(&path);
        let (id, request) = store.create_running("first", None, 1).expect("create");
        assert!(matches!(
            store.begin_request(&id, "second", 2),
            Err(SessionStoreError::AlreadyRunning(_))
        ));
        assert!(matches!(
            store.complete_request(&id, "wrong", "answer", 3),
            Err(SessionStoreError::RequestMismatch(_))
        ));
        store.fail_request(&id, &request, "no pi", 4).expect("fail");
        assert_eq!(store.get(&id).unwrap().status, AgentSessionStatus::Failed);
        let followup = store.begin_request(&id, "second", 5).expect("followup");
        store.cancel_request(&id, &followup, 6).expect("cancel");
        assert_eq!(
            store.get(&id).unwrap().status,
            AgentSessionStatus::Cancelled
        );
        clean(&path);
    }

    #[test]
    fn recent_and_latest_order_by_activity() {
        let path = test_path("ordering").join("sessions.json");
        clean(&path);
        let mut store = AgentSessionStore::new(&path);
        let (first, req) = store.create_running("first", None, 1).expect("first");
        store
            .complete_request(&first, &req, "one", 10)
            .expect("first done");
        let (second, req) = store.create_running("second", None, 2).expect("second");
        store
            .complete_request(&second, &req, "two", 30)
            .expect("second done");
        let _third = store.create_running("third", None, 40).expect("third").0;
        let recent: Vec<&str> = store.recent(3).iter().map(|s| s.title.as_str()).collect();
        assert_eq!(recent, vec!["third", "second", "first"]);
        assert_eq!(store.latest_completed_unvisited().unwrap().id, second);
        store.mark_visited(&second, 50).expect("visit");
        assert_eq!(store.latest_completed_unvisited().unwrap().id, first);
        clean(&path);
    }

    #[test]
    fn unread_is_set_on_completion_and_cleared_on_visit() {
        let path = test_path("unread").join("sessions.json");
        clean(&path);
        let mut store = AgentSessionStore::new(&path);
        let (id, request) = store.create_running("prompt", None, 1).expect("create");
        assert!(!store.get(&id).unwrap().unread);
        store
            .complete_request(&id, &request, "answer", 2)
            .expect("complete");
        assert!(store.get(&id).unwrap().unread);
        store.mark_visited(&id, 3).expect("visit");
        assert!(store.get(&id).unwrap().visited);
        assert!(!store.get(&id).unwrap().unread);
        clean(&path);
    }

    #[test]
    fn malformed_store_is_reported_without_overwriting_file() {
        let path = test_path("malformed").join("sessions.json");
        clean(&path);
        fs::create_dir_all(path.parent().unwrap()).expect("directory");
        fs::write(&path, b"not json").expect("malformed file");
        assert!(matches!(
            AgentSessionStore::load_at(&path, 1),
            Err(SessionStoreError::Json(_))
        ));
        assert_eq!(fs::read(&path).unwrap(), b"not json");
        let (empty, warning) = AgentSessionStore::load_or_empty_at(&path, 1);
        assert!(warning.is_some());
        assert!(empty.sessions().is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"not json");
        clean(&path);
    }

    #[test]
    fn startup_recovers_running_sessions_as_unread_failed() {
        let path = test_path("stale").join("sessions.json");
        clean(&path);
        let mut store = AgentSessionStore::new(&path);
        let (id, _) = store
            .create_running("interrupted", None, 100)
            .expect("create");
        let loaded = AgentSessionStore::load_at(&path, 200).expect("load");
        let session = loaded.get(&id).expect("session");
        assert_eq!(session.status, AgentSessionStatus::Failed);
        assert!(session.unread);
        assert_eq!(session.active_request_id, None);
        assert_eq!(session.updated_at_ms, 200);
        let reloaded = AgentSessionStore::load_at(&path, 300).expect("reload");
        assert_eq!(reloaded.get(&id).unwrap().updated_at_ms, 200);
        clean(&path);
    }

    #[test]
    fn native_transcript_sync_replaces_short_transcript_and_persists_path() {
        let path = test_path("native-sync").join("sessions.json");
        clean(&path);
        let mut store = AgentSessionStore::new(&path);
        let (id, request) = store.create_running("launcher prompt", None, 1).unwrap();
        store
            .complete_request(&id, &request, "launcher answer", 2)
            .unwrap();
        let messages = vec![
            SessionMessage {
                role: SessionMessageRole::User,
                text: "launcher prompt".into(),
                created_at_ms: 10,
            },
            SessionMessage {
                role: SessionMessageRole::Assistant,
                text: "launcher answer".into(),
                created_at_ms: 11,
            },
            SessionMessage {
                role: SessionMessageRole::User,
                text: "terminal follow-up".into(),
                created_at_ms: 12,
            },
            SessionMessage {
                role: SessionMessageRole::Assistant,
                text: "terminal answer".into(),
                created_at_ms: 13,
            },
        ];

        assert!(
            store
                .sync_native_transcript(&id, messages.clone(), Some("/tmp/pi.jsonl".into()))
                .unwrap()
        );
        assert!(
            !store
                .sync_native_transcript(&id, messages.clone(), Some("/tmp/pi.jsonl".into()))
                .unwrap()
        );
        let loaded = AgentSessionStore::load_at(&path, 20).unwrap();
        let session = loaded.get(&id).unwrap();
        assert_eq!(session.messages, messages);
        assert_eq!(
            session.native_session_path.as_deref(),
            Some("/tmp/pi.jsonl")
        );
        assert_eq!(session.updated_at_ms, 13);
        clean(&path);
    }

    #[test]
    fn native_transcript_sync_does_not_overwrite_an_active_follow_up() {
        let path = test_path("native-sync-running").join("sessions.json");
        clean(&path);
        let mut store = AgentSessionStore::new(&path);
        let (id, request) = store.create_running("first", None, 1).unwrap();
        store
            .complete_request(&id, &request, "first answer", 2)
            .unwrap();
        let _follow_up = store.begin_request(&id, "new follow-up", 3).unwrap();
        let stale_messages = vec![SessionMessage {
            role: SessionMessageRole::User,
            text: "first".into(),
            created_at_ms: 1,
        }];

        assert!(
            !store
                .sync_native_transcript(&id, stale_messages, None)
                .unwrap()
        );
        assert_eq!(
            store.get(&id).unwrap().messages.last().unwrap().text,
            "new follow-up"
        );
        clean(&path);
    }
}

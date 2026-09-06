// Headless agent execution for the launcher.
//
// The runner deliberately owns no UI or persistence. It returns the native
// session identity reported by the adapter, leaving DesktopCtl's session store
// to persist that identity and the short transcript shown by the UI.

#![allow(dead_code)] // Adapter API includes cancellation/configuration seams used by future UI.

use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsString,
    fmt, fs,
    io::{self, BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

use serde_json::Value;

/// Agent CLIs supported by the launcher.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Pi,
    Codex,
    Goose,
    OpenCode,
}

impl AgentKind {
    pub const ALL: [Self; 4] = [Self::Pi, Self::Codex, Self::Goose, Self::OpenCode];

    pub fn key(self) -> &'static str {
        match self {
            Self::Pi => "pi",
            Self::Codex => "codex",
            Self::Goose => "goose",
            Self::OpenCode => "opencode",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Pi => "Pi",
            Self::Codex => "Codex",
            Self::Goose => "Goose",
            Self::OpenCode => "OpenCode",
        }
    }

    pub fn from_key(key: &str) -> Self {
        match key {
            "codex" => Self::Codex,
            "goose" => Self::Goose,
            "opencode" => Self::OpenCode,
            _ => Self::Pi,
        }
    }

    pub fn from_code(code: i32) -> Self {
        match code {
            1 => Self::Codex,
            2 => Self::Goose,
            3 => Self::OpenCode,
            _ => Self::Pi,
        }
    }

    fn executable_name(self) -> &'static str {
        self.key()
    }

    fn environment_override(self) -> &'static str {
        match self {
            Self::Pi => "DESKTOPCTL_PI_PATH",
            Self::Codex => "DESKTOPCTL_CODEX_PATH",
            Self::Goose => "DESKTOPCTL_GOOSE_PATH",
            Self::OpenCode => "DESKTOPCTL_OPENCODE_PATH",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentInstallation {
    pub kind: AgentKind,
    pub executable: PathBuf,
}

/// Adapter-neutral request passed to an agent runner.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRequest {
    pub prompt: String,
    pub session: Option<AgentSessionRef>,
    pub target_window: Option<TargetWindow>,
    pub window_context: Option<String>,
    pub read_only: bool,
}

impl AgentRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        Self {
            prompt: prompt.into(),
            session: None,
            target_window: None,
            window_context: None,
            read_only: false,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NativeTranscriptMessage {
    pub user: bool,
    pub text: String,
    pub timestamp_ms: u64,
}

const MAX_STDOUT_BYTES: usize = 8 * 1024 * 1024;
const MAX_STDERR_BYTES: usize = 256 * 1024;
const PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_TRANSCRIPT_CACHE_ENTRIES: usize = 64;
const MAX_TRANSCRIPT_CACHE_BYTES: usize = 4 * 1024 * 1024;
const TRANSCRIPT_CACHE_ENTRY_OVERHEAD: usize = 128;

#[derive(Debug, Clone)]
struct NativeTranscriptEntry {
    id: String,
    parent_id: Option<String>,
    message: Option<NativeTranscriptMessage>,
}

#[derive(Debug, Clone)]
struct CachedTranscript {
    len: u64,
    modified: Option<std::time::SystemTime>,
    identity: Option<(u64, u64)>,
    parsed_len: u64,
    prefix_fingerprint: u64,
    entries: Vec<NativeTranscriptEntry>,
    messages: Vec<NativeTranscriptMessage>,
    bytes: usize,
}

fn transcript_cache() -> &'static Mutex<HashMap<PathBuf, CachedTranscript>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, CachedTranscript>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

pub fn load_native_transcript(
    session: &AgentSessionRef,
) -> Result<(PathBuf, Vec<NativeTranscriptMessage>), AgentRunnerError> {
    let path = resolve_native_session_path(session)?;
    let metadata = fs::metadata(&path).map_err(|source| AgentRunnerError::Io { source })?;
    let cache_key = path.clone();
    let modified = metadata.modified().ok();
    let identity = file_identity(&metadata);
    if let Ok(mut cache) = transcript_cache().lock() {
        if let Some(cached) = cache.get(&cache_key) {
            if cached.len == metadata.len()
                && cached.modified == modified
                && cached.identity == identity
            {
                return Ok((path, cached.messages.clone()));
            }
        }
        // Remove the entry before moving its records. Other readers must never
        // observe valid metadata paired with an emptied record vector.
        let cached = cache.remove(&cache_key);
        drop(cache);
        let can_append = cached.as_ref().is_some_and(|cached| {
            cached.identity == identity
                && metadata.len() > cached.parsed_len
                && file_prefix_fingerprint(&path, cached.parsed_len).ok()
                    == Some(cached.prefix_fingerprint)
        });
        let appendable = cached
            .filter(|_| can_append)
            .map(|cached| (cached.parsed_len, cached.entries));
        if let Some((parsed_len, mut entries)) = appendable {
            let parsed_len = append_native_entries(&path, parsed_len, &mut entries, false)?;
            let messages = transcript_branch(&entries)?;
            store_transcript_cache(
                cache_key,
                &metadata,
                modified,
                identity,
                parsed_len,
                entries,
                messages.clone(),
            );
            return Ok((path, messages));
        }
    }

    let mut entries = Vec::new();
    let parsed_len = append_native_entries(&path, 0, &mut entries, true)?;
    let messages = transcript_branch(&entries)?;
    store_transcript_cache(
        cache_key,
        &metadata,
        modified,
        identity,
        parsed_len,
        entries,
        messages.clone(),
    );
    Ok((path, messages))
}

pub fn load_external_transcript(
    kind: AgentKind,
    session: &AgentSessionRef,
) -> Result<(Option<PathBuf>, Vec<NativeTranscriptMessage>), AgentRunnerError> {
    match kind {
        AgentKind::Pi => Err(AgentRunnerError::Process(
            "Pi uses its native transcript loader".into(),
        )),
        AgentKind::Codex => {
            let path = resolve_codex_session_path(session)?;
            let contents =
                fs::read_to_string(&path).map_err(|source| AgentRunnerError::Io { source })?;
            Ok((Some(path), parse_codex_transcript(&contents)?))
        }
        AgentKind::Goose => Ok((None, parse_goose_transcript(&run_history_export(
            kind,
            session,
            &["session", "export", "--name", "--format", "json"],
        )?)?)),
        AgentKind::OpenCode => Ok((None, parse_opencode_transcript(&run_history_export(
            kind,
            session,
            &["export"],
        )?)?)),
    }
}

fn resolve_codex_session_path(session: &AgentSessionRef) -> Result<PathBuf, AgentRunnerError> {
    let id = session
        .id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| AgentRunnerError::Process("Codex session has no native identity".into()))?;
    let root = env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".codex")))
        .ok_or_else(|| AgentRunnerError::Process("unable to locate Codex home".into()))?;
    let mut directories = vec![root.join("sessions"), root.join("archived_sessions")];
    while let Some(directory) = directories.pop() {
        let Ok(entries) = fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries {
            let path = entry.map_err(|source| AgentRunnerError::Io { source })?.path();
            if path.is_dir() {
                directories.push(path);
                continue;
            }
            if path.extension().and_then(|extension| extension.to_str()) != Some("jsonl")
                || !path
                    .file_name()
                    .is_some_and(|name| name.to_string_lossy().contains(id))
            {
                continue;
            }
            return Ok(path);
        }
    }
    Err(AgentRunnerError::Process(format!(
        "Codex session {id} was not found under {}",
        root.display()
    )))
}

fn run_history_export(
    kind: AgentKind,
    session: &AgentSessionRef,
    args: &[&str],
) -> Result<String, AgentRunnerError> {
    let executable = discover_executable(kind)?;
    let mut command = Command::new(&executable);
    let mut resolved_args = args.iter().map(|arg| (*arg).to_string()).collect::<Vec<_>>();
    if kind == AgentKind::Goose {
        let id = session
            .id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| AgentRunnerError::Process("Goose session has no native identity".into()))?;
        if let Some(name) = resolved_args.iter().position(|arg| arg == "--name") {
            resolved_args.insert(name + 1, id.to_string());
        }
    } else if kind == AgentKind::OpenCode {
        let id = session
            .id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
            .ok_or_else(|| AgentRunnerError::Process("OpenCode session has no native identity".into()))?;
        resolved_args.push(id.to_string());
    }
    command.args(resolved_args);
    command.stdin(Stdio::null());
    if let Some(cwd) = session.cwd.as_deref().filter(|cwd| cwd.is_dir()) {
        command.current_dir(cwd);
    }
    let output = command
        .output()
        .map_err(|source| AgentRunnerError::Spawn {
            executable: Some(executable.clone()),
            source,
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr);
        return Err(AgentRunnerError::Process(format!(
            "{} history export exited with {}{}",
            kind.label(),
            output.status,
            if detail.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", detail.trim())
            }
        )));
    }
    if output.stdout.len() > MAX_STDOUT_BYTES {
        return Err(AgentRunnerError::Process(format!(
            "{} history export exceeded the {} byte limit",
            kind.label(),
            MAX_STDOUT_BYTES
        )));
    }
    String::from_utf8(output.stdout).map_err(|source| AgentRunnerError::Utf8 { source })
}

fn parse_codex_transcript(output: &str) -> Result<Vec<NativeTranscriptMessage>, AgentRunnerError> {
    let mut messages = Vec::new();
    for (line_number, line) in output.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line).map_err(|source| {
            AgentRunnerError::Parse(format!(
                "invalid Codex session JSON on line {}: {source}",
                line_number + 1
            ))
        })?;
        if event.get("type").and_then(Value::as_str) != Some("event_msg") {
            continue;
        }
        let payload = event.get("payload").unwrap_or(&Value::Null);
        if payload.get("type").and_then(Value::as_str) != Some("item_completed") {
            continue;
        }
        let item = payload.get("item").unwrap_or(&Value::Null);
        let item_type = item.get("type").and_then(Value::as_str);
        let (user, text) = match item_type {
            Some("UserMessage") => (true, extract_codex_item_text(item)),
            Some("AgentMessage")
                if item.get("phase").and_then(Value::as_str) == Some("final_answer") =>
            {
                (false, extract_codex_item_text(item))
            }
            _ => (false, None),
        };
        let Some(mut text) = text.filter(|text| !text.trim().is_empty()) else {
            continue;
        };
        if user {
            text = strip_legacy_desktopctl_context(&text).to_string();
        }
        messages.push(NativeTranscriptMessage {
            user,
            text,
            timestamp_ms: payload
                .get("completed_at_ms")
                .or_else(|| payload.get("started_at_ms"))
                .and_then(Value::as_u64)
                .unwrap_or(0),
        });
    }
    Ok(messages)
}

fn extract_codex_item_text(item: &Value) -> Option<String> {
    let content = item.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let text = content
        .as_array()?
        .iter()
        .filter(|piece| {
            matches!(
                piece.get("type").and_then(Value::as_str),
                Some("Text" | "text")
            )
        })
        .filter_map(|piece| piece.get("text").and_then(Value::as_str))
        .collect::<Vec<_>>()
        .join("");
    (!text.is_empty()).then_some(text)
}

fn parse_goose_transcript(output: &str) -> Result<Vec<NativeTranscriptMessage>, AgentRunnerError> {
    let value: Value = serde_json::from_str(output.trim())
        .map_err(|source| AgentRunnerError::Parse(format!("invalid Goose session JSON: {source}")))?;
    Ok(value
        .get("conversation")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| {
            message
                .pointer("/metadata/userVisible")
                .and_then(Value::as_bool)
                .unwrap_or(true)
        })
        .filter_map(|message| {
            let role = message.get("role").and_then(Value::as_str)?;
            let user = match role {
                "user" => true,
                "assistant" => false,
                _ => return None,
            };
            let text = message
                .get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            (!text.trim().is_empty()).then_some(NativeTranscriptMessage {
                user,
                text: if user {
                    strip_legacy_desktopctl_context(&text).to_string()
                } else {
                    text
                },
                timestamp_ms: message
                    .get("created")
                    .and_then(Value::as_u64)
                    .unwrap_or(0)
                    .saturating_mul(1000),
            })
        })
        .collect())
}

fn parse_opencode_transcript(output: &str) -> Result<Vec<NativeTranscriptMessage>, AgentRunnerError> {
    let value: Value = serde_json::from_str(output.trim()).map_err(|source| {
        AgentRunnerError::Parse(format!("invalid OpenCode session JSON: {source}"))
    })?;
    Ok(value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|message| {
            let info = message.get("info")?;
            let role = info.get("role").and_then(Value::as_str)?;
            let user = match role {
                "user" => true,
                "assistant" => false,
                _ => return None,
            };
            let text = message
                .get("parts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("");
            (!text.trim().is_empty()).then_some(NativeTranscriptMessage {
                user,
                text: if user {
                    strip_legacy_desktopctl_context(&text).to_string()
                } else {
                    text
                },
                timestamp_ms: info
                    .pointer("/time/created")
                    .and_then(Value::as_u64)
                    .unwrap_or(0),
            })
        })
        .collect())
}

fn append_native_entries(
    path: &Path,
    offset: u64,
    entries: &mut Vec<NativeTranscriptEntry>,
    allow_final_without_newline: bool,
) -> Result<u64, AgentRunnerError> {
    let mut file = fs::File::open(path).map_err(|source| AgentRunnerError::Io { source })?;
    file.seek(SeekFrom::Start(offset))
        .map_err(|source| AgentRunnerError::Io { source })?;
    let mut reader = BufReader::new(file);
    let mut line = Vec::new();
    let mut parsed_len = offset;
    let mut line_number = entries.len() + 1;
    loop {
        line.clear();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|source| AgentRunnerError::Io { source })?;
        if read == 0 {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            parse_native_entry_line(&line, line_number, entries)?;
            parsed_len += read as u64;
        } else if allow_final_without_newline
            && parse_native_entry_line(&line, line_number, entries).is_ok()
        {
            parsed_len += read as u64;
        } else {
            break;
        }
        line_number += 1;
    }
    Ok(parsed_len)
}

fn parse_native_entry_line(
    line: &[u8],
    line_number: usize,
    entries: &mut Vec<NativeTranscriptEntry>,
) -> Result<(), AgentRunnerError> {
    let line = std::str::from_utf8(line).map_err(|source| {
        AgentRunnerError::Parse(format!(
            "invalid Pi session JSON on line {line_number}: {source}"
        ))
    })?;
    if line.trim().is_empty() {
        return Ok(());
    }
    let value: Value = serde_json::from_str(line).map_err(|source| {
        AgentRunnerError::Parse(format!(
            "invalid Pi session JSON on line {line_number}: {source}"
        ))
    })?;
    if let Some(id) = value.get("id").and_then(Value::as_str) {
        entries.push(NativeTranscriptEntry {
            id: id.to_string(),
            parent_id: value
                .get("parentId")
                .and_then(Value::as_str)
                .map(str::to_string),
            message: native_message(&value),
        });
    }
    Ok(())
}

fn transcript_branch(
    entries: &[NativeTranscriptEntry],
) -> Result<Vec<NativeTranscriptMessage>, AgentRunnerError> {
    let Some(leaf) = entries.last().map(|entry| entry.id.as_str()) else {
        return Ok(Vec::new());
    };
    let by_id: HashMap<&str, &NativeTranscriptEntry> = entries
        .iter()
        .map(|entry| (entry.id.as_str(), entry))
        .collect();
    let mut branch = Vec::new();
    let mut visited = HashSet::new();
    let mut cursor = Some(leaf);
    while let Some(id) = cursor {
        if !visited.insert(id) {
            return Err(AgentRunnerError::Parse(format!(
                "Pi session transcript contains a parent cycle at {id}"
            )));
        }
        let Some(entry) = by_id.get(id) else { break };
        branch.push(*entry);
        cursor = entry.parent_id.as_deref();
    }
    branch.reverse();
    Ok(branch
        .into_iter()
        .filter_map(|entry| entry.message.clone())
        .collect())
}

fn store_transcript_cache(
    key: PathBuf,
    metadata: &fs::Metadata,
    modified: Option<std::time::SystemTime>,
    identity: Option<(u64, u64)>,
    parsed_len: u64,
    entries: Vec<NativeTranscriptEntry>,
    messages: Vec<NativeTranscriptMessage>,
) {
    let prefix_fingerprint = file_prefix_fingerprint(&key, parsed_len).unwrap_or_default();
    let bytes = entries
        .iter()
        .map(|entry| {
            std::mem::size_of::<NativeTranscriptEntry>()
                + entry.id.len()
                + entry.parent_id.as_deref().map_or(0, str::len)
                + entry
                    .message
                    .as_ref()
                    .map_or(0, |message| message.text.len())
        })
        .sum::<usize>()
        + messages
            .iter()
            .map(|message| std::mem::size_of::<NativeTranscriptMessage>() + message.text.len())
            .sum::<usize>()
        + key.to_string_lossy().len()
        + TRANSCRIPT_CACHE_ENTRY_OVERHEAD;
    if bytes > MAX_TRANSCRIPT_CACHE_BYTES {
        return;
    }
    if let Ok(mut cache) = transcript_cache().lock() {
        while (cache.len() >= MAX_TRANSCRIPT_CACHE_ENTRIES
            || cache.values().map(|entry| entry.bytes).sum::<usize>() + bytes
                > MAX_TRANSCRIPT_CACHE_BYTES)
            && !cache.is_empty()
        {
            if let Some(key) = cache.keys().next().cloned() {
                cache.remove(&key);
            }
        }
        cache.insert(
            key,
            CachedTranscript {
                len: metadata.len(),
                modified,
                identity,
                parsed_len,
                prefix_fingerprint,
                entries,
                messages,
                bytes,
            },
        );
    }
}

fn file_prefix_fingerprint(path: &Path, parsed_len: u64) -> Result<u64, AgentRunnerError> {
    let mut file = fs::File::open(path).map_err(|source| AgentRunnerError::Io { source })?;
    let sample_len = parsed_len.min(4096) as usize;
    let mut sample = vec![0u8; sample_len];
    file.read_exact(&mut sample)
        .map_err(|source| AgentRunnerError::Io { source })?;
    if parsed_len > 4096 {
        file.seek(SeekFrom::Start(parsed_len - 4096))
            .map_err(|source| AgentRunnerError::Io { source })?;
        let mut tail = vec![0u8; 4096];
        file.read_exact(&mut tail)
            .map_err(|source| AgentRunnerError::Io { source })?;
        sample.extend_from_slice(&tail);
    }
    let mut hash = 1469598103934665603u64;
    for byte in sample {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(1099511628211);
    }
    Ok(hash)
}

#[cfg(unix)]
fn file_identity(metadata: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn file_identity(_metadata: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

fn native_message(entry: &Value) -> Option<NativeTranscriptMessage> {
    if entry.get("type").and_then(Value::as_str) != Some("message") {
        return None;
    }
    let message = entry.get("message")?;
    let role = message.get("role")?.as_str()?;
    if role != "user" && role != "assistant" {
        return None;
    }
    if role == "assistant" {
        match message.get("stopReason").and_then(Value::as_str) {
            Some("stop" | "length") => {}
            _ => return None,
        }
    }
    let mut text = extract_message_text(message)?;
    if role == "user" {
        text = strip_legacy_desktopctl_context(&text).to_string();
    }
    if text.trim().is_empty() {
        return None;
    }
    Some(NativeTranscriptMessage {
        user: role == "user",
        text,
        timestamp_ms: message
            .get("timestamp")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    })
}

fn strip_legacy_desktopctl_context(text: &str) -> &str {
    text.split_once("\n\n[DesktopCtl target-window context]\n")
        .map(|(prompt, _)| prompt)
        .unwrap_or(text)
}

fn resolve_native_session_path(session: &AgentSessionRef) -> Result<PathBuf, AgentRunnerError> {
    if let Some(path) = session.path.as_ref().filter(|path| path.is_file()) {
        return Ok(path.clone());
    }
    let id = session
        .id
        .as_deref()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| AgentRunnerError::Process("Pi session has no native identity".into()))?;
    let root = env::var_os("PI_CODING_AGENT_SESSION_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            env::var_os("PI_CODING_AGENT_DIR")
                .map(PathBuf::from)
                .map(|dir| dir.join("sessions"))
        })
        .or_else(|| {
            env::var_os("HOME")
                .map(PathBuf::from)
                .map(|home| home.join(".pi/agent/sessions"))
        })
        .ok_or_else(|| AgentRunnerError::Process("unable to locate Pi session directory".into()))?;
    for directory in fs::read_dir(&root).map_err(|source| AgentRunnerError::Io { source })? {
        let directory = directory.map_err(|source| AgentRunnerError::Io { source })?;
        if !directory.path().is_dir() {
            continue;
        }
        for file in
            fs::read_dir(directory.path()).map_err(|source| AgentRunnerError::Io { source })?
        {
            let file = file.map_err(|source| AgentRunnerError::Io { source })?;
            let path = file.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
                continue;
            }
            let Ok(file) = fs::File::open(&path) else {
                continue;
            };
            let mut first_line = String::new();
            if BufReader::new(file).read_line(&mut first_line).is_ok()
                && serde_json::from_str::<Value>(&first_line)
                    .ok()
                    .and_then(|header| header.get("id").and_then(Value::as_str).map(str::to_string))
                    .as_deref()
                    == Some(id)
            {
                return Ok(path);
            }
        }
    }
    Err(AgentRunnerError::Process(format!(
        "Pi session {id} was not found under {}",
        root.display()
    )))
}

/// A small abstraction so another CLI adapter can be added without changing
/// launcher/session UI code.
pub trait AgentRunner: Send + Sync {
    fn spawn(&self, request: AgentRequest) -> Result<AgentProcess, AgentRunnerError>;
}

/// Resolve every supported CLI using both the GUI process PATH and the common
/// package-manager locations used on macOS.
pub fn discover_agent_installations() -> Vec<AgentInstallation> {
    AgentKind::ALL
        .into_iter()
        .filter_map(|kind| {
            discover_executable(kind)
                .ok()
                .map(|executable| AgentInstallation { kind, executable })
        })
        .collect()
}

fn discover_executable(kind: AgentKind) -> Result<PathBuf, AgentRunnerError> {
    let configured = env::var_os(kind.environment_override());
    let path = env::var_os("PATH");
    let home = env::var_os("HOME").map(PathBuf::from);
    resolve_executable(
        kind,
        configured.as_deref(),
        path.as_deref(),
        home.as_deref(),
    )
}

fn resolve_executable(
    kind: AgentKind,
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
                "{} was not found at {}={}; install {}, or configure {} to its executable",
                kind.label(),
                kind.environment_override(),
                configured.display(),
                kind.label(),
                kind.environment_override()
            ),
        });
    }

    let mut candidates = Vec::new();
    if let Some(path) = path {
        candidates.extend(env::split_paths(path).map(|dir| dir.join(kind.executable_name())));
    }
    candidates.extend([
        PathBuf::from("/opt/homebrew/bin").join(kind.executable_name()),
        PathBuf::from("/usr/local/bin").join(kind.executable_name()),
        PathBuf::from("/usr/bin").join(kind.executable_name()),
    ]);
    if let Some(home) = home {
        candidates.extend([
            home.join(".local/bin").join(kind.executable_name()),
            home.join(".npm-global/bin").join(kind.executable_name()),
            home.join(".bun/bin").join(kind.executable_name()),
            home.join(".volta/bin").join(kind.executable_name()),
            home.join(".asdf/shims").join(kind.executable_name()),
            home.join("bin").join(kind.executable_name()),
        ]);
    }
    candidates.dedup();
    candidates
        .into_iter()
        .find(|candidate| is_executable_file(candidate))
        .ok_or_else(|| AgentRunnerError::MissingExecutable {
            configured: None,
            message: format!(
                "{} executable not found. Install {}, or set {} to its full executable path",
                kind.label(),
                kind.label(),
                kind.environment_override()
            ),
        })
}

/// Pi command runner.  The optional executable and cwd are useful for tests
/// and for callers that expose explicit configuration.
#[derive(Debug, Clone, Default)]
pub struct PiRunner {
    executable: Option<PathBuf>,
    current_dir: Option<PathBuf>,
    timing: Option<Arc<crate::trace::E2eTiming>>,
}

impl PiRunner {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_executable(path: impl Into<PathBuf>) -> Self {
        Self {
            executable: Some(path.into()),
            current_dir: None,
            timing: None,
        }
    }

    pub fn with_current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    pub(crate) fn with_timing(mut self, timing: Arc<crate::trace::E2eTiming>) -> Self {
        self.timing = Some(timing);
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
        discover_executable(AgentKind::Pi)
    }

    /// Build the exact argv passed to Pi.  This is intentionally separate from
    /// process creation so callers/tests can verify no shell is involved.
    pub fn args_for(request: &AgentRequest) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("--mode"),
            OsString::from("json"),
            OsString::from("--print"),
        ];
        if request.read_only {
            args.splice(
                0..0,
                [
                    OsString::from("--tools"),
                    OsString::from("read,grep,find,ls"),
                ],
            );
        }
        if let Some(session) = request.session.as_ref().filter(|s| !s.is_empty()) {
            if let Some(path) = session.path.as_ref() {
                args.push(OsString::from("--session"));
                args.push(path.as_os_str().to_os_string());
            } else if let Some(id) = session.id.as_deref().filter(|id| !id.trim().is_empty()) {
                args.push(OsString::from("--session-id"));
                args.push(OsString::from(id));
            }
        }
        if let Some(target) = request.target_window.as_ref() {
            args.push(OsString::from("--append-system-prompt"));
            args.push(OsString::from(target_window_instruction(target)));
        }
        if let Some(context) = request.window_context.as_deref() {
            args.push(OsString::from("--append-system-prompt"));
            args.push(OsString::from(context));
        }
        // `--` protects prompts beginning with a dash while keeping user text
        // an argv element rather than shell source.
        args.push(OsString::from("--"));
        args.push(OsString::from(&request.prompt));
        crate::trace::agent_context(format!(
            "pi_args target_arg={} window_context_arg={} append_system_prompts={} session_arg={} prompt_bytes={}",
            request
                .target_window
                .as_ref()
                .map(|target| target.id.as_str())
                .unwrap_or("none"),
            request.window_context.is_some(),
            args.iter()
                .filter(|arg| *arg == "--append-system-prompt")
                .count(),
            request.session.is_some(),
            request.prompt.len()
        ));
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
        configure_process_group(&mut command);
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
        let process = AgentProcess::new(child, self.timing.clone());
        if let Some(timing) = process.timing.as_ref() {
            timing.mark("pi_spawned", "");
        }
        Ok(process)
    }
}

fn prompt_for_cli(request: &AgentRequest) -> String {
    let mut prompt = String::new();
    if let Some(target) = request.target_window.as_ref() {
        prompt.push_str(&target_window_instruction(target));
        prompt.push_str("\n\n");
    }
    if let Some(context) = request.window_context.as_deref() {
        prompt.push_str(context);
        prompt.push_str("\n\n");
    }
    prompt.push_str(&request.prompt);
    prompt
}

fn target_window_instruction(target: &TargetWindow) -> String {
    format!(
        "The launcher has already bound the target window as {id}. For every desktopctl command that supports a window target, put `--active-window {id}` after the subcommand and its arguments, never before the subcommand. When a detailed tokenized context file is provided, read that file first to inspect the window; do not call `desktopctl screen tokenize` just to rediscover the supplied snapshot. Use `desktopctl screen tokenize --active-window {id}` only when the file is unavailable or a fresh capture is explicitly needed. Example action: `desktopctl pointer click --id <element_id> --active-window {id}`. Do not probe `desktopctl --active-window ... --help`; that syntax is invalid. Use the bound window for this request even if another app becomes frontmost.",
        id = target.id
    )
}

#[derive(Debug, Clone)]
pub struct CodexRunner {
    executable: Option<PathBuf>,
    current_dir: Option<PathBuf>,
    timing: Option<Arc<crate::trace::E2eTiming>>,
}

impl Default for CodexRunner {
    fn default() -> Self {
        Self {
            executable: None,
            current_dir: None,
            timing: None,
        }
    }
}

impl CodexRunner {
    pub const MODEL: &'static str = "gpt-5.6-luna";

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_executable(path: impl Into<PathBuf>) -> Self {
        Self {
            executable: Some(path.into()),
            ..Self::default()
        }
    }

    pub fn with_current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    pub(crate) fn with_timing(mut self, timing: Arc<crate::trace::E2eTiming>) -> Self {
        self.timing = Some(timing);
        self
    }

    fn executable(&self) -> Result<PathBuf, AgentRunnerError> {
        self.executable
            .as_deref()
            .map(|path| {
                if is_executable_file(path) {
                    Ok(path.to_path_buf())
                } else {
                    Err(AgentRunnerError::MissingExecutable {
                        configured: Some(path.to_path_buf()),
                        message: format!(
                            "configured Codex executable is not executable: {}",
                            path.display()
                        ),
                    })
                }
            })
            .unwrap_or_else(|| discover_executable(AgentKind::Codex))
    }

    pub fn args_for(request: &AgentRequest) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("--ask-for-approval"),
            OsString::from("never"),
            OsString::from("--sandbox"),
            OsString::from(if request.read_only {
                "read-only"
            } else {
                "workspace-write"
            }),
            OsString::from("exec"),
            OsString::from("--json"),
            OsString::from("--model"),
            OsString::from(Self::MODEL),
            // Each DesktopCtl session runs in its own workspace directory,
            // which is not necessarily a Git checkout.
            OsString::from("--skip-git-repo-check"),
        ];
        if request
            .session
            .as_ref()
            .is_some_and(|session| !session.is_empty())
        {
            args.push(OsString::from("resume"));
            args.push(OsString::from(
                request
                    .session
                    .as_ref()
                    .and_then(|session| session.id.clone())
                    .unwrap_or_default(),
            ));
        }
        let prompt = if request.read_only {
            format!("{}\n\n{}", read_only_instruction(), prompt_for_cli(request))
        } else {
            prompt_for_cli(request)
        };
        args.push(OsString::from(prompt));
        args
    }

    fn command_for(&self, request: &AgentRequest) -> Result<Command, AgentRunnerError> {
        let executable = self.executable()?;
        let mut command = Command::new(&executable);
        command.args(Self::args_for(request));
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        if let Some(dir) = self.current_dir.as_deref() {
            command.current_dir(dir);
        }
        configure_process_group(&mut command);
        Ok(command)
    }
}

impl AgentRunner for CodexRunner {
    fn spawn(&self, request: AgentRequest) -> Result<AgentProcess, AgentRunnerError> {
        let executable = self.executable().ok();
        let mut command = self.command_for(&request)?;
        let child = command
            .spawn()
            .map_err(|source| AgentRunnerError::Spawn { executable, source })?;
        Ok(AgentProcess::new_with_parser(
            child,
            self.timing.clone(),
            parse_codex_output,
            None,
            AgentKind::Codex.label(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct GooseRunner {
    executable: Option<PathBuf>,
    current_dir: Option<PathBuf>,
    timing: Option<Arc<crate::trace::E2eTiming>>,
}

impl Default for GooseRunner {
    fn default() -> Self {
        Self {
            executable: None,
            current_dir: None,
            timing: None,
        }
    }
}

impl GooseRunner {
    pub const MODEL: &'static str = "deepseek/deepseek-v4-flash-0731";

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    pub(crate) fn with_timing(mut self, timing: Arc<crate::trace::E2eTiming>) -> Self {
        self.timing = Some(timing);
        self
    }

    fn executable(&self) -> Result<PathBuf, AgentRunnerError> {
        self.executable
            .as_deref()
            .map(|path| {
                if is_executable_file(path) {
                    Ok(path.to_path_buf())
                } else {
                    Err(AgentRunnerError::MissingExecutable {
                        configured: Some(path.to_path_buf()),
                        message: format!(
                            "configured Goose executable is not executable: {}",
                            path.display()
                        ),
                    })
                }
            })
            .unwrap_or_else(|| discover_executable(AgentKind::Goose))
    }

    fn session_name(request: &AgentRequest) -> String {
        request
            .session
            .as_ref()
            .and_then(|session| session.id.clone())
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| format!("desktopctl-{}", uuid::Uuid::now_v7()))
    }

    fn command_for(
        &self,
        request: &AgentRequest,
        session_name: &str,
    ) -> Result<Command, AgentRunnerError> {
        let executable = self.executable()?;
        let mut command = Command::new(&executable);
        command.args(Self::args_for(request, session_name));
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        if let Some(dir) = self.current_dir.as_deref() {
            command.current_dir(dir);
        }
        configure_process_group(&mut command);
        Ok(command)
    }

    pub fn args_for(request: &AgentRequest, session_name: &str) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("run"),
            OsString::from("--output-format"),
            OsString::from("json"),
            OsString::from("--provider"),
            OsString::from("openrouter"),
            OsString::from("--model"),
            OsString::from(Self::MODEL),
            OsString::from("--max-turns"),
            OsString::from("1000"),
            OsString::from("--quiet"),
            OsString::from("--name"),
            OsString::from(session_name),
        ];
        if request.read_only {
            args.extend([
                OsString::from("--system"),
                OsString::from(read_only_instruction()),
            ]);
        }
        if request
            .session
            .as_ref()
            .is_some_and(|session| !session.is_empty())
        {
            args.push(OsString::from("--resume"));
        }
        args.extend([
            OsString::from("--text"),
            OsString::from(prompt_for_cli(request)),
        ]);
        args
    }
}

impl AgentRunner for GooseRunner {
    fn spawn(&self, request: AgentRequest) -> Result<AgentProcess, AgentRunnerError> {
        let session_name = Self::session_name(&request);
        let executable = self.executable().ok();
        let mut command = self.command_for(&request, &session_name)?;
        let child = command
            .spawn()
            .map_err(|source| AgentRunnerError::Spawn { executable, source })?;
        Ok(AgentProcess::new_with_parser(
            child,
            self.timing.clone(),
            parse_goose_output,
            Some(AgentSessionRef::id(session_name)),
            AgentKind::Goose.label(),
        ))
    }
}

#[derive(Debug, Clone)]
pub struct OpenCodeRunner {
    executable: Option<PathBuf>,
    current_dir: Option<PathBuf>,
    timing: Option<Arc<crate::trace::E2eTiming>>,
}

impl Default for OpenCodeRunner {
    fn default() -> Self {
        Self {
            executable: None,
            current_dir: None,
            timing: None,
        }
    }
}

impl OpenCodeRunner {
    pub const MODEL: &'static str = "openrouter/deepseek/deepseek-v4-flash-0731";

    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_current_dir(mut self, path: impl Into<PathBuf>) -> Self {
        self.current_dir = Some(path.into());
        self
    }

    pub(crate) fn with_timing(mut self, timing: Arc<crate::trace::E2eTiming>) -> Self {
        self.timing = Some(timing);
        self
    }

    fn executable(&self) -> Result<PathBuf, AgentRunnerError> {
        self.executable
            .as_deref()
            .map(|path| {
                if is_executable_file(path) {
                    Ok(path.to_path_buf())
                } else {
                    Err(AgentRunnerError::MissingExecutable {
                        configured: Some(path.to_path_buf()),
                        message: format!(
                            "configured OpenCode executable is not executable: {}",
                            path.display()
                        ),
                    })
                }
            })
            .unwrap_or_else(|| discover_executable(AgentKind::OpenCode))
    }

    fn command_for(&self, request: &AgentRequest) -> Result<Command, AgentRunnerError> {
        let executable = self.executable()?;
        let mut command = Command::new(&executable);
        command.args(Self::args_for(request));
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        if let Some(dir) = self.current_dir.as_deref() {
            command.current_dir(dir);
        }
        configure_process_group(&mut command);
        Ok(command)
    }

    pub fn args_for(request: &AgentRequest) -> Vec<OsString> {
        let mut args = vec![
            OsString::from("run"),
            OsString::from("--format"),
            OsString::from("json"),
            OsString::from("--model"),
            OsString::from(Self::MODEL),
        ];
        if !request.read_only {
            args.push(OsString::from("--auto"));
        }
        if let Some(session) = request
            .session
            .as_ref()
            .filter(|session| !session.is_empty())
        {
            if let Some(id) = session.id.as_deref().filter(|id| !id.trim().is_empty()) {
                args.extend([OsString::from("--session"), OsString::from(id)]);
            }
        }
        let prompt = if request.read_only {
            format!("{}\n\n{}", read_only_instruction(), prompt_for_cli(request))
        } else {
            prompt_for_cli(request)
        };
        args.push(OsString::from(prompt));
        args
    }
}

fn read_only_instruction() -> &'static str {
    "Read-only mode is enabled. Inspect and explain the existing project, but do not create, edit, delete, rename, or otherwise modify files; do not run commands that modify project or system state."
}

impl AgentRunner for OpenCodeRunner {
    fn spawn(&self, request: AgentRequest) -> Result<AgentProcess, AgentRunnerError> {
        let executable = self.executable().ok();
        let mut command = self.command_for(&request)?;
        let child = command
            .spawn()
            .map_err(|source| AgentRunnerError::Spawn { executable, source })?;
        Ok(AgentProcess::new_with_parser(
            child,
            self.timing.clone(),
            parse_opencode_output,
            None,
            AgentKind::OpenCode.label(),
        ))
    }
}

/// A running adapter process.  `wait_with_cancellation` drains both output
/// streams while polling the child, allowing cancellation without leaving a
/// Pi process behind.
pub struct AgentProcess {
    child: Option<Child>,
    timing: Option<Arc<crate::trace::E2eTiming>>,
    parser: fn(&str) -> Result<AgentResult, AgentRunnerError>,
    session_hint: Option<AgentSessionRef>,
    label: &'static str,
}

impl fmt::Debug for AgentProcess {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentProcess")
            .field("running", &self.child.is_some())
            .finish()
    }
}

impl AgentProcess {
    fn new(child: Child, timing: Option<Arc<crate::trace::E2eTiming>>) -> Self {
        Self::new_with_parser(child, timing, parse_pi_output, None, AgentKind::Pi.label())
    }

    fn new_with_parser(
        child: Child,
        timing: Option<Arc<crate::trace::E2eTiming>>,
        parser: fn(&str) -> Result<AgentResult, AgentRunnerError>,
        session_hint: Option<AgentSessionRef>,
        label: &'static str,
    ) -> Self {
        Self {
            child: Some(child),
            timing: timing.filter(|_| crate::trace::enabled()),
            parser,
            session_hint,
            label,
        }
    }

    pub fn wait(mut self) -> Result<AgentResult, AgentRunnerError> {
        let never_cancelled = AtomicBool::new(false);
        self.wait_with_cancellation(&never_cancelled)
    }

    pub fn wait_with_cancellation(
        &mut self,
        cancellation: &AtomicBool,
    ) -> Result<AgentResult, AgentRunnerError> {
        self.wait_with_cancellation_and_completion(cancellation, |_| {})
    }

    pub fn wait_with_cancellation_and_completion(
        &mut self,
        cancellation: &AtomicBool,
        mut on_completion: impl FnMut(AgentResult),
    ) -> Result<AgentResult, AgentRunnerError> {
        let mut child = self.child.take().ok_or_else(|| {
            AgentRunnerError::Process("process was already waited or cancelled".into())
        })?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| AgentRunnerError::Process("agent stdout pipe was unavailable".into()))?;
        let stderr = child
            .stderr
            .take()
            .ok_or_else(|| AgentRunnerError::Process("agent stderr pipe was unavailable".into()))?;
        let stop_readers = Arc::new(AtomicBool::new(false));
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let (stdout_rx, stdout_thread) = spawn_pipe_reader(
            stdout,
            MAX_STDOUT_BYTES,
            "stdout",
            Arc::clone(&stop_readers),
            self.timing.clone(),
            Some(completion_tx),
        );
        let (stderr_rx, stderr_thread) = spawn_pipe_reader(
            stderr,
            MAX_STDERR_BYTES,
            "stderr",
            Arc::clone(&stop_readers),
            None,
            None,
        );

        let status = loop {
            if cancellation.load(Ordering::Acquire) {
                stop_readers.store(true, Ordering::Release);
                terminate_process_group(&mut child);
                let _ = child.wait();
                let _ = collect_pipe(stdout_rx, stdout_thread);
                let _ = collect_pipe(stderr_rx, stderr_thread);
                return Err(AgentRunnerError::Cancelled);
            }
            if let Ok(result) = completion_rx.try_recv() {
                on_completion(result);
            }
            match child.try_wait() {
                Ok(Some(status)) => {
                    if let Ok(result) = completion_rx.try_recv() {
                        on_completion(result);
                    }
                    if let Some(timing) = self.timing.as_ref() {
                        timing.mark("pi_process_exit", "");
                    }
                    break status;
                }
                Ok(None) => thread::sleep(Duration::from_millis(20)),
                Err(source) => {
                    stop_readers.store(true, Ordering::Release);
                    terminate_process_group(&mut child);
                    let _ = child.wait();
                    let _ = collect_pipe(stdout_rx, stdout_thread);
                    let _ = collect_pipe(stderr_rx, stderr_thread);
                    return Err(AgentRunnerError::Wait { source });
                }
            }
        };

        // A child can exit while a descendant still owns a pipe. Kill the
        // process group before waiting for readers so completion is bounded.
        stop_readers.store(true, Ordering::Release);
        terminate_process_group(&mut child);
        let stdout = collect_pipe(stdout_rx, stdout_thread)?;
        let stderr = collect_pipe(stderr_rx, stderr_thread)?;
        if let Some(timing) = self.timing.as_ref() {
            timing.mark("pi_output_drained", "");
        }
        if !status.success() {
            let detail = stderr.trim();
            return Err(if detail.is_empty() {
                AgentRunnerError::Process(format!("{} exited with {status}", self.label))
            } else {
                AgentRunnerError::Process(format!("{} exited with {status}: {detail}", self.label))
            });
        }
        let mut result = (self.parser)(&stdout);
        if let (Ok(result), Some(hint)) = (&mut result, self.session_hint.as_ref()) {
            if result.session.id.as_deref().is_none_or(str::is_empty) {
                result.session.id = hint.id.clone();
            }
            if result.session.path.is_none() {
                result.session.path = hint.path.clone();
            }
            if result.session.cwd.is_none() {
                result.session.cwd = hint.cwd.clone();
            }
        }
        if let Some(timing) = self.timing.as_ref() {
            timing.mark("pi_return", "");
        }
        result
    }

    pub fn cancel(&mut self) -> Result<(), AgentRunnerError> {
        let Some(mut child) = self.child.take() else {
            return Ok(());
        };
        match child.try_wait() {
            Ok(Some(_)) => Ok(()),
            Ok(None) => {
                terminate_process_group(&mut child);
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
                terminate_process_group(&mut child);
                let _ = child.wait();
            }
        }
    }
}

trait PipeNonblocking {
    fn set_nonblocking(&self) -> io::Result<()>;
}

#[cfg(unix)]
impl PipeNonblocking for std::process::ChildStdout {
    fn set_nonblocking(&self) -> io::Result<()> {
        set_fd_nonblocking(self)
    }
}

#[cfg(unix)]
impl PipeNonblocking for std::process::ChildStderr {
    fn set_nonblocking(&self) -> io::Result<()> {
        set_fd_nonblocking(self)
    }
}

#[cfg(unix)]
fn set_fd_nonblocking<T: std::os::fd::AsRawFd>(pipe: &T) -> io::Result<()> {
    use std::os::fd::RawFd;
    let fd: RawFd = pipe.as_raw_fd();
    unsafe {
        let flags = libc::fcntl(fd, libc::F_GETFL);
        if flags == -1 {
            return Err(io::Error::last_os_error());
        }
        if libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) == -1 {
            return Err(io::Error::last_os_error());
        }
    }
    Ok(())
}

#[cfg(not(unix))]
impl<T> PipeNonblocking for T {
    fn set_nonblocking(&self) -> io::Result<()> {
        Ok(())
    }
}

fn spawn_pipe_reader<R: Read + PipeNonblocking + Send + 'static>(
    mut pipe: R,
    limit: usize,
    stream: &'static str,
    stop: Arc<AtomicBool>,
    timing: Option<Arc<crate::trace::E2eTiming>>,
    completion_sender: Option<SyncSender<AgentResult>>,
) -> (
    Receiver<Result<String, AgentRunnerError>>,
    thread::JoinHandle<()>,
) {
    let (sender, receiver) = mpsc::channel();
    let handle = thread::spawn(move || {
        if let Err(source) = pipe.set_nonblocking() {
            let _ = sender.send(Err(AgentRunnerError::Io { source }));
            return;
        }
        let mut bytes = Vec::with_capacity(limit.min(8192));
        let mut chunk = [0u8; 8192];
        let mut oversized = false;
        let mut event_line = Vec::new();
        let mut first_json = false;
        let mut first_text_delta = false;
        let mut assistant_complete = false;
        let mut stop_deadline = None;
        let mut completion_output = Vec::new();
        let mut completion_sent = false;
        let mut completion_prefix_valid = true;
        loop {
            if stop.load(Ordering::Acquire) {
                let deadline =
                    *stop_deadline.get_or_insert_with(|| Instant::now() + PIPE_DRAIN_TIMEOUT);
                if Instant::now() >= deadline {
                    break;
                }
            }
            match pipe.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => {
                    if timing.is_some() || completion_sender.is_some() {
                        for byte in &chunk[..count] {
                            event_line.push(*byte);
                            if *byte == b'\n' {
                                observe_timing_event(
                                    timing.as_ref(),
                                    &event_line,
                                    &mut first_json,
                                    &mut first_text_delta,
                                    &mut assistant_complete,
                                );
                                if completion_sender.is_some()
                                    && !completion_sent
                                    && completion_prefix_valid
                                {
                                    if completion_output.len().saturating_add(event_line.len())
                                        > limit
                                    {
                                        completion_prefix_valid = false;
                                    } else {
                                        completion_output.extend_from_slice(&event_line);
                                        let is_agent_end =
                                            serde_json::from_slice::<Value>(&event_line)
                                                .ok()
                                                .and_then(|event| {
                                                    event
                                                        .get("type")
                                                        .and_then(Value::as_str)
                                                        .map(str::to_owned)
                                                })
                                                .as_deref()
                                                == Some("agent_end");
                                        if is_agent_end {
                                            // agent_end is terminal. Do not repeatedly reparse
                                            // growing prefixes if a malformed producer repeats it.
                                            completion_prefix_valid = false;
                                            if let Ok(result) =
                                                is_valid_agent_end_completion(&completion_output)
                                            {
                                                completion_sent = true;
                                                if let Some(sender) = completion_sender.as_ref() {
                                                    let _ = sender.try_send(result);
                                                }
                                            }
                                        }
                                    }
                                }
                                event_line.clear();
                            } else if event_line.len() > 1024 * 1024 {
                                completion_prefix_valid = false;
                                event_line.clear();
                            }
                        }
                    }
                    let keep = if bytes.len() < limit {
                        let keep = count.min(limit - bytes.len());
                        bytes.extend_from_slice(&chunk[..keep]);
                        keep
                    } else {
                        0
                    };
                    oversized |= count > keep;
                }
                Err(source) if source.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5));
                }
                Err(source) => {
                    let _ = sender.send(Err(AgentRunnerError::Io { source }));
                    return;
                }
            }
        }
        if !event_line.is_empty() {
            observe_timing_event(
                timing.as_ref(),
                &event_line,
                &mut first_json,
                &mut first_text_delta,
                &mut assistant_complete,
            );
        }
        let result = if oversized {
            Err(AgentRunnerError::Process(format!(
                "Pi {stream} exceeded the {} byte limit",
                limit
            )))
        } else {
            String::from_utf8(bytes).map_err(|source| AgentRunnerError::Utf8 { source })
        };
        let _ = sender.send(result);
    });
    (receiver, handle)
}

fn collect_pipe(
    receiver: Receiver<Result<String, AgentRunnerError>>,
    handle: thread::JoinHandle<()>,
) -> Result<String, AgentRunnerError> {
    let result = receiver
        .recv_timeout(PIPE_DRAIN_TIMEOUT)
        .map_err(|_| AgentRunnerError::Process("timed out draining Pi output pipe".into()))?;
    handle
        .join()
        .map_err(|_| AgentRunnerError::Process("Pi output reader panicked".into()))?;
    result
}

fn observe_timing_event(
    timing: Option<&Arc<crate::trace::E2eTiming>>,
    line: &[u8],
    first_json: &mut bool,
    first_text_delta: &mut bool,
    assistant_complete: &mut bool,
) {
    let Ok(line) = std::str::from_utf8(line) else {
        return;
    };
    let Ok(event) = serde_json::from_str::<Value>(line.trim()) else {
        return;
    };
    let Some(timing) = timing else {
        return;
    };
    if !*first_json {
        *first_json = true;
        timing.mark("pi_first_stdout_json", "");
    }
    match event.get("type").and_then(Value::as_str) {
        Some("tool_execution_start") => timing.mark("pi_tool_start", ""),
        Some("tool_execution_end") => timing.mark("pi_tool_end", ""),
        Some("agent_end") => timing.mark("pi_agent_end", ""),
        _ => {}
    }
    if !*first_text_delta && is_assistant_text_delta(&event) {
        *first_text_delta = true;
        timing.mark("pi_first_assistant_text_delta", "");
    }
    if !*assistant_complete && is_assistant_completion(&event) {
        *assistant_complete = true;
        timing.mark("pi_assistant_complete", "");
    }
}

fn is_assistant_text_delta(event: &Value) -> bool {
    let event_type = event.get("type").and_then(Value::as_str);
    if event_type == Some("message_update") {
        return event
            .get("assistantMessageEvent")
            .and_then(|value| value.get("type"))
            .and_then(Value::as_str)
            == Some("text_delta")
            && event
                .get("assistantMessageEvent")
                .and_then(|value| value.get("delta"))
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty());
    }
    if matches!(event_type, Some("message_delta" | "text_delta")) {
        return event
            .get("delta")
            .or_else(|| event.get("text"))
            .and_then(Value::as_str)
            .is_some_and(|text| !text.is_empty());
    }
    if event_type == Some("message_end") {
        return event
            .get("message")
            .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
            .and_then(extract_message_text)
            .is_some_and(|text| !text.is_empty());
    }
    false
}

fn is_assistant_completion(event: &Value) -> bool {
    match event.get("type").and_then(Value::as_str) {
        Some("agent_end") => true,
        Some("message_end") => event.get("message").is_some_and(|message| {
            message.get("role").and_then(Value::as_str) == Some("assistant")
                && matches!(
                    message.get("stopReason").and_then(Value::as_str),
                    Some("stop" | "length")
                )
        }),
        _ => false,
    }
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == -1 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(unix)]
fn terminate_process_group(child: &mut Child) {
    unsafe {
        if libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) == -1 {
            let _ = child.kill();
        }
    }
}

#[cfg(not(unix))]
fn terminate_process_group(child: &mut Child) {
    let _ = child.kill();
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

pub fn parse_codex_output(output: &str) -> Result<AgentResult, AgentRunnerError> {
    let mut session_id = None;
    let mut final_answer = None;
    let mut saw_event = false;
    for (line_number, line) in output.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line).map_err(|source| {
            AgentRunnerError::Parse(format!(
                "invalid Codex JSON on line {}: {source}",
                line_number + 1
            ))
        })?;
        saw_event = true;
        if event.get("type").and_then(Value::as_str) == Some("thread.started") {
            session_id = event
                .get("thread_id")
                .and_then(Value::as_str)
                .map(str::to_owned);
        }
        if matches!(
            event.get("type").and_then(Value::as_str),
            Some("error" | "turn.failed")
        ) {
            return Err(AgentRunnerError::Parse(
                event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("message").and_then(Value::as_str))
                    .unwrap_or("Codex request failed")
                    .to_string(),
            ));
        }
        if event.get("type").and_then(Value::as_str) == Some("item.completed") {
            let item = event.get("item").unwrap_or(&Value::Null);
            if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                final_answer = item.get("text").and_then(Value::as_str).map(str::to_owned);
            }
        }
    }
    if !saw_event {
        return Err(AgentRunnerError::Parse(
            "Codex produced no JSON events".into(),
        ));
    }
    let final_answer = final_answer
        .filter(|answer| !answer.trim().is_empty())
        .ok_or_else(|| AgentRunnerError::Parse("Codex output contained no final answer".into()))?;
    Ok(AgentResult {
        session: AgentSessionRef::id(session_id.unwrap_or_default()),
        final_answer,
    })
}

pub fn parse_goose_output(output: &str) -> Result<AgentResult, AgentRunnerError> {
    let value: Value = serde_json::from_str(output.trim())
        .map_err(|source| AgentRunnerError::Parse(format!("invalid Goose JSON: {source}")))?;
    if value
        .get("metadata")
        .and_then(|metadata| metadata.get("status"))
        .and_then(Value::as_str)
        .is_some_and(|status| status != "completed")
    {
        return Err(AgentRunnerError::Parse(
            value
                .pointer("/metadata/status")
                .and_then(Value::as_str)
                .unwrap_or("Goose request failed")
                .to_string(),
        ));
    }
    let final_answer = value
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|message| message.get("role").and_then(Value::as_str) == Some("assistant"))
        .filter_map(|message| extract_message_text(message))
        .filter(|text| !text.trim().is_empty())
        .next_back()
        .ok_or_else(|| AgentRunnerError::Parse("Goose output contained no final answer".into()))?;
    Ok(AgentResult {
        session: AgentSessionRef {
            id: None,
            path: None,
            cwd: None,
        },
        final_answer,
    })
}

pub fn parse_opencode_output(output: &str) -> Result<AgentResult, AgentRunnerError> {
    let mut session_id = None;
    let mut final_answer = String::new();
    let mut saw_event = false;
    for (line_number, line) in output.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let event: Value = serde_json::from_str(line).map_err(|source| {
            AgentRunnerError::Parse(format!(
                "invalid OpenCode JSON on line {}: {source}",
                line_number + 1
            ))
        })?;
        saw_event = true;
        if let Some(id) = event.get("sessionID").and_then(Value::as_str) {
            session_id = Some(id.to_owned());
        }
        if event.get("type").and_then(Value::as_str) == Some("error") {
            return Err(AgentRunnerError::Parse(
                event
                    .pointer("/error/message")
                    .and_then(Value::as_str)
                    .or_else(|| event.get("message").and_then(Value::as_str))
                    .unwrap_or("OpenCode request failed")
                    .to_string(),
            ));
        }
        if event.get("type").and_then(Value::as_str) == Some("text") {
            if let Some(text) = event
                .get("text")
                .and_then(Value::as_str)
                .or_else(|| event.pointer("/part/text").and_then(Value::as_str))
            {
                final_answer.push_str(text);
            }
        }
    }
    if !saw_event {
        return Err(AgentRunnerError::Parse(
            "OpenCode produced no JSON events".into(),
        ));
    }
    if final_answer.trim().is_empty() {
        return Err(AgentRunnerError::Parse(
            "OpenCode output contained no final answer".into(),
        ));
    }
    Ok(AgentResult {
        session: AgentSessionRef::id(session_id.unwrap_or_default()),
        final_answer,
    })
}

/// Parse a completion only when the newly observed event is a valid, final
/// agent_end. In particular, do not surface an earlier message_end answer if
/// the agent_end reports an error, abort, or contains only tool messages.
fn is_valid_agent_end_completion(output: &[u8]) -> Result<AgentResult, AgentRunnerError> {
    let text =
        String::from_utf8(output.to_vec()).map_err(|source| AgentRunnerError::Utf8 { source })?;
    let event: Value = serde_json::from_str(
        text.lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .ok_or_else(|| AgentRunnerError::Parse("Pi produced no JSON events".into()))?,
    )
    .map_err(|source| AgentRunnerError::Parse(format!("invalid Pi JSON: {source}")))?;
    if event.get("type").and_then(Value::as_str) != Some("agent_end") {
        return Err(AgentRunnerError::Parse(
            "Pi output did not end with agent_end".into(),
        ));
    }
    let messages = event
        .get("messages")
        .and_then(Value::as_array)
        .ok_or_else(|| AgentRunnerError::Parse("Pi agent_end had no messages".into()))?;
    let mut final_assistant = None;
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("assistant") {
            continue;
        }
        if is_failed_message(message) {
            return Err(AgentRunnerError::Parse(
                message
                    .get("errorMessage")
                    .and_then(Value::as_str)
                    .unwrap_or("Pi assistant turn failed")
                    .to_string(),
            ));
        }
        final_assistant = Some(message);
    }
    let Some(final_assistant) = final_assistant else {
        return Err(AgentRunnerError::Parse(
            "Pi agent_end contained no final assistant answer".into(),
        ));
    };
    if extract_message_text(final_assistant).is_none_or(|text| text.trim().is_empty()) {
        return Err(AgentRunnerError::Parse(
            "Pi agent_end final assistant answer was empty".into(),
        ));
    }
    if !matches!(
        final_assistant.get("stopReason").and_then(Value::as_str),
        Some("stop" | "length")
    ) {
        return Err(AgentRunnerError::Parse(
            "Pi agent_end final assistant answer was not completed".into(),
        ));
    }
    let mut result = parse_pi_output(&text)?;
    result.final_answer = extract_message_text(final_assistant).expect("validated final text");
    Ok(result)
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
                "failed to start agent{}: {source}",
                executable
                    .as_ref()
                    .map(|path| format!(" ({})", path.display()))
                    .unwrap_or_default()
            ),
            Self::Io { source } => write!(f, "failed reading agent output: {source}"),
            Self::Utf8 { source } => write!(f, "agent output was not valid UTF-8: {source}"),
            Self::Wait { source } => write!(f, "failed waiting for agent: {source}"),
            Self::Kill { source } => write!(f, "failed cancelling agent: {source}"),
            Self::Parse(message) => write!(f, "invalid agent output: {message}"),
            Self::Process(message) => f.write_str(message),
            Self::Cancelled => f.write_str("agent request cancelled"),
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
    fn timing_event_extractors_ignore_payloads_and_detect_phases() {
        let delta: Value = serde_json::json!({
            "type": "message_update",
            "assistantMessageEvent": {"type": "text_delta", "delta": "hello"},
            "prompt": "must not be logged"
        });
        let complete: Value = serde_json::json!({
            "type": "message_end",
            "message": {"role": "assistant", "content": "done", "stopReason": "stop"}
        });
        let tool: Value = serde_json::json!({
            "type": "tool_execution_end",
            "result": {"secret": "must not be logged"}
        });
        let tool_message_end: Value = serde_json::json!({
            "type": "message_end",
            "message": {"role": "assistant", "stopReason": "toolUse", "content": []}
        });
        assert!(is_assistant_text_delta(&delta));
        assert!(is_assistant_completion(&complete));
        assert!(!is_assistant_text_delta(&tool));
        assert!(!is_assistant_completion(&tool));
        assert!(!is_assistant_completion(&tool_message_end));
    }

    #[test]
    fn malformed_json_is_rejected() {
        let error = parse_pi_output("{\"type\":\"session\"}\nnot-json\n").unwrap_err();
        assert!(matches!(error, AgentRunnerError::Parse(_)));
    }

    #[test]
    fn parses_codex_jsonl_agent_message() {
        let output = concat!(
            "{\"type\":\"thread.started\",\"thread_id\":\"codex-123\"}\n",
            "{\"type\":\"item.completed\",\"item\":{\"type\":\"agent_message\",\"text\":\"Hello\"}}\n",
            "{\"type\":\"turn.completed\"}\n",
        );
        let result = parse_codex_output(output).expect("valid Codex output");
        assert_eq!(result.session.id.as_deref(), Some("codex-123"));
        assert_eq!(result.final_answer, "Hello");
    }

    #[test]
    fn parses_codex_native_transcript_messages() {
        let output = concat!(
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\",\"item\":{\"type\":\"UserMessage\",\"content\":[{\"type\":\"text\",\"text\":\"question\"}]},\"completed_at_ms\":1000}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\",\"item\":{\"type\":\"AgentMessage\",\"phase\":\"commentary\",\"content\":[{\"type\":\"Text\",\"text\":\"hidden\"}]},\"completed_at_ms\":2001}}\n",
            "{\"type\":\"event_msg\",\"payload\":{\"type\":\"item_completed\",\"item\":{\"type\":\"AgentMessage\",\"phase\":\"final_answer\",\"content\":[{\"type\":\"Text\",\"text\":\"answer\"}]},\"completed_at_ms\":3000}}\n",
        );
        let messages = parse_codex_transcript(output).expect("valid Codex transcript");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "question");
        assert!(messages[0].user);
        assert_eq!(messages[0].timestamp_ms, 1000);
        assert_eq!(messages[1].text, "answer");
        assert!(!messages[1].user);
    }

    #[test]
    fn codex_args_allow_non_git_session_workspaces() {
        let args = CodexRunner::args_for(&AgentRequest::new("hello"));
        assert!(args.iter().any(|arg| arg == "--skip-git-repo-check"));
    }

    #[test]
    fn codex_args_use_read_only_sandbox_when_requested() {
        let mut request = AgentRequest::new("review");
        request.read_only = true;
        let args = CodexRunner::args_for(&request);
        assert!(
            args.windows(2).any(|pair| {
                pair == [OsString::from("--sandbox"), OsString::from("read-only")]
            })
        );
    }

    #[test]
    fn pi_args_limit_tools_in_read_only_mode() {
        let mut request = AgentRequest::new("review");
        request.read_only = true;
        let args = PiRunner::args_for(&request);
        assert!(args.windows(2).any(|pair| {
            pair == [
                OsString::from("--tools"),
                OsString::from("read,grep,find,ls"),
            ]
        }));
        assert!(
            !PiRunner::args_for(&AgentRequest::new("review"))
                .iter()
                .any(|arg| arg == "--tools")
        );
    }

    #[test]
    fn parses_goose_json_messages_and_ignores_thinking() {
        let output = r#"{"messages":[{"role":"assistant","content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":"Hello"}]}],"metadata":{"status":"completed"}}"#;
        let result = parse_goose_output(output).expect("valid Goose output");
        assert_eq!(result.final_answer, "Hello");
    }

    #[test]
    fn parses_goose_export_and_ignores_hidden_turn_context() {
        let output = r#"{"conversation":[{"role":"user","created":1,"metadata":{"userVisible":true},"content":[{"type":"text","text":"question"}]},{"role":"user","created":1,"metadata":{"userVisible":false},"content":[{"type":"text","text":"hidden"}]},{"role":"assistant","created":2,"metadata":{"userVisible":true},"content":[{"type":"thinking","thinking":"hidden"},{"type":"text","text":"answer"}]}]}"#;
        let messages = parse_goose_transcript(output).expect("valid Goose transcript");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "question");
        assert_eq!(messages[0].timestamp_ms, 1000);
        assert_eq!(messages[1].text, "answer");
        assert!(!messages[1].user);
    }

    #[test]
    fn goose_args_resume_named_session_with_requested_model() {
        let mut request = AgentRequest::new("follow up");
        request.session = Some(AgentSessionRef::id("desktopctl-session"));
        let args = GooseRunner::args_for(&request, "desktopctl-session");
        assert!(args.windows(2).any(|pair| {
            pair == [
                OsString::from("--model"),
                OsString::from(GooseRunner::MODEL),
            ]
        }));
        assert!(args.windows(2).any(|pair| {
            pair == [
                OsString::from("--name"),
                OsString::from("desktopctl-session"),
            ]
        }));
        assert!(args.iter().any(|arg| arg == "--resume"));
        assert!(args.iter().any(|arg| arg == "--quiet"));
    }

    #[test]
    fn goose_args_include_read_only_system_instruction() {
        let mut request = AgentRequest::new("review");
        request.read_only = true;
        let args = GooseRunner::args_for(&request, "desktopctl-session");
        assert!(args.windows(2).any(|pair| {
            pair[0] == OsString::from("--system")
                && pair[1]
                    .to_string_lossy()
                    .contains("Read-only mode is enabled")
        }));
    }

    #[test]
    fn parses_opencode_json_events() {
        let output = concat!(
            "{\"type\":\"step_start\",\"sessionID\":\"opencode-123\"}\n",
            "{\"type\":\"text\",\"sessionID\":\"opencode-123\",\"part\":{\"text\":\"Hello\"}}\n",
            "{\"type\":\"step_finish\",\"sessionID\":\"opencode-123\"}\n",
        );
        let result = parse_opencode_output(output).expect("valid OpenCode output");
        assert_eq!(result.session.id.as_deref(), Some("opencode-123"));
        assert_eq!(result.final_answer, "Hello");
    }

    #[test]
    fn parses_opencode_export_text_parts() {
        let output = r#"{"messages":[{"info":{"role":"user","time":{"created":1000}},"parts":[{"type":"text","text":"question"}]},{"info":{"role":"assistant","time":{"created":2000}},"parts":[{"type":"reasoning","text":"hidden"},{"type":"text","text":"answer"}]}]}"#;
        let messages = parse_opencode_transcript(output).expect("valid OpenCode transcript");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].text, "question");
        assert_eq!(messages[1].text, "answer");
        assert_eq!(messages[1].timestamp_ms, 2000);
    }

    #[test]
    fn opencode_args_resume_with_openrouter_model() {
        let mut request = AgentRequest::new("follow up");
        request.session = Some(AgentSessionRef::id("opencode-session"));
        let args = OpenCodeRunner::args_for(&request);
        assert!(args.windows(2).any(|pair| {
            pair == [
                OsString::from("--model"),
                OsString::from(OpenCodeRunner::MODEL),
            ]
        }));
        assert!(args.windows(2).any(|pair| {
            pair == [
                OsString::from("--session"),
                OsString::from("opencode-session"),
            ]
        }));
        assert!(args.iter().any(|arg| arg == "--auto"));
    }

    #[test]
    fn opencode_args_do_not_auto_approve_read_only_requests() {
        let mut request = AgentRequest::new("review");
        request.read_only = true;
        let args = OpenCodeRunner::args_for(&request);
        assert!(!args.iter().any(|arg| arg == "--auto"));
        assert!(
            args.last()
                .is_some_and(|arg| arg.to_string_lossy().contains("Read-only mode is enabled"))
        );
    }

    #[test]
    fn failed_assistant_message_is_rejected() {
        let output = r#"{"type":"message_end","message":{"role":"assistant","content":[],"stopReason":"aborted"}}"#;
        assert!(parse_pi_output(output).is_err());
    }

    #[test]
    fn agent_end_completion_validation_rejects_non_final_answers() {
        let cases = [
            (
                "valid",
                r#"{"type":"agent_end","messages":[{"role":"assistant","content":"final","stopReason":"stop"}]}"#,
                true,
            ),
            (
                "error",
                r#"{"type":"agent_end","messages":[{"role":"assistant","content":"x","stopReason":"error"}]}"#,
                false,
            ),
            (
                "aborted",
                r#"{"type":"agent_end","messages":[{"role":"assistant","content":"x","stopReason":"aborted"}]}"#,
                false,
            ),
            (
                "tool use",
                r#"{"type":"agent_end","messages":[{"role":"assistant","content":"x","stopReason":"toolUse"}]}"#,
                false,
            ),
            (
                "empty",
                r#"{"type":"agent_end","messages":[{"role":"assistant","content":"","stopReason":"stop"}]}"#,
                false,
            ),
            ("malformed", "{not-json}", false),
        ];
        for (name, event, valid) in cases {
            assert_eq!(
                is_valid_agent_end_completion(event.as_bytes()).is_ok(),
                valid,
                "{name}"
            );
        }
    }

    #[test]
    fn agent_end_text_overrides_earlier_message_end() {
        let output = concat!(
            r#"{"type":"message_end","message":{"role":"assistant","content":"stale","stopReason":"stop"}}"#,
            "\n",
            r#"{"type":"agent_end","messages":[{"role":"assistant","content":"final","stopReason":"stop"}]}"#,
        );
        assert_eq!(
            is_valid_agent_end_completion(output.as_bytes())
                .unwrap()
                .final_answer,
            "final"
        );
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
                .contains("screen tokenize --active-window mail_abc123")
        );
        assert!(
            args[context_index + 1]
                .to_string_lossy()
                .contains("never before the subcommand")
        );
    }

    #[test]
    fn args_include_window_context_when_present() {
        let mut request = AgentRequest::new("summarize");
        request.target_window = Some(TargetWindow {
            id: "mail_cef8c8".into(),
            app: Some("Mail".into()),
            title: Some("Inbox".into()),
        });
        request.window_context = Some("Initial environment context ( JSON ): {\"os\":{}}".into());
        let args = PiRunner::args_for(&request);
        let prompt_flags = args
            .iter()
            .filter(|arg| *arg == "--append-system-prompt")
            .count();
        assert_eq!(prompt_flags, 2);
        assert!(args.iter().any(|arg| {
            arg.to_string_lossy()
                .contains("Initial environment context")
        }));
        assert_eq!(args.last(), Some(&OsString::from("summarize")));
    }

    #[test]
    fn args_omit_desktop_context_when_not_requested() {
        let request = AgentRequest::new("summarize");
        let args = PiRunner::args_for(&request);
        assert!(!args.iter().any(|arg| arg == "--append-system-prompt"));
        assert_eq!(args.last(), Some(&OsString::from("summarize")));
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

    #[test]
    fn configured_runner_applies_current_dir_to_command() {
        let directory = env::temp_dir().join(format!(
            "desktopctl-pi-runner-cwd-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&directory).expect("create cwd");

        let runner = PiRunner::with_executable(env::current_exe().expect("test executable"))
            .with_current_dir(&directory);
        let command = runner
            .command_for(&AgentRequest::new("summarize"))
            .expect("build command");
        assert_eq!(command.get_current_dir(), Some(directory.as_path()));

        let _ = fs::remove_dir(directory);
    }

    #[test]
    fn native_transcript_follows_active_branch_and_hides_internal_content() {
        let path = env::temp_dir().join(format!(
            "desktopctl-pi-session-test-{}.jsonl",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let jsonl = concat!(
            "{\"type\":\"session\",\"version\":3,\"id\":\"pi-native\",\"cwd\":\"/tmp\"}\n",
            "{\"type\":\"model_change\",\"id\":\"model\",\"parentId\":null}\n",
            "{\"type\":\"message\",\"id\":\"user\",\"parentId\":\"model\",\"message\":{\"role\":\"user\",\"content\":\"question\\n\\n[DesktopCtl target-window context]\\nlegacy details\",\"timestamp\":10}}\n",
            "{\"type\":\"message\",\"id\":\"tool-use\",\"parentId\":\"user\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"thinking\",\"thinking\":\"hidden\"},{\"type\":\"toolCall\",\"name\":\"bash\"}],\"stopReason\":\"toolUse\",\"timestamp\":11}}\n",
            "{\"type\":\"message\",\"id\":\"tool-result\",\"parentId\":\"tool-use\",\"message\":{\"role\":\"toolResult\",\"content\":\"hidden\",\"timestamp\":12}}\n",
            "{\"type\":\"message\",\"id\":\"answer\",\"parentId\":\"tool-result\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"final answer\"}],\"stopReason\":\"stop\",\"timestamp\":13}}\n"
        );
        fs::write(&path, jsonl).expect("write native session");

        let (_, messages) = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(
            messages,
            vec![
                NativeTranscriptMessage {
                    user: true,
                    text: "question".into(),
                    timestamp_ms: 10,
                },
                NativeTranscriptMessage {
                    user: false,
                    text: "final answer".into(),
                    timestamp_ms: 13,
                },
            ]
        );
    }

    #[test]
    fn native_transcript_rejects_parent_cycles() {
        let path = env::temp_dir().join(format!(
            "desktopctl-pi-session-cycle-test-{}.jsonl",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(
            &path,
            concat!(
                "{\"type\":\"session\",\"id\":\"pi-cycle\"}\n",
                "{\"type\":\"message\",\"id\":\"a\",\"parentId\":\"b\",\"message\":{\"role\":\"user\",\"content\":\"a\"}}\n",
                "{\"type\":\"message\",\"id\":\"b\",\"parentId\":\"a\",\"message\":{\"role\":\"assistant\",\"content\":\"b\",\"stopReason\":\"stop\"}}\n"
            ),
        )
        .expect("write native session");

        let error = load_native_transcript(&AgentSessionRef::path(&path)).unwrap_err();
        let _ = fs::remove_file(path);
        assert!(error.to_string().contains("parent cycle"));
    }

    #[test]
    fn native_transcript_cache_invalidates_when_file_changes() {
        let path = env::temp_dir().join(format!(
            "desktopctl-pi-session-cache-test-{}.jsonl",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let header = "{\"type\":\"session\",\"id\":\"pi-cache\"}\n";
        fs::write(
            &path,
            format!(
                "{header}{{\"type\":\"message\",\"id\":\"u\",\"message\":{{\"role\":\"user\",\"content\":\"first\"}}}}\n"
            ),
        )
        .expect("write native session");
        let (_, first) = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        assert_eq!(first[0].text, "first");

        fs::write(
            &path,
            format!(
                "{header}{{\"type\":\"message\",\"id\":\"u\",\"message\":{{\"role\":\"user\",\"content\":\"changed content\"}}}}\n"
            ),
        )
        .expect("rewrite native session");
        let (_, second) = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(second[0].text, "changed content");
    }

    #[test]
    fn native_transcript_append_updates_active_branch_incrementally() {
        let path = env::temp_dir().join(format!(
            "desktopctl-pi-session-append-test-{}.jsonl",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(
            &path,
            concat!(
                "{\"type\":\"session\",\"id\":\"pi-append\"}\n",
                "{\"type\":\"message\",\"id\":\"u1\",\"message\":{\"role\":\"user\",\"content\":\"first\"}}\n",
                "{\"type\":\"message\",\"id\":\"a1\",\"parentId\":\"u1\",\"message\":{\"role\":\"assistant\",\"content\":\"old\",\"stopReason\":\"stop\"}}\n"
            ),
        )
        .unwrap();
        let _ = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(
            file,
            "{{\"type\":\"message\",\"id\":\"a2\",\"parentId\":\"u1\",\"message\":{{\"role\":\"assistant\",\"content\":\"new branch\",\"stopReason\":\"stop\"}}}}"
        )
        .unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(4));
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                let barrier = barrier.clone();
                thread::spawn(move || {
                    barrier.wait();
                    let (_, messages) =
                        load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
                    assert_eq!(messages.len(), 2);
                    assert_eq!(messages[0].text, "first");
                    assert_eq!(messages[1].text, "new branch");
                })
            })
            .collect();
        for reader in readers {
            reader.join().unwrap();
        }
        let (_, messages) = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(
            messages
                .iter()
                .map(|message| message.text.as_str())
                .collect::<Vec<_>>(),
            ["first", "new branch"]
        );
    }

    #[test]
    fn native_transcript_waits_for_partial_tail_then_parses_completion() {
        let path = env::temp_dir().join(format!(
            "desktopctl-pi-session-partial-test-{}.jsonl",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, "{\"type\":\"session\",\"id\":\"pi-partial\"}\n{\"type\":\"message\",\"id\":\"u\",\"message\":{\"role\":\"user\",\"content\":\"").unwrap();
        let (_, before) = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        assert!(before.is_empty());
        use std::io::Write;
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        file.write_all(b"partial\"}}\n").unwrap();
        let (_, after) = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(after[0].text, "partial");
    }

    #[test]
    fn native_transcript_rebuilds_after_truncation() {
        let path = env::temp_dir().join(format!(
            "desktopctl-pi-session-truncate-test-{}.jsonl",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::write(&path, "{\"type\":\"session\",\"id\":\"pi-truncate\"}\n{\"type\":\"message\",\"id\":\"u\",\"message\":{\"role\":\"user\",\"content\":\"before\"}}\n").unwrap();
        let _ = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        fs::write(&path, "{\"type\":\"session\",\"id\":\"pi-truncate\"}\n{\"type\":\"message\",\"id\":\"u\",\"message\":{\"role\":\"user\",\"content\":\"after\"}}\n").unwrap();
        let (_, messages) = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(messages[0].text, "after");
    }

    #[cfg(unix)]
    #[test]
    fn native_transcript_rebuilds_after_atomic_replacement() {
        let path = env::temp_dir().join(format!(
            "desktopctl-pi-session-replace-test-{}.jsonl",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let replacement = path.with_extension("replacement.jsonl");
        fs::write(
            &path,
            "{\"type\":\"session\",\"id\":\"pi-replace\"}\n{\"type\":\"message\",\"id\":\"u\",\"message\":{\"role\":\"user\",\"content\":\"before\"}}\n",
        )
        .unwrap();
        let _ = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        fs::write(
            &replacement,
            "{\"type\":\"session\",\"id\":\"pi-replace\"}\n{\"type\":\"message\",\"id\":\"u\",\"message\":{\"role\":\"user\",\"content\":\"after replacement\"}}\n",
        )
        .unwrap();
        fs::rename(&replacement, &path).unwrap();
        let (_, messages) = load_native_transcript(&AgentSessionRef::path(&path)).unwrap();
        let _ = fs::remove_file(path);
        assert_eq!(messages[0].text, "after replacement");
    }

    #[cfg(unix)]
    #[test]
    fn pipe_reader_accepts_exact_limit_and_rejects_over_limit() {
        for (payload, expected_error) in [("abc", false), ("abcd", true)] {
            let mut child = Command::new("sh")
                .args(["-c", &format!("printf {payload}")])
                .stdout(Stdio::piped())
                .spawn()
                .expect("spawn shell");
            let stdout = child.stdout.take().expect("stdout pipe");
            let (receiver, reader) = spawn_pipe_reader(
                stdout,
                3,
                "stdout",
                Arc::new(AtomicBool::new(false)),
                None,
                None,
            );
            child.wait().expect("wait shell");
            let result = collect_pipe(receiver, reader);
            assert_eq!(result.is_err(), expected_error, "payload={payload}");
            if !expected_error {
                assert_eq!(result.unwrap(), payload);
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_kills_process_group_with_descendant() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & wait"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        configure_process_group(&mut command);
        let child = command.spawn().expect("spawn shell");
        let mut process = AgentProcess::new(child, None);
        let cancellation = AtomicBool::new(true);
        let started = std::time::Instant::now();
        let error = process
            .wait_with_cancellation(&cancellation)
            .expect_err("cancelled process");
        assert!(matches!(error, AgentRunnerError::Cancelled));
        assert!(started.elapsed() < PIPE_DRAIN_TIMEOUT + Duration::from_secs(1));
    }

    #[cfg(unix)]
    #[test]
    fn oversized_completion_prefix_never_publishes() {
        let mut child = Command::new("sh")
            .args(["-c", r#"printf '%s\n' '{"type":"agent_end","messages":[{"role":"assistant","content":"done","stopReason":"stop"}]}'"#])
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let (completion_tx, completion_rx) = mpsc::sync_channel(1);
        let (receiver, reader) = spawn_pipe_reader(
            child.stdout.take().unwrap(),
            16,
            "stdout",
            Arc::new(AtomicBool::new(false)),
            None,
            Some(completion_tx),
        );
        child.wait().unwrap();
        assert!(collect_pipe(receiver, reader).is_err());
        assert!(completion_rx.try_recv().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn completion_callback_arrives_before_delayed_process_exit_without_trace() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%s\\n' '{\"type\":\"session\",\"id\":\"early\"}' '{\"type\":\"agent_end\",\"messages\":[{\"role\":\"assistant\",\"content\":\"done\",\"stopReason\":\"stop\"}]}' '{\"type\":\"agent_end\",\"messages\":[{\"role\":\"assistant\",\"content\":\"done\",\"stopReason\":\"stop\"}]}' ; sleep 2"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        configure_process_group(&mut command);
        let child = command.spawn().expect("spawn shell");
        let mut process = AgentProcess::new(child, None);
        let callback = Arc::new(Mutex::new(Vec::new()));
        let observed = Arc::clone(&callback);
        let callback_at = Arc::new(Mutex::new(None));
        let observed_at = Arc::clone(&callback_at);
        let started = Instant::now();
        let result = process
            .wait_with_cancellation_and_completion(&AtomicBool::new(false), move |result| {
                observed.lock().unwrap().push(result);
                *observed_at.lock().unwrap() = Some(started.elapsed());
            })
            .expect("agent result");
        assert_eq!(result.final_answer, "done");
        assert_eq!(callback.lock().unwrap().len(), 1);
        assert!(callback_at.lock().unwrap().unwrap() < Duration::from_secs(1));
        assert!(started.elapsed() >= Duration::from_secs(2));
    }

    #[cfg(unix)]
    #[test]
    fn completion_callback_precedes_late_nonzero_exit() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%s\\n' '{\"type\":\"agent_end\",\"messages\":[{\"role\":\"assistant\",\"content\":\"done\",\"stopReason\":\"stop\"}]}' ; sleep 1; exit 7"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        configure_process_group(&mut command);
        let child = command.spawn().unwrap();
        let mut process = AgentProcess::new(child, None);
        let count = Arc::new(Mutex::new(0));
        let observed = Arc::clone(&count);
        let error = process
            .wait_with_cancellation_and_completion(&AtomicBool::new(false), move |_| {
                *observed.lock().unwrap() += 1;
            })
            .unwrap_err();
        assert!(matches!(error, AgentRunnerError::Process(_)));
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn completion_callback_can_cancel_running_process() {
        let mut command = Command::new("sh");
        command.args(["-c", "printf '%s\\n' '{\"type\":\"agent_end\",\"messages\":[{\"role\":\"assistant\",\"content\":\"done\",\"stopReason\":\"stop\"}]}' ; sleep 30"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        configure_process_group(&mut command);
        let child = command.spawn().unwrap();
        let mut process = AgentProcess::new(child, None);
        let cancellation = Arc::new(AtomicBool::new(false));
        let observed_cancel = Arc::clone(&cancellation);
        let count = Arc::new(Mutex::new(0));
        let observed_count = Arc::clone(&count);
        let error = process
            .wait_with_cancellation_and_completion(&cancellation, move |_| {
                *observed_count.lock().unwrap() += 1;
                observed_cancel.store(true, Ordering::Release);
            })
            .unwrap_err();
        assert!(matches!(error, AgentRunnerError::Cancelled));
        assert_eq!(*count.lock().unwrap(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn pre_cancelled_process_never_calls_completion() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        configure_process_group(&mut command);
        let child = command.spawn().unwrap();
        let mut process = AgentProcess::new(child, None);
        let count = Arc::new(Mutex::new(0));
        let observed = Arc::clone(&count);
        let cancellation = AtomicBool::new(true);
        assert!(matches!(
            process.wait_with_cancellation_and_completion(&cancellation, move |_| {
                *observed.lock().unwrap() += 1;
            }),
            Err(AgentRunnerError::Cancelled)
        ));
        assert_eq!(*count.lock().unwrap(), 0);
    }
}

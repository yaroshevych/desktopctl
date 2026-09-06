//! Renderer-neutral launcher presentation contract.
//!
//! Platform renderers consume immutable snapshots and emit explicit actions.
//! Keep platform APIs, service clients, agent runners, and threading out of this
//! module so future platform launchers can share the product-level contract.

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum SessionStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub status: SessionStatus,
    pub unread: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct TranscriptMessage {
    pub user: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum LauncherScreen {
    Launcher,
    Session {
        id: String,
        title: String,
        status: SessionStatus,
        terminal_available: bool,
        continue_label: String,
        messages: Vec<TranscriptMessage>,
        messages_from: usize,
        transcript_epoch: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LauncherSnapshot {
    /// Monotonic controller revision. Renderers discard older snapshots that
    /// arrive late from worker threads.
    pub revision: u64,
    pub screen: LauncherScreen,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_app: Option<String>,
    #[serde(default = "default_render_keyboard_shortcuts")]
    pub render_keyboard_shortcuts: bool,
    pub recent: Vec<SessionSummary>,
    pub all: Vec<SessionSummary>,
    pub history_total: usize,
}

impl Default for LauncherSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            screen: LauncherScreen::Launcher,
            active_app: None,
            render_keyboard_shortcuts: true,
            recent: Vec::new(),
            all: Vec::new(),
            history_total: 0,
        }
    }
}

#[allow(dead_code)]
fn default_render_keyboard_shortcuts() -> bool {
    true
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CompletionNotice {
    pub session_id: String,
    pub prompt: String,
    pub answer_preview: String,
    pub target_app: Option<String>,
    pub follow_up_shortcut: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum LauncherAction {
    ToggleRequested,
    Dismissed,
    OpenSettings,
    ReturnToLauncher,
    ExpandHistory,
    AcknowledgeTranscript {
        session_id: String,
        epoch: u64,
        count: usize,
    },
    ResetTranscript,
    NewRequest {
        prompt: String,
        share_context: bool,
    },
    FollowUp {
        session_id: String,
        prompt: String,
        share_context: bool,
    },
    OpenSession {
        session_id: String,
    },
    CancelSession {
        session_id: String,
    },
    OpenInTerminal {
        session_id: String,
    },
}

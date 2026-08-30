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
        messages: Vec<TranscriptMessage>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct LauncherSnapshot {
    /// Monotonic controller revision. Renderers discard older snapshots that
    /// arrive late from worker threads.
    pub revision: u64,
    pub screen: LauncherScreen,
    pub recent: Vec<SessionSummary>,
    pub all: Vec<SessionSummary>,
}

impl Default for LauncherSnapshot {
    fn default() -> Self {
        Self {
            revision: 0,
            screen: LauncherScreen::Launcher,
            recent: Vec::new(),
            all: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct CompletionNotice {
    pub title: String,
    pub answer_preview: String,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub enum LauncherAction {
    ToggleRequested,
    Dismissed,
    OpenSettings,
    ReturnToLauncher,
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
    OpenInGhostty {
        session_id: String,
    },
}

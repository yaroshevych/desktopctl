#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
mod controller {
    use std::{
        collections::HashMap,
        path::PathBuf,
        sync::{Arc, Mutex, OnceLock, atomic::AtomicBool},
        thread,
    };

    use crate::{
        agent_runner::{AgentRequest, AgentRunner, AgentSessionRef, PiRunner, TargetWindow},
        agent_sessions::{
            AgentSession, AgentSessionStatus, AgentSessionStore, SessionMessageRole,
            TargetWindowMetadata, truncate_one_line, unix_now_ms,
        },
        daemon, trace,
    };

    use super::macos::{
        CompletionNotice, LauncherAction, LauncherCallbacks, LauncherScreen, LauncherSnapshot,
        SessionStatus, SessionSummary, TranscriptMessage,
    };

    struct State {
        store: AgentSessionStore,
        pending_target: Option<TargetWindowMetadata>,
        open_session: Option<String>,
        cancellations: HashMap<String, Arc<AtomicBool>>,
    }

    static STATE: OnceLock<Arc<Mutex<State>>> = OnceLock::new();

    fn lock_state() -> Option<std::sync::MutexGuard<'static, State>> {
        let state = STATE.get()?;
        Some(match state.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        })
    }

    pub fn initialize() -> Result<(), desktop_core::error::AppError> {
        let (store, warning) = match AgentSessionStore::default_path() {
            Some(path) => AgentSessionStore::load_or_empty_at(path, unix_now_ms()),
            None => AgentSessionStore::load_or_empty_at(
                PathBuf::from("/tmp/desktopctl-agent-sessions.json"),
                unix_now_ms(),
            ),
        };
        if let Some(warning) = warning {
            trace::log(format!("agent_launcher:store_warning {warning}"));
        }
        let _ = STATE.set(Arc::new(Mutex::new(State {
            store,
            pending_target: None,
            open_session: None,
            cancellations: HashMap::new(),
        })));
        super::macos::initialize(LauncherCallbacks {
            on_action: Arc::new(handle_action),
        })?;
        refresh();
        Ok(())
    }

    pub fn toggle() {
        if super::macos::is_visible() {
            super::macos::hide();
            return;
        }
        let target = daemon::bind_active_window_for_agent_launcher()
            .map(target_metadata)
            .map_err(|error| trace::log(format!("agent_launcher:target_bind_warning {error}")))
            .ok();
        if let Some(mut state) = lock_state() {
            state.pending_target = target;
            state.open_session = None;
        }
        refresh();
        super::macos::show();
    }

    fn handle_action(action: LauncherAction) {
        match action {
            LauncherAction::ToggleRequested => toggle(),
            LauncherAction::ReturnToLauncher => {
                if let Some(mut state) = lock_state() {
                    state.open_session = None;
                }
                refresh();
            }
            LauncherAction::NewRequest { prompt } => start_new(prompt),
            LauncherAction::FollowUp { session_id, prompt } => follow_up(session_id, prompt),
            LauncherAction::OpenSession { session_id } => open_session(session_id),
        }
    }

    fn start_new(prompt: String) {
        let created = lock_state().and_then(|mut state| {
            let target = state.pending_target.take();
            match state
                .store
                .create_running(&prompt, target.clone(), unix_now_ms())
            {
                Ok((session_id, request_id)) => Some((session_id, request_id, target)),
                Err(error) => {
                    trace::log(format!("agent_launcher:create_error {error}"));
                    None
                }
            }
        });
        if let Some((session_id, request_id, target)) = created {
            refresh();
            run_pi(session_id, request_id, prompt, None, target);
        }
    }

    fn follow_up(session_id: String, prompt: String) {
        let request = lock_state().and_then(|mut state| {
            let session = state.store.get(&session_id)?.clone();
            match state
                .store
                .begin_request(&session_id, &prompt, unix_now_ms())
            {
                Ok(request_id) => Some((request_id, session)),
                Err(error) => {
                    trace::log(format!("agent_launcher:follow_up_error {error}"));
                    None
                }
            }
        });
        if let Some((request_id, session)) = request {
            let native = Some(AgentSessionRef {
                id: session.native_session_id,
                path: session.native_session_path.map(PathBuf::from),
            });
            run_pi(
                session_id,
                request_id,
                prompt,
                native,
                session.target_window,
            );
            refresh();
        }
    }

    fn open_session(session_id: String) {
        if let Some(mut state) = lock_state() {
            if let Err(error) = state.store.mark_visited(&session_id, unix_now_ms()) {
                trace::log(format!("agent_launcher:visit_error {error}"));
                return;
            }
            state.open_session = Some(session_id);
        }
        refresh();
    }

    fn run_pi(
        session_id: String,
        request_id: String,
        prompt: String,
        native_session: Option<AgentSessionRef>,
        target: Option<TargetWindowMetadata>,
    ) {
        let cancellation = Arc::new(AtomicBool::new(false));
        if let Some(mut state) = lock_state() {
            state
                .cancellations
                .insert(session_id.clone(), cancellation.clone());
        }
        thread::spawn(move || {
            let mut request = AgentRequest::new(prompt);
            request.session = native_session.filter(|session| {
                session.id.as_deref().is_some_and(|id| !id.is_empty()) || session.path.is_some()
            });
            request.target_window = target.as_ref().and_then(runner_target);
            let result = PiRunner::new()
                .spawn(request)
                .and_then(|mut process| process.wait_with_cancellation(&cancellation));
            finish_run(&session_id, &request_id, result);
        });
    }

    fn finish_run(
        session_id: &str,
        request_id: &str,
        result: Result<crate::agent_runner::AgentResult, crate::agent_runner::AgentRunnerError>,
    ) {
        let mut notice = None;
        if let Some(mut state) = lock_state() {
            state.cancellations.remove(session_id);
            match result {
                Ok(result) => {
                    let native_path = result
                        .session
                        .path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string());
                    if let Err(error) =
                        state
                            .store
                            .bind_native_session(session_id, result.session.id, native_path)
                    {
                        trace::log(format!("agent_launcher:native_session_error {error}"));
                    }
                    if let Err(error) = state.store.complete_request(
                        session_id,
                        request_id,
                        &result.final_answer,
                        unix_now_ms(),
                    ) {
                        trace::log(format!("agent_launcher:complete_error {error}"));
                    } else if let Some(session) = state.store.get(session_id) {
                        notice = Some(CompletionNotice {
                            title: session.title.clone(),
                            answer_preview: truncate_one_line(&result.final_answer, 120),
                        });
                    }
                }
                Err(crate::agent_runner::AgentRunnerError::Cancelled) => {
                    let _ = state
                        .store
                        .cancel_request(session_id, request_id, unix_now_ms());
                }
                Err(error) => {
                    let message = error.to_string();
                    let _ =
                        state
                            .store
                            .fail_request(session_id, request_id, &message, unix_now_ms());
                    if let Some(session) = state.store.get(session_id) {
                        notice = Some(CompletionNotice {
                            title: session.title.clone(),
                            answer_preview: truncate_one_line(&message, 120),
                        });
                    }
                }
            }
        }
        refresh();
        if !super::macos::is_visible() {
            if let Some(notice) = notice {
                super::macos::show_completion(notice);
            }
        }
    }

    #[allow(dead_code)]
    pub fn cancel(session_id: &str) {
        if let Some(state) = lock_state() {
            if let Some(cancellation) = state.cancellations.get(session_id) {
                cancellation.store(true, std::sync::atomic::Ordering::Release);
            }
        }
    }

    fn refresh() {
        let snapshot = lock_state().map(|state| snapshot(&state));
        if let Some(snapshot) = snapshot {
            super::macos::refresh(snapshot);
        }
    }

    fn snapshot(state: &State) -> LauncherSnapshot {
        let pinned = state
            .store
            .latest_completed_unvisited()
            .map(|session| session.id.clone());
        let mut recent: Vec<SessionSummary> = state
            .store
            .recent_default()
            .into_iter()
            .map(summary)
            .collect();
        if let Some(pinned) = pinned {
            if let Some(index) = recent.iter().position(|session| session.id == pinned) {
                let session = recent.remove(index);
                recent.insert(0, session);
            }
        }
        let screen = state
            .open_session
            .as_deref()
            .and_then(|id| state.store.get(id))
            .map(session_screen)
            .unwrap_or(LauncherScreen::Launcher);
        LauncherSnapshot { screen, recent }
    }

    fn summary(session: &AgentSession) -> SessionSummary {
        SessionSummary {
            id: session.id.clone(),
            title: session.title.clone(),
            preview: session
                .answer_preview(140)
                .or_else(|| session.error.clone())
                .unwrap_or_default(),
            status: match session.status {
                AgentSessionStatus::Running => SessionStatus::Running,
                AgentSessionStatus::Completed => SessionStatus::Completed,
                AgentSessionStatus::Failed => SessionStatus::Failed,
                AgentSessionStatus::Cancelled => SessionStatus::Cancelled,
            },
            unread: session.unread,
        }
    }

    fn session_screen(session: &AgentSession) -> LauncherScreen {
        LauncherScreen::Session {
            id: session.id.clone(),
            title: session.title.clone(),
            messages: session
                .messages
                .iter()
                .map(|message| TranscriptMessage {
                    user: message.role == SessionMessageRole::User,
                    text: message.text.clone(),
                })
                .collect(),
        }
    }

    fn target_metadata(window: crate::platform::windowing::WindowInfo) -> TargetWindowMetadata {
        TargetWindowMetadata {
            window_ref: window.window_ref,
            native_id: Some(window.id),
            pid: Some(window.pid),
            app: Some(window.app),
            title: Some(window.title),
        }
    }

    fn runner_target(target: &TargetWindowMetadata) -> Option<TargetWindow> {
        let id = target
            .window_ref
            .clone()
            .or_else(|| target.native_id.clone())?;
        Some(TargetWindow {
            id,
            app: target.app.clone(),
            title: target.title.clone(),
        })
    }
}

#[cfg(target_os = "macos")]
pub(crate) use controller::{initialize, toggle};

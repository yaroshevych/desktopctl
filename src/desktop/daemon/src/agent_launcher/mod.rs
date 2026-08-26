#[cfg(target_os = "macos")]
mod macos;

#[cfg(target_os = "macos")]
mod controller {
    use std::{
        collections::HashMap,
        path::{Path, PathBuf},
        sync::{Arc, Mutex, OnceLock, atomic::AtomicBool},
        thread,
    };

    use crate::{
        agent_runner::{
            AgentRequest, AgentRunner, AgentSessionRef, PiRunner, TargetWindow,
            discover_pi_executable, load_native_transcript,
        },
        agent_sessions::{
            AgentSession, AgentSessionStatus, AgentSessionStore, SessionMessage,
            SessionMessageRole, TargetWindowMetadata, truncate_one_line, unix_now_ms,
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
        restore_pid: Option<i64>,
        launch_generation: u64,
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
        let path = AgentSessionStore::default_path().ok_or_else(|| {
            desktop_core::error::AppError::backend_unavailable(
                "unable to resolve DesktopCtl workspace directory",
            )
        })?;
        let (store, warning) = AgentSessionStore::load_or_empty_at(path, unix_now_ms());
        if let Some(warning) = warning {
            trace::log(format!("agent_launcher:store_warning {warning}"));
        }
        let _ = STATE.set(Arc::new(Mutex::new(State {
            store,
            pending_target: None,
            restore_pid: None,
            launch_generation: 0,
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
        let target_hint = daemon::capture_active_window_for_agent_launcher();
        let generation = if let Some(mut state) = lock_state() {
            state.launch_generation = state.launch_generation.wrapping_add(1);
            state.pending_target = None;
            state.restore_pid = target_hint;
            state.open_session = None;
            state.launch_generation
        } else {
            return;
        };
        refresh();
        super::macos::show();

        let Some(pid) = target_hint else {
            return;
        };
        thread::spawn(move || {
            let target = daemon::resolve_agent_launcher_target(pid)
                .map(target_metadata)
                .map_err(|error| trace::log(format!("agent_launcher:target_bind_warning {error}")))
                .ok();
            if let Some(mut state) = lock_state() {
                if state.launch_generation == generation {
                    state.pending_target = target;
                }
            }
        });
    }

    fn handle_action(action: LauncherAction) {
        match action {
            LauncherAction::ToggleRequested => toggle(),
            LauncherAction::Dismissed => restore_focus(),
            LauncherAction::ReturnToLauncher => {
                if let Some(mut state) = lock_state() {
                    state.open_session = None;
                }
                refresh();
            }
            LauncherAction::NewRequest { prompt } => start_new(prompt),
            LauncherAction::FollowUp { session_id, prompt } => follow_up(session_id, prompt),
            LauncherAction::OpenSession { session_id } => open_session(session_id),
            LauncherAction::CancelSession { session_id } => cancel(&session_id),
            LauncherAction::OpenInGhostty { session_id } => open_in_ghostty(session_id),
        }
    }

    fn restore_focus() {
        let pid = lock_state().and_then(|mut state| state.restore_pid.take());
        if let Some(pid) = pid {
            let _ = crate::platform::apps::activate_pid_immediately(pid);
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
                cwd: session.native_session_cwd.map(PathBuf::from),
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
            state.open_session = Some(session_id.clone());
        }
        refresh();
        sync_native_session(session_id);
    }

    fn sync_native_session(session_id: String) {
        let native = lock_state().and_then(|state| {
            let session = state.store.get(&session_id)?;
            if session.native_session_id.is_none() && session.native_session_path.is_none() {
                return None;
            }
            Some(AgentSessionRef {
                id: session.native_session_id.clone(),
                path: session.native_session_path.as_deref().map(PathBuf::from),
                cwd: session.native_session_cwd.as_deref().map(PathBuf::from),
            })
        });
        let Some(native) = native else {
            return;
        };
        thread::spawn(move || match load_native_transcript(&native) {
            Ok((path, messages)) => {
                let messages = messages
                    .into_iter()
                    .map(|message| SessionMessage {
                        role: if message.user {
                            SessionMessageRole::User
                        } else {
                            SessionMessageRole::Assistant
                        },
                        text: message.text,
                        created_at_ms: message.timestamp_ms,
                    })
                    .collect();
                let changed = if let Some(mut state) = lock_state() {
                    match state.store.sync_native_transcript(
                        &session_id,
                        messages,
                        Some(path.to_string_lossy().into_owned()),
                    ) {
                        Ok(changed) => changed,
                        Err(error) => {
                            trace::log(format!("agent_launcher:native_sync_error {error}"));
                            false
                        }
                    }
                } else {
                    false
                };
                if changed {
                    refresh();
                }
            }
            Err(error) => trace::log(format!("agent_launcher:native_read_error {error}")),
        });
    }

    fn open_in_ghostty(session_id: String) {
        let session = lock_state().and_then(|state| state.store.get(&session_id).cloned());
        let Some(session) = session else {
            return;
        };
        if session.status == AgentSessionStatus::Running {
            return;
        }
        let native_session = session
            .native_session_path
            .clone()
            .or(session.native_session_id.clone());
        let Some(native_session) = native_session else {
            return;
        };
        thread::spawn(move || {
            let result = (|| -> Result<(), String> {
                let pi = discover_pi_executable().map_err(|error| error.to_string())?;
                let cwd = session
                    .native_session_cwd
                    .as_deref()
                    .map(PathBuf::from)
                    .or_else(|| std::env::current_dir().ok())
                    .unwrap_or_else(|| PathBuf::from("/"));
                let command = ghostty_command(&pi, &native_session);
                let script = r#"on run argv
set commandText to item 1 of argv
set cwdText to item 2 of argv
tell application "Ghostty"
    activate
    make new window with configuration {command:commandText, initial working directory:cwdText, wait after command:true}
end tell
end run"#;
                let output = std::process::Command::new("/usr/bin/osascript")
                    .args(["-e", script, "--", &command, &cwd.to_string_lossy()])
                    .output()
                    .map_err(|error| format!("failed to open Ghostty: {error}"))?;
                if output.status.success() {
                    Ok(())
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    Err(format!(
                        "Ghostty could not open the Pi session: {}",
                        stderr.trim()
                    ))
                }
            })();
            if let Err(error) = result {
                super::macos::show_completion(CompletionNotice {
                    title: session.title,
                    answer_preview: truncate_one_line(&error, 120),
                });
            }
        });
    }

    fn posix_quote(value: &str) -> String {
        format!("'{}'", value.replace('\'', "'\\''"))
    }

    fn ghostty_command(pi: &Path, native_session: &str) -> String {
        let mut paths = pi
            .parent()
            .map(PathBuf::from)
            .into_iter()
            .collect::<Vec<_>>();
        if let Some(current_path) = std::env::var_os("PATH") {
            paths.extend(std::env::split_paths(&current_path));
        }
        let path = std::env::join_paths(paths).unwrap_or_else(|_| "/usr/bin:/bin".into());
        format!(
            "/usr/bin/env {} {} --session {}",
            posix_quote(&format!("PATH={}", path.to_string_lossy())),
            posix_quote(&pi.to_string_lossy()),
            posix_quote(native_session)
        )
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
        crate::app_runtime::set_agent_running(true);
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
            crate::app_runtime::set_agent_running(!state.cancellations.is_empty());
            match result {
                Ok(result) => {
                    let native_path = result
                        .session
                        .path
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string());
                    let native_cwd = result
                        .session
                        .cwd
                        .as_ref()
                        .map(|path| path.to_string_lossy().to_string());
                    if let Err(error) = state.store.bind_native_session(
                        session_id,
                        result.session.id,
                        native_path,
                        native_cwd,
                    ) {
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
        const RECENT_WINDOW_MS: u64 = 30 * 60 * 1_000;
        let cutoff = unix_now_ms().saturating_sub(RECENT_WINDOW_MS);
        let pinned = state
            .store
            .latest_completed_unvisited()
            .map(|session| session.id.clone());
        let all: Vec<SessionSummary> = state
            .store
            .recent(usize::MAX)
            .into_iter()
            .map(summary)
            .collect();
        let mut recent: Vec<SessionSummary> = state
            .store
            .recent(usize::MAX)
            .into_iter()
            .filter(|session| session.updated_at_ms >= cutoff)
            .take(3)
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
        LauncherSnapshot {
            screen,
            recent,
            all,
        }
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
            status: match session.status {
                AgentSessionStatus::Running => SessionStatus::Running,
                AgentSessionStatus::Completed => SessionStatus::Completed,
                AgentSessionStatus::Failed => SessionStatus::Failed,
                AgentSessionStatus::Cancelled => SessionStatus::Cancelled,
            },
            terminal_available: session.status != AgentSessionStatus::Running
                && (session.native_session_path.is_some() || session.native_session_id.is_some()),
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

    #[cfg(test)]
    mod tests {
        use super::{ghostty_command, posix_quote};

        #[test]
        fn ghostty_arguments_are_posix_quoted() {
            assert_eq!(posix_quote("simple"), "'simple'");
            assert_eq!(posix_quote("a b'c"), "'a b'\\''c'");
        }

        #[test]
        fn ghostty_command_does_not_include_exec() {
            let command = ghostty_command(
                std::path::Path::new("/opt/homebrew/bin/pi"),
                "/tmp/session file.jsonl",
            );
            assert!(command.starts_with("/usr/bin/env 'PATH=/opt/homebrew/bin:"));
            assert!(command.ends_with(
                "' '/opt/homebrew/bin/pi' --session '/tmp/session file.jsonl'"
            ));
            assert!(!command.contains(" exec "));
        }
    }
}

#[cfg(target_os = "macos")]
pub(crate) use controller::{initialize, toggle};

use std::{
    sync::atomic::{AtomicBool, Ordering},
    thread,
    time::Duration,
};

use desktop_core::protocol::Command;

use super::RequestContext;
#[cfg(target_os = "macos")]
use crate::overlay;
use crate::{daemon::window_target, trace};

const OVERLAY_WATCH_TRACK_INTERVAL_MS: u64 = 40;
const OVERLAY_SCREEN_CAPTURE_MODE_LOCK_MS: u64 = 2_000;
const PRIVACY_OVERLAY_STOP_DELAY_MS: u64 = 2_200;
static OVERLAY_WATCH_TRACK_RUNNING: AtomicBool = AtomicBool::new(false);
#[cfg(target_os = "macos")]
static JOURNAL_PRIVACY_GLOW_SHOWN: AtomicBool = AtomicBool::new(false);

#[derive(Debug, Clone, Copy)]
pub(super) struct CommandRuntimeState {
    pub overlay_token_updates_enabled: bool,
    transient_overlay_started: bool,
}

#[cfg(target_os = "macos")]
pub(super) fn bootstrap_overlay_glow() {
    trace::log("overlay:bootstrap ready");
    start_overlay_watch_tracker();
    if overlay::is_active() {
        let (mode, bounds) = if let Some(bounds) = window_target::frontmost_window_bounds() {
            (overlay::WatchMode::WindowMode, Some(bounds))
        } else {
            (overlay::WatchMode::DesktopMode, None)
        };
        if let Err(err) = overlay::watch_mode_changed(mode, bounds) {
            trace::log(format!("overlay:bootstrap mode_warn {err}"));
        }
        if let Err(err) = overlay::confidence_changed(1.0) {
            trace::log(format!("overlay:bootstrap confidence_warn {err}"));
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn bootstrap_overlay_glow() {}

#[cfg(target_os = "macos")]
fn start_overlay_watch_tracker() {
    if OVERLAY_WATCH_TRACK_RUNNING
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        return;
    }
    thread::spawn(|| {
        trace::log("overlay:watch_tracker start");
        loop {
            if overlay::is_active() {
                if !overlay::is_agent_active() && !overlay::is_watch_mode_locked() {
                    if let Some(bounds) = window_target::frontmost_window_bounds() {
                        let _ = overlay::watch_mode_changed(
                            overlay::WatchMode::WindowMode,
                            Some(bounds),
                        );
                    } else {
                        let _ = overlay::watch_mode_changed(overlay::WatchMode::DesktopMode, None);
                    }
                }
            }
            thread::sleep(Duration::from_millis(OVERLAY_WATCH_TRACK_INTERVAL_MS));
        }
    });
}

#[cfg(target_os = "macos")]
pub(super) fn command_requires_frontmost_snapshot(command: &Command) -> bool {
    if matches!(command, Command::ScreenTokenize { .. }) {
        // Tokenize resolves the active window in its own execution path.
        // Avoid expensive pre-execute frontmost snapshot here.
        return false;
    }
    command_requires_privacy_signal(command)
}

#[cfg(not(target_os = "macos"))]
pub(super) fn command_requires_frontmost_snapshot(_command: &Command) -> bool {
    false
}

pub(super) fn request_frontmost_bounds(
    context: &RequestContext,
) -> Option<desktop_core::protocol::Bounds> {
    context
        .frontmost
        .as_ref()
        .and_then(|snapshot| snapshot.bounds.clone())
        .or_else(window_target::frontmost_window_bounds)
}

pub(super) fn request_frontmost_app(context: &RequestContext) -> Option<String> {
    context
        .frontmost
        .as_ref()
        .and_then(|snapshot| snapshot.app.clone())
        .or_else(window_target::frontmost_app_name)
}

#[cfg(target_os = "macos")]
pub(super) fn begin_command(command: &Command, context: &RequestContext) -> CommandRuntimeState {
    let transient_overlay_started = maybe_start_privacy_overlay(command, context);
    let overlay_token_updates_enabled =
        !transient_overlay_started && !super::PRIVACY_OVERLAY_ACTIVE.load(Ordering::SeqCst);

    if matches!(
        command,
        Command::ScreenCapture {
            active_window: false,
            ..
        }
    ) {
        let _ = overlay::lock_watch_mode(
            overlay::WatchMode::DesktopMode,
            None,
            Duration::from_millis(OVERLAY_SCREEN_CAPTURE_MODE_LOCK_MS),
        );
    } else if !matches!(command, Command::ScreenTokenize { .. }) {
        if let Some(bounds) = request_frontmost_bounds(context) {
            let _ = overlay::watch_mode_changed(overlay::WatchMode::WindowMode, Some(bounds));
        }
    }
    let _ = overlay::agent_active_changed(true);

    CommandRuntimeState {
        overlay_token_updates_enabled,
        transient_overlay_started,
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn begin_command(_command: &Command, _context: &RequestContext) -> CommandRuntimeState {
    CommandRuntimeState {
        overlay_token_updates_enabled: true,
        transient_overlay_started: false,
    }
}

#[cfg(target_os = "macos")]
pub(super) fn end_command(state: CommandRuntimeState) {
    let _ = overlay::agent_active_changed(false);
    if state.transient_overlay_started {
        schedule_transient_overlay_stop();
    }
}

#[cfg(not(target_os = "macos"))]
pub(super) fn end_command(_state: CommandRuntimeState) {}

#[cfg(target_os = "macos")]
fn maybe_start_privacy_overlay(command: &Command, context: &RequestContext) -> bool {
    let is_journal_tokenize = matches!(command, Command::ScreenTokenize { journal: true, .. });
    let should_show_journal_glow = if is_journal_tokenize {
        JOURNAL_PRIVACY_GLOW_SHOWN
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    } else {
        false
    };
    if is_journal_tokenize && !should_show_journal_glow {
        return false;
    }
    if !command_requires_privacy_signal(command) {
        return false;
    }
    if overlay::is_active() {
        if is_journal_tokenize {
            let _ = overlay::watch_mode_changed(overlay::WatchMode::DesktopMode, None);
        }
        return false;
    }
    match overlay::start_overlay() {
        Ok(started) => {
            if started {
                super::PRIVACY_OVERLAY_ACTIVE.store(true, Ordering::SeqCst);
                if is_journal_tokenize {
                    if let Err(err) =
                        overlay::watch_mode_changed(overlay::WatchMode::DesktopMode, None)
                    {
                        trace::log(format!(
                            "overlay:privacy_auto_start mode_warn command={} err={err}",
                            command.name()
                        ));
                    }
                } else if matches!(command, Command::ScreenTokenize { .. }) {
                    let command_name = command.name().to_string();
                    let context = context.clone();
                    thread::spawn(move || {
                        let (mode, bounds) =
                            if let Some(bounds) = request_frontmost_bounds(&context) {
                                (overlay::WatchMode::WindowMode, Some(bounds))
                            } else {
                                (overlay::WatchMode::DesktopMode, None)
                            };
                        if let Err(err) = overlay::watch_mode_changed(mode, bounds) {
                            trace::log(format!(
                                "overlay:privacy_auto_start mode_warn command={} err={err}",
                                command_name
                            ));
                        }
                    });
                } else {
                    let (mode, bounds) = if let Some(bounds) = request_frontmost_bounds(context) {
                        (overlay::WatchMode::WindowMode, Some(bounds))
                    } else {
                        (overlay::WatchMode::DesktopMode, None)
                    };
                    if let Err(err) = overlay::watch_mode_changed(mode, bounds) {
                        trace::log(format!(
                            "overlay:privacy_auto_start mode_warn command={} err={err}",
                            command.name()
                        ));
                    }
                }
            }
            trace::log(format!(
                "overlay:privacy_auto_start command={} started={started}",
                command.name()
            ));
            started
        }
        Err(err) => {
            trace::log(format!(
                "overlay:privacy_auto_start_warn command={} err={err}",
                command.name()
            ));
            false
        }
    }
}

#[cfg(target_os = "macos")]
fn schedule_transient_overlay_stop() {
    thread::spawn(|| {
        thread::sleep(Duration::from_millis(PRIVACY_OVERLAY_STOP_DELAY_MS));
        if overlay::is_agent_active() || !overlay::is_active() {
            return;
        }
        match overlay::stop_overlay() {
            Ok(stopped) => {
                if stopped {
                    super::PRIVACY_OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
                }
                trace::log(format!("overlay:privacy_auto_stop stopped={stopped}"));
            }
            Err(err) => trace::log(format!("overlay:privacy_auto_stop_warn {err}")),
        }
    });
}

#[cfg(target_os = "macos")]
fn command_requires_privacy_signal(command: &Command) -> bool {
    matches!(
        command,
        Command::ScreenCapture { .. }
            | Command::ScreenTokenize { .. }
            | Command::ScreenFindText { .. }
            | Command::WaitText { .. }
            | Command::PointerMove { .. }
            | Command::PointerDown { .. }
            | Command::PointerUp { .. }
            | Command::PointerClick { .. }
            | Command::PointerClickText { .. }
            | Command::PointerClickId { .. }
            | Command::PointerScroll { .. }
            | Command::PointerDrag { .. }
            | Command::UiType { .. }
            | Command::KeyHotkey { .. }
            | Command::KeyEnter { .. }
            | Command::KeyEscape { .. }
    )
}

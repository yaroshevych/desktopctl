// Native Spotlight-style launcher UI.
//
// This module intentionally owns only AppKit presentation and hotkey plumbing. The
// daemon/controller supplies session data through `refresh` and receives user
// intent through `LauncherCallbacks::on_action`. `initialize`, `show`, `hide`,
// `toggle`, `refresh`, and `show_completion` must be called from the AppKit main
// thread, except that these entry points are safe to call from a worker: they
// post their work to the main queue.
//
// The controller should bind the active DesktopCtl window before presenting this
// panel, then include that binding in the Pi prompt. The panel deliberately does
// not call the daemon itself, so opening it cannot change the target before the
// controller has captured it.

use std::{
    cell::RefCell,
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{Arc, Mutex, OnceLock},
    thread,
    time::{Duration, Instant},
};

use dispatch2::{DispatchQueue, DispatchTime};
use objc2::{
    MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
};
use objc2_app_kit::{
    NSAnimationContext, NSApplication, NSBackingStoreType, NSColor, NSCursor, NSEvent,
    NSEventModifierFlags, NSFloatingWindowLevel, NSFont, NSPanel, NSTextAlignment, NSTextField,
    NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial, NSVisualEffectState,
    NSVisualEffectView, NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use super::swift_bridge;
use crate::trace;
use desktop_core::error::AppError;

const PANEL_WIDTH: f64 = 700.0;
const SESSION_PANEL_HEIGHT: f64 = 320.0;
const SESSION_MIN_WIDTH: f64 = 520.0;
const SESSION_MAX_WIDTH: f64 = 1200.0;
const SESSION_MIN_HEIGHT: f64 = 240.0;
const SESSION_MAX_HEIGHT: f64 = 900.0;
const WINDOW_SAFE_ZONE: f64 = 16.0;
const OUTSIDE_CLICK_GRACE_NANOS: i64 = 100_000_000;
const MAX_HISTORY_PANEL_HEIGHT: f64 = 450.0;
const MIN_LAUNCHER_PANEL_HEIGHT: f64 = 50.0;
const COMPLETION_WIDTH: f64 = 520.0;
const COMPLETION_HEIGHT: f64 = 116.0;
const COMPLETION_CORNER_RADIUS: f64 = 10.0;
const COMPLETION_CONTEXT_PILL_MAX_WIDTH: f64 = 220.0;
const COMPLETION_CONTEXT_PILL_HEIGHT: f64 = 28.0;
const COMPLETION_CONTEXT_PILL_HORIZONTAL_PADDING: f64 = 16.0;
const COMPLETION_CONTEXT_PROMPT_GAP: f64 = 8.0;
const COMPLETION_SEPARATOR_Y: f64 = 68.0;
const COMPLETION_PROMPT_Y: f64 = 79.0;
const COMPLETION_CONTEXT_PILL_Y: f64 = 78.0;
const COMPLETION_ANSWER_Y: f64 = 38.0;
const COMPLETION_FOOTER_Y: f64 = 12.0;
const COMPLETION_FADE_SECONDS: f64 = 0.2;
const COMPLETION_VISIBLE_MILLIS: u64 = 4_000;
// Keep this in sync with the intrinsic two-line SwiftUI session row.
const ROW_HEIGHT: f64 = 42.0;
const ROW_SPACING: f64 = 2.0;
const LIST_VERTICAL_INSET: f64 = 16.0;
const KEY_RETURN: u16 = 36;
const KEY_ENTER: u16 = 76;
const KEY_ESCAPE: u16 = 53;
const KEY_TAB: u16 = 48;
const KEY_UP: u16 = 126;
const KEY_DOWN: u16 = 125;
const NOTIFICATION_HOTKEY_WINDOW: Duration = Duration::from_millis(4_200);

struct PendingNotification {
    session_id: String,
    shown_at: Instant,
}

static LAST_NOTIFICATION: OnceLock<Mutex<Option<PendingNotification>>> = OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq)]
struct WorkArea {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

fn centered_top_origin(area: WorkArea, width: f64, height: f64, gap: f64) -> (f64, f64) {
    let x = area.x + ((area.width - width) / 2.0).max(0.0);
    let y = (area.y + area.height - height - gap).max(area.y);
    (x, y)
}

fn point_in_work_area(point: NSPoint, area: NSRect) -> bool {
    point.x >= area.origin.x
        && point.x < area.origin.x + area.size.width
        && point.y >= area.origin.y
        && point.y < area.origin.y + area.size.height
}

fn point_in_window_safe_zone(point: NSPoint, frame: NSRect) -> bool {
    point.x >= frame.origin.x - WINDOW_SAFE_ZONE
        && point.x <= frame.origin.x + frame.size.width + WINDOW_SAFE_ZONE
        && point.y >= frame.origin.y - WINDOW_SAFE_ZONE
        && point.y <= frame.origin.y + frame.size.height + WINDOW_SAFE_ZONE
}

fn screen_index_for_point(point: NSPoint, frames: &[NSRect]) -> Option<usize> {
    frames
        .iter()
        .position(|frame| point_in_work_area(point, *frame))
}

pub use super::core::{
    CompletionNotice, LauncherAction, LauncherScreen, LauncherSnapshot, SessionStatus,
    SessionSummary, TranscriptMessage,
};

pub type LauncherActionHandler = Arc<dyn Fn(LauncherAction) + Send + Sync + 'static>;

#[derive(Clone)]
pub struct LauncherCallbacks {
    pub on_action: LauncherActionHandler,
}

static CALLBACKS: OnceLock<LauncherCallbacks> = OnceLock::new();
static VISIBLE: AtomicBool = AtomicBool::new(false);
// Desired state updates immediately; main-queue work may lag behind hotkey input.
static REQUESTED_VISIBLE: AtomicBool = AtomicBool::new(false);
static NEXT_LIFECYCLE_SEQUENCE: AtomicU64 = AtomicU64::new(0);
static HOTKEY_REGISTERED: AtomicBool = AtomicBool::new(false);
static LIVE_RESIZE: AtomicBool = AtomicBool::new(false);

#[derive(Default)]
struct LauncherPanelIvars;

define_class!(
    // SAFETY: NSPanel has no subclassing requirements beyond NSObject's normal
    // object lifetime, and this class is only used on the AppKit main thread.
    #[unsafe(super = NSPanel)]
    #[thread_kind = MainThreadOnly]
    #[ivars = LauncherPanelIvars]
    struct LauncherPanel;

    // SAFETY: NSObjectProtocol has no additional subclassing requirements.
    unsafe impl NSObjectProtocol for LauncherPanel {}

    impl LauncherPanel {
        #[unsafe(method(canBecomeKeyWindow))]
        fn can_become_key_window(&self) -> bool { true }

        #[unsafe(method(canBecomeMainWindow))]
        fn can_become_main_window(&self) -> bool { true }

        #[unsafe(method(animationResizeTime:))]
        fn animation_resize_time(&self, _new_frame: NSRect) -> f64 { 0.24 }

        #[unsafe(method(performKeyEquivalent:))]
        fn perform_key_equivalent(&self, event: &NSEvent) -> Bool {
            if handle_key_event(event) {
                Bool::new(true)
            } else {
                unsafe { msg_send![super(self), performKeyEquivalent: event] }
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            // NSTextField normally sends its action for Return. This fallback
            // catches Return when a custom input method consumes the key first.
            if !handle_key_event(event) {
                unsafe { let _: () = msg_send![super(self), keyDown: event]; }
            }
        }

    }

    // SAFETY: NSWindowDelegate has no additional invariants for these methods.
    unsafe impl NSWindowDelegate for LauncherPanel {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            // orderOut can deliver this notification after a rapid reopen. Do
            // not let that stale resignation close a panel that is key again.
            // AppKit can also resign the panel while the user is dragging a
            // borderless window resize handle. That is still an in-window
            // interaction, not an outside click.
            if self.isKeyWindow()
                || LIVE_RESIZE.load(Ordering::SeqCst)
                || self.inLiveResize()
                || swift_bridge::actions_menu_handles_navigation()
            {
                return;
            }
            // AppKit may report the key-window change before it reports the
            // live-resize start. Give the resize hit-test a moment to settle
            // before treating this as an outside click.
            let resignation_sequence = NEXT_LIFECYCLE_SEQUENCE.load(Ordering::SeqCst);
            let _ = DispatchQueue::main().after(
                DispatchTime::NOW.time(OUTSIDE_CLICK_GRACE_NANOS),
                move || {
                    if LIVE_RESIZE.load(Ordering::SeqCst)
                        || swift_bridge::actions_menu_handles_navigation()
                        || !is_open_requested()
                        || NEXT_LIFECYCLE_SEQUENCE.load(Ordering::SeqCst) != resignation_sequence
                    {
                        return;
                    }
                    let should_hide = UI.with(|cell| {
                        let ui = cell.borrow();
                        let Some(panel) = ui.panel.as_ref() else {
                            return false;
                        };
                        if panel.inLiveResize() {
                            return false;
                        }
                        panel.isVisible()
                            && !point_in_window_safe_zone(
                                NSEvent::mouseLocation(),
                                panel.frame(),
                            )
                    });
                    if should_hide {
                        hide_on_main();
                    }
                },
            );
        }

        #[unsafe(method(windowWillStartLiveResize:))]
        fn window_will_start_live_resize(&self, _notification: &NSNotification) {
            LIVE_RESIZE.store(true, Ordering::SeqCst);
            self.setHidesOnDeactivate(false);
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
            self.makeKeyAndOrderFront(None);
        }

        #[unsafe(method(windowDidEndLiveResize:))]
        fn window_did_end_live_resize(&self, _notification: &NSNotification) {
            LIVE_RESIZE.store(false, Ordering::SeqCst);
            self.setHidesOnDeactivate(true);
            let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
            #[allow(deprecated)]
            app.activateIgnoringOtherApps(true);
            self.makeKeyAndOrderFront(None);
        }

    }
);

#[derive(Default)]
struct CompletionViewIvars;

define_class!(
    // SAFETY: NSView has no subclassing requirements beyond NSObject's normal
    // object lifetime, and this class is only used on the AppKit main thread.
    #[unsafe(super = NSView)]
    #[thread_kind = MainThreadOnly]
    #[ivars = CompletionViewIvars]
    struct CompletionView;

    // SAFETY: NSObjectProtocol has no additional subclassing requirements.
    unsafe impl NSObjectProtocol for CompletionView {}

    impl CompletionView {
        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, _event: &NSEvent) {
            completion_clicked_on_main();
        }
    }
);

struct UiState {
    panel: Option<Retained<LauncherPanel>>,
    content: Option<Retained<NSView>>,
    show_all: bool,
    history_expansion_pending: bool,
    completion_panel: Option<Retained<NSPanel>>,
    completion_session_id: Option<String>,
    completion_prompt_label: Option<Retained<NSTextField>>,
    completion_answer_label: Option<Retained<NSTextField>>,
    completion_follow_up_label: Option<Retained<NSTextField>>,
    completion_context_pill: Option<Retained<NSView>>,
    completion_context_label: Option<Retained<NSTextField>>,
    completion_generation: u64,
    anchor_visible_frame: Option<NSRect>,
    lifecycle_sequence: u64,
    rendered_session: bool,
    snapshot: LauncherSnapshot,
    snapshot_json: Option<Vec<u8>>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            panel: None,
            content: None,
            show_all: false,
            history_expansion_pending: false,
            completion_panel: None,
            completion_session_id: None,
            completion_prompt_label: None,
            completion_answer_label: None,
            completion_follow_up_label: None,
            completion_context_pill: None,
            completion_context_label: None,
            completion_generation: 0,
            anchor_visible_frame: None,
            lifecycle_sequence: 0,
            rendered_session: false,
            snapshot: LauncherSnapshot::default(),
            snapshot_json: None,
        }
    }
}

thread_local! { static UI: RefCell<UiState> = RefCell::new(UiState::default()); }

/// Register the launcher hotkey and prepare the panel. Call once from
/// the existing `NSApplication` main thread before `NSApplication::run`.
pub fn initialize(
    callbacks: LauncherCallbacks,
    shortcut: LauncherShortcut,
) -> Result<(), AppError> {
    if CALLBACKS.set(callbacks).is_err() {
        return Ok(());
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(AppError::backend_unavailable(
            "launcher initialization must run on the main thread",
        ));
    };
    install_hotkey(shortcut)?;
    create_panel(mtm)
}

pub fn is_visible() -> bool {
    VISIBLE.load(Ordering::SeqCst)
}

/// Return desired visibility, including requests still queued on AppKit main.
pub fn is_open_requested() -> bool {
    REQUESTED_VISIBLE.load(Ordering::SeqCst)
}

fn next_lifecycle_sequence() -> u64 {
    NEXT_LIFECYCLE_SEQUENCE
        .fetch_add(1, Ordering::Relaxed)
        .wrapping_add(1)
}

pub fn show() {
    REQUESTED_VISIBLE.store(true, Ordering::SeqCst);
    let sequence = next_lifecycle_sequence();
    DispatchQueue::main().exec_async(move || apply_show(sequence));
}

pub fn hide() {
    REQUESTED_VISIBLE.store(false, Ordering::SeqCst);
    let sequence = next_lifecycle_sequence();
    DispatchQueue::main().exec_async(move || apply_hide(sequence));
}

pub fn refresh(snapshot: LauncherSnapshot) {
    thread::spawn(move || {
        let Some(snapshot_json) = swift_bridge::serialize_snapshot(&snapshot) else {
            return;
        };
        DispatchQueue::main().exec_async(move || {
            let animate_history_expansion = UI.with(|cell| {
                let mut ui = cell.borrow_mut();
                if !accepts_newer_revision(ui.snapshot.revision, snapshot.revision) {
                    return None;
                }
                let animate_history_expansion = ui.history_expansion_pending
                    && ui.show_all
                    && snapshot.all.len() > ui.snapshot.all.len();
                ui.snapshot = snapshot;
                ui.snapshot_json = Some(snapshot_json);
                if animate_history_expansion {
                    ui.history_expansion_pending = false;
                }
                Some(animate_history_expansion)
            });
            let Some(animate_history_expansion) = animate_history_expansion else {
                return;
            };
            if is_visible() {
                render_on_main(animate_history_expansion);
            }
        });
    });
}

fn accepts_newer_revision(current: u64, incoming: u64) -> bool {
    incoming > current
}

fn accepts_lifecycle_sequence(current: u64, incoming: u64) -> bool {
    incoming > current
}

pub(crate) fn show_completion(
    notice: CompletionNotice,
    use_native_notifications: bool,
    timing: Option<Arc<trace::E2eTiming>>,
) {
    DispatchQueue::main().exec_async(move || {
        if is_visible() {
            if let Some(timing) = timing.as_ref() {
                timing.mark("notification_skipped", "launcher_visible=true");
            }
            return;
        }
        remember_notification(&notice.session_id);
        if use_native_notifications {
            show_native_completion_notification(notice, timing);
        } else {
            show_completion_on_main(notice, true, timing);
        }
    });
}

pub fn show_completion_immediately(notice: CompletionNotice, use_native_notifications: bool) {
    DispatchQueue::main().exec_async(move || {
        if is_visible() {
            return;
        }
        remember_notification(&notice.session_id);
        if use_native_notifications {
            show_native_completion_notification(notice, None);
        } else {
            show_completion_on_main(notice, false, None);
        }
    });
}

fn show_native_completion_notification(
    notice: CompletionNotice,
    timing: Option<Arc<trace::E2eTiming>>,
) {
    let title = notice
        .target_app
        .as_deref()
        .map(|app| format!("DesktopCtl · {app}"))
        .unwrap_or_else(|| "DesktopCtl".to_owned());
    let body = one_line(&notice.answer_preview, 120);
    if let Some(timing) = timing.as_ref() {
        timing.mark(
            "notification_request_submitted",
            format!("session={}", notice.session_id),
        );
    }
    swift_bridge::show_completion_notification_for_session(&title, &body, &notice.session_id);
}

fn remember_notification(session_id: &str) {
    if session_id.is_empty() {
        return;
    }
    let pending = LAST_NOTIFICATION.get_or_init(|| Mutex::new(None));
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *pending = Some(PendingNotification {
        session_id: session_id.to_owned(),
        shown_at: Instant::now(),
    });
}

pub fn take_recent_notification_session() -> Option<String> {
    let pending = LAST_NOTIFICATION.get()?;
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let notification = pending.take()?;
    (notification.shown_at.elapsed() <= NOTIFICATION_HOTKEY_WINDOW)
        .then_some(notification.session_id)
}

fn clear_notification_session(session_id: &str) {
    let Some(pending) = LAST_NOTIFICATION.get() else {
        return;
    };
    let mut pending = pending
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if pending
        .as_ref()
        .is_some_and(|notification| notification.session_id == session_id)
    {
        *pending = None;
    }
}

pub(crate) unsafe extern "C" fn notification_action_callback(
    ptr: *const std::ffi::c_char,
    length: usize,
) {
    if ptr.is_null() {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), length) };
    let Ok(session_id) = String::from_utf8(bytes.to_owned()) else {
        return;
    };
    if session_id.is_empty() {
        return;
    }
    DispatchQueue::main().exec_async(move || {
        open_notification_session(session_id);
    });
}

fn open_notification_session(session_id: String) {
    clear_notification_session(&session_id);
    show();
    if let Some(on_action) = CALLBACKS.get().map(|callbacks| callbacks.on_action.clone()) {
        thread::spawn(move || {
            on_action(LauncherAction::OpenSession { session_id });
        });
    }
}

fn completion_clicked_on_main() {
    let session_id = UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        ui.completion_generation = ui.completion_generation.wrapping_add(1);
        if let Some(panel) = ui.completion_panel.as_ref() {
            panel.orderOut(None);
        }
        ui.completion_session_id.take()
    });
    if let Some(session_id) = session_id.filter(|id| !id.is_empty()) {
        open_notification_session(session_id);
    }
}

fn create_panel(mtm: MainThreadMarker) -> Result<(), AppError> {
    UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        if ui.panel.is_some() { return Ok(()); }
        let panel = unsafe {
            let allocated = LauncherPanel::alloc(mtm).set_ivars(LauncherPanelIvars);
            msg_send![super(allocated), initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_WIDTH, MIN_LAUNCHER_PANEL_HEIGHT)), styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel, backing: NSBackingStoreType::Buffered, defer: false]
        };
        let panel: Retained<LauncherPanel> = panel;
        unsafe { panel.setReleasedWhenClosed(false); }
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(false);
        panel.setHidesOnDeactivate(true);
        panel.setHasShadow(true);
        panel.setBackgroundColor(Some(&NSColor::clearColor()));
        panel.setOpaque(false);
        panel.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary | NSWindowCollectionBehavior::Stationary);
        panel.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*panel)));

        let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_WIDTH, MIN_LAUNCHER_PANEL_HEIGHT)));
        content.setWantsLayer(true);
        panel.setContentView(Some(&content));
        if !swift_bridge::mount(
            (&*content as *const NSView).cast_mut().cast(),
            swift_action_callback,
        ) {
            return Err(AppError::backend_unavailable(
                "Swift launcher view failed to mount",
            ));
        }
        ui.panel = Some(panel);
        ui.content = Some(content);
        Ok(())
    })
}

unsafe extern "C" fn swift_action_callback(ptr: *const std::ffi::c_char, length: usize) {
    if ptr.is_null() {
        return;
    }
    let bytes = unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), length) };
    if swift_requests_history_expansion(bytes) {
        DispatchQueue::main().exec_async(expand_history_on_main);
    }
    let Ok(parsed) = std::panic::catch_unwind(|| parse_swift_action(bytes)) else {
        return;
    };
    let Some((launcher_action, hide_before_dispatch)) = parsed else {
        return;
    };
    let main_thread_only = matches!(launcher_action, LauncherAction::OpenSettings);
    // Swift callbacks arrive on AppKit's main thread. Keep only presentation
    // changes there; controller actions perform store/workspace/IPC work off-main.
    if hide_before_dispatch {
        hide_on_main();
    }
    let Some(on_action) = CALLBACKS.get().map(|callbacks| callbacks.on_action.clone()) else {
        return;
    };
    if main_thread_only {
        DispatchQueue::main().exec_async(move || on_action(launcher_action));
    } else {
        thread::spawn(move || on_action(launcher_action));
    }
}

fn parse_swift_action(bytes: &[u8]) -> Option<(LauncherAction, bool)> {
    let action = serde_json::from_slice::<serde_json::Value>(bytes).ok()?;
    let launcher_action = match action.get("type").and_then(|value| value.as_str()) {
        Some("new_request") => {
            let prompt = action.get("prompt").and_then(|value| value.as_str())?;
            let prompt = prompt.trim();
            if prompt.is_empty() {
                return None;
            }
            LauncherAction::NewRequest {
                prompt: prompt.to_owned(),
                share_context: action
                    .get("share_context")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true),
                read_only: action
                    .get("read_only")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
            }
        }
        Some("open_session") => {
            let session_id = action.get("session_id").and_then(|value| value.as_str())?;
            LauncherAction::OpenSession {
                session_id: session_id.to_owned(),
            }
        }
        Some("follow_up") => {
            let (Some(session_id), Some(prompt)) = (
                action.get("session_id").and_then(|value| value.as_str()),
                action.get("prompt").and_then(|value| value.as_str()),
            ) else {
                return None;
            };
            let prompt = prompt.trim();
            if prompt.is_empty() {
                return None;
            }
            LauncherAction::FollowUp {
                session_id: session_id.to_owned(),
                prompt: prompt.to_owned(),
                share_context: action
                    .get("share_context")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true),
                read_only: action
                    .get("read_only")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(false),
            }
        }
        Some("cancel_session") => {
            let session_id = action.get("session_id").and_then(|value| value.as_str())?;
            LauncherAction::CancelSession {
                session_id: session_id.to_owned(),
            }
        }
        Some("open_in_terminal") | Some("open_in_ghostty") => {
            let session_id = action.get("session_id").and_then(|value| value.as_str())?;
            LauncherAction::OpenInTerminal {
                session_id: session_id.to_owned(),
            }
        }
        Some("return_to_launcher") => LauncherAction::ReturnToLauncher,
        Some("expand_history") => LauncherAction::ExpandHistory,
        Some("ack_transcript") => LauncherAction::AcknowledgeTranscript {
            session_id: action.get("session_id")?.as_str()?.to_owned(),
            epoch: action.get("epoch")?.as_u64()?,
            count: usize::try_from(action.get("count")?.as_u64()?).ok()?,
        },
        Some("reset_transcript") => LauncherAction::ResetTranscript,
        Some("open_settings") => LauncherAction::OpenSettings,
        _ => return None,
    };
    let hide_before_dispatch = matches!(
        launcher_action,
        LauncherAction::NewRequest { .. } | LauncherAction::OpenSettings
    );
    Some((launcher_action, hide_before_dispatch))
}

fn swift_requests_history_expansion(bytes: &[u8]) -> bool {
    serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|action| {
            action
                .get("type")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
        })
        .as_deref()
        == Some("expand_history")
}

fn expand_history_on_main() {
    let animate_now = UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        if matches!(ui.snapshot.screen, LauncherScreen::Launcher)
            && !ui.show_all
            && ui.snapshot.history_total > ui.snapshot.recent.len()
        {
            ui.show_all = true;
            if ui.snapshot.all.is_empty() {
                ui.history_expansion_pending = true;
                false
            } else {
                true
            }
        } else {
            false
        }
    });
    if animate_now {
        render_on_main(true);
    }
}

fn show_completion_on_main(
    notice: CompletionNotice,
    animated: bool,
    timing: Option<Arc<trace::E2eTiming>>,
) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let generation = UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        if ui.completion_panel.is_none() {
            let panel = NSPanel::initWithContentRect_styleMask_backing_defer(
                NSPanel::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(COMPLETION_WIDTH, COMPLETION_HEIGHT),
                ),
                NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel,
                NSBackingStoreType::Buffered,
                false,
            );
            unsafe {
                panel.setReleasedWhenClosed(false);
            }
            panel.setFloatingPanel(true);
            panel.setBecomesKeyOnlyIfNeeded(true);
            panel.setHidesOnDeactivate(false);
            panel.setHasShadow(false);
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::clearColor()));
            panel.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary,
            );
            panel.setIgnoresMouseEvents(false);
            panel.setLevel(NSFloatingWindowLevel);

            let container = NSView::initWithFrame(
                NSView::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(COMPLETION_WIDTH, COMPLETION_HEIGHT),
                ),
            );
            let arrow_cursor = NSCursor::arrowCursor();
            container.addCursorRect_cursor(
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(COMPLETION_WIDTH, COMPLETION_HEIGHT),
                ),
                &arrow_cursor,
            );

            let surface = NSVisualEffectView::initWithFrame(
                NSVisualEffectView::alloc(mtm),
                NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(COMPLETION_WIDTH, COMPLETION_HEIGHT),
                ),
            );
            surface.setMaterial(NSVisualEffectMaterial::HUDWindow);
            surface.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
            surface.setState(NSVisualEffectState::Active);
            style_completion_surface(&surface);
            let scrim = completion_scrim_view(mtm);
            surface.addSubview(&scrim);
            let separator = completion_separator_view(mtm);
            surface.addSubview(&separator);

            let prompt_label = text_field(
                mtm,
                "",
                NSRect::new(
                    NSPoint::new(16.0, COMPLETION_PROMPT_Y),
                    NSSize::new(COMPLETION_WIDTH - 32.0, 22.0),
                ),
                false,
            );
            prompt_label.setSelectable(false);
            prompt_label.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
            prompt_label.setTextColor(Some(&NSColor::labelColor()));

            let answer_label = text_field(
                mtm,
                "",
                NSRect::new(
                    NSPoint::new(16.0, COMPLETION_ANSWER_Y),
                    NSSize::new(COMPLETION_WIDTH - 32.0, 22.0),
                ),
                false,
            );
            answer_label.setSelectable(false);
            answer_label.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
            answer_label.setTextColor(Some(&NSColor::secondaryLabelColor()));

            let follow_up_label = text_field(
                mtm,
                "",
                NSRect::new(
                    NSPoint::new(16.0, COMPLETION_FOOTER_Y),
                    NSSize::new(COMPLETION_WIDTH - 32.0, 18.0),
                ),
                false,
            );
            follow_up_label.setSelectable(false);
            follow_up_label.setFont(Some(&NSFont::systemFontOfSize(11.0)));
            follow_up_label.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
            follow_up_label.setTextColor(Some(&NSColor::secondaryLabelColor()));

            let (context_pill, context_label) = completion_context_pill(mtm);
            surface.addSubview(&context_pill);
            surface.addSubview(&prompt_label);
            surface.addSubview(&answer_label);
            surface.addSubview(&follow_up_label);
            container.addSubview(&surface);
            let click_overlay: Retained<CompletionView> = unsafe {
                let allocated = CompletionView::alloc(mtm).set_ivars(CompletionViewIvars);
                msg_send![super(allocated), initWithFrame: NSRect::new(
                    NSPoint::new(0.0, 0.0),
                    NSSize::new(COMPLETION_WIDTH, COMPLETION_HEIGHT),
                )]
            };
            container.addSubview(&click_overlay);
            panel.setContentView(Some(&container));
            ui.completion_prompt_label = Some(prompt_label);
            ui.completion_answer_label = Some(answer_label);
            ui.completion_follow_up_label = Some(follow_up_label);
            ui.completion_context_pill = Some(context_pill);
            ui.completion_context_label = Some(context_label);
            ui.completion_panel = Some(panel);
        }

        ui.completion_generation = ui.completion_generation.wrapping_add(1);
        ui.completion_session_id = Some(notice.session_id.clone());
        let generation = ui.completion_generation;
        let panel = ui.completion_panel.as_ref().unwrap();
        let context_pill_width = if let (Some(pill), Some(label)) = (
            ui.completion_context_pill.as_ref(),
            ui.completion_context_label.as_ref(),
        ) {
            if let Some(app) = notice.target_app.as_deref() {
                let width = layout_completion_context_pill(pill, label, app);
                pill.setHidden(false);
                width
            } else {
                pill.setHidden(true);
                0.0
            }
        } else {
            0.0
        };
        ui.completion_prompt_label
            .as_ref()
            .unwrap()
            .setFrame(NSRect::new(
                NSPoint::new(16.0, COMPLETION_PROMPT_Y),
                NSSize::new(
                    if context_pill_width > 0.0 {
                        COMPLETION_WIDTH - 32.0 - context_pill_width - COMPLETION_CONTEXT_PROMPT_GAP
                    } else {
                        COMPLETION_WIDTH - 32.0
                    },
                    22.0,
                ),
            ));
        ui.completion_prompt_label
            .as_ref()
            .unwrap()
            .setStringValue(&NSString::from_str(&one_line(&notice.prompt, 120)));
        ui.completion_answer_label
            .as_ref()
            .unwrap()
            .setStringValue(&NSString::from_str(&one_line(&notice.answer_preview, 120)));
        ui.completion_follow_up_label
            .as_ref()
            .unwrap()
            .setStringValue(&NSString::from_str(&format!(
                "{} for follow-up",
                notice.follow_up_shortcut
            )));
        position_completion(panel, ui.anchor_visible_frame);
        NSCursor::arrowCursor().set();
        if animated {
            panel.setAlphaValue(0.0);
            panel.orderFrontRegardless();
            animate_alpha(panel, 1.0);
        } else {
            panel.setAlphaValue(1.0);
            panel.orderFrontRegardless();
        }
        if let Some(timing) = timing.as_ref() {
            timing.mark(
                "completion_panel_visible",
                format!("session={} animated={animated}", notice.session_id),
            );
        }
        generation
    });

    if !animated {
        return;
    }

    thread::spawn(move || {
        thread::sleep(Duration::from_millis(COMPLETION_VISIBLE_MILLIS));
        DispatchQueue::main().exec_async(move || {
            UI.with(|cell| {
                let ui = cell.borrow();
                if ui.completion_generation == generation {
                    if let Some(panel) = ui.completion_panel.as_ref() {
                        animate_alpha(panel, 0.0);
                    }
                }
            });
        });
        thread::sleep(Duration::from_millis(
            (COMPLETION_FADE_SECONDS * 1_000.0) as u64,
        ));
        DispatchQueue::main().exec_async(move || {
            UI.with(|cell| {
                let ui = cell.borrow();
                if ui.completion_generation == generation {
                    if let Some(panel) = ui.completion_panel.as_ref() {
                        panel.orderOut(None);
                    }
                }
            });
        });
    });
}

fn completion_scrim_view(mtm: MainThreadMarker) -> Retained<NSView> {
    let scrim = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, 0.0),
            NSSize::new(COMPLETION_WIDTH, COMPLETION_HEIGHT),
        ),
    );
    scrim.setWantsLayer(true);
    let layer: *mut AnyObject = unsafe { msg_send![&*scrim, layer] };
    if !layer.is_null() {
        let color = NSColor::blackColor().colorWithAlphaComponent(0.22);
        let cg_color: *mut AnyObject = unsafe { msg_send![&*color, CGColor] };
        unsafe {
            let _: () = msg_send![layer, setBackgroundColor: cg_color];
        }
    }
    scrim
}

fn completion_separator_view(mtm: MainThreadMarker) -> Retained<NSView> {
    let separator = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, COMPLETION_SEPARATOR_Y),
            NSSize::new(COMPLETION_WIDTH, 0.5),
        ),
    );
    separator.setWantsLayer(true);
    let layer: *mut AnyObject = unsafe { msg_send![&*separator, layer] };
    if !layer.is_null() {
        let color = NSColor::tertiaryLabelColor().colorWithAlphaComponent(0.24);
        let cg_color: *mut AnyObject = unsafe { msg_send![&*color, CGColor] };
        unsafe {
            let _: () = msg_send![layer, setBackgroundColor: cg_color];
        }
    }
    separator
}

fn completion_context_pill(mtm: MainThreadMarker) -> (Retained<NSView>, Retained<NSTextField>) {
    let pill = NSView::initWithFrame(
        NSView::alloc(mtm),
        NSRect::new(
            NSPoint::new(0.0, COMPLETION_CONTEXT_PILL_Y),
            NSSize::new(64.0, COMPLETION_CONTEXT_PILL_HEIGHT),
        ),
    );
    style_completion_pill(&pill);

    let label = text_field(
        mtm,
        "",
        NSRect::new(NSPoint::new(8.0, 2.0), NSSize::new(48.0, 20.0)),
        false,
    );
    label.setSelectable(false);
    label.setAlignment(NSTextAlignment::Center);
    label.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
    label.setTextColor(Some(&NSColor::secondaryLabelColor()));

    pill.addSubview(&label);
    pill.setHidden(true);
    (pill, label)
}

fn layout_completion_context_pill(pill: &NSView, label: &NSTextField, app: &str) -> f64 {
    label.setStringValue(&NSString::from_str(app));
    label.sizeToFit();
    let text_width = label.frame().size.width;
    let width = (text_width + COMPLETION_CONTEXT_PILL_HORIZONTAL_PADDING)
        .clamp(64.0, COMPLETION_CONTEXT_PILL_MAX_WIDTH);
    pill.setFrame(NSRect::new(
        NSPoint::new(COMPLETION_WIDTH - 16.0 - width, COMPLETION_CONTEXT_PILL_Y),
        NSSize::new(width, COMPLETION_CONTEXT_PILL_HEIGHT),
    ));
    label.setFrame(NSRect::new(
        NSPoint::new(8.0, 2.0),
        NSSize::new(width - COMPLETION_CONTEXT_PILL_HORIZONTAL_PADDING, 20.0),
    ));
    width
}

fn style_completion_pill(view: &NSView) {
    view.setWantsLayer(true);
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if layer.is_null() {
        return;
    }
    let background = NSColor::whiteColor().colorWithAlphaComponent(0.07);
    let background_cg_color: *mut AnyObject = unsafe { msg_send![&*background, CGColor] };
    unsafe {
        let _: () = msg_send![layer, setCornerRadius: 10.0_f64];
        let _: () = msg_send![layer, setMasksToBounds: true];
        let _: () = msg_send![layer, setBackgroundColor: background_cg_color];
    }
}

fn style_completion_surface(view: &NSView) {
    view.setWantsLayer(true);
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if layer.is_null() {
        return;
    }
    let border_color = NSColor::separatorColor().colorWithAlphaComponent(0.5);
    let border_cg_color: *mut AnyObject = unsafe { msg_send![&*border_color, CGColor] };
    unsafe {
        let _: () = msg_send![layer, setCornerRadius: COMPLETION_CORNER_RADIUS];
        let _: () = msg_send![layer, setMasksToBounds: true];
        let _: () = msg_send![layer, setBorderWidth: 0.5_f64];
        let _: () = msg_send![layer, setBorderColor: border_cg_color];
    }
}

fn animate_alpha(panel: &NSPanel, alpha: f64) {
    NSAnimationContext::beginGrouping();
    NSAnimationContext::currentContext().setDuration(COMPLETION_FADE_SECONDS);
    unsafe {
        let animator: *mut AnyObject = msg_send![panel, animator];
        let _: () = msg_send![animator, setAlphaValue: alpha];
    }
    NSAnimationContext::endGrouping();
}

fn active_visible_frame(mtm: MainThreadMarker) -> Option<NSRect> {
    let point = NSEvent::mouseLocation();
    let screens = objc2_app_kit::NSScreen::screens(mtm);
    let frames: Vec<NSRect> = screens.iter().map(|screen| screen.frame()).collect();
    let index = screen_index_for_point(point, &frames);
    index
        .and_then(|index| {
            screens
                .iter()
                .nth(index)
                .map(|screen| screen.visibleFrame())
        })
        .or_else(|| objc2_app_kit::NSScreen::mainScreen(mtm).map(|screen| screen.visibleFrame()))
}

fn cached_or_active_visible_frame(mtm: MainThreadMarker, cached: Option<NSRect>) -> Option<NSRect> {
    let screens = objc2_app_kit::NSScreen::screens(mtm);
    if let Some(cached) = cached {
        if screens.iter().any(|screen| screen.visibleFrame() == cached) {
            return Some(cached);
        }
    }
    active_visible_frame(mtm)
}

fn position_completion(panel: &NSPanel, cached_frame: Option<NSRect>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(frame) = cached_or_active_visible_frame(mtm, cached_frame) else {
        return;
    };
    let (x, y) = centered_top_origin(
        WorkArea {
            x: frame.origin.x,
            y: frame.origin.y,
            width: frame.size.width,
            height: frame.size.height,
        },
        COMPLETION_WIDTH,
        COMPLETION_HEIGHT,
        72.0,
    );
    panel.setFrameOrigin(NSPoint::new(x, y));
}

fn text_field(
    mtm: MainThreadMarker,
    placeholder: &str,
    frame: NSRect,
    editable: bool,
) -> Retained<NSTextField> {
    let field = NSTextField::initWithFrame(NSTextField::alloc(mtm), frame);
    field.setEditable(editable);
    field.setSelectable(true);
    field.setBezeled(editable);
    field.setBordered(editable);
    field.setDrawsBackground(editable);
    field.setPlaceholderString(Some(&NSString::from_str(placeholder)));
    field.setFont(Some(&NSFont::systemFontOfSize(if editable {
        17.0
    } else {
        13.0
    })));
    field
}

fn show_on_main() {
    apply_show(next_lifecycle_sequence());
}

fn apply_show(sequence: u64) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let accepted = UI.with(|cell| {
        let Ok(mut ui) = cell.try_borrow_mut() else {
            DispatchQueue::main().exec_async(move || apply_show(sequence));
            return false;
        };
        if !accepts_lifecycle_sequence(ui.lifecycle_sequence, sequence) {
            return false;
        }
        ui.lifecycle_sequence = sequence;
        true
    });
    if !accepted {
        return;
    }
    REQUESTED_VISIBLE.store(true, Ordering::SeqCst);
    dismiss_completion_on_main();
    if let Some(frame) = active_visible_frame(MainThreadMarker::new().unwrap()) {
        UI.with(|cell| cell.borrow_mut().anchor_visible_frame = Some(frame));
    }
    let has_panel = UI.with(|cell| cell.borrow().panel.is_some());
    if !has_panel {
        return;
    }
    let was_visible = is_visible();
    if !was_visible {
        UI.with(|cell| {
            let mut ui = cell.borrow_mut();
            ui.show_all = false;
            ui.history_expansion_pending = false;
            ui.rendered_session = false;
        });
        swift_bridge::prepare_for_presentation();
    }
    render_on_main(false);
    let controls = UI.with(|cell| {
        let ui = cell.borrow();
        ui.panel
            .as_ref()
            .cloned()
            .map(|panel| (panel, ui.anchor_visible_frame))
    });
    let Some((panel, anchor_visible_frame)) = controls else {
        return;
    };
    position_panel(&panel, anchor_visible_frame);
    let app = NSApplication::sharedApplication(MainThreadMarker::new().unwrap());
    #[allow(deprecated)]
    app.activateIgnoringOtherApps(true);
    panel.makeKeyAndOrderFront(None);
    swift_bridge::focus_prompt();
    VISIBLE.store(true, Ordering::SeqCst);
    if let Some(timing) = trace::current_launcher_timing() {
        timing.mark("launcher_panel_visible", "");
    }
}

fn hide_on_main() {
    apply_hide(next_lifecycle_sequence());
}

fn apply_hide(sequence: u64) {
    if MainThreadMarker::new().is_none() {
        return;
    }
    let panel = UI.with(|cell| {
        let Ok(mut ui) = cell.try_borrow_mut() else {
            DispatchQueue::main().exec_async(move || apply_hide(sequence));
            return None;
        };
        if !accepts_lifecycle_sequence(ui.lifecycle_sequence, sequence) {
            return None;
        }
        ui.lifecycle_sequence = sequence;
        Some(ui.panel.as_ref().cloned())
    });
    let Some(panel) = panel else {
        return;
    };
    REQUESTED_VISIBLE.store(false, Ordering::SeqCst);
    if let Some(panel) = panel.as_ref() {
        panel.orderOut(None);
    }
    if VISIBLE.swap(false, Ordering::SeqCst) {
        // Let AppKit finish resigning/ordering out before restoring prior app.
        // Skip stale restoration if launcher reopened meanwhile.
        DispatchQueue::main().exec_async(|| {
            if !is_open_requested() {
                if let Some(callbacks) = CALLBACKS.get() {
                    (callbacks.on_action)(LauncherAction::Dismissed);
                }
            }
        });
    }
}

fn dismiss_completion_on_main() {
    let panel = UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        ui.completion_generation = ui.completion_generation.wrapping_add(1);
        ui.completion_session_id = None;
        ui.completion_panel.as_ref().cloned()
    });
    if let Some(panel) = panel.as_ref() {
        panel.orderOut(None);
    }
}

fn position_panel(panel: &NSPanel, cached_frame: Option<NSRect>) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let Some(frame) = cached_or_active_visible_frame(mtm, cached_frame) else {
        return;
    };
    let (x, y) = centered_top_origin(
        WorkArea {
            x: frame.origin.x,
            y: frame.origin.y,
            width: frame.size.width,
            height: frame.size.height,
        },
        panel.frame().size.width,
        panel.frame().size.height,
        72.0,
    );
    panel.setFrameOrigin(NSPoint::new(x, y));
}

fn render_on_main(animate_resize: bool) {
    UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        let Some(content) = ui.content.as_ref().cloned() else {
            return;
        };
        let is_session = matches!(ui.snapshot.screen, LauncherScreen::Session { .. });
        let entering_session = is_session && !ui.rendered_session;
        let size = match ui.snapshot.screen {
            LauncherScreen::Launcher => NSSize::new(PANEL_WIDTH, launcher_panel_height(&ui)),
            LauncherScreen::Session { .. } if entering_session => {
                NSSize::new(PANEL_WIDTH, SESSION_PANEL_HEIGHT)
            }
            LauncherScreen::Session { .. } => ui
                .panel
                .as_ref()
                .map(|panel| panel.frame().size)
                .unwrap_or_else(|| NSSize::new(PANEL_WIDTH, SESSION_PANEL_HEIGHT)),
        };
        configure_panel_resizing(&ui, is_session);
        resize_panel(&ui, &content, size, animate_resize);
        if let Some(snapshot_json) = ui.snapshot_json.as_deref() {
            swift_bridge::set_snapshot_json(snapshot_json);
        } else if let Some(snapshot_json) = swift_bridge::serialize_snapshot(&ui.snapshot) {
            swift_bridge::set_snapshot_json(&snapshot_json);
        }
        ui.rendered_session = is_session;
    });
}

fn launcher_panel_height(ui: &UiState) -> f64 {
    let session_count = if ui.show_all {
        ui.snapshot.all.len()
    } else {
        ui.snapshot.recent.len()
    };
    let show_all_count = if ui.snapshot.history_total > session_count {
        1
    } else {
        0
    };
    let visible_row_count = session_count + show_all_count;
    let list_inset = if visible_row_count == 0 {
        0.0
    } else {
        LIST_VERTICAL_INSET
    };
    let rows_height = visible_row_count as f64 * ROW_HEIGHT
        + visible_row_count.saturating_sub(1) as f64 * ROW_SPACING;
    (50.0 + list_inset + rows_height).clamp(MIN_LAUNCHER_PANEL_HEIGHT, MAX_HISTORY_PANEL_HEIGHT)
}

fn configure_panel_resizing(ui: &UiState, session: bool) {
    let Some(panel) = ui.panel.as_ref() else {
        return;
    };
    let mut style_mask = panel.styleMask();
    if session {
        style_mask.insert(NSWindowStyleMask::Resizable);
        style_mask.remove(NSWindowStyleMask::NonactivatingPanel);
        panel.setContentMinSize(NSSize::new(SESSION_MIN_WIDTH, SESSION_MIN_HEIGHT));
        panel.setContentMaxSize(NSSize::new(SESSION_MAX_WIDTH, SESSION_MAX_HEIGHT));
    } else {
        style_mask.remove(NSWindowStyleMask::Resizable);
        style_mask.insert(NSWindowStyleMask::NonactivatingPanel);
        panel.setContentMinSize(NSSize::new(PANEL_WIDTH, MIN_LAUNCHER_PANEL_HEIGHT));
        panel.setContentMaxSize(NSSize::new(PANEL_WIDTH, MAX_HISTORY_PANEL_HEIGHT));
    }
    if style_mask != panel.styleMask() {
        panel.setStyleMask(style_mask);
    }
}

fn resize_panel(ui: &UiState, content: &NSView, size: NSSize, animate: bool) {
    let Some(panel) = ui.panel.as_ref() else {
        return;
    };
    if animate && panel.isVisible() {
        let frame = panel.frame();
        let top = frame.origin.y + frame.size.height;
        let target = NSRect::new(NSPoint::new(frame.origin.x, top - size.height), size);
        NSAnimationContext::beginGrouping();
        NSAnimationContext::currentContext().setDuration(0.24);
        unsafe {
            let animator: *mut AnyObject = msg_send![panel, animator];
            let _: () = msg_send![animator, setFrame: target, display: true];
        }
        NSAnimationContext::endGrouping();
        return;
    }
    panel.setContentSize(size);
    content.setFrameSize(size);
    position_panel(panel, ui.anchor_visible_frame);
}

fn handle_key_event(event: &NSEvent) -> bool {
    if !VISIBLE.load(Ordering::SeqCst) {
        return false;
    }
    let modifiers = event.modifierFlags()
        & (NSEventModifierFlags::Control
            | NSEventModifierFlags::Option
            | NSEventModifierFlags::Shift
            | NSEventModifierFlags::Command);
    let characters = event
        .charactersIgnoringModifiers()
        .map(|characters| characters.to_string());
    let command_comma =
        modifiers == NSEventModifierFlags::Command && characters.as_deref() == Some(",");
    if command_comma {
        hide_on_main();
        if let Some(callbacks) = CALLBACKS.get() {
            (callbacks.on_action)(LauncherAction::OpenSettings);
        }
        return true;
    }
    let command_k = modifiers == NSEventModifierFlags::Command
        && characters
            .as_deref()
            .is_some_and(|value| value.eq_ignore_ascii_case("k"));
    if command_k {
        swift_bridge::toggle_actions_menu();
        return true;
    }

    let command_return = modifiers == NSEventModifierFlags::Command
        && matches!(event.keyCode(), KEY_RETURN | KEY_ENTER);
    if command_return {
        let session_id = UI.with(|cell| {
            let ui = cell.borrow();
            match &ui.snapshot.screen {
                LauncherScreen::Session {
                    id,
                    terminal_available: true,
                    ..
                } => Some(id.clone()),
                _ => None,
            }
        });
        if let Some(session_id) = session_id {
            DispatchQueue::main().exec_async(move || {
                if let Some(callbacks) = CALLBACKS.get() {
                    (callbacks.on_action)(LauncherAction::OpenInTerminal { session_id });
                }
            });
            return true;
        }
    }

    match event.keyCode() {
        KEY_ESCAPE => {
            if swift_bridge::dismiss_actions_menu() {
                return true;
            }
            let in_session = UI.with(|cell| {
                matches!(
                    cell.borrow().snapshot.screen,
                    LauncherScreen::Session { .. }
                )
            });
            if in_session {
                show_launcher_on_main();
                if let Some(callbacks) = CALLBACKS.get() {
                    (callbacks.on_action)(LauncherAction::ReturnToLauncher);
                }
            } else {
                hide_on_main();
            }
            true
        }
        KEY_UP => {
            if swift_bridge::actions_menu_handles_navigation() {
                return swift_bridge::move_actions_menu_focus(-1, true);
            }
            swift_bridge::move_selection(-1);
            true
        }
        KEY_DOWN => {
            if swift_bridge::actions_menu_handles_navigation() {
                return swift_bridge::move_actions_menu_focus(1, false);
            }
            swift_bridge::move_selection(1);
            true
        }
        KEY_TAB => {
            if swift_bridge::actions_menu_handles_navigation() {
                let backwards = modifiers.contains(NSEventModifierFlags::Shift);
                return swift_bridge::move_actions_menu_focus(
                    if backwards { -1 } else { 1 },
                    false,
                );
            }
            if modifiers.is_empty() || modifiers == NSEventModifierFlags::Shift {
                swift_bridge::move_selection(if modifiers.is_empty() { 1 } else { -1 });
                return true;
            }
            false
        }
        KEY_RETURN | KEY_ENTER => swift_bridge::activate_actions_menu(),
        _ => false,
    }
}

fn show_launcher_on_main() {
    UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        ui.snapshot.screen = LauncherScreen::Launcher;
    });
    show_on_main();
}

fn one_line(text: &str, max: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max {
        return compact;
    }
    compact
        .chars()
        .take(max.saturating_sub(1))
        .collect::<String>()
        + "…"
}

// Carbon Event Manager's application hotkey API is delivered through the
// process' normal NSApplication run loop and does not need Accessibility access.
type OSStatus = i32;
type EventHotKeyRef = *mut c_void;
type EventTargetRef = *mut c_void;
#[repr(C)]
struct EventTypeSpec {
    event_class: u32,
    event_kind: u32,
}
#[repr(C)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

const EVENT_CLASS_KEYBOARD: u32 = u32::from_be_bytes(*b"keyb");
const EVENT_HOT_KEY_PRESSED: u32 = 5;
const HOTKEY_ID: u32 = 0x4454_4c41;

#[derive(Clone, Copy, Debug, serde::Deserialize)]
pub struct LauncherShortcut {
    #[serde(default = "default_launcher_key_code")]
    pub key_code: u32,
    #[serde(default = "default_launcher_modifiers")]
    pub modifiers: u32,
}

impl Default for LauncherShortcut {
    fn default() -> Self {
        Self {
            key_code: default_launcher_key_code(),
            modifiers: default_launcher_modifiers(),
        }
    }
}

fn default_launcher_key_code() -> u32 {
    49 // kVK_Space
}

fn default_launcher_modifiers() -> u32 {
    1 << 11 // optionKey
}

pub fn shortcut_label(shortcut: LauncherShortcut) -> String {
    let mut label = String::new();
    if shortcut.modifiers & (1 << 12) != 0 {
        label.push('⌃');
    }
    if shortcut.modifiers & (1 << 11) != 0 {
        label.push('⌥');
    }
    if shortcut.modifiers & (1 << 9) != 0 {
        label.push('⇧');
    }
    if shortcut.modifiers & (1 << 8) != 0 {
        label.push('⌘');
    }
    label.push_str(match shortcut.key_code {
        49 => "Space",
        36 => "↵",
        76 => "⌤",
        48 => "⇥",
        51 => "⌫",
        117 => "⌦",
        53 => "⎋",
        123 => "←",
        124 => "→",
        126 => "↑",
        125 => "↓",
        122 => "F1",
        120 => "F2",
        99 => "F3",
        118 => "F4",
        96 => "F5",
        97 => "F6",
        98 => "F7",
        100 => "F8",
        101 => "F9",
        109 => "F10",
        103 => "F11",
        111 => "F12",
        105 => "F13",
        107 => "F14",
        113 => "F15",
        106 => "F16",
        64 => "F17",
        79 => "F18",
        80 => "F19",
        90 => "F20",
        0 => "A",
        1 => "S",
        2 => "D",
        3 => "F",
        4 => "H",
        5 => "G",
        6 => "Z",
        7 => "X",
        8 => "C",
        9 => "V",
        11 => "B",
        12 => "Q",
        13 => "W",
        14 => "E",
        15 => "R",
        16 => "Y",
        17 => "T",
        18 => "1",
        19 => "2",
        20 => "3",
        21 => "4",
        22 => "6",
        23 => "5",
        24 => "=",
        25 => "9",
        26 => "7",
        27 => "-",
        28 => "8",
        29 => "0",
        30 => "]",
        31 => "O",
        32 => "U",
        33 => "[",
        34 => "I",
        35 => "P",
        37 => "L",
        38 => "J",
        39 => "'",
        40 => "K",
        41 => ";",
        42 => "\\",
        43 => ",",
        44 => "/",
        45 => "N",
        46 => "M",
        47 => ".",
        50 => "`",
        key_code => return format!("{label}Key {key_code}"),
    });
    label
}

#[link(name = "Carbon", kind = "framework")]
unsafe extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn InstallEventHandler(
        target: EventTargetRef,
        handler: unsafe extern "C" fn(*mut c_void, *mut c_void, *mut c_void) -> OSStatus,
        count: u32,
        types: *const EventTypeSpec,
        user_data: *mut c_void,
        handler_ref: *mut *mut c_void,
    ) -> OSStatus;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        hot_key_id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hot_key_ref: EventHotKeyRef) -> OSStatus;
}

unsafe extern "C" fn hotkey_handler(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> OSStatus {
    trace::log("e2e event=hotkey_received");
    if let Some(callbacks) = CALLBACKS.get() {
        (callbacks.on_action)(LauncherAction::ToggleRequested);
    }
    0
}

fn install_hotkey(shortcut: LauncherShortcut) -> Result<(), AppError> {
    unsafe {
        let target = GetApplicationEventTarget();
        if !HOTKEY_REGISTERED.swap(true, Ordering::SeqCst) {
            let event = EventTypeSpec {
                event_class: EVENT_CLASS_KEYBOARD,
                event_kind: EVENT_HOT_KEY_PRESSED,
            };
            let mut handler: *mut c_void = std::ptr::null_mut();
            let status = InstallEventHandler(
                target,
                hotkey_handler,
                1,
                &event,
                std::ptr::null_mut(),
                &mut handler,
            );
            if status != 0 {
                HOTKEY_REGISTERED.store(false, Ordering::SeqCst);
                return Err(AppError::backend_unavailable(format!(
                    "Carbon hotkey handler registration failed ({status})"
                )));
            }
        }
        register_hotkey(shortcut, target)
    }
}

static HOTKEY_REF: std::sync::OnceLock<std::sync::Mutex<Option<usize>>> =
    std::sync::OnceLock::new();

fn hotkey_ref() -> &'static std::sync::Mutex<Option<usize>> {
    HOTKEY_REF.get_or_init(|| std::sync::Mutex::new(None))
}

fn register_hotkey(shortcut: LauncherShortcut, target: EventTargetRef) -> Result<(), AppError> {
    let mut current = hotkey_ref()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let old = *current;
    let mut hotkey: EventHotKeyRef = std::ptr::null_mut();
    let status = unsafe {
        RegisterEventHotKey(
            shortcut.key_code,
            shortcut.modifiers,
            EventHotKeyID {
                signature: HOTKEY_ID,
                id: 1,
            },
            target,
            0,
            &mut hotkey,
        )
    };
    if status != 0 {
        return Err(AppError::backend_unavailable(format!(
            "launcher hotkey registration failed ({status})"
        )));
    }
    if let Some(old) = old {
        let _ = unsafe { UnregisterEventHotKey(old as EventHotKeyRef) };
    }
    *current = Some(hotkey as usize);
    Ok(())
}

pub fn reload_hotkey(shortcut: LauncherShortcut) {
    if MainThreadMarker::new().is_some() {
        let target = unsafe { GetApplicationEventTarget() };
        if let Err(error) = register_hotkey(shortcut, target) {
            eprintln!("launcher: {error}");
        }
        return;
    }
    DispatchQueue::main().exec_async(move || unsafe {
        let target = GetApplicationEventTarget();
        if let Err(error) = register_hotkey(shortcut, target) {
            eprintln!("launcher: {error}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        LauncherAction, NSPoint, NSRect, WorkArea, accepts_lifecycle_sequence,
        accepts_newer_revision, centered_top_origin, parse_swift_action, screen_index_for_point,
        swift_requests_history_expansion,
    };

    #[test]
    fn stale_snapshot_revision_is_rejected() {
        assert!(accepts_newer_revision(0, 1));
        assert!(accepts_newer_revision(1, 2));
        assert!(!accepts_newer_revision(2, 2));
        assert!(!accepts_newer_revision(2, 1));
    }

    #[test]
    fn stale_lifecycle_sequence_is_rejected() {
        assert!(accepts_lifecycle_sequence(0, 1));
        assert!(accepts_lifecycle_sequence(1, 2));
        assert!(!accepts_lifecycle_sequence(2, 2));
        assert!(!accepts_lifecycle_sequence(2, 1));
    }

    #[test]
    fn placement_centers_and_preserves_top_anchor() {
        let area = WorkArea {
            x: -1920.0,
            y: 0.0,
            width: 1920.0,
            height: 1080.0,
        };
        let (_, short_y) = centered_top_origin(area, 680.0, 80.0, 72.0);
        let (_, tall_y) = centered_top_origin(area, 680.0, 500.0, 72.0);
        assert_eq!(short_y + 80.0, tall_y + 500.0);
        assert_eq!(centered_top_origin(area, 750.0, 80.0, 72.0).0, -1335.0);
    }

    #[test]
    fn placement_clamps_oversized_panel_to_work_area() {
        let area = WorkArea {
            x: 100.0,
            y: 50.0,
            width: 400.0,
            height: 300.0,
        };
        let (x, y) = centered_top_origin(area, 800.0, 500.0, 72.0);
        assert_eq!((x, y), (100.0, 50.0));
    }

    #[test]
    fn mouse_point_selects_secondary_display_with_negative_origin() {
        let frames = [
            NSRect::new(
                NSPoint::new(0.0, 0.0),
                objc2_foundation::NSSize::new(1920.0, 1080.0),
            ),
            NSRect::new(
                NSPoint::new(-1440.0, 0.0),
                objc2_foundation::NSSize::new(1440.0, 900.0),
            ),
        ];
        assert_eq!(
            screen_index_for_point(NSPoint::new(-700.0, 400.0), &frames),
            Some(1)
        );
        assert_eq!(
            screen_index_for_point(NSPoint::new(2500.0, 400.0), &frames),
            None
        );
    }

    #[test]
    fn swift_new_request_is_parsed_for_deferred_hide() {
        let (action, hide) =
            parse_swift_action(br#"{"type":"new_request","prompt":" ask this "}"#).unwrap();
        assert_eq!(
            action,
            LauncherAction::NewRequest {
                prompt: "ask this".into(),
                share_context: true,
                read_only: false,
            }
        );
        assert!(hide);
    }

    #[test]
    fn swift_new_request_can_disable_window_context() {
        let (action, _) = parse_swift_action(
            br#"{"type":"new_request","prompt":"ask this","share_context":false}"#,
        )
        .unwrap();
        assert_eq!(
            action,
            LauncherAction::NewRequest {
                prompt: "ask this".into(),
                share_context: false,
                read_only: false,
            }
        );
    }

    #[test]
    fn swift_new_request_can_enable_read_only_mode() {
        let (action, _) =
            parse_swift_action(br#"{"type":"new_request","prompt":"review","read_only":true}"#)
                .unwrap();
        assert_eq!(
            action,
            LauncherAction::NewRequest {
                prompt: "review".into(),
                share_context: true,
                read_only: true,
            }
        );
    }

    #[test]
    fn swift_follow_up_can_disable_window_context() {
        let (action, _) = parse_swift_action(
            br#"{"type":"follow_up","session_id":"session","prompt":"ask this","share_context":false}"#,
        )
        .unwrap();
        assert_eq!(
            action,
            LauncherAction::FollowUp {
                session_id: "session".into(),
                prompt: "ask this".into(),
                share_context: false,
                read_only: false,
            }
        );
    }

    #[test]
    fn malformed_swift_action_is_ignored() {
        assert!(parse_swift_action(br#"{"type":"new_request","prompt":"  "}"#).is_none());
        assert!(parse_swift_action(br#"{"type":"unknown"}"#).is_none());
        assert!(parse_swift_action(b"not-json").is_none());
    }

    #[test]
    fn swift_history_expansion_action_is_recognized() {
        assert!(swift_requests_history_expansion(
            br#"{"type":"expand_history"}"#
        ));
        assert!(!swift_requests_history_expansion(br#"{"type":"unknown"}"#));
    }
}

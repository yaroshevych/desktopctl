// Native Spotlight-style launcher UI.
//
// This module intentionally owns only AppKit presentation and hotkey plumbing. The
// daemon/controller supplies session data through `refresh` and receives user
// intent through `LauncherCallbacks::on_action`. `initialize`, `show`, `hide`,
// `toggle`, `refresh`, and `show_completion` must be called from the AppKit main
// thread, except that the latter four are safe to call from a worker: they post
// their work to the main queue.
//
// The controller should bind the active DesktopCtl window before presenting this
// panel, then include that binding in the Pi prompt. The panel deliberately does
// not call the daemon itself, so opening it cannot change the target before the
// controller has captured it.

use std::{
    cell::RefCell,
    ffi::c_void,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    sync::{Arc, OnceLock},
    thread,
    time::Duration,
};

use dispatch2::DispatchQueue;
use objc2::{
    MainThreadOnly, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
};
use objc2_app_kit::{
    NSAnimationContext, NSApplication, NSBackingStoreType, NSColor, NSEvent,
    NSEventModifierFlags, NSFloatingWindowLevel, NSFont, NSPanel, NSTextField, NSView,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use super::swift_bridge;
use desktop_core::error::AppError;

const PANEL_WIDTH: f64 = 700.0;
const SESSION_PANEL_HEIGHT: f64 = 360.0;
const MAX_HISTORY_PANEL_HEIGHT: f64 = 450.0;
const MIN_LAUNCHER_PANEL_HEIGHT: f64 = 50.0;
const COMPLETION_WIDTH: f64 = 520.0;
const COMPLETION_HEIGHT: f64 = 48.0;
const COMPLETION_FADE_SECONDS: f64 = 0.2;
const COMPLETION_VISIBLE_MILLIS: u64 = 1_600;
// Keep this in sync with the intrinsic two-line SwiftUI session row.
const ROW_HEIGHT: f64 = 42.0;
const ROW_SPACING: f64 = 2.0;
const LIST_VERTICAL_INSET: f64 = 16.0;
const KEY_RETURN: u16 = 36;
const KEY_ENTER: u16 = 76;
const KEY_ESCAPE: u16 = 53;
const KEY_UP: u16 = 126;
const KEY_DOWN: u16 = 125;

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
            Bool::new(handle_key_event(event))
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
            if self.isKeyWindow() {
                return;
            }
            hide_on_main();
        }
    }
);

struct UiState {
    panel: Option<Retained<LauncherPanel>>,
    content: Option<Retained<NSView>>,
    show_all: bool,
    completion_panel: Option<Retained<NSPanel>>,
    completion_label: Option<Retained<NSTextField>>,
    completion_generation: u64,
    anchor_visible_frame: Option<NSRect>,
    lifecycle_sequence: u64,
    snapshot: LauncherSnapshot,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            panel: None,
            content: None,
            show_all: false,
            completion_panel: None,
            completion_label: None,
            completion_generation: 0,
            anchor_visible_frame: None,
            lifecycle_sequence: 0,
            snapshot: LauncherSnapshot::default(),
        }
    }
}

thread_local! { static UI: RefCell<UiState> = RefCell::new(UiState::default()); }

/// Register the native Option-Space hotkey and prepare the panel. Call once from
/// the existing `NSApplication` main thread before `NSApplication::run`.
pub fn initialize(callbacks: LauncherCallbacks) -> Result<(), AppError> {
    if CALLBACKS.set(callbacks).is_err() {
        return Ok(());
    }
    let Some(mtm) = MainThreadMarker::new() else {
        return Err(AppError::backend_unavailable(
            "launcher initialization must run on the main thread",
        ));
    };
    install_hotkey()?;
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
    DispatchQueue::main().exec_async(move || {
        let accepted = UI.with(|cell| {
            let mut ui = cell.borrow_mut();
            if !accepts_newer_revision(ui.snapshot.revision, snapshot.revision) {
                return false;
            }
            ui.snapshot = snapshot.clone();
            true
        });
        if !accepted {
            return;
        }
        if is_visible() {
            render_on_main(false);
        }
    });
}

fn accepts_newer_revision(current: u64, incoming: u64) -> bool {
    incoming > current
}

fn accepts_lifecycle_sequence(current: u64, incoming: u64) -> bool {
    incoming > current
}

pub fn show_completion(notice: CompletionNotice) {
    DispatchQueue::main().exec_async(move || {
        if is_visible() {
            return;
        }
        show_completion_on_main(notice);
    });
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
        return;
    }
    let Ok(parsed) = std::panic::catch_unwind(|| parse_swift_action(bytes)) else {
        return;
    };
    let Some((launcher_action, hide_before_dispatch)) = parsed else {
        return;
    };
    // Swift can call back while Rust is updating the hosted view under a UiState
    // RefCell borrow. Defer all UI/controller work until the callback unwinds.
    DispatchQueue::main().exec_async(move || {
        if hide_before_dispatch {
            hide_on_main();
        }
        if let Some(callbacks) = CALLBACKS.get() {
            (callbacks.on_action)(launcher_action);
        }
    });
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
                share_context: true,
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
                share_context: true,
            }
        }
        Some("cancel_session") => {
            let session_id = action.get("session_id").and_then(|value| value.as_str())?;
            LauncherAction::CancelSession {
                session_id: session_id.to_owned(),
            }
        }
        Some("open_in_ghostty") => {
            let session_id = action.get("session_id").and_then(|value| value.as_str())?;
            LauncherAction::OpenInGhostty {
                session_id: session_id.to_owned(),
            }
        }
        Some("return_to_launcher") => LauncherAction::ReturnToLauncher,
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
    let expanded = UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        if matches!(ui.snapshot.screen, LauncherScreen::Launcher)
            && ui.snapshot.all.len() > ui.snapshot.recent.len()
        {
            ui.show_all = true;
            true
        } else {
            false
        }
    });
    if expanded {
        render_on_main(true);
    }
}

fn show_completion_on_main(notice: CompletionNotice) {
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
            panel.setHasShadow(true);
            panel.setOpaque(false);
            panel.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
            panel.setCollectionBehavior(
                NSWindowCollectionBehavior::CanJoinAllSpaces
                    | NSWindowCollectionBehavior::FullScreenAuxiliary
                    | NSWindowCollectionBehavior::Stationary,
            );
            panel.setIgnoresMouseEvents(true);
            panel.setLevel(NSFloatingWindowLevel);

            let label = text_field(
                mtm,
                "",
                NSRect::new(
                    NSPoint::new(16.0, 8.0),
                    NSSize::new(COMPLETION_WIDTH - 32.0, 32.0),
                ),
                false,
            );
            label.setSelectable(false);
            label.setLineBreakMode(objc2_app_kit::NSLineBreakMode::ByTruncatingTail);
            panel.setContentView(Some(&label));
            ui.completion_label = Some(label);
            ui.completion_panel = Some(panel);
        }

        ui.completion_generation = ui.completion_generation.wrapping_add(1);
        let generation = ui.completion_generation;
        let panel = ui.completion_panel.as_ref().unwrap();
        let preview = if notice.answer_preview.trim().is_empty() {
            notice.title
        } else {
            notice.answer_preview
        };
        ui.completion_label
            .as_ref()
            .unwrap()
            .setStringValue(&NSString::from_str(&one_line(&preview, 120)));
        position_completion(panel, ui.anchor_visible_frame);
        panel.setAlphaValue(0.0);
        panel.orderFrontRegardless();
        animate_alpha(panel, 1.0);
        generation
    });

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
        UI.with(|cell| cell.borrow_mut().show_all = false);
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
        PANEL_WIDTH,
        panel.frame().size.height,
        72.0,
    );
    panel.setFrameOrigin(NSPoint::new(x, y));
}

fn render_on_main(animate_resize: bool) {
    UI.with(|cell| {
        let ui = cell.borrow();
        let Some(content) = ui.content.as_ref().cloned() else {
            return;
        };
        let height = match ui.snapshot.screen {
            LauncherScreen::Launcher => launcher_panel_height(&ui),
            LauncherScreen::Session { .. } => SESSION_PANEL_HEIGHT,
        };
        resize_panel(&ui, &content, height, animate_resize);
        swift_bridge::set_snapshot(&ui.snapshot);
    });
}

fn launcher_panel_height(ui: &UiState) -> f64 {
    let session_count = if ui.show_all {
        ui.snapshot.all.len()
    } else {
        ui.snapshot.recent.len()
    };
    let list_inset = if session_count == 0 {
        0.0
    } else {
        LIST_VERTICAL_INSET
    };
    let rows_height = session_count as f64 * ROW_HEIGHT
        + session_count.saturating_sub(1) as f64 * ROW_SPACING;
    (50.0 + list_inset + rows_height)
        .clamp(MIN_LAUNCHER_PANEL_HEIGHT, MAX_HISTORY_PANEL_HEIGHT)
}

fn resize_panel(ui: &UiState, content: &NSView, height: f64, animate: bool) {
    let Some(panel) = ui.panel.as_ref() else {
        return;
    };
    if animate && panel.isVisible() {
        let frame = panel.frame();
        let top = frame.origin.y + frame.size.height;
        let target = NSRect::new(
            NSPoint::new(frame.origin.x, top - height),
            NSSize::new(PANEL_WIDTH, height),
        );
        NSAnimationContext::beginGrouping();
        NSAnimationContext::currentContext().setDuration(0.24);
        unsafe {
            let animator: *mut AnyObject = msg_send![panel, animator];
            let _: () = msg_send![animator, setFrame: target, display: true];
        }
        NSAnimationContext::endGrouping();
        return;
    }
    panel.setContentSize(NSSize::new(PANEL_WIDTH, height));
    content.setFrameSize(NSSize::new(PANEL_WIDTH, height));
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
    let command_comma = modifiers == NSEventModifierFlags::Command
        && characters.as_deref() == Some(",");
    if command_comma {
        hide_on_main();
        if let Some(callbacks) = CALLBACKS.get() {
            (callbacks.on_action)(LauncherAction::OpenSettings);
        }
        return true;
    }
    let command_k = modifiers == NSEventModifierFlags::Command
        && characters.as_deref().is_some_and(|value| value.eq_ignore_ascii_case("k"));
    if command_k {
        swift_bridge::toggle_actions_menu();
        return true;
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
                return true;
            }
            swift_bridge::move_selection(-1);
            true
        }
        KEY_DOWN => {
            if swift_bridge::actions_menu_handles_navigation() {
                return true;
            }
            swift_bridge::move_selection(1);
            true
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
const OPTION_KEY: u32 = 1 << 11;
const SPACE_KEYCODE: u32 = 49;

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
}

unsafe extern "C" fn hotkey_handler(_: *mut c_void, _: *mut c_void, _: *mut c_void) -> OSStatus {
    if let Some(callbacks) = CALLBACKS.get() {
        (callbacks.on_action)(LauncherAction::ToggleRequested);
    }
    0
}

fn install_hotkey() -> Result<(), AppError> {
    if HOTKEY_REGISTERED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }
    unsafe {
        let event = EventTypeSpec {
            event_class: EVENT_CLASS_KEYBOARD,
            event_kind: EVENT_HOT_KEY_PRESSED,
        };
        let mut handler: *mut c_void = std::ptr::null_mut();
        let target = GetApplicationEventTarget();
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
        let mut hotkey: EventHotKeyRef = std::ptr::null_mut();
        let status = RegisterEventHotKey(
            SPACE_KEYCODE,
            OPTION_KEY,
            EventHotKeyID {
                signature: HOTKEY_ID,
                id: 1,
            },
            target,
            0,
            &mut hotkey,
        );
        if status != 0 {
            HOTKEY_REGISTERED.store(false, Ordering::SeqCst);
            return Err(AppError::backend_unavailable(format!(
                "Option-Space registration failed ({status})"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{
        LauncherAction, NSPoint, NSRect, WorkArea, accepts_lifecycle_sequence,
        accepts_newer_revision, centered_top_origin, parse_swift_action,
        screen_index_for_point, swift_requests_history_expansion,
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
            }
        );
        assert!(hide);
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
        assert!(!swift_requests_history_expansion(
            br#"{"type":"unknown"}"#
        ));
    }
}

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
    sync::atomic::{AtomicBool, Ordering},
    sync::{Arc, OnceLock},
};

use dispatch2::DispatchQueue;
use objc2::{
    MainThreadOnly, class, define_class, msg_send,
    rc::Retained,
    runtime::{AnyObject, Bool},
    sel,
};
use objc2_app_kit::{
    NSBackingStoreType, NSButton, NSColor, NSEvent, NSFont, NSPanel, NSProgressIndicator,
    NSProgressIndicatorStyle, NSScrollView, NSTextAlignment, NSTextField, NSTextView, NSView,
    NSWindowCollectionBehavior, NSWindowDelegate, NSWindowStyleMask,
};
use objc2_foundation::{
    MainThreadMarker, NSNotification, NSObjectProtocol, NSPoint, NSRect, NSSize, NSString,
};

use desktop_core::error::AppError;

const PANEL_WIDTH: f64 = 680.0;
const PANEL_HEIGHT: f64 = 360.0;
const INPUT_HEIGHT: f64 = 38.0;
const ROW_HEIGHT: f64 = 42.0;
const KEY_ESCAPE: u16 = 53;
const KEY_RETURN: u16 = 36;
const KEY_ENTER: u16 = 76;
const KEY_UP: u16 = 126;
const KEY_DOWN: u16 = 125;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionStatus {
    Running,
    Completed,
    Failed,
    Cancelled,
}

impl SessionStatus {
    fn label(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Completed => "complete",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    pub preview: String,
    pub status: SessionStatus,
    pub unread: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TranscriptMessage {
    pub user: bool,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LauncherSnapshot {
    pub screen: LauncherScreen,
    pub recent: Vec<SessionSummary>,
}

impl Default for LauncherSnapshot {
    fn default() -> Self {
        Self {
            screen: LauncherScreen::Launcher,
            recent: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionNotice {
    pub title: String,
    pub answer_preview: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LauncherAction {
    ToggleRequested,
    ReturnToLauncher,
    NewRequest { prompt: String },
    FollowUp { session_id: String, prompt: String },
    OpenSession { session_id: String },
    CancelSession { session_id: String },
    OpenInTerminal { session_id: String },
}

pub type LauncherActionHandler = Arc<dyn Fn(LauncherAction) + Send + Sync + 'static>;

#[derive(Clone)]
pub struct LauncherCallbacks {
    pub on_action: LauncherActionHandler,
}

static CALLBACKS: OnceLock<LauncherCallbacks> = OnceLock::new();
static VISIBLE: AtomicBool = AtomicBool::new(false);
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

        #[unsafe(method(submit:))]
        fn submit(&self, _sender: Option<&AnyObject>) { submit_active(); }

        #[unsafe(method(openRow:))]
        fn open_row(&self, sender: &AnyObject) {
            let tag: isize = unsafe { msg_send![sender, tag] };
            open_row(tag.max(0) as usize);
        }

        #[unsafe(method(back:))]
        fn back(&self, _sender: Option<&AnyObject>) {
            show_launcher_on_main();
            if let Some(callbacks) = CALLBACKS.get() {
                (callbacks.on_action)(LauncherAction::ReturnToLauncher);
            }
        }

        #[unsafe(method(cancelSession:))]
        fn cancel_session(&self, _sender: Option<&AnyObject>) {
            let session_id = UI.with(|cell| cell.borrow().session_id.clone());
            if let (Some(callbacks), Some(session_id)) = (CALLBACKS.get(), session_id) {
                (callbacks.on_action)(LauncherAction::CancelSession { session_id });
            }
        }

        #[unsafe(method(openInTerminal:))]
        fn open_in_terminal(&self, _sender: Option<&AnyObject>) {
            let session_id = UI.with(|cell| cell.borrow().session_id.clone());
            if let (Some(callbacks), Some(session_id)) = (CALLBACKS.get(), session_id) {
                (callbacks.on_action)(LauncherAction::OpenInTerminal { session_id });
            }
        }
    }

    // SAFETY: NSWindowDelegate has no additional invariants for these methods.
    unsafe impl NSWindowDelegate for LauncherPanel {
        #[unsafe(method(windowDidResignKey:))]
        fn window_did_resign_key(&self, _notification: &NSNotification) {
            hide_on_main();
        }
    }
);

struct UiState {
    panel: Option<Retained<LauncherPanel>>,
    content: Option<Retained<NSView>>,
    input: Option<Retained<NSTextField>>,
    composer: Option<Retained<NSTextField>>,
    transcript: Option<Retained<NSTextView>>,
    transcript_scroll: Option<Retained<NSScrollView>>,
    rows: Vec<Retained<NSButton>>,
    back: Option<Retained<NSButton>>,
    activity: Option<Retained<NSProgressIndicator>>,
    status_label: Option<Retained<NSTextField>>,
    stop: Option<Retained<NSButton>>,
    terminal: Option<Retained<NSButton>>,
    snapshot: LauncherSnapshot,
    selected: Option<usize>,
    session_id: Option<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            panel: None,
            content: None,
            input: None,
            composer: None,
            transcript: None,
            transcript_scroll: None,
            rows: Vec::new(),
            back: None,
            activity: None,
            status_label: None,
            stop: None,
            terminal: None,
            snapshot: LauncherSnapshot::default(),
            selected: None,
            session_id: None,
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

pub fn show() {
    DispatchQueue::main().exec_async(show_on_main);
}

pub fn hide() {
    DispatchQueue::main().exec_async(hide_on_main);
}

pub fn refresh(snapshot: LauncherSnapshot) {
    DispatchQueue::main().exec_async(move || {
        UI.with(|cell| cell.borrow_mut().snapshot = snapshot);
        if is_visible() {
            render_on_main();
        }
    });
}

pub fn show_completion(notice: CompletionNotice) {
    DispatchQueue::main().exec_async(move || {
        if is_visible() {
            return;
        }
        unsafe {
            let notification: *mut AnyObject = msg_send![class!(NSUserNotification), new];
            if notification.is_null() {
                return;
            }
            let title = NSString::from_str(&format!("DesktopCtl: {}", notice.title));
            let body = NSString::from_str(&notice.answer_preview);
            let _: () = msg_send![notification, setTitle: &*title];
            let _: () = msg_send![notification, setInformativeText: &*body];
            let center: *mut AnyObject = msg_send![
                class!(NSUserNotificationCenter),
                defaultUserNotificationCenter
            ];
            if !center.is_null() {
                let _: () = msg_send![center, deliverNotification: notification];
            }
        }
    });
}

fn create_panel(mtm: MainThreadMarker) -> Result<(), AppError> {
    UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        if ui.panel.is_some() { return Ok(()); }
        let panel = unsafe {
            let allocated = LauncherPanel::alloc(mtm).set_ivars(LauncherPanelIvars);
            msg_send![super(allocated), initWithContentRect: NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_WIDTH, PANEL_HEIGHT)), styleMask: NSWindowStyleMask::Borderless | NSWindowStyleMask::NonactivatingPanel, backing: NSBackingStoreType::Buffered, defer: false]
        };
        let panel: Retained<LauncherPanel> = panel;
        unsafe { panel.setReleasedWhenClosed(false); }
        panel.setFloatingPanel(true);
        panel.setBecomesKeyOnlyIfNeeded(false);
        panel.setHidesOnDeactivate(true);
        panel.setHasShadow(true);
        panel.setBackgroundColor(Some(&NSColor::windowBackgroundColor()));
        panel.setOpaque(false);
        panel.setCollectionBehavior(NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::FullScreenAuxiliary | NSWindowCollectionBehavior::Stationary);
        panel.setDelegate(Some(objc2::runtime::ProtocolObject::from_ref(&*panel)));

        let content = NSView::initWithFrame(NSView::alloc(mtm), NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(PANEL_WIDTH, PANEL_HEIGHT)));
        content.setWantsLayer(true);
        panel.setContentView(Some(&content));
        let input = text_field(mtm, "Ask DesktopCtl…", NSRect::new(NSPoint::new(18.0, PANEL_HEIGHT - 58.0), NSSize::new(PANEL_WIDTH - 36.0, INPUT_HEIGHT)), true);
        unsafe {
            input.setTarget(Some(&*panel));
            input.setAction(Some(sel!(submit:)));
        }
        content.addSubview(&input);
        ui.panel = Some(panel); ui.content = Some(content); ui.input = Some(input);
        Ok(())
    })
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
    if MainThreadMarker::new().is_none() {
        return;
    }
    UI.with(|cell| {
        if cell.borrow().panel.is_none() {
            return;
        }
        render_on_main();
        let ui = cell.borrow();
        let Some(panel) = ui.panel.as_ref() else {
            return;
        };
        position_panel(panel);
        panel.makeKeyAndOrderFront(None);
        if let Some(composer) = ui.composer.as_ref() {
            panel.makeFirstResponder(Some(composer));
        } else if let Some(input) = ui.input.as_ref() {
            panel.makeFirstResponder(Some(input));
        }
        VISIBLE.store(true, Ordering::SeqCst);
    });
}

fn hide_on_main() {
    UI.with(|cell| {
        if let Some(panel) = cell.borrow().panel.as_ref() {
            panel.orderOut(None);
        }
        VISIBLE.store(false, Ordering::SeqCst);
    });
}

fn position_panel(panel: &NSPanel) {
    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    let screen = objc2_app_kit::NSScreen::mainScreen(mtm);
    let Some(screen) = screen else {
        return;
    };
    let frame = screen.visibleFrame();
    panel.setFrameOrigin(NSPoint::new(
        frame.origin.x + (frame.size.width - PANEL_WIDTH) / 2.0,
        frame.origin.y + frame.size.height - PANEL_HEIGHT - 72.0,
    ));
}

fn render_on_main() {
    UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        let Some(content) = ui.content.as_ref().cloned() else {
            return;
        };
        clear_dynamic_views(&mut ui);
        match ui.snapshot.screen.clone() {
            LauncherScreen::Launcher => render_launcher(&mut ui, &content),
            LauncherScreen::Session {
                id,
                title,
                status,
                terminal_available,
                messages,
            } => render_session(
                &mut ui,
                &content,
                &id,
                &title,
                status,
                terminal_available,
                &messages,
            ),
        }
    });
}

fn clear_dynamic_views(ui: &mut UiState) {
    for row in ui.rows.drain(..) {
        row.removeFromSuperview();
    }
    ui.transcript.take();
    if let Some(scroll) = ui.transcript_scroll.take() {
        scroll.removeFromSuperview();
    }
    if let Some(composer) = ui.composer.take() {
        composer.removeFromSuperview();
    }
    if let Some(back) = ui.back.take() {
        back.removeFromSuperview();
    }
    if let Some(activity) = ui.activity.take() {
        unsafe { activity.stopAnimation(None) };
        activity.removeFromSuperview();
    }
    if let Some(label) = ui.status_label.take() {
        label.removeFromSuperview();
    }
    if let Some(stop) = ui.stop.take() {
        stop.removeFromSuperview();
    }
    if let Some(terminal) = ui.terminal.take() {
        terminal.removeFromSuperview();
    }
}

fn render_launcher(ui: &mut UiState, content: &NSView) {
    if let Some(input) = ui.input.as_ref() {
        input.setHidden(false);
    }
    ui.selected = None;
    ui.session_id = None;
    for (index, session) in ui.snapshot.recent.iter().take(6).enumerate() {
        let y = PANEL_HEIGHT - 104.0 - index as f64 * ROW_HEIGHT;
        let title = format!(
            "{}  ·  {}  {}",
            if session.unread { "●" } else { "○" },
            session.title,
            session.status.label()
        );
        let button = NSButton::initWithFrame(
            NSButton::alloc(MainThreadMarker::new().unwrap()),
            NSRect::new(NSPoint::new(18.0, y), NSSize::new(PANEL_WIDTH - 36.0, 34.0)),
        );
        button.setTitle(&NSString::from_str(&format!(
            "{title}\n{}",
            one_line(&session.preview, 110)
        )));
        button.setBordered(false);
        button.setAlignment(NSTextAlignment::Left);
        button.setFont(Some(&NSFont::systemFontOfSize(13.0)));
        button.setTag(index as isize);
        unsafe {
            button.setTarget(Some(ui.panel.as_ref().unwrap()));
            button.setAction(Some(sel!(openRow:)));
        }
        content.addSubview(&button);
        ui.rows.push(button);
    }
}

fn render_session(
    ui: &mut UiState,
    content: &NSView,
    id: &str,
    title: &str,
    status: SessionStatus,
    terminal_available: bool,
    messages: &[TranscriptMessage],
) {
    if let Some(input) = ui.input.as_ref() {
        input.setHidden(true);
    }
    ui.session_id = Some(id.to_string());
    let back = NSButton::initWithFrame(
        NSButton::alloc(MainThreadMarker::new().unwrap()),
        NSRect::new(
            NSPoint::new(18.0, PANEL_HEIGHT - 44.0),
            NSSize::new(100.0, 26.0),
        ),
    );
    back.setTitle(&NSString::from_str("‹ Sessions"));
    back.setBordered(false);
    back.setAlignment(NSTextAlignment::Left);
    unsafe {
        back.setTarget(Some(ui.panel.as_ref().unwrap()));
        back.setAction(Some(sel!(back:)));
    }
    content.addSubview(&back);
    ui.back = Some(back);
    if terminal_available {
        let terminal = NSButton::initWithFrame(
            NSButton::alloc(MainThreadMarker::new().unwrap()),
            NSRect::new(
                NSPoint::new(116.0, PANEL_HEIGHT - 46.0),
                NSSize::new(132.0, 26.0),
            ),
        );
        terminal.setTitle(&NSString::from_str("Open in Terminal"));
        terminal.setBezelStyle(objc2_app_kit::NSBezelStyle::Push);
        unsafe {
            terminal.setTarget(Some(ui.panel.as_ref().unwrap()));
            terminal.setAction(Some(sel!(openInTerminal:)));
        }
        content.addSubview(&terminal);
        ui.terminal = Some(terminal);
    }
    if status == SessionStatus::Running {
        let activity = NSProgressIndicator::initWithFrame(
            NSProgressIndicator::alloc(MainThreadMarker::new().unwrap()),
            NSRect::new(
                NSPoint::new(PANEL_WIDTH - 194.0, PANEL_HEIGHT - 42.0),
                NSSize::new(18.0, 18.0),
            ),
        );
        activity.setStyle(NSProgressIndicatorStyle::Spinning);
        activity.setIndeterminate(true);
        unsafe { activity.startAnimation(None) };
        content.addSubview(&activity);
        ui.activity = Some(activity);

        let label = text_field(
            MainThreadMarker::new().unwrap(),
            "",
            NSRect::new(
                NSPoint::new(PANEL_WIDTH - 170.0, PANEL_HEIGHT - 44.0),
                NSSize::new(100.0, 22.0),
            ),
            false,
        );
        label.setStringValue(&NSString::from_str("Pi is working…"));
        content.addSubview(&label);
        ui.status_label = Some(label);

        let stop = NSButton::initWithFrame(
            NSButton::alloc(MainThreadMarker::new().unwrap()),
            NSRect::new(
                NSPoint::new(PANEL_WIDTH - 70.0, PANEL_HEIGHT - 46.0),
                NSSize::new(54.0, 26.0),
            ),
        );
        stop.setTitle(&NSString::from_str("Stop"));
        stop.setBezelStyle(objc2_app_kit::NSBezelStyle::Push);
        unsafe {
            stop.setTarget(Some(ui.panel.as_ref().unwrap()));
            stop.setAction(Some(sel!(cancelSession:)));
        }
        content.addSubview(&stop);
        ui.stop = Some(stop);
    }
    let transcript = NSTextView::initWithFrame(
        NSTextView::alloc(MainThreadMarker::new().unwrap()),
        NSRect::new(
            NSPoint::new(18.0, 62.0),
            NSSize::new(PANEL_WIDTH - 36.0, PANEL_HEIGHT - 116.0),
        ),
    );
    transcript.setEditable(false);
    transcript.setSelectable(true);
    let mut text = format!("{title}\n\n");
    for message in messages {
        text.push_str(if message.user { "You: " } else { "Pi: " });
        text.push_str(&message.text);
        text.push_str("\n\n");
    }
    unsafe {
        let _: () = msg_send![&*transcript, setString: &*NSString::from_str(&text)];
    }
    let scroll = NSScrollView::initWithFrame(
        NSScrollView::alloc(MainThreadMarker::new().unwrap()),
        NSRect::new(
            NSPoint::new(18.0, 62.0),
            NSSize::new(PANEL_WIDTH - 36.0, PANEL_HEIGHT - 116.0),
        ),
    );
    scroll.setHasVerticalScroller(true);
    scroll.setDocumentView(Some(&transcript));
    content.addSubview(&scroll);
    unsafe {
        let _: () = msg_send![&*transcript, scrollToEndOfDocument: std::ptr::null::<AnyObject>()];
    }
    ui.transcript = Some(transcript);
    ui.transcript_scroll = Some(scroll);
    let composer = text_field(
        MainThreadMarker::new().unwrap(),
        if status == SessionStatus::Running {
            "Wait for Pi to finish…"
        } else {
            "Follow up…"
        },
        NSRect::new(
            NSPoint::new(18.0, 18.0),
            NSSize::new(PANEL_WIDTH - 36.0, 34.0),
        ),
        true,
    );
    composer.setEditable(status != SessionStatus::Running);
    unsafe {
        composer.setTarget(Some(ui.panel.as_ref().unwrap()));
        composer.setAction(Some(sel!(submit:)));
    }
    content.addSubview(&composer);
    ui.composer = Some(composer);
}

fn submit_active() {
    let selected = UI.with(|cell| {
        let ui = cell.borrow();
        if matches!(ui.snapshot.screen, LauncherScreen::Launcher) {
            ui.selected
        } else {
            None
        }
    });
    if let Some(index) = selected {
        open_row(index);
        return;
    }
    let action = UI.with(|cell| {
        let ui = cell.borrow();
        let field = if ui.session_id.is_some() {
            ui.composer.as_ref()
        } else {
            ui.input.as_ref()
        }?;
        let text = field.stringValue().to_string();
        let text = text.trim().to_string();
        if text.is_empty() {
            return None;
        }
        field.setStringValue(&NSString::new());
        ui.session_id
            .as_ref()
            .map(|id| LauncherAction::FollowUp {
                session_id: id.clone(),
                prompt: text.clone(),
            })
            .or_else(|| Some(LauncherAction::NewRequest { prompt: text }))
    });
    if let Some(action) = action {
        if matches!(action, LauncherAction::NewRequest { .. }) {
            hide_on_main();
        }
        if let Some(callbacks) = CALLBACKS.get() {
            (callbacks.on_action)(action);
        }
    }
}

fn handle_key_event(event: &NSEvent) -> bool {
    if !VISIBLE.load(Ordering::SeqCst) {
        return false;
    }
    match event.keyCode() {
        KEY_ESCAPE => {
            hide_on_main();
            true
        }
        KEY_RETURN | KEY_ENTER => {
            submit_active();
            true
        }
        KEY_UP => {
            move_selection(-1);
            true
        }
        KEY_DOWN => {
            move_selection(1);
            true
        }
        _ => false,
    }
}

fn open_row(index: usize) {
    let id = UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        let row = ui.snapshot.recent.get(index)?.clone();
        ui.snapshot.screen = LauncherScreen::Session {
            id: row.id.clone(),
            title: row.title,
            status: row.status,
            terminal_available: false,
            messages: Vec::new(),
        };
        ui.selected = None;
        Some(row.id)
    });
    if let Some(session_id) = id {
        render_on_main();
        if let Some(callbacks) = CALLBACKS.get() {
            (callbacks.on_action)(LauncherAction::OpenSession { session_id });
        }
    }
}

fn move_selection(delta: isize) {
    UI.with(|cell| {
        let mut ui = cell.borrow_mut();
        if !matches!(ui.snapshot.screen, LauncherScreen::Launcher) {
            return;
        }
        let count = ui.snapshot.recent.len().min(6);
        if count == 0 {
            return;
        }
        let old = ui
            .selected
            .map(|value| value as isize)
            .unwrap_or_else(|| if delta < 0 { 0 } else { -1 });
        ui.selected = Some((old + delta).rem_euclid(count as isize) as usize);
        for (idx, row) in ui.rows.iter().enumerate() {
            row.highlight(ui.selected == Some(idx));
        }
    });
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

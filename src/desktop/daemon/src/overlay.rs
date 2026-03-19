use std::{
    cell::RefCell,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

use desktop_core::{
    error::AppError,
    protocol::{TokenEntry, TokenizePayload, TokenizeWindow},
};
use dispatch2::DispatchQueue;
use objc2::{MainThreadMarker, MainThreadOnly, msg_send, rc::Retained};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFloatingWindowLevel, NSScreen,
    NSTitlePosition, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::trace;

const MAX_OVERLAY_RECTS: usize = 900;
const PROBE_WIDTH: f64 = 120.0;
const PROBE_HEIGHT: f64 = 80.0;

#[derive(Debug, Clone)]
struct OverlayRect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    kind: OverlayKind,
}

#[derive(Debug, Clone, Copy)]
enum OverlayKind {
    Text,
    Box,
    Glyph,
}

#[derive(Default)]
struct OverlayUiState {
    window: Option<Retained<NSWindow>>,
    content_view: Option<Retained<NSView>>,
    token_views: Vec<Retained<NSBox>>,
    screen_frame: Option<NSRect>,
}

thread_local! {
    static OVERLAY_UI: RefCell<OverlayUiState> = RefCell::new(OverlayUiState::default());
}

static OVERLAY_ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn is_active() -> bool {
    OVERLAY_ACTIVE.load(Ordering::SeqCst)
}

pub fn start_overlay() -> Result<bool, AppError> {
    if OVERLAY_ACTIVE
        .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        trace::log("overlay:start already_active");
        return Ok(false);
    }
    trace::log("overlay:start requested");
    if let Err(err) = run_on_main_sync(start_overlay_on_main) {
        OVERLAY_ACTIVE.store(false, Ordering::SeqCst);
        trace::log(format!("overlay:start failed {err}"));
        return Err(err);
    }
    trace::log("overlay:start ok");
    Ok(true)
}

pub fn stop_overlay() -> Result<bool, AppError> {
    if OVERLAY_ACTIVE
        .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
        .is_err()
    {
        trace::log("overlay:stop already_stopped");
        return Ok(false);
    }
    trace::log("overlay:stop requested");
    run_on_main_sync(stop_overlay_on_main)?;
    trace::log("overlay:stop ok");
    Ok(true)
}

pub fn update_from_tokenize(payload: &TokenizePayload) -> Result<(), AppError> {
    if !OVERLAY_ACTIVE.load(Ordering::SeqCst) {
        return Ok(());
    }
    let rects = overlay_rects_from_payload(payload);
    trace::log(format!(
        "overlay:update windows={} tokens={} rects={}",
        payload.windows.len(),
        payload.tokens.len(),
        rects.len()
    ));
    dispatch_main(move || {
        apply_overlay_update_on_main(rects);
    });
    Ok(())
}

fn start_overlay_on_main() -> Result<(), String> {
    OVERLAY_UI.with(|cell| {
        let mut state = cell.borrow_mut();
        if state.window.is_some() {
            return Ok(());
        }
        let mtm = MainThreadMarker::new().ok_or("overlay start must run on main thread")?;
        let screen = NSScreen::mainScreen(mtm).ok_or("no main screen available for overlay")?;
        let frame = screen.frame();

        let window = unsafe {
            NSWindow::initWithContentRect_styleMask_backing_defer(
                NSWindow::alloc(mtm),
                frame,
                NSWindowStyleMask::Borderless,
                NSBackingStoreType::Buffered,
                false,
            )
        };
        window.setBackgroundColor(Some(&NSColor::clearColor()));
        window.setOpaque(false);
        window.setHasShadow(false);
        window.setIgnoresMouseEvents(true);
        // Keep overlay above regular app windows while avoiding extreme system levels.
        window.setLevel(NSFloatingWindowLevel + 20);
        window.setCollectionBehavior(
            NSWindowCollectionBehavior::CanJoinAllSpaces | NSWindowCollectionBehavior::Stationary,
        );
        unsafe {
            window.setReleasedWhenClosed(false);
        }

        let content_view = NSView::initWithFrame(
            NSView::alloc(mtm),
            NSRect::new(NSPoint::new(0.0, 0.0), frame.size),
        );
        content_view.setWantsLayer(true);
        window.setContentView(Some(&content_view));
        window.orderFrontRegardless();

        state.window = Some(window);
        state.content_view = Some(content_view);
        state.token_views.clear();
        state.screen_frame = Some(frame);
        Ok(())
    })
}

fn stop_overlay_on_main() -> Result<(), String> {
    OVERLAY_UI.with(|cell| {
        let mut state = cell.borrow_mut();
        for view in state.token_views.drain(..) {
            view.removeFromSuperview();
        }
        if let Some(window) = state.window.take() {
            window.orderOut(None);
            window.close();
        }
        state.content_view = None;
        state.screen_frame = None;
        Ok(())
    })
}

fn apply_overlay_update_on_main(rects: Vec<OverlayRect>) {
    if !OVERLAY_ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    OVERLAY_UI.with(|cell| {
        let mut state = cell.borrow_mut();
        let Some(content) = state.content_view.as_ref().cloned() else {
            return;
        };
        let Some(screen_frame) = state.screen_frame else {
            return;
        };
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };

        for view in state.token_views.drain(..) {
            view.removeFromSuperview();
        }

        let mut drawn = 0usize;
        for rect in rects {
            let Some(frame) = rect_to_overlay_frame(&rect, screen_frame) else {
                continue;
            };
            let token_box: Retained<NSBox> =
                unsafe { msg_send![NSBox::alloc(mtm), initWithFrame: frame] };
            token_box.setBoxType(NSBoxType::Custom);
            token_box.setTitlePosition(NSTitlePosition::NoTitle);
            token_box.setBorderWidth(1.3);
            token_box.setTransparent(false);
            token_box.setFillColor(&NSColor::clearColor());
            token_box.setBorderColor(&overlay_color(rect.kind));
            content.addSubview(&token_box);
            state.token_views.push(token_box);
            drawn += 1;
        }
        if drawn == 0 {
            // Probe marker helps verify overlay rendering when tokenization yielded no drawables.
            let probe_x = (screen_frame.size.width - PROBE_WIDTH) / 2.0;
            let probe_y = (screen_frame.size.height - PROBE_HEIGHT) / 2.0;
            let probe_frame = NSRect::new(
                NSPoint::new(probe_x.max(0.0), probe_y.max(0.0)),
                NSSize::new(PROBE_WIDTH, PROBE_HEIGHT),
            );
            let probe_box: Retained<NSBox> =
                unsafe { msg_send![NSBox::alloc(mtm), initWithFrame: probe_frame] };
            probe_box.setBoxType(NSBoxType::Custom);
            probe_box.setTitlePosition(NSTitlePosition::NoTitle);
            probe_box.setBorderWidth(2.2);
            probe_box.setTransparent(false);
            probe_box.setFillColor(&NSColor::clearColor());
            probe_box.setBorderColor(&NSColor::systemRedColor());
            content.addSubview(&probe_box);
            state.token_views.push(probe_box);
            drawn += 1;
            trace::log("overlay:apply_on_main probe_drawn");
        }

        content.setNeedsDisplay(true);
        trace::log(format!("overlay:apply_on_main drawn={drawn}"));
    });
}

fn overlay_color(kind: OverlayKind) -> Retained<NSColor> {
    match kind {
        OverlayKind::Text => NSColor::systemGreenColor(),
        OverlayKind::Box => NSColor::systemBlueColor(),
        OverlayKind::Glyph => NSColor::yellowColor(),
    }
}

fn rect_to_overlay_frame(rect: &OverlayRect, screen_frame: NSRect) -> Option<NSRect> {
    if rect.width < 2.0 || rect.height < 2.0 {
        return None;
    }
    let x = rect.x - screen_frame.origin.x;
    let y_top = rect.y - screen_frame.origin.y;
    let y = screen_frame.size.height - (y_top + rect.height);
    let frame = NSRect::new(NSPoint::new(x, y), NSSize::new(rect.width, rect.height));
    if frame.size.width <= 0.0 || frame.size.height <= 0.0 {
        return None;
    }
    Some(frame)
}

fn overlay_rects_from_payload(payload: &TokenizePayload) -> Vec<OverlayRect> {
    let mut out = Vec::new();
    let image_w = payload.image.as_ref().map(|i| i.width as f64);
    let image_h = payload.image.as_ref().map(|i| i.height as f64);

    for window in &payload.windows {
        append_window_rects(window, image_w, image_h, &mut out);
        if out.len() >= MAX_OVERLAY_RECTS {
            break;
        }
    }
    let mut used_token_fallback = false;
    if out.is_empty() && !payload.tokens.is_empty() {
        if let Some(window) = payload.windows.first() {
            append_token_rects(window, &payload.tokens, image_w, image_h, &mut out);
            used_token_fallback = true;
        }
    }
    out.truncate(MAX_OVERLAY_RECTS);
    if used_token_fallback {
        trace::log("overlay:update using_token_fallback");
    }
    out
}

fn append_window_rects(
    window: &TokenizeWindow,
    image_w: Option<f64>,
    image_h: Option<f64>,
    out: &mut Vec<OverlayRect>,
) {
    let anchor_bounds = window.os_bounds.as_ref().unwrap_or(&window.bounds);
    let img_w = image_w.unwrap_or(window.bounds.width).max(1.0);
    let img_h = image_h.unwrap_or(window.bounds.height).max(1.0);
    let sx = (anchor_bounds.width / img_w).max(0.0001);
    let sy = (anchor_bounds.height / img_h).max(0.0001);

    for element in &window.elements {
        let kind = match element.kind.as_str() {
            "text" => OverlayKind::Text,
            "box" => OverlayKind::Box,
            "glyph" => OverlayKind::Glyph,
            _ => continue,
        };
        let bbox = element.bbox;
        let width = bbox[2] * sx;
        let height = bbox[3] * sy;
        if width < 2.0 || height < 2.0 {
            continue;
        }
        out.push(OverlayRect {
            x: anchor_bounds.x + (bbox[0] * sx),
            y: anchor_bounds.y + (bbox[1] * sy),
            width,
            height,
            kind,
        });
        if out.len() >= MAX_OVERLAY_RECTS {
            break;
        }
    }
}

fn append_token_rects(
    window: &TokenizeWindow,
    tokens: &[TokenEntry],
    image_w: Option<f64>,
    image_h: Option<f64>,
    out: &mut Vec<OverlayRect>,
) {
    let anchor_bounds = window.os_bounds.as_ref().unwrap_or(&window.bounds);
    let img_w = image_w.unwrap_or(window.bounds.width).max(1.0);
    let img_h = image_h.unwrap_or(window.bounds.height).max(1.0);
    let sx = (anchor_bounds.width / img_w).max(0.0001);
    let sy = (anchor_bounds.height / img_h).max(0.0001);
    for token in tokens {
        let width = token.bounds.width * sx;
        let height = token.bounds.height * sy;
        if width < 2.0 || height < 2.0 {
            continue;
        }
        out.push(OverlayRect {
            x: anchor_bounds.x + (token.bounds.x * sx),
            y: anchor_bounds.y + (token.bounds.y * sy),
            width,
            height,
            kind: OverlayKind::Text,
        });
        if out.len() >= MAX_OVERLAY_RECTS {
            break;
        }
    }
}

fn run_on_main_sync<F>(job: F) -> Result<(), AppError>
where
    F: FnOnce() -> Result<(), String> + Send + 'static,
{
    if MainThreadMarker::new().is_some() {
        return job().map_err(AppError::backend_unavailable);
    }
    let (tx, rx) = mpsc::sync_channel(1);
    dispatch_main(move || {
        let _ = tx.send(job());
    });
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(msg)) => Err(AppError::backend_unavailable(msg)),
        Err(_) => Err(AppError::backend_unavailable(
            "timed out waiting for main-thread overlay operation",
        )),
    }
}

fn dispatch_main<F>(job: F)
where
    F: FnOnce() + Send + 'static,
{
    DispatchQueue::main().exec_async(job);
}

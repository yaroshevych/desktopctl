use std::{
    cell::RefCell,
    f64::consts::PI,
    ffi::c_void,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

use desktop_core::{
    error::AppError,
    protocol::{Bounds, TokenizePayload, TokenizeWindow},
};
use dispatch2::DispatchQueue;
use objc2::{MainThreadMarker, MainThreadOnly, class, msg_send, rc::Retained, runtime::AnyObject};
use objc2_app_kit::{
    NSBackingStoreType, NSBox, NSBoxType, NSColor, NSFloatingWindowLevel, NSScreen,
    NSTitlePosition, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowStyleMask,
};
use objc2_foundation::{NSPoint, NSRect, NSSize};

use crate::trace;

const MAX_OVERLAY_RECTS: usize = 900;
const PROBE_WIDTH: f64 = 120.0;
const PROBE_HEIGHT: f64 = 80.0;
const NS_WINDOW_SHARING_NONE: isize = 0;

const BRAND_R: f64 = 124.0 / 255.0;
const BRAND_G: f64 = 58.0 / 255.0;
const BRAND_B: f64 = 1.0;
const GLOW_BORDER_WIDTH: f64 = 2.0;
const GLOW_WINDOW_CORNER_RADIUS: f64 = 10.0;
const GLOW_SHADOW_RADIUS_SCALE: f64 = 1.35;
const GLOW_WINDOW_SHADOW_RADIUS_MAX: f64 = 32.0;
const MODE_CROSSFADE_SECS: f64 = 0.30;
const WINDOW_TRACKING_SECS: f64 = 0.06;
const GLOW_TICK_MS: u64 = 16;
const GLOW_POST_ACTIVE_SECS: f64 = 2.0;
const GLOW_FADE_TAIL_SECS: f64 = 0.35;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    WindowMode,
    DesktopMode,
}

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
    BorderedText,
    Box,
    Glyph,
}

#[derive(Debug, Clone)]
struct GlowModel {
    mode: WatchMode,
    window_bounds: Option<Bounds>,
    agent_active: bool,
    glow_hold_until: Option<Instant>,
    watch_mode_lock_until: Option<Instant>,
    confidence: f32,
    transition_from: Option<WatchMode>,
    transition_started_at: Option<Instant>,
}

impl Default for GlowModel {
    fn default() -> Self {
        Self {
            mode: WatchMode::DesktopMode,
            window_bounds: None,
            agent_active: false,
            glow_hold_until: None,
            watch_mode_lock_until: None,
            confidence: 1.0,
            transition_from: None,
            transition_started_at: None,
        }
    }
}

#[derive(Default)]
struct OverlayUiState {
    window: Option<Retained<NSWindow>>,
    content_view: Option<Retained<NSView>>,
    window_glow_box: Option<Retained<NSBox>>,
    desktop_glow_box: Option<Retained<NSBox>>,
    token_views: Vec<Retained<NSBox>>,
    screen_frame: Option<NSRect>,
    window_frame_current: Option<NSRect>,
    window_frame_target: Option<NSRect>,
    window_frame_anim_from: Option<NSRect>,
    window_frame_anim_started_at: Option<Instant>,
}

thread_local! {
    static OVERLAY_UI: RefCell<OverlayUiState> = RefCell::new(OverlayUiState::default());
}

static OVERLAY_ACTIVE: AtomicBool = AtomicBool::new(false);
static GLOW_LOOP_SEQ: AtomicU64 = AtomicU64::new(0);
static GLOW_MODEL: OnceLock<Mutex<GlowModel>> = OnceLock::new();

#[derive(Debug, Clone, Copy)]
struct GlowParams {
    border_min: f64,
    border_max: f64,
    bloom_min: f64,
    bloom_max: f64,
    blur_min: f64,
    blur_max: f64,
    spread_min: f64,
    spread_max: f64,
    corner_radius: f64,
}

pub fn is_active() -> bool {
    OVERLAY_ACTIVE.load(Ordering::SeqCst)
}

pub fn tracked_window_bounds() -> Option<Bounds> {
    let model = lock_glow_model();
    if model.mode == WatchMode::WindowMode {
        model.window_bounds.clone()
    } else {
        None
    }
}

pub fn is_agent_active() -> bool {
    let model = lock_glow_model();
    model.agent_active
}

pub fn is_watch_mode_locked() -> bool {
    let model = lock_glow_model();
    model
        .watch_mode_lock_until
        .map(|until| until > Instant::now())
        .unwrap_or(false)
}

pub fn lock_watch_mode(
    mode: WatchMode,
    window_bounds: Option<Bounds>,
    duration: Duration,
) -> Result<(), AppError> {
    {
        let mut model = lock_glow_model();
        if model.mode != mode {
            model.transition_from = Some(model.mode);
            model.transition_started_at = Some(Instant::now());
            model.mode = mode;
        }
        model.window_bounds = window_bounds;
        model.watch_mode_lock_until = Some(Instant::now() + duration);
    }
    request_glow_refresh();
    Ok(())
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
    start_glow_loop();
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
    stop_glow_loop();
    trace::log("overlay:stop requested");
    run_on_main_sync(stop_overlay_on_main)?;
    trace::log("overlay:stop ok");
    Ok(true)
}

pub fn watch_mode_changed(mode: WatchMode, window_bounds: Option<Bounds>) -> Result<(), AppError> {
    {
        let mut model = lock_glow_model();
        if model.mode != mode {
            model.transition_from = Some(model.mode);
            model.transition_started_at = Some(Instant::now());
            model.mode = mode;
        }
        model.window_bounds = window_bounds;
        model.watch_mode_lock_until = None;
    }
    request_glow_refresh();
    Ok(())
}

#[allow(dead_code)]
pub fn agent_active_changed(active: bool) -> Result<(), AppError> {
    {
        let mut model = lock_glow_model();
        model.agent_active = active;
        model.glow_hold_until =
            Some(Instant::now() + Duration::from_secs_f64(GLOW_POST_ACTIVE_SECS));
    }
    request_glow_refresh();
    Ok(())
}

pub fn confidence_changed(confidence: f32) -> Result<(), AppError> {
    {
        let mut model = lock_glow_model();
        model.confidence = confidence.clamp(0.0, 1.0);
    }
    request_glow_refresh();
    Ok(())
}

pub fn update_from_tokenize(payload: &TokenizePayload) -> Result<(), AppError> {
    if !OVERLAY_ACTIVE.load(Ordering::SeqCst) {
        return Ok(());
    }

    let (sum, count) = payload
        .windows
        .iter()
        .flat_map(|window| window.elements.iter())
        .filter_map(|element| element.confidence.map(|v| v as f64))
        .fold((0.0_f64, 0_usize), |(sum, count), v| (sum + v, count + 1));
    if count > 0 {
        let _ = confidence_changed((sum / count as f64) as f32);
    } else {
        let _ = confidence_changed(1.0);
    }

    let rects = overlay_rects_from_payload(payload);
    trace::log(format!(
        "overlay:update windows={} rects={}",
        payload.windows.len(),
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
        // Exclude overlay window from screen capture to avoid self-induced
        // diffs in tokenize/capture loops.
        unsafe {
            let _: () = msg_send![&window, setSharingType: NS_WINDOW_SHARING_NONE];
        }
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

        let desktop_glow_box: Retained<NSBox> =
            unsafe { msg_send![NSBox::alloc(mtm), initWithFrame: full_overlay_frame(frame)] };
        configure_glow_box_base(&desktop_glow_box);
        set_view_hidden(&desktop_glow_box, false);
        set_layer_z(&desktop_glow_box, 0.0);
        content_view.addSubview(&desktop_glow_box);

        let window_glow_box: Retained<NSBox> =
            unsafe { msg_send![NSBox::alloc(mtm), initWithFrame: full_overlay_frame(frame)] };
        configure_glow_box_base(&window_glow_box);
        set_view_hidden(&window_glow_box, true);
        set_layer_z(&window_glow_box, 0.0);
        content_view.addSubview(&window_glow_box);

        state.window = Some(window);
        state.content_view = Some(content_view);
        state.window_glow_box = Some(window_glow_box);
        state.desktop_glow_box = Some(desktop_glow_box);
        state.token_views.clear();
        state.screen_frame = Some(frame);
        state.window_frame_current = None;
        state.window_frame_target = None;
        state.window_frame_anim_from = None;
        state.window_frame_anim_started_at = None;
        Ok(())
    })
}

fn stop_overlay_on_main() -> Result<(), String> {
    OVERLAY_UI.with(|cell| {
        let mut state = cell.borrow_mut();
        for view in state.token_views.drain(..) {
            view.removeFromSuperview();
        }
        if let Some(window_box) = state.window_glow_box.take() {
            window_box.removeFromSuperview();
        }
        if let Some(desktop_box) = state.desktop_glow_box.take() {
            desktop_box.removeFromSuperview();
        }
        if let Some(window) = state.window.take() {
            window.orderOut(None);
            window.close();
        }
        state.content_view = None;
        state.screen_frame = None;
        state.window_frame_current = None;
        state.window_frame_target = None;
        state.window_frame_anim_from = None;
        state.window_frame_anim_started_at = None;
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

        let mut drawable_rects: Vec<(NSRect, OverlayKind)> = Vec::with_capacity(rects.len());
        for rect in rects {
            if let Some(frame) = rect_to_overlay_frame(&rect, screen_frame) {
                drawable_rects.push((frame, rect.kind));
            }
        }

        let needs_probe = drawable_rects.is_empty();
        let required_views = if needs_probe { 1 } else { drawable_rects.len() };
        while state.token_views.len() < required_views {
            let token_box: Retained<NSBox> = unsafe {
                msg_send![NSBox::alloc(mtm), initWithFrame: full_overlay_frame(screen_frame)]
            };
            token_box.setBoxType(NSBoxType::Custom);
            token_box.setTitlePosition(NSTitlePosition::NoTitle);
            token_box.setTransparent(false);
            token_box.setFillColor(&NSColor::clearColor());
            set_layer_z(&token_box, 1.0);
            set_view_hidden(&token_box, true);
            content.addSubview(&token_box);
            state.token_views.push(token_box);
        }

        let mut drawn = 0usize;
        if needs_probe {
            // Probe marker helps verify overlay rendering when tokenization yielded no drawables.
            let probe_x = (screen_frame.size.width - PROBE_WIDTH) / 2.0;
            let probe_y = (screen_frame.size.height - PROBE_HEIGHT) / 2.0;
            let probe_frame = NSRect::new(
                NSPoint::new(probe_x.max(0.0), probe_y.max(0.0)),
                NSSize::new(PROBE_WIDTH, PROBE_HEIGHT),
            );
            if let Some(probe_box) = state.token_views.first() {
                set_view_frame(probe_box, probe_frame);
                probe_box.setBorderWidth(2.2);
                probe_box.setBorderColor(&NSColor::systemRedColor());
                set_view_hidden(probe_box, false);
            }
            drawn += 1;
            trace::log("overlay:apply_on_main probe_drawn");
        } else {
            for (idx, (frame, kind)) in drawable_rects.iter().enumerate() {
                if let Some(token_box) = state.token_views.get(idx) {
                    set_view_frame(token_box, *frame);
                    token_box.setBorderWidth(1.3);
                    token_box.setBorderColor(&overlay_color(*kind));
                    set_view_hidden(token_box, false);
                    drawn += 1;
                }
            }
        }

        for idx in drawn..state.token_views.len() {
            if let Some(view) = state.token_views.get(idx) {
                set_view_hidden(view, true);
            }
        }

        content.setNeedsDisplay(true);
        trace::log(format!("overlay:apply_on_main drawn={drawn}"));
    });
}

fn start_glow_loop() {
    let seq = GLOW_LOOP_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    thread::spawn(move || {
        while OVERLAY_ACTIVE.load(Ordering::SeqCst) && GLOW_LOOP_SEQ.load(Ordering::SeqCst) == seq {
            dispatch_main(move || {
                apply_glow_frame_on_main(seq);
            });
            thread::sleep(Duration::from_millis(GLOW_TICK_MS));
        }
    });
}

fn stop_glow_loop() {
    GLOW_LOOP_SEQ.fetch_add(1, Ordering::SeqCst);
}

fn request_glow_refresh() {
    if !OVERLAY_ACTIVE.load(Ordering::SeqCst) {
        return;
    }
    let seq = GLOW_LOOP_SEQ.load(Ordering::SeqCst);
    dispatch_main(move || {
        apply_glow_frame_on_main(seq);
    });
}

fn apply_glow_frame_on_main(seq: u64) {
    if !OVERLAY_ACTIVE.load(Ordering::SeqCst) || GLOW_LOOP_SEQ.load(Ordering::SeqCst) != seq {
        return;
    }

    OVERLAY_UI.with(|cell| {
        let mut state = cell.borrow_mut();
        let Some(screen_frame) = state.screen_frame else {
            return;
        };
        let Some(window_glow_box) = state.window_glow_box.as_ref().cloned() else {
            return;
        };
        let Some(desktop_glow_box) = state.desktop_glow_box.as_ref().cloned() else {
            return;
        };

        let mut model = lock_glow_model();
        let (window_weight, desktop_weight) = mode_weights(&mut model);
        let now = Instant::now();
        let hold_alpha = if model.agent_active {
            1.0
        } else if let Some(until) = model.glow_hold_until {
            let remain = until.saturating_duration_since(now).as_secs_f64();
            if remain <= 0.0 {
                0.0
            } else if remain >= GLOW_FADE_TAIL_SECS {
                1.0
            } else {
                (remain / GLOW_FADE_TAIL_SECS).clamp(0.0, 1.0)
            }
        } else {
            0.0
        };
        if hold_alpha <= 0.001 {
            set_view_hidden(&desktop_glow_box, true);
            set_view_hidden(&window_glow_box, true);
            return;
        }
        let alpha_scale = hold_alpha;

        let window_params = params_for_mode(WatchMode::WindowMode);
        let desktop_params = params_for_mode(WatchMode::DesktopMode);
        let window_wave = 0.58;
        let desktop_wave = 0.50;

        let desktop_frame = full_overlay_frame(screen_frame);
        apply_glow_style(
            &desktop_glow_box,
            desktop_frame,
            desktop_params,
            desktop_wave,
            desktop_weight,
            alpha_scale,
        );

        let target_window_frame = model
            .window_bounds
            .as_ref()
            .and_then(|b| rect_to_overlay_frame(&bounds_to_overlay_rect(b), screen_frame));
        update_window_frame_animation(&mut state, target_window_frame);
        let current_window_frame = animated_window_frame(&mut state);
        if let Some(window_frame) = current_window_frame {
            let window_glow_frame = outset_rect(window_frame, 2.0, 2.0, screen_frame);
            apply_glow_style(
                &window_glow_box,
                window_glow_frame,
                window_params,
                window_wave,
                window_weight,
                alpha_scale,
            );
        } else {
            set_view_hidden(&window_glow_box, true);
        }
    });
}

fn update_window_frame_animation(state: &mut OverlayUiState, target: Option<NSRect>) {
    let changed = match (state.window_frame_target, target) {
        (Some(a), Some(b)) => rect_distance(a, b) > 0.5,
        (None, None) => false,
        _ => true,
    };
    if !changed {
        return;
    }
    state.window_frame_anim_from = state.window_frame_current.or(state.window_frame_target);
    state.window_frame_target = target;
    state.window_frame_anim_started_at = Some(Instant::now());
}

fn animated_window_frame(state: &mut OverlayUiState) -> Option<NSRect> {
    let target = state.window_frame_target?;
    let Some(started_at) = state.window_frame_anim_started_at else {
        state.window_frame_current = Some(target);
        return Some(target);
    };
    let from = state.window_frame_anim_from.unwrap_or(target);
    let elapsed = started_at.elapsed().as_secs_f64();
    if elapsed >= WINDOW_TRACKING_SECS {
        state.window_frame_current = Some(target);
        state.window_frame_anim_started_at = None;
        state.window_frame_anim_from = Some(target);
        return Some(target);
    }
    let t = (elapsed / WINDOW_TRACKING_SECS).clamp(0.0, 1.0);
    let eased = 0.5 - 0.5 * (PI * t).cos();
    let frame = lerp_rect(from, target, eased);
    state.window_frame_current = Some(frame);
    Some(frame)
}

fn apply_glow_style(
    box_view: &NSBox,
    frame: NSRect,
    params: GlowParams,
    wave: f64,
    mode_weight: f64,
    alpha_scale: f64,
) {
    if mode_weight <= 0.001 {
        set_view_hidden(box_view, true);
        return;
    }

    set_view_hidden(box_view, false);
    set_view_frame(box_view, frame);

    let border_alpha = lerp(params.border_min, params.border_max, wave) * mode_weight * alpha_scale;
    let bloom_alpha = lerp(params.bloom_min, params.bloom_max, wave) * mode_weight * alpha_scale;
    let blur = lerp(params.blur_min, params.blur_max, wave);
    let spread = lerp(params.spread_min, params.spread_max, wave);
    let mut shadow_radius = (blur + spread * 0.6).max(0.0) * GLOW_SHADOW_RADIUS_SCALE;
    if params.corner_radius > 0.5 {
        shadow_radius = shadow_radius.min(GLOW_WINDOW_SHADOW_RADIUS_MAX);
    }

    set_layer_border(box_view, GLOW_BORDER_WIDTH, border_alpha);
    set_layer_background_clear(box_view);

    set_layer_corner_radius(box_view, params.corner_radius);
    set_layer_shadow(box_view, bloom_alpha.clamp(0.0, 1.0), shadow_radius);
    set_layer_shadow_path(box_view, frame, params.corner_radius, shadow_radius, spread);
}

fn mode_weights(model: &mut GlowModel) -> (f64, f64) {
    let (mut window_weight, mut desktop_weight) = match model.mode {
        WatchMode::WindowMode => (1.0, 0.0),
        WatchMode::DesktopMode => (0.0, 1.0),
    };

    let Some(from_mode) = model.transition_from else {
        return (window_weight, desktop_weight);
    };
    let Some(started_at) = model.transition_started_at else {
        model.transition_from = None;
        return (window_weight, desktop_weight);
    };

    let elapsed = started_at.elapsed().as_secs_f64();
    if elapsed >= MODE_CROSSFADE_SECS {
        model.transition_from = None;
        model.transition_started_at = None;
        return (window_weight, desktop_weight);
    }

    let t = (elapsed / MODE_CROSSFADE_SECS).clamp(0.0, 1.0);
    let eased = 0.5 - 0.5 * (PI * t).cos();
    let from_weight = 1.0 - eased;
    let to_weight = eased;

    match (from_mode, model.mode) {
        (WatchMode::WindowMode, WatchMode::DesktopMode) => {
            window_weight = from_weight;
            desktop_weight = to_weight;
        }
        (WatchMode::DesktopMode, WatchMode::WindowMode) => {
            desktop_weight = from_weight;
            window_weight = to_weight;
        }
        _ => {}
    }
    (window_weight, desktop_weight)
}

fn params_for_mode(mode: WatchMode) -> GlowParams {
    match mode {
        WatchMode::WindowMode => GlowParams {
            border_min: 0.65,
            border_max: 1.0,
            bloom_min: 0.24,
            bloom_max: 0.46,
            blur_min: 10.0,
            blur_max: 20.0,
            spread_min: 1.0,
            spread_max: 5.0,
            corner_radius: GLOW_WINDOW_CORNER_RADIUS,
        },
        WatchMode::DesktopMode => GlowParams {
            border_min: 0.30,
            border_max: 0.60,
            bloom_min: 0.0,
            bloom_max: 0.06,
            blur_min: 20.0,
            blur_max: 42.0,
            spread_min: 0.0,
            spread_max: 12.0,
            corner_radius: 0.0,
        },
    }
}

fn configure_glow_box_base(box_view: &NSBox) {
    box_view.setBoxType(NSBoxType::Custom);
    box_view.setTitlePosition(NSTitlePosition::NoTitle);
    box_view.setTransparent(true);
    box_view.setFillColor(&NSColor::clearColor());
    box_view.setBorderWidth(0.0);
    box_view.setWantsLayer(true);
    set_layer_background_clear(box_view);
    set_layer_shadow(box_view, 0.0, 0.0);
}

fn full_overlay_frame(screen_frame: NSRect) -> NSRect {
    NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(screen_frame.size.width, screen_frame.size.height),
    )
}

fn set_layer_z(view: &NSBox, z: f64) {
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if layer.is_null() {
        return;
    }
    unsafe {
        let _: () = msg_send![layer, setZPosition: z];
    }
}

fn set_layer_corner_radius(view: &NSBox, corner_radius: f64) {
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if layer.is_null() {
        return;
    }
    unsafe {
        let _: () = msg_send![layer, setCornerRadius: corner_radius.max(0.0)];
    }
}

fn set_layer_background_clear(view: &NSBox) {
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if layer.is_null() {
        return;
    }
    let clear = NSColor::clearColor();
    let cg_color: *mut AnyObject = unsafe { msg_send![&*clear, CGColor] };
    unsafe {
        let _: () = msg_send![layer, setBackgroundColor: cg_color];
    }
}

fn set_layer_border(view: &NSBox, width: f64, alpha: f64) {
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if layer.is_null() {
        return;
    }
    let color = plasma_violet(alpha);
    let cg_color: *mut AnyObject = unsafe { msg_send![&*color, CGColor] };
    unsafe {
        let _: () = msg_send![layer, setBorderWidth: width.max(0.0)];
        let _: () = msg_send![layer, setBorderColor: cg_color];
    }
}

fn set_layer_shadow(view: &NSBox, opacity: f64, radius: f64) {
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if layer.is_null() {
        return;
    }
    let color = plasma_violet(1.0);
    let cg_color: *mut AnyObject = unsafe { msg_send![&*color, CGColor] };
    unsafe {
        let _: () = msg_send![layer, setMasksToBounds: false];
        let _: () = msg_send![layer, setShadowOffset: NSSize::new(0.0, 0.0)];
        let _: () = msg_send![layer, setShadowOpacity: opacity.clamp(0.0, 1.0) as f32];
        let _: () = msg_send![layer, setShadowRadius: radius.max(0.0)];
        let _: () = msg_send![layer, setShadowColor: cg_color];
    }
}

fn set_layer_shadow_path(
    view: &NSBox,
    _frame: NSRect,
    _corner_radius: f64,
    _shadow_radius: f64,
    _spread: f64,
) {
    let layer: *mut AnyObject = unsafe { msg_send![view, layer] };
    if layer.is_null() {
        return;
    }
    unsafe {
        // Let CoreAnimation derive the shadow from composited alpha (border-only content).
        let _: () = msg_send![layer, setShadowPath: std::ptr::null::<c_void>()];
    }
}

fn set_view_frame(view: &NSBox, frame: NSRect) {
    unsafe {
        let _: () = msg_send![view, setFrame: frame];
    }
}

fn set_view_hidden(view: &NSBox, hidden: bool) {
    unsafe {
        let _: () = msg_send![view, setHidden: hidden];
    }
}

fn plasma_violet(alpha: f64) -> Retained<NSColor> {
    let a = alpha.clamp(0.0, 1.0);
    unsafe {
        msg_send![
            class!(NSColor),
            colorWithSRGBRed: BRAND_R,
            green: BRAND_G,
            blue: BRAND_B,
            alpha: a
        ]
    }
}

fn bounds_to_overlay_rect(bounds: &Bounds) -> OverlayRect {
    OverlayRect {
        x: bounds.x,
        y: bounds.y,
        width: bounds.width,
        height: bounds.height,
        kind: OverlayKind::Box,
    }
}

fn rect_distance(a: NSRect, b: NSRect) -> f64 {
    (a.origin.x - b.origin.x).abs()
        + (a.origin.y - b.origin.y).abs()
        + (a.size.width - b.size.width).abs()
        + (a.size.height - b.size.height).abs()
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

fn lerp_rect(a: NSRect, b: NSRect, t: f64) -> NSRect {
    NSRect::new(
        NSPoint::new(
            lerp(a.origin.x, b.origin.x, t),
            lerp(a.origin.y, b.origin.y, t),
        ),
        NSSize::new(
            lerp(a.size.width, b.size.width, t),
            lerp(a.size.height, b.size.height, t),
        ),
    )
}

fn outset_rect(rect: NSRect, dx: f64, dy: f64, clamp_to: NSRect) -> NSRect {
    let x = (rect.origin.x - dx).max(clamp_to.origin.x);
    let y = (rect.origin.y - dy).max(clamp_to.origin.y);
    let right = (rect.origin.x + rect.size.width + dx).min(clamp_to.origin.x + clamp_to.size.width);
    let top = (rect.origin.y + rect.size.height + dy).min(clamp_to.origin.y + clamp_to.size.height);
    let width = (right - x).max(1.0);
    let height = (top - y).max(1.0);
    NSRect::new(NSPoint::new(x, y), NSSize::new(width, height))
}

fn overlay_color(kind: OverlayKind) -> Retained<NSColor> {
    match kind {
        OverlayKind::Text => NSColor::systemGreenColor(),
        OverlayKind::BorderedText => NSColor::systemRedColor(),
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
    out.truncate(MAX_OVERLAY_RECTS);
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
    let (sx, sy) = if window.os_bounds.is_some() {
        (1.0, 1.0)
    } else {
        (
            (anchor_bounds.width / img_w).max(0.0001),
            (anchor_bounds.height / img_h).max(0.0001),
        )
    };

    let bordered_boxes: Vec<[f64; 4]> = window
        .elements
        .iter()
        .filter(|element| element.has_border.unwrap_or(false))
        .map(|element| element.bbox)
        .collect();

    for element in &window.elements {
        let bbox = element.bbox;
        let has_text = element
            .text
            .as_ref()
            .map(|t| !t.trim().is_empty())
            .unwrap_or(false);
        let kind = match element.kind.as_str() {
            "text" if element.has_border.unwrap_or(false) => OverlayKind::BorderedText,
            "text" => {
                if bordered_boxes
                    .iter()
                    .any(|outer| should_suppress_inner_text_overlay_bbox(&bbox, outer))
                {
                    continue;
                }
                OverlayKind::Text
            }
            "box" => OverlayKind::Box,
            "glyph" => OverlayKind::Glyph,
            "" if element.has_border.unwrap_or(false) && has_text => OverlayKind::BorderedText,
            "" if has_text => {
                if bordered_boxes
                    .iter()
                    .any(|outer| should_suppress_inner_text_overlay_bbox(&bbox, outer))
                {
                    continue;
                }
                OverlayKind::Text
            }
            _ if has_text => {
                if element.has_border.unwrap_or(false) {
                    OverlayKind::BorderedText
                } else {
                    if bordered_boxes
                        .iter()
                        .any(|outer| should_suppress_inner_text_overlay_bbox(&bbox, outer))
                    {
                        continue;
                    }
                    OverlayKind::Text
                }
            }
            _ => continue,
        };
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

fn bbox_overlap_area(a: &[f64; 4], b: &[f64; 4]) -> f64 {
    let ax2 = a[0] + a[2];
    let ay2 = a[1] + a[3];
    let bx2 = b[0] + b[2];
    let by2 = b[1] + b[3];
    let ix1 = a[0].max(b[0]);
    let iy1 = a[1].max(b[1]);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    iw * ih
}

fn bbox_center_inside(inner: &[f64; 4], outer: &[f64; 4]) -> bool {
    let cx = inner[0] + inner[2] * 0.5;
    let cy = inner[1] + inner[3] * 0.5;
    cx >= outer[0] && cx <= outer[0] + outer[2] && cy >= outer[1] && cy <= outer[1] + outer[3]
}

fn should_suppress_inner_text_overlay_bbox(inner: &[f64; 4], outer: &[f64; 4]) -> bool {
    let inner_area = (inner[2] * inner[3]).max(1.0);
    let overlap = bbox_overlap_area(inner, outer);
    let mostly_inside = overlap / inner_area >= 0.75;
    let center_in = bbox_center_inside(inner, outer);
    let clearly_smaller = outer[2] >= inner[2] + 8.0 && outer[3] >= inner[3] + 8.0;
    center_in && mostly_inside && clearly_smaller
}

fn lock_glow_model() -> std::sync::MutexGuard<'static, GlowModel> {
    let lock = GLOW_MODEL.get_or_init(|| Mutex::new(GlowModel::default()));
    match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_core::protocol::TokenizeElement;

    #[test]
    fn suppresses_inner_text_when_bordered_box_contains_it() {
        let inner = [20.0, 20.0, 30.0, 10.0];
        let outer = [10.0, 10.0, 100.0, 40.0];
        assert!(should_suppress_inner_text_overlay_bbox(&inner, &outer));
    }

    #[test]
    fn append_window_rects_marks_bordered_text_and_drops_inner_text() {
        let window = TokenizeWindow {
            id: "win_1".to_string(),
            window_ref: None,
            title: "t".to_string(),
            app: None,
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 300.0,
            },
            os_bounds: None,
            elements: vec![
                TokenizeElement {
                    id: "text_0001".to_string(),
                    kind: "text".to_string(),
                    bbox: [10.0, 10.0, 100.0, 40.0],
                    has_border: Some(true),
                    text: Some("Apple".to_string()),
                    text_truncated: None,
                    confidence: Some(1.0),
                    source: "sat_control_v1".to_string(),
                },
                TokenizeElement {
                    id: "text_0002".to_string(),
                    kind: "text".to_string(),
                    bbox: [20.0, 20.0, 30.0, 10.0],
                    has_border: None,
                    text: Some("Apple".to_string()),
                    text_truncated: None,
                    confidence: Some(1.0),
                    source: "vision_ocr".to_string(),
                },
                TokenizeElement {
                    id: "text_0003".to_string(),
                    kind: "text".to_string(),
                    bbox: [200.0, 200.0, 30.0, 10.0],
                    has_border: None,
                    text: Some("Outside".to_string()),
                    text_truncated: None,
                    confidence: Some(1.0),
                    source: "vision_ocr".to_string(),
                },
            ],
        };

        let mut out = Vec::new();
        append_window_rects(&window, Some(300.0), Some(300.0), &mut out);
        assert_eq!(out.len(), 2);
        assert!(
            out.iter()
                .any(|r| matches!(r.kind, OverlayKind::BorderedText))
        );
        assert!(out.iter().any(|r| matches!(r.kind, OverlayKind::Text)));
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

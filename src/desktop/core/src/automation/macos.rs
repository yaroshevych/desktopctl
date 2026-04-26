use std::{
    ffi::{CStr, CString, c_char, c_int, c_void},
    fs::OpenOptions,
    io::Write,
    process::Command,
    sync::OnceLock,
    time::{SystemTime, UNIX_EPOCH},
};

#[allow(deprecated)]
use cocoa::{
    appkit::{NSEvent, NSEventModifierFlags, NSEventType},
    base::nil,
    foundation::{NSInteger, NSPoint},
};
use core_graphics::{
    display::CGDisplay,
    event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, EventField, ScrollEventUnit},
    event_source::{CGEventSource, CGEventSourceStateID},
    geometry::CGPoint,
};
use foreign_types::ForeignType;

use crate::error::AppError;

use super::{Automation, BackgroundInputBackend, BackgroundInputTarget, Point};

pub struct MacosAutomation;
pub struct MacosBackgroundInput;

impl MacosAutomation {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for MacosAutomation {
    fn default() -> Self {
        Self::new()
    }
}

impl MacosBackgroundInput {
    pub const fn new() -> Self {
        Self
    }
}

impl Automation for MacosAutomation {
    fn check_accessibility_permission(&self) -> Result<(), AppError> {
        if ax_is_process_trusted() {
            Ok(())
        } else {
            Err(AppError::permission_denied(
                "accessibility permission required. enable it for DesktopCtl.app in System Settings -> Privacy & Security -> Accessibility",
            ))
        }
    }

    fn press_hotkey(&self, hotkey: &str) -> Result<(), AppError> {
        let script = applescript_hotkey(hotkey)?;
        run_osascript(&script)
    }

    fn press_enter(&self) -> Result<(), AppError> {
        run_osascript(r#"tell application "System Events" to key code 36"#)
    }

    fn press_escape(&self) -> Result<(), AppError> {
        run_osascript(r#"tell application "System Events" to key code 53"#)
    }

    fn type_text(&self, text: &str) -> Result<(), AppError> {
        let escaped = text.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!(r#"tell application "System Events" to keystroke "{escaped}""#);
        run_osascript(&script)
    }

    fn move_mouse(&self, point: Point) -> Result<(), AppError> {
        post_mouse_event(CGEventType::MouseMoved, point, CGMouseButton::Left)
    }

    fn left_down(&self, point: Point) -> Result<(), AppError> {
        post_mouse_event(CGEventType::LeftMouseDown, point, CGMouseButton::Left)
    }

    fn left_drag(&self, point: Point) -> Result<(), AppError> {
        post_mouse_event(CGEventType::LeftMouseDragged, point, CGMouseButton::Left)
    }

    fn left_up(&self, point: Point) -> Result<(), AppError> {
        post_mouse_event(CGEventType::LeftMouseUp, point, CGMouseButton::Left)
    }

    fn left_click(&self, point: Point) -> Result<(), AppError> {
        self.left_down(point)?;
        self.left_up(point)
    }

    fn right_down(&self, point: Point) -> Result<(), AppError> {
        post_mouse_event(CGEventType::RightMouseDown, point, CGMouseButton::Right)
    }

    fn right_up(&self, point: Point) -> Result<(), AppError> {
        post_mouse_event(CGEventType::RightMouseUp, point, CGMouseButton::Right)
    }

    fn right_click(&self, point: Point) -> Result<(), AppError> {
        self.right_down(point)?;
        self.right_up(point)
    }

    fn scroll_wheel(&self, dx: i32, dy: i32) -> Result<(), AppError> {
        post_scroll_event(dx, dy)
    }
}

impl BackgroundInputBackend for MacosBackgroundInput {
    fn preflight(&self, target: &BackgroundInputTarget) -> Result<(), AppError> {
        if target.pid <= 0 || target.window_id == 0 {
            return Err(background_input_unavailable(
                "background input target is missing pid/window id",
            ));
        }
        let symbols = skylight_symbols()?;
        let routing = resolve_background_routing(target, symbols)?;
        preflight_window_server_target(target, routing.psn, symbols)?;
        Ok(())
    }

    fn left_click(&self, target: &BackgroundInputTarget, point: Point) -> Result<(), AppError> {
        if target.pid <= 0 || target.window_id == 0 {
            return Err(background_input_unavailable(
                "background input target is missing pid/window id",
            ));
        }
        let symbols = skylight_symbols()?;
        let routing = resolve_background_routing(target, symbols)?;
        preflight_window_server_target(target, routing.psn, symbols)?;
        let target_point = BackgroundMousePoint::from_screen_point(target, point);
        let primer_point = BackgroundMousePoint::offscreen_primer();
        post_background_mouse_event(
            target,
            routing,
            target_point,
            CGEventType::MouseMoved,
            0,
            symbols,
        )?;
        post_background_mouse_event(
            target,
            routing,
            primer_point,
            CGEventType::LeftMouseDown,
            1,
            symbols,
        )?;
        post_background_mouse_event(
            target,
            routing,
            primer_point,
            CGEventType::LeftMouseUp,
            1,
            symbols,
        )?;
        post_background_mouse_event(
            target,
            routing,
            target_point,
            CGEventType::LeftMouseDown,
            1,
            symbols,
        )?;
        post_background_mouse_event(
            target,
            routing,
            target_point,
            CGEventType::LeftMouseUp,
            1,
            symbols,
        )
    }

    fn type_text(&self, target: &BackgroundInputTarget, text: &str) -> Result<(), AppError> {
        if target.pid <= 0 || target.window_id == 0 {
            return Err(background_input_unavailable(
                "background input target is missing pid/window id",
            ));
        }
        let symbols = skylight_symbols()?;
        let routing = resolve_background_routing(target, symbols)?;
        preflight_window_server_target(target, routing.psn, symbols)?;
        for ch in text.chars() {
            post_background_text_event(target, routing, ch, true, symbols)?;
            post_background_text_event(target, routing, ch, false, symbols)?;
        }
        Ok(())
    }
}

fn post_mouse_event(
    event_type: CGEventType,
    point: Point,
    button: CGMouseButton,
) -> Result<(), AppError> {
    let cg_point = to_core_graphics_point(point);
    let bounds = CGDisplay::main().bounds();
    trace_mouse(format!(
        "mouse_event:post type={:?} logical=({}, {}) cg=({:.2}, {:.2}) display_origin=({:.2}, {:.2}) display_size=({:.2}, {:.2})",
        event_type,
        point.x,
        point.y,
        cg_point.x,
        cg_point.y,
        bounds.origin.x,
        bounds.origin.y,
        bounds.size.width,
        bounds.size.height
    ));

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| AppError::backend_unavailable("failed to create CoreGraphics event source"))?;

    let event = CGEvent::new_mouse_event(source, event_type, cg_point, button)
        .map_err(|_| AppError::backend_unavailable("failed to create mouse event"))?;

    event.post(CGEventTapLocation::HID);
    trace_mouse(format!("mouse_event:posted type={:?}", event_type));
    Ok(())
}

fn post_background_mouse_event(
    target: &BackgroundInputTarget,
    routing: BackgroundRouting,
    point: BackgroundMousePoint,
    event_type: CGEventType,
    click_state: i64,
    symbols: SkyLightSymbols,
) -> Result<(), AppError> {
    let event = new_background_mouse_event(event_type, point, click_state)?;
    stamp_background_event(
        &event,
        target,
        routing,
        Some(point.window_local),
        click_state,
        symbols,
    );
    trace_mouse(format!(
        "background_input:post_mouse type={:?} pid={} window_id={} screen=({:.1}, {:.1}) local=({:.1}, {:.1})",
        event_type,
        target.pid,
        target.window_id,
        point.screen.x,
        point.screen.y,
        point.window_local.x,
        point.window_local.y
    ));
    post_event_to_pid(target.pid, &event, symbols)
}

fn new_background_mouse_event(
    event_type: CGEventType,
    point: BackgroundMousePoint,
    click_state: i64,
) -> Result<CGEvent, AppError> {
    match background_mouse_event_constructor() {
        BackgroundMouseEventConstructor::CoreGraphics => {
            let source =
                CGEventSource::new(CGEventSourceStateID::HIDSystemState).map_err(|_| {
                    AppError::backend_unavailable("failed to create CoreGraphics event source")
                })?;
            CGEvent::new_mouse_event(source, event_type, point.screen, CGMouseButton::Left).map_err(
                |_| AppError::backend_unavailable("failed to create background mouse event"),
            )
        }
        BackgroundMouseEventConstructor::NsEvent => {
            new_nsevent_backed_mouse_event(event_type, point.window_local, click_state)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundMouseEventConstructor {
    CoreGraphics,
    NsEvent,
}

fn background_mouse_event_constructor() -> BackgroundMouseEventConstructor {
    std::env::var("DESKTOPCTL_BACKGROUND_MOUSE_EVENT")
        .ok()
        .filter(|value| value.trim().eq_ignore_ascii_case("nsevent"))
        .map(|_| BackgroundMouseEventConstructor::NsEvent)
        .unwrap_or(BackgroundMouseEventConstructor::CoreGraphics)
}

#[allow(deprecated)]
fn new_nsevent_backed_mouse_event(
    event_type: CGEventType,
    window_local: CGPoint,
    click_state: i64,
) -> Result<CGEvent, AppError> {
    let event_type = nsevent_type_for_mouse_event(event_type)?;
    let location = NSPoint::new(window_local.x, window_local.y);
    let pressure = if click_state > 0 { 1.0 } else { 0.0 };
    let event = unsafe {
        NSEvent::mouseEventWithType_location_modifierFlags_timestamp_windowNumber_context_eventNumber_clickCount_pressure_(
            nil,
            event_type,
            location,
            NSEventModifierFlags::empty(),
            0.0,
            0 as NSInteger,
            nil,
            0 as NSInteger,
            click_state as NSInteger,
            pressure,
        )
    };
    if event == nil {
        return Err(AppError::backend_unavailable(
            "failed to create NSEvent-backed background mouse event",
        ));
    }
    let cg_event = unsafe { event.CGEvent() };
    if cg_event.is_null() {
        return Err(AppError::backend_unavailable(
            "NSEvent-backed background mouse event did not produce a CGEvent",
        ));
    }
    let retained = unsafe { CFRetain(cg_event.cast::<c_void>()) };
    if retained.is_null() {
        return Err(AppError::backend_unavailable(
            "failed to retain NSEvent-backed CGEvent",
        ));
    }
    Ok(unsafe { CGEvent::from_ptr(retained.cast()) })
}

#[allow(deprecated)]
fn nsevent_type_for_mouse_event(event_type: CGEventType) -> Result<NSEventType, AppError> {
    match event_type {
        CGEventType::MouseMoved => Ok(NSEventType::NSMouseMoved),
        CGEventType::LeftMouseDown => Ok(NSEventType::NSLeftMouseDown),
        CGEventType::LeftMouseUp => Ok(NSEventType::NSLeftMouseUp),
        CGEventType::LeftMouseDragged => Ok(NSEventType::NSLeftMouseDragged),
        _ => Err(AppError::backend_unavailable(format!(
            "NSEvent-backed background mouse event does not support {event_type:?}"
        ))),
    }
}

fn post_background_text_event(
    target: &BackgroundInputTarget,
    routing: BackgroundRouting,
    ch: char,
    keydown: bool,
    symbols: SkyLightSymbols,
) -> Result<(), AppError> {
    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| AppError::backend_unavailable("failed to create CoreGraphics event source"))?;
    let event = CGEvent::new_keyboard_event(source, 0, keydown)
        .map_err(|_| AppError::backend_unavailable("failed to create background keyboard event"))?;
    event.set_string(&ch.to_string());
    stamp_background_event(&event, target, routing, None, 0, symbols);
    trace_mouse(format!(
        "background_input:post_text keydown={} pid={} window_id={} char={:?}",
        keydown, target.pid, target.window_id, ch
    ));
    post_event_to_pid(target.pid, &event, symbols)
}

fn stamp_background_event(
    event: &CGEvent,
    target: &BackgroundInputTarget,
    routing: BackgroundRouting,
    window_local: Option<CGPoint>,
    click_state: i64,
    symbols: SkyLightSymbols,
) {
    event.set_integer_value_field(EventField::EVENT_TARGET_UNIX_PROCESS_ID, target.pid as i64);
    event.set_integer_value_field(
        EventField::EVENT_TARGET_PROCESS_SERIAL_NUMBER,
        routing.psn.as_i64(),
    );
    event.set_integer_value_field(EventField::MOUSE_EVENT_BUTTON_NUMBER, 0);
    event.set_integer_value_field(EventField::MOUSE_EVENT_SUB_TYPE, 3);
    event.set_integer_value_field(EventField::MOUSE_EVENT_CLICK_STATE, click_state);
    event.set_integer_value_field(
        EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER,
        target.window_id as i64,
    );
    event.set_integer_value_field(
        EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER_THAT_CAN_HANDLE_THIS_EVENT,
        target.window_id as i64,
    );
    set_skylight_integer(
        event,
        symbols,
        EventField::EVENT_TARGET_UNIX_PROCESS_ID,
        target.pid as i64,
    );
    set_skylight_integer(
        event,
        symbols,
        PRIVATE_EVENT_TARGET_WINDOW_ID_FIELD,
        target.window_id as i64,
    );
    set_skylight_integer(
        event,
        symbols,
        PRIVATE_EVENT_OWNER_CONNECTION_FIELD,
        routing.owner_connection as i64,
    );
    set_skylight_integer(
        event,
        symbols,
        PRIVATE_EVENT_OWNER_CONNECTION_FIELD_ALT,
        routing.owner_connection as i64,
    );
    set_skylight_integer(
        event,
        symbols,
        EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER,
        target.window_id as i64,
    );
    set_skylight_integer(
        event,
        symbols,
        EventField::MOUSE_EVENT_WINDOW_UNDER_MOUSE_POINTER_THAT_CAN_HANDLE_THIS_EVENT,
        target.window_id as i64,
    );
    if click_state > 0 {
        event.set_double_value_field(EventField::MOUSE_EVENT_PRESSURE, 1.0);
        event.set_integer_value_field(PRIVATE_EVENT_MOUSE_DOWN_FIELD, 1);
    }
    if let Some(window_local) = window_local {
        unsafe { (symbols.set_window_location)(event.as_ptr().cast::<c_void>(), window_local) };
    }
}

fn set_skylight_integer(event: &CGEvent, symbols: SkyLightSymbols, field: u32, value: i64) {
    unsafe { (symbols.set_integer_field)(event.as_ptr().cast::<c_void>(), field, value) };
    event.set_integer_value_field(field, value);
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct ProcessSerialNumber {
    high: u32,
    low: u32,
}

impl ProcessSerialNumber {
    fn as_i64(self) -> i64 {
        ((self.high as u64) << 32 | self.low as u64) as i64
    }
}

#[derive(Debug, Clone, Copy)]
struct BackgroundRouting {
    owner_connection: c_int,
    psn: ProcessSerialNumber,
}

#[derive(Debug, Clone, Copy)]
struct BackgroundMousePoint {
    screen: CGPoint,
    window_local: CGPoint,
}

impl BackgroundMousePoint {
    fn from_screen_point(target: &BackgroundInputTarget, point: Point) -> Self {
        Self {
            screen: to_core_graphics_point(point),
            window_local: CGPoint::new(
                point.x as f64 - target.bounds.x,
                point.y as f64 - target.bounds.y,
            ),
        }
    }

    fn offscreen_primer() -> Self {
        Self {
            screen: CGPoint::new(-1.0, -1.0),
            window_local: CGPoint::new(-1.0, -1.0),
        }
    }
}

fn post_scroll_event(dx: i32, dy: i32) -> Result<(), AppError> {
    trace_mouse(format!("scroll_event:post dx={} dy={}", dx, dy));

    let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
        .map_err(|_| AppError::backend_unavailable("failed to create CoreGraphics event source"))?;

    // Command semantics: positive `dy` means scroll down (screen-space Y+).
    // CoreGraphics wheel1 uses positive values for up, so invert `dy`.
    let vertical = -dy;
    let horizontal = dx;
    let event =
        CGEvent::new_scroll_event(source, ScrollEventUnit::LINE, 2, vertical, horizontal, 0)
            .map_err(|_| AppError::backend_unavailable("failed to create scroll event"))?;

    event.post(CGEventTapLocation::HID);
    trace_mouse(format!(
        "scroll_event:posted wheel1(vertical)={} wheel2(horizontal)={}",
        vertical, horizontal
    ));
    Ok(())
}

type SLEventPostToPid = unsafe extern "C" fn(c_int, *mut c_void);
type SLEventSetIntegerValueField = unsafe extern "C" fn(*mut c_void, u32, i64);
type CGEventSetWindowLocation = unsafe extern "C" fn(*mut c_void, CGPoint);
type CGSMainConnectionID = unsafe extern "C" fn() -> c_int;
type CGSGetWindowOwner = unsafe extern "C" fn(c_int, u32, *mut c_int) -> c_int;
type CGSGetConnectionPSN = unsafe extern "C" fn(c_int, *mut ProcessSerialNumber) -> c_int;
type SLPSPostEventRecordTo = unsafe extern "C" fn(*const c_void, *const u8) -> c_int;

#[derive(Clone, Copy)]
struct SkyLightSymbols {
    post_to_pid: SLEventPostToPid,
    set_integer_field: SLEventSetIntegerValueField,
    set_window_location: CGEventSetWindowLocation,
    main_connection_id: CGSMainConnectionID,
    get_window_owner: CGSGetWindowOwner,
    get_connection_psn: CGSGetConnectionPSN,
    post_event_record_to: SLPSPostEventRecordTo,
}

fn skylight_symbols() -> Result<SkyLightSymbols, AppError> {
    static SYMBOLS: OnceLock<Result<SkyLightSymbols, String>> = OnceLock::new();
    match SYMBOLS.get_or_init(load_skylight_symbols) {
        Ok(symbols) => Ok(*symbols),
        Err(message) => Err(background_input_unavailable(message.clone())),
    }
}

fn load_skylight_symbols() -> Result<SkyLightSymbols, String> {
    let framework = CString::new("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight")
        .expect("static path has no nul");
    let handle = unsafe { dlopen(framework.as_ptr(), RTLD_NOW) };
    if handle.is_null() {
        return Err(format!(
            "SkyLight framework unavailable: {}; switch to frontmost mode",
            dlerror_message()
        ));
    }
    Ok(SkyLightSymbols {
        post_to_pid: load_symbol(handle, "SLEventPostToPid")?,
        set_integer_field: load_symbol(handle, "SLEventSetIntegerValueField")?,
        set_window_location: load_symbol(handle, "CGEventSetWindowLocation")?,
        main_connection_id: load_symbol(handle, "CGSMainConnectionID")?,
        get_window_owner: load_symbol(handle, "CGSGetWindowOwner")?,
        get_connection_psn: load_symbol(handle, "CGSGetConnectionPSN")?,
        post_event_record_to: load_symbol(handle, "SLPSPostEventRecordTo")?,
    })
}

fn load_symbol<T>(handle: *mut c_void, name: &str) -> Result<T, String> {
    let symbol = CString::new(name).expect("static symbol has no nul");
    let ptr = unsafe { dlsym(handle, symbol.as_ptr()) };
    if ptr.is_null() {
        return Err(format!(
            "SkyLight {name} unavailable: {}; switch to frontmost mode",
            dlerror_message()
        ));
    }
    Ok(unsafe { std::mem::transmute_copy::<*mut c_void, T>(&ptr) })
}

fn resolve_background_routing(
    target: &BackgroundInputTarget,
    symbols: SkyLightSymbols,
) -> Result<BackgroundRouting, AppError> {
    let mut owner_connection: c_int = 0;
    let owner_status = unsafe {
        (symbols.get_window_owner)(
            (symbols.main_connection_id)(),
            target.window_id,
            &mut owner_connection,
        )
    };
    if owner_status != 0 {
        return Err(background_input_unavailable(format!(
            "CGSGetWindowOwner status={owner_status} for window {}; switch to frontmost mode",
            target.window_id
        )));
    }
    let mut psn = ProcessSerialNumber { high: 0, low: 0 };
    let psn_status = unsafe { (symbols.get_connection_psn)(owner_connection, &mut psn) };
    if psn_status != 0 {
        return Err(background_input_unavailable(format!(
            "CGSGetConnectionPSN status={psn_status} for owner connection {owner_connection}; switch to frontmost mode"
        )));
    }
    Ok(BackgroundRouting {
        owner_connection,
        psn,
    })
}

fn preflight_window_server_target(
    target: &BackgroundInputTarget,
    psn: ProcessSerialNumber,
    symbols: SkyLightSymbols,
) -> Result<(), AppError> {
    let record = target_only_focus_record(target.window_id);
    let status = unsafe {
        (symbols.post_event_record_to)(
            (&psn as *const ProcessSerialNumber).cast::<c_void>(),
            record.as_ptr(),
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(background_input_unavailable(format!(
            "SLPSPostEventRecordTo target-only focus failed with status {status}; switch to frontmost mode"
        )))
    }
}

fn target_only_focus_record(window_id: u32) -> [u8; 0xF8] {
    let mut record = [0_u8; 0xF8];
    record[0x04] = 0xF8;
    record[0x08] = 0x0D;
    stamp_window_id_le(window_id, &mut record, 0x3C);
    record[0x8A] = 0x01;
    record
}

fn stamp_window_id_le(window_id: u32, record: &mut [u8], offset: usize) {
    let bytes = window_id.to_le_bytes();
    record[offset..offset + 4].copy_from_slice(&bytes);
}

fn post_event_to_pid(pid: i32, event: &CGEvent, symbols: SkyLightSymbols) -> Result<(), AppError> {
    unsafe { (symbols.post_to_pid)(pid as c_int, event.as_ptr().cast::<c_void>()) };
    Ok(())
}

fn dlerror_message() -> String {
    let err = unsafe { dlerror() };
    if err.is_null() {
        return "unknown dynamic loader error".to_string();
    }
    unsafe { CStr::from_ptr(err) }
        .to_string_lossy()
        .into_owned()
}

fn background_input_unavailable(message: impl Into<String>) -> AppError {
    AppError::backend_unavailable(message)
}

// Private CGEvent fields used by WindowServer for target-window delivery. Keep
// these isolated so the prototype can be adjusted if macOS changes them.
const PRIVATE_EVENT_TARGET_WINDOW_ID_FIELD: u32 = 51;
const PRIVATE_EVENT_OWNER_CONNECTION_FIELD: u32 = 52;
const PRIVATE_EVENT_OWNER_CONNECTION_FIELD_ALT: u32 = 85;
const PRIVATE_EVENT_MOUSE_DOWN_FIELD: u32 = 108;

const RTLD_NOW: c_int = 0x2;

unsafe extern "C" {
    fn dlopen(path: *const c_char, mode: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *const c_char;
    fn CFRetain(cf: *const c_void) -> *mut c_void;
}

fn to_core_graphics_point(point: Point) -> CGPoint {
    // DesktopCtl coordinates are absolute screen coordinates from the top-left
    // of the main display, which is what CGEvent mouse APIs consume.
    let bounds = CGDisplay::main().bounds();
    let x = bounds.origin.x + point.x as f64;
    let y = bounds.origin.y + point.y as f64;
    CGPoint::new(x, y)
}

fn run_osascript(script: &str) -> Result<(), AppError> {
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| {
            AppError::backend_unavailable(format!("osascript failed to start: {err}"))
        })?;

    if output.status.success() {
        return Ok(());
    }

    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(AppError::internal(format!(
        "osascript command failed: {}",
        stderr.trim()
    )))
}

fn applescript_hotkey(input: &str) -> Result<String, AppError> {
    let lower = input.trim().to_lowercase();
    let parts: Vec<&str> = lower
        .split('+')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(AppError::invalid_argument(format!(
            "invalid hotkey format: {input}"
        )));
    }

    let key = parts
        .last()
        .ok_or_else(|| AppError::invalid_argument(format!("invalid hotkey format: {input}")))?;
    let modifiers = parts[..parts.len() - 1]
        .iter()
        .map(|p| match *p {
            "cmd" | "command" => Ok("command down"),
            "shift" => Ok("shift down"),
            "ctrl" | "control" => Ok("control down"),
            "opt" | "option" | "alt" => Ok("option down"),
            _ => Err(AppError::invalid_argument(format!(
                "invalid hotkey format: {input}"
            ))),
        })
        .collect::<Result<Vec<&str>, AppError>>()?;

    let using = modifiers.join(", ");
    let using_clause = if using.is_empty() {
        String::new()
    } else {
        format!(" using {{{using}}}")
    };
    let script = if let Some(code) = keycode_for_name(key) {
        format!(r#"tell application "System Events" to key code {code}{using_clause}"#)
    } else if key.len() == 1 {
        format!(r#"tell application "System Events" to keystroke "{key}"{using_clause}"#)
    } else {
        return Err(AppError::invalid_argument(format!(
            "invalid hotkey format: {input}"
        )));
    };

    Ok(script)
}

fn keycode_for_name(name: &str) -> Option<u16> {
    match name {
        "space" => Some(49),
        "tab" => Some(48),
        "enter" | "return" => Some(36),
        "escape" | "esc" => Some(53),
        "delete" | "backspace" => Some(51),
        "forwarddelete" | "forward_delete" | "del" => Some(117),
        "left" | "leftarrow" | "left_arrow" => Some(123),
        "right" | "rightarrow" | "right_arrow" => Some(124),
        "down" | "downarrow" | "down_arrow" => Some(125),
        "up" | "uparrow" | "up_arrow" => Some(126),
        "home" => Some(115),
        "end" => Some(119),
        "pageup" | "page_up" => Some(116),
        "pagedown" | "page_down" => Some(121),
        "f1" => Some(122),
        "f2" => Some(120),
        "f3" => Some(99),
        "f4" => Some(118),
        "f5" => Some(96),
        "f6" => Some(97),
        "f7" => Some(98),
        "f8" => Some(100),
        "f9" => Some(101),
        "f10" => Some(109),
        "f11" => Some(103),
        "f12" => Some(111),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use crate::protocol::Bounds;

    use super::{
        BackgroundInputTarget, BackgroundMousePoint, Point, ProcessSerialNumber,
        applescript_hotkey, nsevent_type_for_mouse_event, target_only_focus_record,
    };
    #[allow(deprecated)]
    use cocoa::appkit::NSEventType;
    use core_graphics::event::CGEventType;

    #[test]
    fn hotkey_supports_standalone_delete() {
        let script = applescript_hotkey("delete").expect("delete should parse");
        assert_eq!(script, r#"tell application "System Events" to key code 51"#);
    }

    #[test]
    fn hotkey_supports_arrow_with_modifier() {
        let script = applescript_hotkey("cmd+left").expect("cmd+left should parse");
        assert_eq!(
            script,
            r#"tell application "System Events" to key code 123 using {command down}"#
        );
    }

    #[test]
    fn hotkey_supports_single_char_without_modifier() {
        let script = applescript_hotkey("a").expect("single key should parse");
        assert_eq!(
            script,
            r#"tell application "System Events" to keystroke "a""#
        );
    }

    #[test]
    fn process_serial_number_packs_high_low_words() {
        let psn = ProcessSerialNumber {
            high: 0x0000_0001,
            low: 0x0000_0002,
        };
        assert_eq!(psn.as_i64(), 0x0000_0001_0000_0002);
    }

    #[test]
    fn target_only_focus_record_stamps_window_server_fields() {
        let record = target_only_focus_record(0x1122_3344);
        assert_eq!(record.len(), 0xF8);
        assert_eq!(record[0x04], 0xF8);
        assert_eq!(record[0x08], 0x0D);
        assert_eq!(&record[0x3C..0x40], &[0x44, 0x33, 0x22, 0x11]);
        assert_eq!(record[0x8A], 0x01);
    }

    #[test]
    fn background_mouse_point_uses_window_local_coordinates() {
        let target = BackgroundInputTarget {
            pid: 42,
            window_id: 7,
            bounds: Bounds {
                x: 100.0,
                y: 200.0,
                width: 300.0,
                height: 400.0,
            },
        };
        let point = BackgroundMousePoint::from_screen_point(&target, Point::new(125, 260));
        assert_eq!(point.window_local.x, 25.0);
        assert_eq!(point.window_local.y, 60.0);

        let primer = BackgroundMousePoint::offscreen_primer();
        assert_eq!(primer.screen.x, -1.0);
        assert_eq!(primer.screen.y, -1.0);
        assert_eq!(primer.window_local.x, -1.0);
        assert_eq!(primer.window_local.y, -1.0);
    }

    #[test]
    #[allow(deprecated)]
    fn nsevent_mouse_type_mapping_covers_supported_background_mouse_events() {
        assert_eq!(
            nsevent_type_for_mouse_event(CGEventType::MouseMoved).expect("mouse moved"),
            NSEventType::NSMouseMoved
        );
        assert_eq!(
            nsevent_type_for_mouse_event(CGEventType::LeftMouseDown).expect("left down"),
            NSEventType::NSLeftMouseDown
        );
        assert_eq!(
            nsevent_type_for_mouse_event(CGEventType::LeftMouseUp).expect("left up"),
            NSEventType::NSLeftMouseUp
        );
        assert_eq!(
            nsevent_type_for_mouse_event(CGEventType::LeftMouseDragged).expect("left drag"),
            NSEventType::NSLeftMouseDragged
        );
        assert!(nsevent_type_for_mouse_event(CGEventType::ScrollWheel).is_err());
    }
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

fn ax_is_process_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

fn trace_mouse(message: impl AsRef<str>) {
    let trace_enabled = std::env::var("DESKTOPCTL_TRACE")
        .ok()
        .map(|v| {
            let lowered = v.trim().to_ascii_lowercase();
            lowered == "1" || lowered == "true" || lowered == "yes" || lowered == "on"
        })
        .unwrap_or(false);
    let has_custom_path = std::env::var("DESKTOPCTL_TRACE_PATH")
        .ok()
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false);
    if !trace_enabled && !has_custom_path {
        return;
    }

    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let pid = std::process::id();
    let tid = format!("{:?}", std::thread::current().id());
    let line = format!("{ts} pid={pid} tid={tid} {}\n", message.as_ref());

    let path = std::env::var("DESKTOPCTL_TRACE_PATH")
        .ok()
        .filter(|p| !p.trim().is_empty())
        .unwrap_or_else(|| "/tmp/desktopctld.trace.log".to_string());
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(path) {
        let _ = file.write_all(line.as_bytes());
    }
}

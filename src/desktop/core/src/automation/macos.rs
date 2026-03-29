use std::{
    fs::OpenOptions,
    io::Write,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

use core_graphics::{
    display::CGDisplay,
    event::{CGEvent, CGEventTapLocation, CGEventType, CGMouseButton, ScrollEventUnit},
    event_source::{CGEventSource, CGEventSourceStateID},
    geometry::CGPoint,
};

use crate::error::AppError;

use super::{Automation, Point};

pub struct MacosAutomation;

impl MacosAutomation {
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
    use super::applescript_hotkey;

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
}

#[link(name = "ApplicationServices", kind = "framework")]
unsafe extern "C" {
    fn AXIsProcessTrusted() -> bool;
}

fn ax_is_process_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

fn trace_mouse(message: impl AsRef<str>) {
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

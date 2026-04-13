use crate::error::AppError;

use super::{Automation, Point};

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
    MAPVK_VK_TO_VSC, MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
    MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, MapVirtualKeyW,
    SendInput, VIRTUAL_KEY, VK_BACK, VK_DELETE, VK_DOWN, VK_END, VK_ESCAPE, VK_F1, VK_F2, VK_F3,
    VK_F4, VK_F5, VK_F6, VK_F7, VK_F8, VK_F9, VK_F10, VK_F11, VK_F12, VK_HOME, VK_LEFT, VK_NEXT,
    VK_PRIOR, VK_RETURN, VK_RIGHT, VK_SPACE, VK_TAB, VK_UP,
};
use windows_sys::Win32::UI::WindowsAndMessaging::SetCursorPos;

const WHEEL_DELTA: i32 = 120;
const VK_SHIFT: u16 = 0x10;
const VK_CONTROL: u16 = 0x11;
const VK_MENU: u16 = 0x12;

pub struct WindowsAutomation;

impl WindowsAutomation {
    pub const fn new() -> Self {
        Self
    }
}

impl Default for WindowsAutomation {
    fn default() -> Self {
        Self::new()
    }
}

impl Automation for WindowsAutomation {
    fn check_accessibility_permission(&self) -> Result<(), AppError> {
        Ok(())
    }

    fn press_hotkey(&self, hotkey: &str) -> Result<(), AppError> {
        let (modifiers, key) = parse_hotkey(hotkey)?;
        for modifier in &modifiers {
            send_vk(*modifier, false)?;
        }
        send_vk(key, false)?;
        send_vk(key, true)?;
        for modifier in modifiers.iter().rev() {
            send_vk(*modifier, true)?;
        }
        Ok(())
    }

    fn press_enter(&self) -> Result<(), AppError> {
        tap_vk(VK_RETURN)
    }

    fn press_escape(&self) -> Result<(), AppError> {
        tap_vk(VK_ESCAPE)
    }

    fn type_text(&self, text: &str) -> Result<(), AppError> {
        for unit in text.encode_utf16() {
            send_unicode(unit, false)?;
            send_unicode(unit, true)?;
        }
        Ok(())
    }

    fn move_mouse(&self, point: Point) -> Result<(), AppError> {
        move_cursor(point)
    }

    fn left_down(&self, point: Point) -> Result<(), AppError> {
        move_cursor(point)?;
        send_mouse_flags(MOUSEEVENTF_LEFTDOWN, 0)
    }

    fn left_drag(&self, point: Point) -> Result<(), AppError> {
        move_cursor(point)
    }

    fn left_up(&self, point: Point) -> Result<(), AppError> {
        move_cursor(point)?;
        send_mouse_flags(MOUSEEVENTF_LEFTUP, 0)
    }

    fn left_click(&self, point: Point) -> Result<(), AppError> {
        move_cursor(point)?;
        send_mouse_flags(MOUSEEVENTF_LEFTDOWN, 0)?;
        send_mouse_flags(MOUSEEVENTF_LEFTUP, 0)
    }

    fn right_down(&self, point: Point) -> Result<(), AppError> {
        move_cursor(point)?;
        send_mouse_flags(MOUSEEVENTF_RIGHTDOWN, 0)
    }

    fn right_up(&self, point: Point) -> Result<(), AppError> {
        move_cursor(point)?;
        send_mouse_flags(MOUSEEVENTF_RIGHTUP, 0)
    }

    fn right_click(&self, point: Point) -> Result<(), AppError> {
        move_cursor(point)?;
        send_mouse_flags(MOUSEEVENTF_RIGHTDOWN, 0)?;
        send_mouse_flags(MOUSEEVENTF_RIGHTUP, 0)
    }

    fn scroll_wheel(&self, dx: i32, dy: i32) -> Result<(), AppError> {
        // DesktopCtl semantics: positive dy means scroll down.
        let vertical = -dy.saturating_mul(WHEEL_DELTA);
        if vertical != 0 {
            send_mouse_flags(MOUSEEVENTF_WHEEL, vertical)?;
        }
        let horizontal = dx.saturating_mul(WHEEL_DELTA);
        if horizontal != 0 {
            send_mouse_flags(MOUSEEVENTF_HWHEEL, horizontal)?;
        }
        Ok(())
    }
}

fn move_cursor(point: Point) -> Result<(), AppError> {
    let x = i32::try_from(point.x).map_err(|_| {
        AppError::invalid_argument(format!("mouse x coordinate is out of range: {}", point.x))
    })?;
    let y = i32::try_from(point.y).map_err(|_| {
        AppError::invalid_argument(format!("mouse y coordinate is out of range: {}", point.y))
    })?;

    // SAFETY: SetCursorPos is a leaf Win32 API; coordinates are validated above.
    let ok = unsafe { SetCursorPos(x, y) };
    if ok == 0 {
        return Err(AppError::backend_unavailable(
            "SetCursorPos failed on Windows",
        ));
    }
    Ok(())
}

fn tap_vk(vk: VIRTUAL_KEY) -> Result<(), AppError> {
    send_vk(vk, false)?;
    send_vk(vk, true)
}

fn send_vk(vk: VIRTUAL_KEY, key_up: bool) -> Result<(), AppError> {
    // SAFETY: MapVirtualKeyW is pure for this usage.
    let scan = unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16;
    let mut flags = 0;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: scan,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send_inputs(&[input])
}

fn send_unicode(unit: u16, key_up: bool) -> Result<(), AppError> {
    let mut flags = KEYEVENTF_UNICODE;
    if key_up {
        flags |= KEYEVENTF_KEYUP;
    }
    let input = INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: 0,
                wScan: unit,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send_inputs(&[input])
}

fn send_mouse_flags(flags: u32, data: i32) -> Result<(), AppError> {
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: 0,
                dy: 0,
                mouseData: data as u32,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    send_inputs(&[input])
}

fn send_inputs(inputs: &[INPUT]) -> Result<(), AppError> {
    // SAFETY: inputs points to a valid contiguous slice for the duration of the call.
    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    if sent != inputs.len() as u32 {
        return Err(AppError::backend_unavailable(format!(
            "SendInput sent {} of {} input events",
            sent,
            inputs.len()
        )));
    }
    Ok(())
}

fn parse_hotkey(input: &str) -> Result<(Vec<VIRTUAL_KEY>, VIRTUAL_KEY), AppError> {
    let parts: Vec<&str> = input
        .split('+')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .collect();
    if parts.is_empty() {
        return Err(AppError::invalid_argument(format!(
            "invalid hotkey format: {input}"
        )));
    }

    let key = parse_key(parts[parts.len() - 1], input)?;
    let mut modifiers = Vec::new();
    for item in &parts[..parts.len() - 1] {
        let modifier = match item.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => VK_CONTROL,
            "shift" => VK_SHIFT,
            "alt" | "option" => VK_MENU,
            "cmd" | "command" | "win" | "windows" => VK_CONTROL,
            _ => {
                return Err(AppError::invalid_argument(format!(
                    "invalid hotkey format: {input}"
                )));
            }
        };
        modifiers.push(modifier);
    }

    Ok((modifiers, key))
}

fn parse_key(part: &str, input: &str) -> Result<VIRTUAL_KEY, AppError> {
    let lower = part.to_ascii_lowercase();
    let key = match lower.as_str() {
        "space" => VK_SPACE,
        "tab" => VK_TAB,
        "enter" | "return" => VK_RETURN,
        "escape" | "esc" => VK_ESCAPE,
        "delete" | "backspace" => VK_BACK,
        "forwarddelete" | "forward_delete" | "del" => VK_DELETE,
        "left" | "leftarrow" | "left_arrow" => VK_LEFT,
        "right" | "rightarrow" | "right_arrow" => VK_RIGHT,
        "down" | "downarrow" | "down_arrow" => VK_DOWN,
        "up" | "uparrow" | "up_arrow" => VK_UP,
        "home" => VK_HOME,
        "end" => VK_END,
        "pageup" | "page_up" => VK_PRIOR,
        "pagedown" | "page_down" => VK_NEXT,
        "f1" => VK_F1,
        "f2" => VK_F2,
        "f3" => VK_F3,
        "f4" => VK_F4,
        "f5" => VK_F5,
        "f6" => VK_F6,
        "f7" => VK_F7,
        "f8" => VK_F8,
        "f9" => VK_F9,
        "f10" => VK_F10,
        "f11" => VK_F11,
        "f12" => VK_F12,
        _ if part.len() == 1 => part.as_bytes()[0].to_ascii_uppercase() as u16,
        _ => {
            return Err(AppError::invalid_argument(format!(
                "invalid hotkey format: {input}"
            )));
        }
    };

    Ok(key)
}

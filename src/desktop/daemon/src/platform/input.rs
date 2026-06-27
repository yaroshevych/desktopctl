use desktop_core::{
    automation::Automation,
    error::AppError,
};
#[cfg(target_os = "linux")]
use desktop_core::automation::Point;

#[cfg(not(target_os = "linux"))]
pub fn new_backend() -> Result<Box<dyn Automation>, AppError> {
    desktop_core::automation::new_backend()
}

#[cfg(target_os = "linux")]
pub fn new_backend() -> Result<Box<dyn Automation>, AppError> {
    Ok(Box::new(LinuxAutomation::new()?))
}

#[cfg(target_os = "linux")]
struct LinuxAutomation {
    backend: crate::platform::linux::input::InputBackend,
}

#[cfg(target_os = "linux")]
impl LinuxAutomation {
    fn new() -> Result<Self, AppError> {
        Ok(Self {
            backend: crate::platform::linux::input::InputBackend::new(None)?,
        })
    }
}

#[cfg(target_os = "linux")]
impl Automation for LinuxAutomation {
    fn check_accessibility_permission(&self) -> Result<(), AppError> {
        Ok(())
    }

    fn press_hotkey(&self, hotkey: &str) -> Result<(), AppError> {
        let (modifiers, key) = parse_hotkey(hotkey)?;
        self.backend.hotkey(&modifiers, &key)
    }

    fn press_enter(&self) -> Result<(), AppError> {
        self.backend.hotkey(&[], "Return")
    }

    fn press_escape(&self) -> Result<(), AppError> {
        self.backend.hotkey(&[], "Escape")
    }

    fn type_text(&self, text: &str) -> Result<(), AppError> {
        self.backend.type_text(text)
    }

    fn move_mouse(&self, point: Point) -> Result<(), AppError> {
        self.backend
            .pointer_move_absolute(f64::from(point.x), f64::from(point.y))
    }

    fn left_down(&self, point: Point) -> Result<(), AppError> {
        self.move_mouse(point)?;
        self.backend
            .button(crate::platform::linux::input::BTN_LEFT, true)
    }

    fn left_drag(&self, point: Point) -> Result<(), AppError> {
        self.move_mouse(point)
    }

    fn left_up(&self, point: Point) -> Result<(), AppError> {
        self.move_mouse(point)?;
        self.backend
            .button(crate::platform::linux::input::BTN_LEFT, false)
    }

    fn left_click(&self, point: Point) -> Result<(), AppError> {
        self.left_down(point)?;
        self.left_up(point)
    }

    fn right_down(&self, point: Point) -> Result<(), AppError> {
        self.move_mouse(point)?;
        self.backend
            .button(crate::platform::linux::input::BTN_RIGHT, true)
    }

    fn right_up(&self, point: Point) -> Result<(), AppError> {
        self.move_mouse(point)?;
        self.backend
            .button(crate::platform::linux::input::BTN_RIGHT, false)
    }

    fn right_click(&self, point: Point) -> Result<(), AppError> {
        self.right_down(point)?;
        self.right_up(point)
    }

    fn scroll_wheel(&self, dx: i32, dy: i32) -> Result<(), AppError> {
        self.backend.scroll(f64::from(dx), f64::from(dy))
    }
}

#[cfg(target_os = "linux")]
fn parse_hotkey(
    input: &str,
) -> Result<(Vec<crate::platform::linux::input::Modifier>, String), AppError> {
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

    let mut modifiers = Vec::new();
    for item in &parts[..parts.len() - 1] {
        let modifier = match item.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => crate::platform::linux::input::Modifier::Ctrl,
            "shift" => crate::platform::linux::input::Modifier::Shift,
            "alt" | "option" => crate::platform::linux::input::Modifier::Alt,
            "cmd" | "command" | "super" | "win" | "windows" => {
                crate::platform::linux::input::Modifier::Super
            }
            _ => {
                return Err(AppError::invalid_argument(format!(
                    "invalid hotkey format: {input}"
                )));
            }
        };
        modifiers.push(modifier);
    }

    Ok((modifiers, canonical_key(parts[parts.len() - 1])))
}

#[cfg(target_os = "linux")]
fn canonical_key(part: &str) -> String {
    let lower = part.to_ascii_lowercase();
    match lower.as_str() {
        "enter" | "return" => "Return".to_string(),
        "escape" | "esc" => "Escape".to_string(),
        "backspace" | "delete" => "BackSpace".to_string(),
        "forwarddelete" | "forward_delete" | "del" => "Delete".to_string(),
        "tab" => "Tab".to_string(),
        "space" => "space".to_string(),
        "left" | "leftarrow" | "left_arrow" => "Left".to_string(),
        "right" | "rightarrow" | "right_arrow" => "Right".to_string(),
        "down" | "downarrow" | "down_arrow" => "Down".to_string(),
        "up" | "uparrow" | "up_arrow" => "Up".to_string(),
        "pageup" | "page_up" => "Page_Up".to_string(),
        "pagedown" | "page_down" => "Page_Down".to_string(),
        "home" => "Home".to_string(),
        "end" => "End".to_string(),
        f if f.len() >= 2 && f.starts_with('f') && f[1..].chars().all(|c| c.is_ascii_digit()) => {
            f.to_ascii_uppercase()
        }
        _ => part.to_string(),
    }
}

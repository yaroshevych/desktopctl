use desktop_core::{error::AppError, protocol::Bounds};
use serde_json::{Value, json};

#[derive(Debug, Clone)]
pub struct WindowInfo {
    pub id: String,
    pub window_ref: Option<String>,
    pub parent_id: Option<String>,
    pub pid: i64,
    pub index: u32,
    pub app: String,
    pub title: String,
    pub bounds: Bounds,
    pub frontmost: bool,
    pub visible: bool,
    pub modal: Option<bool>,
}

impl WindowInfo {
    pub fn as_json(&self) -> Value {
        let public_id = self.window_ref.as_deref().unwrap_or(self.id.as_str());
        json!({
            "id": public_id,
            "parent_id": self.parent_id,
            "app": self.app,
            "title": self.title,
            "bounds": self.bounds,
            "frontmost": self.frontmost,
            "visible": self.visible,
            "modal": self.modal
        })
    }
}

#[derive(Debug, Clone)]
pub struct FrontmostWindowContext {
    pub app: Option<String>,
    pub bounds: Option<Bounds>,
}

#[cfg(target_os = "macos")]
pub fn main_display_bounds() -> Option<Bounds> {
    use core_graphics::display::CGDisplay;

    let bounds = CGDisplay::main().bounds();
    Some(Bounds {
        x: bounds.origin.x,
        y: bounds.origin.y,
        width: bounds.size.width.max(0.0),
        height: bounds.size.height.max(0.0),
    })
}

#[cfg(not(target_os = "macos"))]
pub fn main_display_bounds() -> Option<Bounds> {
    None
}

#[cfg(target_os = "macos")]
pub fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    use std::process::Command as ProcessCommand;

    let script = r#"tell application "System Events"
	set frontProc to first application process whose frontmost is true
	set appName to name of frontProc
	if (count of windows of frontProc) is 0 then
	    return appName
	end if
	set winPos to position of front window of frontProc
	set winSize to size of front window of frontProc
	return appName & tab & (item 1 of winPos as string) & tab & (item 2 of winPos as string) & tab & (item 1 of winSize as string) & tab & (item 2 of winSize as string)
	end tell"#;
    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if raw.is_empty() {
        return None;
    }
    let parts: Vec<&str> = raw.split('\t').map(str::trim).collect();
    let app = parts
        .first()
        .map(|v| v.to_string())
        .filter(|v| !v.is_empty());
    let bounds = if parts.len() >= 5 {
        let parsed: Vec<f64> = parts[1..5]
            .iter()
            .filter_map(|v| v.parse::<f64>().ok())
            .collect();
        if parsed.len() == 4 {
            Some(Bounds {
                x: parsed[0].max(0.0),
                y: parsed[1].max(0.0),
                width: parsed[2].max(0.0),
                height: parsed[3].max(0.0),
            })
        } else {
            None
        }
    } else {
        None
    };
    Some(FrontmostWindowContext { app, bounds })
}

#[cfg(not(target_os = "macos"))]
pub fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    None
}

#[cfg(target_os = "macos")]
pub fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    let mut windows = list_windows_coregraphics()?;
    if let Ok(frontmost_windows) = list_frontmost_app_windows() {
        merge_frontmost_windows(&mut windows, frontmost_windows);
    }
    windows.sort_by(|a, b| {
        b.frontmost
            .cmp(&a.frontmost)
            .then_with(|| a.app.to_lowercase().cmp(&b.app.to_lowercase()))
            .then_with(|| a.index.cmp(&b.index))
    });
    augment_with_ax_metadata(&mut windows);
    Ok(windows)
}

#[cfg(target_os = "macos")]
fn list_windows_coregraphics() -> Result<Vec<WindowInfo>, AppError> {
    use core_foundation::{
        base::{CFType, TCFType},
        boolean::CFBoolean,
        dictionary::CFDictionary,
        number::CFNumber,
        string::CFString,
    };
    use core_graphics::{
        display::CGDisplay,
        window::{
            kCGNullWindowID, kCGWindowBounds, kCGWindowIsOnscreen, kCGWindowLayer,
            kCGWindowListExcludeDesktopElements, kCGWindowListOptionOnScreenOnly, kCGWindowName,
            kCGWindowNumber, kCGWindowOwnerName, kCGWindowOwnerPID,
        },
    };
    use std::collections::HashMap;
    use std::ffi::c_void;

    fn dict_number_i64(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<i64> {
        let value = dict.find(key)?;
        value
            .downcast::<CFNumber>()
            .and_then(|n| n.to_i64().or_else(|| n.to_f64().map(|v| v.round() as i64)))
    }

    fn dict_number_f64_untyped(dict: &CFDictionary, key: &str) -> Option<f64> {
        let key = CFString::new(key);
        let raw = dict.find(key.as_concrete_TypeRef() as *const c_void)?;
        let value = unsafe { CFType::wrap_under_get_rule(*raw as _) };
        value
            .downcast::<CFNumber>()
            .and_then(|n| n.to_f64().or_else(|| n.to_i64().map(|v| v as f64)))
    }

    fn dict_string(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<String> {
        let value = dict.find(key)?;
        value.downcast::<CFString>().map(|s| s.to_string())
    }

    fn dict_bool(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<bool> {
        let value = dict.find(key)?;
        value.downcast::<CFBoolean>().map(bool::from)
    }

    fn dict_bounds(dict: &CFDictionary<CFString, CFType>, key: &CFString) -> Option<Bounds> {
        let value = dict.find(key)?;
        let bounds = value.downcast::<CFDictionary>()?;
        let x = dict_number_f64_untyped(&bounds, "X")?;
        let y = dict_number_f64_untyped(&bounds, "Y")?;
        let width = dict_number_f64_untyped(&bounds, "Width")?;
        let height = dict_number_f64_untyped(&bounds, "Height")?;
        Some(Bounds {
            x: x.max(0.0),
            y: y.max(0.0),
            width: width.max(0.0),
            height: height.max(0.0),
        })
    }

    let key_number = unsafe { CFString::wrap_under_get_rule(kCGWindowNumber) };
    let key_pid = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerPID) };
    let key_owner_name = unsafe { CFString::wrap_under_get_rule(kCGWindowOwnerName) };
    let key_name = unsafe { CFString::wrap_under_get_rule(kCGWindowName) };
    let key_layer = unsafe { CFString::wrap_under_get_rule(kCGWindowLayer) };
    let key_bounds = unsafe { CFString::wrap_under_get_rule(kCGWindowBounds) };
    let key_onscreen = unsafe { CFString::wrap_under_get_rule(kCGWindowIsOnscreen) };

    let frontmost_app = frontmost_window_context().and_then(|ctx| ctx.app);
    let option = kCGWindowListOptionOnScreenOnly | kCGWindowListExcludeDesktopElements;
    let rows = CGDisplay::window_list_info(option, Some(kCGNullWindowID)).ok_or_else(|| {
        AppError::backend_unavailable("failed to enumerate windows via CoreGraphics")
    })?;

    let mut per_pid_index: HashMap<i64, u32> = HashMap::new();
    let mut windows: Vec<WindowInfo> = Vec::new();
    for raw in rows.get_all_values() {
        let row = unsafe { CFDictionary::<CFString, CFType>::wrap_under_get_rule(raw as _) };
        let Some(layer) = dict_number_i64(&row, &key_layer) else {
            continue;
        };
        if layer != 0 {
            continue;
        }
        let Some(pid) = dict_number_i64(&row, &key_pid) else {
            continue;
        };
        let app = dict_string(&row, &key_owner_name).unwrap_or_default();
        if app.is_empty() || app == "Window Server" {
            continue;
        }
        let Some(bounds) = dict_bounds(&row, &key_bounds) else {
            continue;
        };
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            continue;
        }
        let cg_id = dict_number_i64(&row, &key_number).unwrap_or(0);
        let title = dict_string(&row, &key_name).unwrap_or_default();
        let visible = dict_bool(&row, &key_onscreen).unwrap_or(true);
        let next = per_pid_index.entry(pid).or_insert(0);
        *next = next.saturating_add(1);
        let index = *next;
        windows.push(WindowInfo {
            id: format!("{pid}:{cg_id}"),
            window_ref: None,
            parent_id: None,
            pid,
            index,
            app: app.clone(),
            title,
            bounds,
            frontmost: frontmost_app.as_deref() == Some(app.as_str()),
            visible,
            modal: None,
        });
    }

    Ok(windows)
}

#[cfg(not(target_os = "macos"))]
pub fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

#[cfg(target_os = "macos")]
pub fn list_windows_basic() -> Result<Vec<WindowInfo>, AppError> {
    finalize_basic_window_list(list_windows_coregraphics()?)
}

#[cfg(not(target_os = "macos"))]
pub fn list_windows_basic() -> Result<Vec<WindowInfo>, AppError> {
    list_windows()
}

#[cfg(target_os = "macos")]
fn finalize_basic_window_list(mut windows: Vec<WindowInfo>) -> Result<Vec<WindowInfo>, AppError> {
    windows.sort_by(|a, b| {
        b.frontmost
            .cmp(&a.frontmost)
            .then_with(|| a.app.to_lowercase().cmp(&b.app.to_lowercase()))
            .then_with(|| a.index.cmp(&b.index))
    });
    Ok(windows)
}

#[cfg(target_os = "macos")]
pub fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    use std::process::Command as ProcessCommand;

    let script = r#"tell application "System Events"
set resultRows to {}
set frontProc to first application process whose frontmost is true
set pname to (name of frontProc) as text
set pvisible to (visible of frontProc) as string
set ppid to unix id of frontProc
set widx to 0
repeat with w in (windows of frontProc)
    set widx to widx + 1
    try
        set wname to (name of w) as text
    on error
        set wname to ""
    end try
    try
        set winPos to position of w
        set winSize to size of w
        set wx to item 1 of winPos
        set wy to item 2 of winPos
        set ww to item 1 of winSize
        set wh to item 2 of winSize
        set end of resultRows to (ppid as string) & tab & (widx as string) & tab & pname & tab & wname & tab & (wx as string) & tab & (wy as string) & tab & (ww as string) & tab & (wh as string) & tab & "true" & tab & pvisible
    end try
end repeat
set AppleScript's text item delimiters to linefeed
set outputText to resultRows as text
set AppleScript's text item delimiters to ""
return outputText
end tell"#;

    let output = ProcessCommand::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|err| AppError::backend_unavailable(format!("failed to run osascript: {err}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(AppError::backend_unavailable(format!(
            "failed to enumerate frontmost app windows: {stderr}"
        )));
    }

    let raw = String::from_utf8_lossy(&output.stdout);
    let mut windows: Vec<WindowInfo> = raw.lines().filter_map(parse_window_line).collect();
    windows.sort_by_key(|window| std::cmp::Reverse(window_area(&window.bounds)));
    Ok(windows)
}

#[cfg(not(target_os = "macos"))]
pub fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    Err(AppError::backend_unavailable(format!(
        "unsupported platform: {}",
        std::env::consts::OS
    )))
}

fn parse_applescript_bool(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("true")
}

fn parse_window_line(line: &str) -> Option<WindowInfo> {
    let fields: Vec<&str> = line.split('\t').collect();
    if fields.len() != 10 {
        return None;
    }

    let pid = fields[0].trim().parse::<i64>().ok()?;
    let index = fields[1].trim().parse::<u32>().ok()?;
    let app = fields[2].trim().to_string();
    let title = fields[3].trim().to_string();
    let x = fields[4].trim().parse::<f64>().ok()?;
    let y = fields[5].trim().parse::<f64>().ok()?;
    let width = fields[6].trim().parse::<f64>().ok()?;
    let height = fields[7].trim().parse::<f64>().ok()?;
    let frontmost = parse_applescript_bool(fields[8]);
    let visible = parse_applescript_bool(fields[9]);

    Some(WindowInfo {
        id: format!("{pid}:{index}"),
        window_ref: None,
        parent_id: None,
        pid,
        index,
        app,
        title,
        bounds: Bounds {
            x: x.max(0.0),
            y: y.max(0.0),
            width: width.max(0.0),
            height: height.max(0.0),
        },
        frontmost,
        visible,
        modal: None,
    })
}

fn window_area(bounds: &Bounds) -> u64 {
    let area = bounds.width.max(0.0) * bounds.height.max(0.0);
    area.round().max(0.0) as u64
}

#[cfg(target_os = "macos")]
fn merge_frontmost_windows(
    all_windows: &mut Vec<WindowInfo>,
    mut frontmost_windows: Vec<WindowInfo>,
) {
    fn overlap_area(a: &Bounds, b: &Bounds) -> f64 {
        let ax2 = a.x + a.width;
        let ay2 = a.y + a.height;
        let bx2 = b.x + b.width;
        let by2 = b.y + b.height;
        let x1 = a.x.max(b.x);
        let y1 = a.y.max(b.y);
        let x2 = ax2.min(bx2);
        let y2 = ay2.min(by2);
        let w = (x2 - x1).max(0.0);
        let h = (y2 - y1).max(0.0);
        w * h
    }

    fn iou(a: &Bounds, b: &Bounds) -> f64 {
        let inter = overlap_area(a, b);
        if inter <= 0.0 {
            return 0.0;
        }
        let aa = (a.width.max(0.0) * a.height.max(0.0)).max(0.0);
        let ba = (b.width.max(0.0) * b.height.max(0.0)).max(0.0);
        let union = aa + ba - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }

    for candidate in frontmost_windows.drain(..) {
        let duplicate = all_windows.iter().any(|existing| {
            if !existing.app.eq_ignore_ascii_case(&candidate.app) {
                return false;
            }
            let title_match = !candidate.title.is_empty()
                && !existing.title.is_empty()
                && existing.title.eq_ignore_ascii_case(&candidate.title);
            title_match && iou(&existing.bounds, &candidate.bounds) >= 0.8
                || iou(&existing.bounds, &candidate.bounds) >= 0.96
        });
        if !duplicate {
            all_windows.push(candidate);
        }
    }
}

#[cfg(target_os = "macos")]
fn augment_with_ax_metadata(windows: &mut [WindowInfo]) {
    use accessibility::{AXAttribute, AXUIElement, AXUIElementAttributes};
    use accessibility_sys::{
        AXValueGetType, AXValueGetValue, AXValueRef, kAXPositionAttribute, kAXSizeAttribute,
        kAXValueTypeCGPoint, kAXValueTypeCGSize,
    };
    use core_foundation::{
        base::{CFType, TCFType},
        boolean::CFBoolean,
        number::CFNumber,
        string::CFString,
    };
    use std::{
        collections::{HashMap, HashSet},
        ffi::c_void,
    };

    #[repr(C)]
    struct CGPoint {
        x: f64,
        y: f64,
    }

    #[repr(C)]
    struct CGSize {
        width: f64,
        height: f64,
    }

    #[derive(Clone)]
    struct AxRefMeta {
        title: String,
        bounds: Bounds,
    }

    #[derive(Clone)]
    struct AxWindowMeta {
        title: String,
        bounds: Bounds,
        modal: Option<bool>,
        parent_window: Option<AxRefMeta>,
        sheet_children: Vec<AxRefMeta>,
    }

    fn decode_point(value: &CFType) -> Option<(f64, f64)> {
        let mut point = CGPoint { x: 0.0, y: 0.0 };
        let ax_value = value.as_CFTypeRef() as AXValueRef;
        let ok = unsafe {
            AXValueGetType(ax_value) == kAXValueTypeCGPoint
                && AXValueGetValue(
                    ax_value,
                    kAXValueTypeCGPoint,
                    (&mut point as *mut CGPoint).cast::<c_void>(),
                )
        };
        ok.then_some((point.x, point.y))
    }

    fn decode_size(value: &CFType) -> Option<(f64, f64)> {
        let mut size = CGSize {
            width: 0.0,
            height: 0.0,
        };
        let ax_value = value.as_CFTypeRef() as AXValueRef;
        let ok = unsafe {
            AXValueGetType(ax_value) == kAXValueTypeCGSize
                && AXValueGetValue(
                    ax_value,
                    kAXValueTypeCGSize,
                    (&mut size as *mut CGSize).cast::<c_void>(),
                )
        };
        ok.then_some((size.width, size.height))
    }

    fn ax_bounds(window: &AXUIElement) -> Option<Bounds> {
        let pos_attr =
            AXAttribute::<CFType>::new(&CFString::from_static_string(kAXPositionAttribute));
        let size_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXSizeAttribute));
        let pos = window.attribute(&pos_attr).ok()?;
        let size = window.attribute(&size_attr).ok()?;
        let (x, y) = decode_point(&pos)?;
        let (w, h) = decode_size(&size)?;
        if w <= 1.0 || h <= 1.0 {
            return None;
        }
        Some(Bounds {
            x: x.max(0.0),
            y: y.max(0.0),
            width: w.max(0.0),
            height: h.max(0.0),
        })
    }

    fn ax_text_attr(window: &AXUIElement, name: &'static str) -> Option<String> {
        let attr = AXAttribute::<CFType>::new(&CFString::from_static_string(name));
        let value = window.attribute(&attr).ok()?;
        if !value.instance_of::<CFString>() {
            return None;
        }
        let s = unsafe { CFString::wrap_under_get_rule(value.as_CFTypeRef() as _) }.to_string();
        let s = s.trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    fn ax_modal_attr(window: &AXUIElement) -> Option<bool> {
        let attr = AXAttribute::<CFType>::new(&CFString::from_static_string("AXModal"));
        let value = window.attribute(&attr).ok()?;
        if let Some(v) = value.downcast::<CFBoolean>() {
            return Some(bool::from(v));
        }
        if let Some(v) = value.downcast::<CFNumber>() {
            let n = v
                .to_i64()
                .or_else(|| v.to_f64().map(|f| f.round() as i64))?;
            return Some(n > 0);
        }
        None
    }

    fn parent_window_ref(window: &AXUIElement, self_bounds: &Bounds) -> Option<AxRefMeta> {
        let parent_attr = AXAttribute::<CFType>::new(&CFString::from_static_string("AXParent"));
        let mut current_cf = window.attribute(&parent_attr).ok()?;
        for _ in 0..10 {
            if !current_cf.instance_of::<AXUIElement>() {
                return None;
            }
            let current =
                unsafe { AXUIElement::wrap_under_get_rule(current_cf.as_CFTypeRef() as _) };
            let role = current
                .role()
                .ok()
                .map(|v| v.to_string())
                .unwrap_or_default();
            if role == "AXWindow" {
                let bounds = ax_bounds(&current)?;
                // Ignore self-parent cycles.
                let same_bounds = (bounds.x - self_bounds.x).abs() <= 1.0
                    && (bounds.y - self_bounds.y).abs() <= 1.0
                    && (bounds.width - self_bounds.width).abs() <= 1.0
                    && (bounds.height - self_bounds.height).abs() <= 1.0;
                if same_bounds {
                    return None;
                }
                let title = current
                    .title()
                    .ok()
                    .map(|v| v.to_string())
                    .unwrap_or_default();
                return Some(AxRefMeta { title, bounds });
            }
            current_cf = current.attribute(&parent_attr).ok()?;
        }
        None
    }

    fn collect_ax_windows_for_pid(pid: i64) -> Option<(Vec<AxWindowMeta>, Option<AxRefMeta>)> {
        let app = AXUIElement::application(pid as i32);
        let app_main_window = app
            .main_window()
            .ok()
            .and_then(|w| {
                let bounds = ax_bounds(&w)?;
                let title = w.title().ok().map(|v| v.to_string()).unwrap_or_default();
                Some(AxRefMeta { title, bounds })
            })
            .or_else(|| {
                app.focused_window().ok().and_then(|w| {
                    let bounds = ax_bounds(&w)?;
                    let title = w.title().ok().map(|v| v.to_string()).unwrap_or_default();
                    Some(AxRefMeta { title, bounds })
                })
            });
        let windows = app.windows().ok()?;
        let mut out: Vec<AxWindowMeta> = Vec::new();
        for window in windows.iter() {
            let Some(bounds) = ax_bounds(&window) else {
                continue;
            };
            let parent_window = parent_window_ref(&window, &bounds);
            let sheet_children = {
                let mut items: Vec<AxRefMeta> = Vec::new();
                if let Ok(children) = window.children() {
                    for child in children.iter() {
                        let role = child.role().ok().map(|v| v.to_string()).unwrap_or_default();
                        let subrole = ax_text_attr(&child, "AXSubrole").unwrap_or_default();
                        let is_sheet_like = role == "AXSheet"
                            || (role == "AXWindow" && subrole.eq_ignore_ascii_case("AXDialog"));
                        if !is_sheet_like {
                            continue;
                        }
                        if let Some(sheet_bounds) = ax_bounds(&child) {
                            let sheet_title = child
                                .title()
                                .ok()
                                .map(|v| v.to_string())
                                .unwrap_or_default();
                            items.push(AxRefMeta {
                                title: sheet_title,
                                bounds: sheet_bounds,
                            });
                        }
                    }
                }
                items
            };
            out.push(AxWindowMeta {
                title: window
                    .title()
                    .ok()
                    .map(|v| v.to_string())
                    .unwrap_or_default(),
                bounds,
                modal: ax_modal_attr(&window),
                parent_window,
                sheet_children,
            });
        }
        Some((out, app_main_window))
    }

    fn overlap_area(a: &Bounds, b: &Bounds) -> f64 {
        let ax2 = a.x + a.width;
        let ay2 = a.y + a.height;
        let bx2 = b.x + b.width;
        let by2 = b.y + b.height;
        let x1 = a.x.max(b.x);
        let y1 = a.y.max(b.y);
        let x2 = ax2.min(bx2);
        let y2 = ay2.min(by2);
        let w = (x2 - x1).max(0.0);
        let h = (y2 - y1).max(0.0);
        w * h
    }

    fn iou(a: &Bounds, b: &Bounds) -> f64 {
        let inter = overlap_area(a, b);
        if inter <= 0.0 {
            return 0.0;
        }
        let aa = (a.width.max(0.0) * a.height.max(0.0)).max(0.0);
        let ba = (b.width.max(0.0) * b.height.max(0.0)).max(0.0);
        let union = aa + ba - inter;
        if union <= 0.0 { 0.0 } else { inter / union }
    }

    fn match_score(cg: &WindowInfo, ax: &AxWindowMeta) -> f64 {
        let mut score = iou(&cg.bounds, &ax.bounds) * 100.0;
        if !cg.title.is_empty() && !ax.title.is_empty() && cg.title.eq_ignore_ascii_case(&ax.title)
        {
            score += 10.0;
        }
        score
    }

    let pids: Vec<i64> = windows
        .iter()
        .map(|w| w.pid)
        .collect::<HashSet<_>>()
        .into_iter()
        .collect();

    for pid in pids {
        let candidate_indices: Vec<usize> = windows
            .iter()
            .enumerate()
            .filter(|(_, w)| w.pid == pid && w.visible)
            .map(|(idx, _)| idx)
            .collect();
        if candidate_indices.is_empty() {
            continue;
        }
        let Some((ax_windows, app_main_window)) = collect_ax_windows_for_pid(pid) else {
            continue;
        };
        if ax_windows.is_empty() {
            continue;
        }

        let mut used_ax = vec![false; ax_windows.len()];
        let mut cg_to_ax: HashMap<usize, usize> = HashMap::new();
        let mut ax_to_cg: HashMap<usize, usize> = HashMap::new();
        for cg_idx in candidate_indices {
            let mut best: Option<(usize, f64)> = None;
            for (ax_idx, ax) in ax_windows.iter().enumerate() {
                if used_ax[ax_idx] {
                    continue;
                }
                let score = match_score(&windows[cg_idx], ax);
                if score <= 0.01 {
                    continue;
                }
                match best {
                    None => best = Some((ax_idx, score)),
                    Some((_, best_score)) if score > best_score => best = Some((ax_idx, score)),
                    _ => {}
                }
            }
            if let Some((ax_idx, _)) = best {
                used_ax[ax_idx] = true;
                cg_to_ax.insert(cg_idx, ax_idx);
                ax_to_cg.insert(ax_idx, cg_idx);
                let ax = &ax_windows[ax_idx];
                windows[cg_idx].modal = ax.modal;
            }
        }

        for (cg_idx, ax_idx) in &cg_to_ax {
            let mut best_parent: Option<(usize, f64)> = None;
            let child_ax = &ax_windows[*ax_idx];
            for (parent_ax_idx, parent_ax) in ax_windows.iter().enumerate() {
                // Primary: parent window explicitly lists this dialog as an AXSheet.
                for sheet in &parent_ax.sheet_children {
                    let mut score = iou(&child_ax.bounds, &sheet.bounds) * 200.0;
                    if !sheet.title.is_empty()
                        && !child_ax.title.is_empty()
                        && sheet.title.eq_ignore_ascii_case(&child_ax.title)
                    {
                        score += 30.0;
                    }
                    match best_parent {
                        None if score > 0.01 => best_parent = Some((parent_ax_idx, score)),
                        Some((_, best_score)) if score > best_score => {
                            best_parent = Some((parent_ax_idx, score))
                        }
                        _ => {}
                    }
                }
            }
            // Fallback: nearest AXWindow ancestor from AXParent chain.
            if best_parent.is_none() {
                if let Some(parent_ref) = child_ax.parent_window.as_ref() {
                    for (parent_ax_idx, parent_ax) in ax_windows.iter().enumerate() {
                        let mut score = iou(&parent_ax.bounds, &parent_ref.bounds) * 100.0;
                        if !parent_ref.title.is_empty()
                            && !parent_ax.title.is_empty()
                            && parent_ref.title.eq_ignore_ascii_case(&parent_ax.title)
                        {
                            score += 10.0;
                        }
                        if score <= 0.01 {
                            continue;
                        }
                        match best_parent {
                            None => best_parent = Some((parent_ax_idx, score)),
                            Some((_, best_score)) if score > best_score => {
                                best_parent = Some((parent_ax_idx, score))
                            }
                            _ => {}
                        }
                    }
                }
            }
            let Some((parent_ax_idx, _)) = best_parent else {
                continue;
            };
            let Some(parent_cg_idx) = ax_to_cg.get(&parent_ax_idx).copied() else {
                continue;
            };
            if parent_cg_idx == *cg_idx {
                continue;
            }
            windows[*cg_idx].parent_id = Some(windows[parent_cg_idx].id.clone());
        }

        if let Some(main_ref) = app_main_window.as_ref() {
            let mut best_main: Option<(usize, f64)> = None;
            for (ax_idx, ax) in ax_windows.iter().enumerate() {
                let mut score = iou(&ax.bounds, &main_ref.bounds) * 100.0;
                if !main_ref.title.is_empty()
                    && !ax.title.is_empty()
                    && main_ref.title.eq_ignore_ascii_case(&ax.title)
                {
                    score += 10.0;
                }
                match best_main {
                    None if score > 0.01 => best_main = Some((ax_idx, score)),
                    Some((_, best_score)) if score > best_score => {
                        best_main = Some((ax_idx, score))
                    }
                    _ => {}
                }
            }
            if let Some((main_ax_idx, _)) = best_main {
                if let Some(main_cg_idx) = ax_to_cg.get(&main_ax_idx).copied() {
                    let main_parent_id = windows[main_cg_idx].id.clone();
                    for (cg_idx, _ax_idx) in &cg_to_ax {
                        let window = &mut windows[*cg_idx];
                        if window.parent_id.is_none() && window.modal == Some(true) {
                            if *cg_idx != main_cg_idx {
                                window.parent_id = Some(main_parent_id.clone());
                            }
                        }
                    }
                }
            }
        }
    }
}

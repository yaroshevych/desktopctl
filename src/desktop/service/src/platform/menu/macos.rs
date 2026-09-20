use super::{MenuActionResult, MenuNode, MenuSnapshot};
use accessibility::{AXAttribute, AXUIElement, Error as AxError};
use accessibility_sys::{
    AXUIElementCopyMultipleAttributeValues, kAXEnabledAttribute, kAXErrorAPIDisabled,
    kAXErrorActionUnsupported, kAXMenuBarAttribute, kAXMenuItemCmdCharAttribute,
    kAXMenuItemCmdGlyphAttribute, kAXMenuItemCmdModifiersAttribute,
    kAXMenuItemCmdVirtualKeyAttribute, kAXMenuItemMarkCharAttribute, kAXMenuItemModifierControl,
    kAXMenuItemModifierNoCommand, kAXMenuItemModifierOption, kAXMenuItemModifierShift,
    kAXPressAction, kAXRoleAttribute, kAXTitleAttribute,
};
use core_foundation::{
    array::{CFArray, CFArrayRef},
    base::{CFType, TCFType},
    boolean::CFBoolean,
    number::CFNumber,
    string::CFString,
};
use desktop_core::error::{AppError, ErrorCode};
use std::cell::OnceCell;
use std::collections::HashMap;
use std::thread;
use unicode_normalization::UnicodeNormalization;

const MENU_CHILD_TRAVERSAL_LIMIT: usize = 15;
const MENU_CHILD_TRUNCATION_THRESHOLD: usize = 20;

#[derive(Clone, Copy)]
struct MenuOptions {
    early_prune: bool,
    parallelism: usize,
    batch_attributes: bool,
}

fn menu_options() -> MenuOptions {
    let env_flag = |name: &str| std::env::var(name).ok().as_deref() == Some("1");
    let parallelism = std::env::var("DESKTOPCTL_MENU_PARALLELISM")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|value| *value > 1)
        .unwrap_or(1)
        .min(16);
    MenuOptions {
        early_prune: env_flag("DESKTOPCTL_MENU_EARLY_PRUNE"),
        parallelism,
        batch_attributes: env_flag("DESKTOPCTL_MENU_BATCH_ATTRIBUTES"),
    }
}

#[derive(Clone)]
struct InternalNode {
    id: String,
    title: String,
    enabled: bool,
    action_supported: bool,
    shortcut: Option<String>,
    element: AXUIElement,
    structural: bool,
    path: String,
}

pub fn list(pid: i64, app_name: &str, system: bool, all: bool) -> Result<MenuSnapshot, AppError> {
    let mut options = menu_options();
    options.early_prune &= !all;
    let (items, _) = build_tree(pid, app_name, system, options, false)?;
    let mut items = items;
    if !options.early_prune {
        for item in &mut items {
            annotate_node(item, all);
        }
    }
    Ok(MenuSnapshot { items })
}

pub fn click(
    pid: i64,
    app_name: &str,
    id: Option<&str>,
    path: Option<&str>,
) -> Result<MenuActionResult, AppError> {
    // Click resolution includes system nodes; `--system` controls list output only.
    let mut options = menu_options();
    // A click must always be able to resolve an item omitted from a bounded
    // list response. The parallel and batched paths are still safe to use.
    options.early_prune = false;
    let (_, internals) = build_tree(pid, app_name, true, options, true)?;
    let matches: Vec<&InternalNode> = if let Some(id) = id {
        internals.iter().filter(|node| node.id == id).collect()
    } else {
        let wanted: Vec<&str> = path.unwrap_or_default().split('>').map(str::trim).collect();
        internals
            .iter()
            .filter(|node| {
                node.path == path.unwrap_or_default().trim()
                    && node.title == wanted.last().copied().unwrap_or_default()
            })
            .collect()
    };
    let node = if id.is_some() {
        matches
            .first()
            .copied()
            .ok_or_else(|| AppError::new(ErrorCode::MenuItemIdNotFound, "menu item id not found"))?
    } else {
        if matches.is_empty() {
            return Err(AppError::new(
                ErrorCode::MenuItemNotFound,
                "menu item path not found",
            ));
        }
        if matches.len() > 1 {
            return Err(AppError::new(
                ErrorCode::MenuItemAmbiguous,
                "menu item path is ambiguous",
            ));
        }
        matches[0]
    };
    if node.structural || node.title.trim().is_empty() || !node.action_supported {
        return Err(AppError::new(
            ErrorCode::MenuActionUnsupported,
            "menu item does not support AXPress",
        ));
    }
    if !node.enabled {
        return Err(AppError::new(
            ErrorCode::MenuItemDisabled,
            "menu item is disabled",
        ));
    }
    if crate::platform::ax::frontmost_app_pid() != Some(pid) {
        return Err(AppError::new(
            ErrorCode::MenuActionUnsupported,
            "active window owner changed before menu action",
        ));
    }
    let action = CFString::from_static_string(kAXPressAction);
    match node.element.perform_action(&action) {
        Ok(()) => Ok(MenuActionResult {
            id: node.id.clone(),
            title: node.title.clone(),
            shortcut: node.shortcut.clone(),
        }),
        Err(AxError::Ax(code)) if code == kAXErrorActionUnsupported => Err(AppError::new(
            ErrorCode::MenuActionUnsupported,
            "menu item does not support AXPress",
        )),
        Err(err) => Err(map_ax_error(err, "failed to press menu item")),
    }
}

fn build_tree(
    pid: i64,
    app_name: &str,
    include_system: bool,
    options: MenuOptions,
    include_internals: bool,
) -> Result<(Vec<MenuNode>, Vec<InternalNode>), AppError> {
    if pid <= 0 {
        return Err(AppError::new(
            ErrorCode::MenuBarUnavailable,
            "invalid application PID",
        ));
    }
    let app = AXUIElement::application(pid as _);
    let menu_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXMenuBarAttribute));
    let menu_value = app
        .attribute(&menu_attr)
        .map_err(|err| map_ax_error(err, "menu bar unavailable"))?;
    if !menu_value.instance_of::<AXUIElement>() {
        return Err(AppError::new(
            ErrorCode::MenuBarUnavailable,
            "menu bar unavailable",
        ));
    }
    let menu_bar = unsafe { AXUIElement::wrap_under_get_rule(menu_value.as_CFTypeRef() as _) };
    let children = menu_bar
        .attribute(&AXAttribute::children())
        .map_err(|err| map_ax_error(err, "menu bar unavailable"))?;
    let child_indices: Vec<usize> = if include_system {
        (0..children.len() as usize).collect()
    } else {
        let mut system_checked = false;
        let mut indices = Vec::new();
        for (index, child) in children.iter().enumerate() {
            if !system_checked {
                let role = attr_string(&child, kAXRoleAttribute).unwrap_or_default();
                if role == "AXMenuBarItem" {
                    system_checked = true;
                    continue;
                }
            }
            indices.push(index);
        }
        indices
    };
    let initial_counts = root_sibling_counts(&children, &child_indices, options.batch_attributes);
    let branches = if options.parallelism > 1 && child_indices.len() > 1 && !include_internals {
        build_branches_parallel(pid, &child_indices, &initial_counts, options)?
    } else {
        build_branches_serial(&children, &child_indices, &initial_counts, options)?
    };
    let mut items = Vec::new();
    let mut internals = Vec::new();
    for (branch_items, branch_internals) in branches {
        items.extend(branch_items);
        internals.extend(branch_internals);
    }
    if items.is_empty() {
        return Err(AppError::new(
            ErrorCode::MenuBarUnavailable,
            "menu bar exposes no children",
        ));
    }
    let _ = app_name;
    Ok((items, internals))
}

type BranchResult = Result<Vec<MenuNode>, AppError>;

fn root_sibling_counts(
    children: &CFArray<AXUIElement>,
    indices: &[usize],
    batch_attributes: bool,
) -> Vec<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    let mut initial = vec![HashMap::new(); children.len() as usize];
    for index in indices {
        let Some(child) = children.get(*index as isize) else {
            continue;
        };
        initial[*index] = counts.clone();
        let attrs = read_node_attributes(&child, batch_attributes);
        let role = attrs.role.as_deref().unwrap_or("AXUnknown");
        let title = attrs.title.as_deref().unwrap_or_default();
        if is_separator(role, title) {
            continue;
        }
        let id_title = if title.trim().is_empty() {
            if role == "AXMenuItem" {
                "separator"
            } else {
                "untitled"
            }
        } else {
            title
        };
        let base = format!("menu_{}", slug(id_title));
        *counts.entry(base).or_insert(0) += 1;
    }
    initial
}

fn build_branches_serial(
    children: &CFArray<AXUIElement>,
    indices: &[usize],
    initial_counts: &[HashMap<String, usize>],
    options: MenuOptions,
) -> Result<Vec<(Vec<MenuNode>, Vec<InternalNode>)>, AppError> {
    indices
        .iter()
        .map(|index| {
            let child = children.get(*index as isize).ok_or_else(|| {
                AppError::new(ErrorCode::MenuBarUnavailable, "menu child unavailable")
            })?;
            let mut items = Vec::new();
            let mut internals = Vec::new();
            let mut sibling_counts = initial_counts[*index].clone();
            append_public(
                &child,
                &[],
                &mut sibling_counts,
                &mut items,
                &mut internals,
                options,
            );
            Ok((items, internals))
        })
        .collect()
}

fn build_branches_parallel(
    pid: i64,
    indices: &[usize],
    initial_counts: &[HashMap<String, usize>],
    options: MenuOptions,
) -> Result<Vec<(Vec<MenuNode>, Vec<InternalNode>)>, AppError> {
    let worker_count = options.parallelism.min(indices.len()).max(1);
    let mut output: Vec<Option<BranchResult>> = (0..indices.len()).map(|_| None).collect();
    thread::scope(|scope| {
        let mut handles = Vec::new();
        for chunk in indices
            .iter()
            .copied()
            .enumerate()
            .collect::<Vec<_>>()
            .chunks((indices.len() + worker_count - 1) / worker_count)
        {
            // AXUIElement is intentionally not moved across threads. Each
            // worker reacquires the application and branch by index.
            let chunk = chunk.to_vec();
            handles.push(scope.spawn(move || {
                let mut results = Vec::new();
                for (position, index) in chunk {
                    let result = (|| {
                        let app = AXUIElement::application(pid as _);
                        let menu_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(
                            kAXMenuBarAttribute,
                        ));
                        let menu_value = app
                            .attribute(&menu_attr)
                            .map_err(|err| map_ax_error(err, "menu bar unavailable"))?;
                        let menu_bar = if menu_value.instance_of::<AXUIElement>() {
                            unsafe {
                                AXUIElement::wrap_under_get_rule(menu_value.as_CFTypeRef() as _)
                            }
                        } else {
                            return Err(AppError::new(
                                ErrorCode::MenuBarUnavailable,
                                "menu bar unavailable",
                            ));
                        };
                        let children = menu_bar
                            .attribute(&AXAttribute::children())
                            .map_err(|err| map_ax_error(err, "menu bar unavailable"))?;
                        let child = children.get(index as isize).ok_or_else(|| {
                            AppError::new(ErrorCode::MenuBarUnavailable, "menu child unavailable")
                        })?;
                        let mut items = Vec::new();
                        let mut internals = Vec::new();
                        let mut sibling_counts = initial_counts[index].clone();
                        append_public(
                            &child,
                            &[],
                            &mut sibling_counts,
                            &mut items,
                            &mut internals,
                            options,
                        );
                        drop(internals);
                        Ok(items)
                    })();
                    results.push((position, result));
                }
                results
            }));
        }
        for handle in handles {
            for (position, result) in handle.join().unwrap_or_else(|_| {
                vec![(
                    0,
                    Err(AppError::new(
                        ErrorCode::MenuBarUnavailable,
                        "menu worker panicked",
                    )),
                )]
            }) {
                output[position] = Some(result);
            }
        }
    });
    output
        .into_iter()
        .map(|result| {
            result
                .expect("parallel menu branch missing")
                .map(|items| (items, Vec::new()))
        })
        .collect()
}

fn append_public(
    element: &AXUIElement,
    ancestors: &[String],
    sibling_counts: &mut HashMap<String, usize>,
    output: &mut Vec<MenuNode>,
    internals: &mut Vec<InternalNode>,
    options: MenuOptions,
) {
    let attrs = read_node_attributes(element, options.batch_attributes);
    let role = attrs
        .role
        .clone()
        .unwrap_or_else(|| "AXUnknown".to_string());
    if role == "AXMenu" {
        if let Some(children) = children(element) {
            let limit = child_limit(children.len(), options.early_prune);
            for (index, child) in children.iter().enumerate() {
                if index >= limit {
                    break;
                }
                append_public(
                    &child,
                    ancestors,
                    sibling_counts,
                    output,
                    internals,
                    options,
                );
            }
        }
        return;
    }
    let title = attrs.title.clone().unwrap_or_default();
    if is_separator(&role, &title) {
        return;
    }
    let path = ancestors_with(&ancestors, &title).join(" > ");
    let id_title = if title.trim().is_empty() {
        if role == "AXMenuItem" {
            "separator"
        } else {
            "untitled"
        }
    } else {
        title.as_str()
    };
    let enabled = attrs.enabled.unwrap_or(true);
    let action_supported = action_supported(element);
    let base = format!(
        "menu_{}",
        slug(
            &ancestors
                .iter()
                .cloned()
                .chain(std::iter::once(id_title.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        )
    );
    let count = sibling_counts.entry(base.clone()).or_insert(0);
    *count += 1;
    let id = if *count == 1 {
        base
    } else {
        format!("{base}_{}", count)
    };
    let shortcut = shortcut_from_attributes(element, &attrs);
    let mark = attrs.mark.filter(|s| !s.is_empty());
    let mut child_nodes = Vec::new();
    let mut child_counts = HashMap::new();
    if let Some(children) = children(element) {
        let limit = child_limit(children.len(), options.early_prune);
        for (index, child) in children.iter().enumerate() {
            if index >= limit {
                break;
            }
            append_public(
                &child,
                &ancestors_with(&ancestors, &title),
                &mut child_counts,
                &mut child_nodes,
                internals,
                options,
            );
        }
    }
    let kind = classify_kind(&title, enabled, action_supported, !child_nodes.is_empty());
    let (truncated, omitted_count) = if options.early_prune {
        let full_count = children(element)
            .map(|value| value.len() as usize)
            .unwrap_or(0);
        if full_count > MENU_CHILD_TRUNCATION_THRESHOLD {
            (true, full_count.saturating_sub(MENU_CHILD_TRAVERSAL_LIMIT))
        } else {
            (false, 0)
        }
    } else {
        (false, 0)
    };
    let node = InternalNode {
        id: id.clone(),
        title: title.clone(),
        enabled,
        action_supported,
        shortcut: shortcut.clone(),
        element: element.clone(),
        structural: kind == "group",
        path,
    };
    internals.push(node);
    output.push(MenuNode {
        id,
        title,
        role,
        enabled,
        action_supported,
        shortcut,
        mark,
        kind: kind.to_string(),
        children: child_nodes,
        truncated,
        omitted_count,
    });
}

fn child_limit(child_count: isize, early_prune: bool) -> usize {
    let child_count = child_count.max(0) as usize;
    if early_prune && child_count > MENU_CHILD_TRUNCATION_THRESHOLD {
        MENU_CHILD_TRAVERSAL_LIMIT
    } else {
        child_count
    }
}

fn annotate_node(node: &mut MenuNode, all: bool) {
    let full_count = node.children.len();
    if !all && full_count > 20 {
        node.children.truncate(15);
        node.truncated = true;
        node.omitted_count = full_count - 15;
    } else {
        node.truncated = false;
        node.omitted_count = 0;
    }
    for child in &mut node.children {
        annotate_node(child, all);
    }
}

fn classify_kind(
    title: &str,
    enabled: bool,
    action_supported: bool,
    has_children: bool,
) -> &'static str {
    if has_children {
        "submenu"
    } else if !title.trim().is_empty() && !enabled && !action_supported {
        "group"
    } else {
        "item"
    }
}

fn is_separator(role: &str, title: &str) -> bool {
    role == "AXMenuItem" && title.trim().is_empty()
}

fn ancestors_with(ancestors: &[String], title: &str) -> Vec<String> {
    let mut out = ancestors.to_vec();
    if !title.is_empty() {
        out.push(title.to_string());
    }
    out
}

fn children(element: &AXUIElement) -> Option<CFArray<AXUIElement>> {
    let attr = AXAttribute::children();
    element.attribute(&attr).ok()
}

#[derive(Default)]
struct NodeAttributes {
    role: Option<String>,
    title: Option<String>,
    enabled: Option<bool>,
    cmd_char: Option<String>,
    cmd_glyph: Option<String>,
    cmd_glyph_value: Option<u32>,
    cmd_modifiers: Option<u32>,
    cmd_virtual_key: Option<u32>,
    mark: Option<String>,
}

const BATCH_ATTRIBUTE_NAMES: [&str; 8] = [
    kAXRoleAttribute,
    kAXTitleAttribute,
    kAXEnabledAttribute,
    kAXMenuItemCmdCharAttribute,
    kAXMenuItemCmdGlyphAttribute,
    kAXMenuItemCmdModifiersAttribute,
    kAXMenuItemCmdVirtualKeyAttribute,
    kAXMenuItemMarkCharAttribute,
];

thread_local! {
    static MENU_BATCH_ATTRIBUTES: OnceCell<CFArray<CFString>> = const { OnceCell::new() };
}

fn batch_attribute_array() -> CFArrayRef {
    MENU_BATCH_ATTRIBUTES.with(|cell| {
        cell.get_or_init(|| {
            let names: Vec<CFString> = BATCH_ATTRIBUTE_NAMES
                .iter()
                .map(|name| CFString::from_static_string(name))
                .collect();
            CFArray::from_CFTypes(&names)
        })
        .as_concrete_TypeRef()
    })
}

fn read_node_attributes(element: &AXUIElement, batch: bool) -> NodeAttributes {
    if batch {
        if let Some(values) = batch_node_attributes(element) {
            return NodeAttributes {
                role: values
                    .get(0)
                    .and_then(|value| value.as_ref().and_then(value_string)),
                title: values
                    .get(1)
                    .and_then(|value| value.as_ref().and_then(value_string)),
                enabled: values
                    .get(2)
                    .and_then(|value| value.as_ref().and_then(value_bool)),
                cmd_char: values
                    .get(3)
                    .and_then(|value| value.as_ref().and_then(value_string)),
                cmd_glyph: values
                    .get(4)
                    .and_then(|value| value.as_ref().and_then(value_string)),
                cmd_glyph_value: values
                    .get(4)
                    .and_then(|value| value.as_ref().and_then(value_u32)),
                cmd_modifiers: values
                    .get(5)
                    .and_then(|value| value.as_ref().and_then(value_u32)),
                cmd_virtual_key: values
                    .get(6)
                    .and_then(|value| value.as_ref().and_then(value_u32)),
                mark: values
                    .get(7)
                    .and_then(|value| value.as_ref().and_then(value_string)),
            };
        }
    }
    NodeAttributes {
        role: attr_string(element, kAXRoleAttribute),
        title: attr_string(element, kAXTitleAttribute),
        enabled: attr_bool(element, kAXEnabledAttribute),
        cmd_char: attr_string(element, kAXMenuItemCmdCharAttribute),
        cmd_glyph: attr_string(element, kAXMenuItemCmdGlyphAttribute),
        cmd_glyph_value: attr_u32(element, kAXMenuItemCmdGlyphAttribute),
        cmd_modifiers: attr_u32(element, kAXMenuItemCmdModifiersAttribute),
        cmd_virtual_key: attr_u32(element, kAXMenuItemCmdVirtualKeyAttribute),
        mark: attr_string(element, kAXMenuItemMarkCharAttribute),
    }
}

fn batch_node_attributes(element: &AXUIElement) -> Option<Vec<Option<CFType>>> {
    let mut out: CFArrayRef = std::ptr::null();
    let error = unsafe {
        AXUIElementCopyMultipleAttributeValues(
            element.as_concrete_TypeRef(),
            batch_attribute_array(),
            0,
            &mut out,
        )
    };
    if error != accessibility_sys::kAXErrorSuccess || out.is_null() {
        return None;
    }
    let values = unsafe { CFArray::<CFType>::wrap_under_create_rule(out) };
    Some(
        (0..BATCH_ATTRIBUTE_NAMES.len())
            .map(|index| values.get(index as isize).map(|value| (*value).clone()))
            .collect(),
    )
}

fn value_string(value: &CFType) -> Option<String> {
    value.downcast::<CFString>().map(|value| value.to_string())
}

fn value_bool(value: &CFType) -> Option<bool> {
    value.downcast::<CFBoolean>().map(bool::from)
}

fn value_u32(value: &CFType) -> Option<u32> {
    value
        .downcast::<CFNumber>()
        .and_then(|value| value.to_i64().map(|value| value as u32))
}

fn attr_string(element: &AXUIElement, name: &str) -> Option<String> {
    let attr = AXAttribute::<CFType>::new(&CFString::new(name));
    let value = element.attribute(&attr).ok()?;
    value.downcast::<CFString>().map(|s| s.to_string())
}

fn attr_bool(element: &AXUIElement, name: &str) -> Option<bool> {
    let attr = AXAttribute::<CFType>::new(&CFString::new(name));
    let value = element.attribute(&attr).ok()?;
    value.downcast::<CFBoolean>().map(bool::from)
}

fn attr_u32(element: &AXUIElement, name: &str) -> Option<u32> {
    let attr = AXAttribute::<CFType>::new(&CFString::new(name));
    let value = element.attribute(&attr).ok()?;
    value
        .downcast::<CFNumber>()
        .and_then(|n| n.to_i64().map(|v| v as u32))
}

fn action_supported(element: &AXUIElement) -> bool {
    let Ok(actions) = element.action_names() else {
        return false;
    };
    actions
        .iter()
        .any(|name| name.to_string() == kAXPressAction)
}

fn shortcut(element: &AXUIElement) -> Option<String> {
    let key = attr_string(element, kAXMenuItemCmdCharAttribute)
        .filter(|v| !v.is_empty())
        .or_else(|| glyph_key(element))
        .or_else(|| attr_u32(element, kAXMenuItemCmdVirtualKeyAttribute).and_then(virtual_key));
    let modifiers = attr_u32(element, kAXMenuItemCmdModifiersAttribute).unwrap_or(0);
    format_shortcut(key?, modifiers)
}

fn shortcut_from_attributes(element: &AXUIElement, attrs: &NodeAttributes) -> Option<String> {
    let key = attrs
        .cmd_char
        .clone()
        .filter(|value| !value.is_empty())
        .or_else(|| {
            attrs
                .cmd_glyph
                .clone()
                .filter(|value| !value.is_empty())
                .and_then(|value| {
                    if value == "🎤" || value == "🌐" {
                        None
                    } else {
                        Some(value)
                    }
                })
        })
        .or_else(|| attrs.cmd_glyph_value.and_then(glyph_key_value))
        .or_else(|| attrs.cmd_virtual_key.and_then(virtual_key));
    let modifiers = attrs.cmd_modifiers.unwrap_or(0);
    key.map(|key| format_shortcut(key, modifiers))
        .flatten()
        .or_else(|| shortcut(element))
}

fn glyph_key(element: &AXUIElement) -> Option<String> {
    if let Some(value) =
        attr_string(element, kAXMenuItemCmdGlyphAttribute).filter(|v| !v.is_empty())
    {
        if value == "🎤" || value == "🌐" {
            return None;
        }
        return Some(value);
    }
    attr_u32(element, kAXMenuItemCmdGlyphAttribute).and_then(glyph_key_value)
}

fn format_shortcut(key: String, modifiers: u32) -> Option<String> {
    let key = normalize_shortcut_key(key)?;
    if key.chars().all(|ch| ch.is_ascii_digit()) {
        return None;
    }
    let mut out = Vec::new();
    if modifiers & kAXMenuItemModifierControl != 0 {
        out.push("ctrl");
    }
    if modifiers & kAXMenuItemModifierOption != 0 {
        out.push("alt");
    }
    if modifiers & kAXMenuItemModifierShift != 0 {
        out.push("shift");
    }
    if modifiers & kAXMenuItemModifierNoCommand == 0 {
        out.push("cmd");
    }
    out.push(key.as_str());
    Some(out.join("+"))
}

fn normalize_shortcut_key(key: String) -> Option<String> {
    let key = key.trim().to_lowercase();
    if key.is_empty() || key == "🎤" || key == "🌐" {
        return None;
    }
    Some(match key.as_str() {
        "\u{f700}" => "up".to_string(),
        "\u{f701}" => "down".to_string(),
        "\u{f702}" => "left".to_string(),
        "\u{f703}" => "right".to_string(),
        _ => key,
    })
}

fn virtual_key(key: u32) -> Option<String> {
    Some(
        match key {
            51 => "delete",
            36 => "return",
            48 => "tab",
            49 => "space",
            53 => "escape",
            117 => "forwarddelete",
            123 => "left",
            124 => "right",
            125 => "down",
            126 => "up",
            _ => return None,
        }
        .to_string(),
    )
}

fn glyph_key_value(value: u32) -> Option<String> {
    // AXMenuItemCmdGlyph uses AppKit glyph values. Accept known printable
    // glyphs only; unknown numeric values must fall through to virtual key.
    let key = match value {
        0xf700 => "up",
        0xf701 => "down",
        0xf702 => "left",
        0xf703 => "right",
        0x232b => "delete",
        0x2326 => "forwarddelete",
        0x21b5 => "return",
        0x21e5 => "tab",
        0x238b => "escape",
        0x2423 => "space",
        0x2190 => "left",
        0x2191 => "up",
        0x2192 => "right",
        0x2193 => "down",
        _ => return None,
    };
    Some(key.to_string())
}

fn slug(value: &str) -> String {
    let mut out = String::new();
    for ch in value
        .trim()
        .nfc()
        .collect::<String>()
        .to_lowercase()
        .chars()
    {
        if ch.is_alphanumeric() {
            out.push(ch);
        } else if ch.is_whitespace() && !out.ends_with('_') {
            out.push('_');
        }
    }
    while out.ends_with('_') {
        out.pop();
    }
    if out.is_empty() {
        "untitled".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MENU_CHILD_TRUNCATION_THRESHOLD, annotate_node, child_limit, classify_kind,
        format_shortcut, glyph_key_value, is_separator, slug,
    };
    use crate::platform::menu::MenuNode;
    use accessibility_sys::{
        kAXMenuItemModifierControl, kAXMenuItemModifierNoCommand, kAXMenuItemModifierOption,
        kAXMenuItemModifierShift,
    };

    #[test]
    fn formats_shortcuts_with_inverted_command_modifier() {
        let modifiers =
            kAXMenuItemModifierControl | kAXMenuItemModifierOption | kAXMenuItemModifierShift;
        assert_eq!(
            format_shortcut("N".into(), modifiers),
            Some("ctrl+alt+shift+cmd+n".into())
        );
        assert_eq!(
            format_shortcut("N".into(), modifiers | kAXMenuItemModifierNoCommand),
            Some("ctrl+alt+shift+n".into())
        );
    }

    #[test]
    fn rejects_empty_or_numeric_shortcuts() {
        assert_eq!(format_shortcut("".into(), 0), None);
        assert_eq!(format_shortcut("8".into(), 0), None);
        assert_eq!(format_shortcut("🎤".into(), 0), None);
        assert_eq!(format_shortcut("\u{f700}".into(), 0), Some("cmd+up".into()));
    }

    #[test]
    fn maps_known_glyph_values_only() {
        assert_eq!(glyph_key_value(0xf700).as_deref(), Some("up"));
        assert_eq!(glyph_key_value(0xf703).as_deref(), Some("right"));
        assert_eq!(glyph_key_value(0x232b).as_deref(), Some("delete"));
        assert_eq!(glyph_key_value(0x2326).as_deref(), Some("forwarddelete"));
        assert_eq!(glyph_key_value(8), None);
    }

    #[test]
    fn slug_normalizes_unicode_and_retains_non_latin() {
        assert_eq!(slug("Cafe\u{301}"), slug("Café"));
        assert_eq!(slug("日本語 メニュー"), "日本語_メニュー");
    }

    #[test]
    fn classifies_noninteractive_titled_nodes_as_groups() {
        assert_eq!(classify_kind("Halves", false, false, false), "group");
        assert_eq!(
            classify_kind("Move & Resize", false, false, true),
            "submenu"
        );
        assert_eq!(classify_kind("Open", true, true, false), "item");
    }

    #[test]
    fn separators_are_not_public_nodes() {
        assert!(is_separator("AXMenuItem", "  "));
        assert!(!is_separator("AXMenuItem", "Open"));
        assert!(!is_separator("AXMenuBarItem", ""));
    }

    fn node_with_children(count: usize) -> MenuNode {
        MenuNode {
            id: "menu_parent".into(),
            title: "Parent".into(),
            role: "AXMenuItem".into(),
            enabled: true,
            action_supported: true,
            shortcut: None,
            mark: None,
            kind: "submenu".into(),
            children: (0..count)
                .map(|i| MenuNode {
                    id: format!("menu_child_{i}"),
                    title: format!("Child {i}"),
                    role: "AXMenuItem".into(),
                    enabled: true,
                    action_supported: true,
                    shortcut: None,
                    mark: None,
                    kind: "item".into(),
                    children: Vec::new(),
                    truncated: false,
                    omitted_count: 0,
                })
                .collect(),
            truncated: false,
            omitted_count: 0,
        }
    }

    #[test]
    fn truncates_children_at_boundary_and_recurses() {
        let mut twenty = node_with_children(20);
        annotate_node(&mut twenty, false);
        assert_eq!(twenty.children.len(), 20);
        assert!(!twenty.truncated);
        assert_eq!(twenty.omitted_count, 0);

        let mut twenty_one = node_with_children(21);
        annotate_node(&mut twenty_one, false);
        assert_eq!(twenty_one.children.len(), 15);
        assert!(twenty_one.truncated);
        assert_eq!(twenty_one.omitted_count, 6);
    }

    #[test]
    fn early_prune_only_limits_large_child_lists() {
        assert_eq!(child_limit(20, true), 20);
        assert_eq!(
            child_limit((MENU_CHILD_TRUNCATION_THRESHOLD + 1) as isize, true),
            15
        );
        assert_eq!(child_limit(200, false), 200);
    }

    #[test]
    fn all_keeps_full_recursive_children() {
        let mut node = node_with_children(21);
        node.children[0].children = (0..21)
            .map(|i| node_with_children(i).children)
            .flatten()
            .collect();
        annotate_node(&mut node, true);
        assert_eq!(node.children.len(), 21);
        assert!(!node.truncated);
        assert_eq!(node.omitted_count, 0);
        assert_eq!(node.children[0].children.len(), 210);
        assert!(!node.children[0].truncated);
    }
}

fn map_ax_error(err: AxError, context: &str) -> AppError {
    if matches!(err, AxError::Ax(code) if code == kAXErrorAPIDisabled) {
        AppError::new(
            ErrorCode::AccessibilityPermissionRequired,
            "Accessibility permission required; enable DesktopCtl in System Settings → Privacy & Security → Accessibility",
        )
    } else {
        AppError::new(ErrorCode::MenuBarUnavailable, format!("{context}: {err}"))
    }
}

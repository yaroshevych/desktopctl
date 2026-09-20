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
use std::time::{Duration, Instant};
use unicode_normalization::UnicodeNormalization;

const MENU_CHILD_TRAVERSAL_LIMIT: usize = 15;
const MENU_CHILD_TRUNCATION_THRESHOLD: usize = 20;

#[derive(Clone, Copy)]
struct MenuOptions {
    early_prune: bool,
    batch_attributes: bool,
}

fn menu_options() -> MenuOptions {
    let env_flag = |name: &str| std::env::var(name).ok().as_deref() == Some("1");
    MenuOptions {
        early_prune: env_flag("DESKTOPCTL_MENU_EARLY_PRUNE"),
        batch_attributes: env_flag("DESKTOPCTL_MENU_BATCH_ATTRIBUTES"),
    }
}

const MENU_PROFILE_SLOW_AX_MS: u128 = 2;
const MENU_PROFILE_MAX_SAMPLES: usize = 20;

struct MenuProfile {
    started: Instant,
    menu_bar_lookup: Duration,
    root_children: Duration,
    tree: Duration,
    annotation: Duration,
    attrs_calls: usize,
    batch_attrs_calls: usize,
    scalar_attrs_calls: usize,
    attrs_time: Duration,
    children_calls: usize,
    children_time: Duration,
    visited_nodes: usize,
    output_nodes: usize,
    omitted_nodes: usize,
    samples: Vec<String>,
}

impl MenuProfile {
    fn from_env() -> Option<Self> {
        (std::env::var("DESKTOPCTL_MENU_PROFILE").ok().as_deref() == Some("1")).then(|| Self {
            started: Instant::now(),
            menu_bar_lookup: Duration::ZERO,
            root_children: Duration::ZERO,
            tree: Duration::ZERO,
            annotation: Duration::ZERO,
            attrs_calls: 0,
            batch_attrs_calls: 0,
            scalar_attrs_calls: 0,
            attrs_time: Duration::ZERO,
            children_calls: 0,
            children_time: Duration::ZERO,
            visited_nodes: 0,
            output_nodes: 0,
            omitted_nodes: 0,
            samples: Vec::new(),
        })
    }

    fn record_ax(&mut self, operation: &str, elapsed: Duration, path: &str) {
        if elapsed.as_millis() < MENU_PROFILE_SLOW_AX_MS
            || self.samples.len() >= MENU_PROFILE_MAX_SAMPLES
        {
            return;
        }
        self.samples.push(format!(
            "op={operation} ms={:.1} path={}",
            elapsed.as_secs_f64() * 1000.0,
            safe_profile_label(path),
        ));
    }

    fn phase(name: &str, elapsed: Duration) {
        crate::trace::log(format!(
            "menu_profile:phase name={name} ms={:.1}",
            elapsed.as_secs_f64() * 1000.0
        ));
    }

    fn finish(self) {
        crate::trace::log(format!(
            "menu_profile:summary total_ms={:.1} tree_ms={:.1} annotate_ms={:.1} nodes={} visited={} omitted={} attrs={} batch_attrs={} scalar_attrs={} attrs_ms={:.1} children={} children_ms={:.1} samples={}",
            self.started.elapsed().as_secs_f64() * 1000.0,
            self.tree.as_secs_f64() * 1000.0,
            self.annotation.as_secs_f64() * 1000.0,
            self.output_nodes,
            self.visited_nodes,
            self.omitted_nodes,
            self.attrs_calls,
            self.batch_attrs_calls,
            self.scalar_attrs_calls,
            self.attrs_time.as_secs_f64() * 1000.0,
            self.children_calls,
            self.children_time.as_secs_f64() * 1000.0,
            self.samples.len(),
        ));
        for sample in self.samples {
            crate::trace::log(format!("menu_profile:slow_ax {sample}"));
        }
    }
}

fn safe_profile_label(value: &str) -> String {
    let mut label = value
        .chars()
        .filter(|ch| !ch.is_control())
        .map(|ch| if ch == '=' { '_' } else { ch })
        .collect::<String>();
    if label.len() > 100 {
        label.truncate(100);
        label.push('…');
    }
    if label.is_empty() {
        "<root>".to_string()
    } else {
        label
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
    let mut profile = MenuProfile::from_env();
    if profile.is_some() {
        crate::trace::log(format!(
            "menu_profile:start pid={pid} system={system} all={all}"
        ));
    }
    let tree_started = profile.as_ref().map(|_| Instant::now());
    let (items, _) = build_tree(pid, app_name, system, options, &mut profile)?;
    if let (Some(profile), Some(started)) = (profile.as_mut(), tree_started) {
        profile.tree = started.elapsed();
        MenuProfile::phase("tree", profile.tree);
    }
    let mut items = items;
    let annotation_started = profile.as_ref().map(|_| Instant::now());
    if !options.early_prune {
        for item in &mut items {
            annotate_node(item, all, &mut profile);
        }
    }
    if let (Some(profile), Some(started)) = (profile.as_mut(), annotation_started) {
        profile.annotation = started.elapsed();
        MenuProfile::phase("annotation", profile.annotation);
    }
    if let Some(profile) = profile {
        profile.finish();
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
    // list response.
    options.early_prune = false;
    let mut no_profile = None;
    let (_, internals) = build_tree(pid, app_name, true, options, &mut no_profile)?;
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
    if node.structural || node.title.trim().is_empty() {
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
    if !node.action_supported {
        return Err(AppError::new(
            ErrorCode::MenuActionUnsupported,
            "menu item does not support AXPress",
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
    profile: &mut Option<MenuProfile>,
) -> Result<(Vec<MenuNode>, Vec<InternalNode>), AppError> {
    if pid <= 0 {
        return Err(AppError::new(
            ErrorCode::MenuBarUnavailable,
            "invalid application PID",
        ));
    }
    let app = AXUIElement::application(pid as _);
    let menu_attr = AXAttribute::<CFType>::new(&CFString::from_static_string(kAXMenuBarAttribute));
    let menu_started = profile.as_ref().map(|_| Instant::now());
    let menu_value = app
        .attribute(&menu_attr)
        .map_err(|err| map_ax_error(err, "menu bar unavailable"))?;
    if let (Some(profile), Some(started)) = (profile.as_mut(), menu_started) {
        profile.menu_bar_lookup = started.elapsed();
        MenuProfile::phase("menu_bar_lookup", profile.menu_bar_lookup);
        profile.record_ax("menu_bar_lookup", profile.menu_bar_lookup, "<app>");
    }
    if !menu_value.instance_of::<AXUIElement>() {
        return Err(AppError::new(
            ErrorCode::MenuBarUnavailable,
            "menu bar unavailable",
        ));
    }
    let menu_bar = unsafe { AXUIElement::wrap_under_get_rule(menu_value.as_CFTypeRef() as _) };
    let root_started = profile.as_ref().map(|_| Instant::now());
    let children = menu_bar
        .attribute(&AXAttribute::children())
        .map_err(|err| map_ax_error(err, "menu bar unavailable"))?;
    if let (Some(profile), Some(started)) = (profile.as_mut(), root_started) {
        profile.root_children = started.elapsed();
        MenuProfile::phase("root_children", profile.root_children);
        profile.record_ax("root_children", profile.root_children, "<menu_bar>");
    }
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
    let initial_counts =
        root_sibling_counts(&children, &child_indices, options.batch_attributes, profile);
    let branches =
        build_branches_serial(&children, &child_indices, &initial_counts, options, profile)?;
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

fn root_sibling_counts(
    children: &CFArray<AXUIElement>,
    indices: &[usize],
    batch_attributes: bool,
    profile: &mut Option<MenuProfile>,
) -> Vec<HashMap<String, usize>> {
    let mut counts = HashMap::new();
    let mut initial = vec![HashMap::new(); children.len() as usize];
    for index in indices {
        let Some(child) = children.get(*index as isize) else {
            continue;
        };
        initial[*index] = counts.clone();
        let attrs = read_node_attributes(&child, batch_attributes, profile, "<root>");
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
    profile: &mut Option<MenuProfile>,
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
            let branch_started = profile.as_ref().map(|_| Instant::now());
            let attrs_before = profile.as_ref().map_or(0, |profile| profile.attrs_calls);
            let children_before = profile
                .as_ref()
                .map_or(0, |profile| profile.children_calls);
            append_public(
                &child,
                &[],
                &mut sibling_counts,
                &mut items,
                &mut internals,
                options,
                profile,
            );
            if let (Some(profile), Some(started)) = (profile.as_mut(), branch_started) {
                let title = items
                    .first()
                    .map(|item| safe_profile_label(&item.title))
                    .unwrap_or_else(|| format!("<branch_{index}>"));
                crate::trace::log(format!(
                    "menu_profile:branch index={index} title={title} ms={:.1} nodes={} attrs={} children={}",
                    started.elapsed().as_secs_f64() * 1000.0,
                    internals.len(),
                    profile.attrs_calls.saturating_sub(attrs_before),
                    profile.children_calls.saturating_sub(children_before),
                ));
            }
            Ok((items, internals))
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
    profile: &mut Option<MenuProfile>,
) {
    let path_hint = ancestors.join(" > ");
    let attrs = read_node_attributes(element, options.batch_attributes, profile, &path_hint);
    if let Some(profile) = profile.as_mut() {
        profile.visited_nodes += 1;
    }
    let role = attrs
        .role
        .clone()
        .unwrap_or_else(|| "AXUnknown".to_string());
    if role == "AXMenu" {
        if let Some(children) = children(element, profile, &path_hint) {
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
                    profile,
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
    // Fetch children once. The same snapshot determines traversal, submenu
    // classification, and early-prune omission metadata.
    let child_elements = children(element, profile, &path);
    if let Some(children) = child_elements.as_ref() {
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
                profile,
            );
        }
    }
    let has_children = !child_nodes.is_empty();
    let kind = classify_kind(&title, enabled, has_children);
    let is_leaf = child_elements
        .as_ref()
        .map_or(true, |children| children.is_empty());
    let action_supported = menu_item_action_supported(&role, enabled, is_leaf);
    let (truncated, omitted_count) = if options.early_prune {
        let full_count = child_elements
            .as_ref()
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
    if let Some(profile) = profile.as_mut() {
        profile.output_nodes += 1;
        profile.omitted_nodes += omitted_count;
    }
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

fn annotate_node(node: &mut MenuNode, all: bool, profile: &mut Option<MenuProfile>) {
    let full_count = node.children.len();
    if !all && full_count > 20 {
        node.children.truncate(15);
        node.truncated = true;
        node.omitted_count = full_count - 15;
        if let Some(profile) = profile.as_mut() {
            profile.omitted_nodes += node.omitted_count;
        }
    } else {
        node.truncated = false;
        node.omitted_count = 0;
    }
    for child in &mut node.children {
        annotate_node(child, all, profile);
    }
}

fn classify_kind(title: &str, enabled: bool, has_children: bool) -> &'static str {
    if has_children {
        "submenu"
    } else if !title.trim().is_empty() && !enabled {
        "group"
    } else {
        "item"
    }
}

fn menu_item_action_supported(role: &str, enabled: bool, is_leaf: bool) -> bool {
    role == "AXMenuItem" && enabled && is_leaf
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

fn children(
    element: &AXUIElement,
    profile: &mut Option<MenuProfile>,
    path: &str,
) -> Option<CFArray<AXUIElement>> {
    let attr = AXAttribute::children();
    if profile.is_none() {
        return element.attribute(&attr).ok();
    }
    let started = Instant::now();
    let result = element.attribute(&attr).ok();
    let elapsed = started.elapsed();
    if let Some(profile) = profile.as_mut() {
        profile.children_calls += 1;
        profile.children_time += elapsed;
        profile.record_ax("children", elapsed, path);
    }
    result
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

fn read_node_attributes(
    element: &AXUIElement,
    batch: bool,
    profile: &mut Option<MenuProfile>,
    path: &str,
) -> NodeAttributes {
    let started = profile.as_ref().map(|_| Instant::now());
    if batch {
        if let Some(values) = batch_node_attributes(element) {
            if let (Some(profile), Some(started)) = (profile.as_mut(), started) {
                let elapsed = started.elapsed();
                profile.attrs_calls += 1;
                profile.batch_attrs_calls += 1;
                profile.attrs_time += elapsed;
                profile.record_ax("batch_attributes", elapsed, path);
            }
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
    let attrs = NodeAttributes {
        role: attr_string(element, kAXRoleAttribute),
        title: attr_string(element, kAXTitleAttribute),
        enabled: attr_bool(element, kAXEnabledAttribute),
        cmd_char: attr_string(element, kAXMenuItemCmdCharAttribute),
        cmd_glyph: attr_string(element, kAXMenuItemCmdGlyphAttribute),
        cmd_glyph_value: attr_u32(element, kAXMenuItemCmdGlyphAttribute),
        cmd_modifiers: attr_u32(element, kAXMenuItemCmdModifiersAttribute),
        cmd_virtual_key: attr_u32(element, kAXMenuItemCmdVirtualKeyAttribute),
        mark: attr_string(element, kAXMenuItemMarkCharAttribute),
    };
    if let (Some(profile), Some(started)) = (profile.as_mut(), started) {
        let elapsed = started.elapsed();
        profile.attrs_calls += 1;
        profile.scalar_attrs_calls += 1;
        profile.attrs_time += elapsed;
        profile.record_ax("scalar_attributes", elapsed, path);
    }
    attrs
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
        MENU_CHILD_TRUNCATION_THRESHOLD, MENU_PROFILE_MAX_SAMPLES, MenuProfile, annotate_node,
        child_limit, classify_kind, format_shortcut, glyph_key_value, is_separator,
        menu_item_action_supported, safe_profile_label, slug,
    };
    use crate::platform::menu::MenuNode;
    use accessibility_sys::{
        kAXMenuItemModifierControl, kAXMenuItemModifierNoCommand, kAXMenuItemModifierOption,
        kAXMenuItemModifierShift,
    };
    use std::time::{Duration, Instant};

    #[test]
    fn profile_labels_are_bounded_and_log_safe() {
        let label = safe_profile_label("a=b\n\t".repeat(40).as_str());
        assert!(!label.contains('='));
        assert!(!label.chars().any(char::is_control));
        assert!(label.chars().count() <= 101);
    }

    #[test]
    fn profile_slow_samples_are_bounded() {
        let mut profile = MenuProfile {
            started: Instant::now(),
            menu_bar_lookup: Duration::ZERO,
            root_children: Duration::ZERO,
            tree: Duration::ZERO,
            annotation: Duration::ZERO,
            attrs_calls: 0,
            batch_attrs_calls: 0,
            scalar_attrs_calls: 0,
            attrs_time: Duration::ZERO,
            children_calls: 0,
            children_time: Duration::ZERO,
            visited_nodes: 0,
            output_nodes: 0,
            omitted_nodes: 0,
            samples: Vec::new(),
        };
        for _ in 0..(MENU_PROFILE_MAX_SAMPLES + 5) {
            profile.record_ax("children", Duration::from_millis(3), "item");
        }
        assert_eq!(profile.samples.len(), MENU_PROFILE_MAX_SAMPLES);
    }

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
        assert_eq!(classify_kind("Halves", false, false), "group");
        assert_eq!(classify_kind("Move & Resize", false, true), "submenu");
        assert_eq!(classify_kind("Open", true, false), "item");
    }

    #[test]
    fn supports_actions_only_for_enabled_leaf_menu_items() {
        assert!(menu_item_action_supported("AXMenuItem", true, true));
        assert!(!menu_item_action_supported("AXMenuItem", false, true));
        assert!(!menu_item_action_supported("AXMenuItem", true, false));
        assert!(!menu_item_action_supported("AXMenuBarItem", true, true));
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
        let mut no_profile = None;
        annotate_node(&mut twenty, false, &mut no_profile);
        assert_eq!(twenty.children.len(), 20);
        assert!(!twenty.truncated);
        assert_eq!(twenty.omitted_count, 0);

        let mut twenty_one = node_with_children(21);
        annotate_node(&mut twenty_one, false, &mut no_profile);
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
        let mut no_profile = None;
        node.children[0].children = (0..21)
            .map(|i| node_with_children(i).children)
            .flatten()
            .collect();
        annotate_node(&mut node, true, &mut no_profile);
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

//! Linux (AT-SPI2) accessibility backend.
//!
//! AT-SPI2 over the session a11y D-Bus is the semantic UI source feeding the
//! tokenizer (DesktopCtl-8u3 / -8o9). This collector walks the desktop tree
//!
//!   registry root -> application accessibles -> window/frame accessibles
//!   -> descendants
//!
//! and maps each accessible to an [`AxElement`].
//!
//! The daemon is synchronous and single-threaded, while `atspi`/`zbus` are
//! async; every async call is driven to completion with
//! [`futures_lite::future::block_on`]. No tokio, nothing `async` leaks out of
//! this module.
//!
//! Per the spec the AT-SPI tree is treated as fallible: objects go defunct,
//! trees are large/incomplete, bounds can be stale/missing, and custom-rendered
//! apps (Electron/Flutter/games) expose little. Traversal is therefore bounded
//! (depth + node caps + a per-call time budget), defunct objects are skipped,
//! and a missing/empty a11y bus yields `Ok(empty)`/`Ok(None)` rather than a
//! panic. A bus or connection failure surfaces as
//! `AppError::backend_unavailable(..)` with `failure_state =
//! "accessibility_unavailable"`.

use desktop_core::{
    error::AppError,
    protocol::{Bounds, ToggleState},
};

#[derive(Debug, Clone)]
pub struct AxElement {
    pub role: String,
    pub text: Option<String>,
    pub bounds: Bounds,
    pub ax_identifier: Option<String>,
    pub checked: Option<ToggleState>,
}

// ---------------------------------------------------------------------------
// Tuning knobs for bounded traversal.
// ---------------------------------------------------------------------------

/// Maximum descent below a top-level window before we stop recursing.
const MAX_DEPTH: u32 = 12;
/// Hard cap on the number of accessibles examined per collection call.
const MAX_NODES: usize = 2000;
/// Wall-clock budget per public collection call.
const TIME_BUDGET: std::time::Duration = std::time::Duration::from_millis(2500);

// ---------------------------------------------------------------------------
// Internals. Kept in a private module so the public surface stays minimal.
// ---------------------------------------------------------------------------

mod imp {
    use super::{AxElement, Bounds, MAX_DEPTH, MAX_NODES, TIME_BUDGET, ToggleState};
    use atspi::connection::AccessibilityConnection;
    use atspi::proxy::accessible::{AccessibleProxy, ObjectRefExt};
    use atspi::proxy::component::ComponentProxy;
    use atspi::proxy::text::TextProxy;
    use atspi::{CoordType, ObjectRef, Role, State, StateSet};
    use desktop_core::error::AppError;
    use futures_lite::future::block_on;
    use serde_json::json;
    use std::time::Instant;
    use zbus::Connection;

    /// Registry root accessible (the parent of every application root object).
    const REGISTRY_DEST: &str = "org.a11y.atspi.Registry";
    const REGISTRY_ROOT_PATH: &str = "/org/a11y/atspi/accessible/root";

    fn accessibility_unavailable(detail: impl std::fmt::Display) -> AppError {
        AppError::backend_unavailable(format!("accessibility bus unavailable: {detail}"))
            .with_details(json!({
                "failure_state": "accessibility_unavailable",
                "remediation": "ensure the AT-SPI accessibility bus is running for this session",
            }))
    }

    /// Shared per-call traversal state: connection handle plus the bounds we use
    /// to stop early (time budget + node cap).
    struct Walk<'a> {
        conn: &'a Connection,
        deadline: Instant,
        nodes: usize,
        out: Vec<AxElement>,
    }

    impl<'a> Walk<'a> {
        fn new(conn: &'a Connection) -> Self {
            Self {
                conn,
                deadline: Instant::now() + TIME_BUDGET,
                nodes: 0,
                out: Vec::new(),
            }
        }

        fn exhausted(&self) -> bool {
            self.nodes >= MAX_NODES || Instant::now() >= self.deadline
        }
    }

    /// Connect to the a11y bus. A failure here is a hard backend error.
    fn connect() -> Result<AccessibilityConnection, AppError> {
        block_on(AccessibilityConnection::new()).map_err(accessibility_unavailable)
    }

    /// Build an [`AccessibleProxy`] for the registry root.
    fn registry_root(conn: &Connection) -> Result<AccessibleProxy<'static>, zbus::Error> {
        block_on(
            AccessibleProxy::builder(conn)
                .destination(REGISTRY_DEST)?
                .path(REGISTRY_ROOT_PATH)?
                .cache_properties(zbus::proxy::CacheProperties::No)
                .build(),
        )
    }

    fn accessible_for<'a>(
        conn: &Connection,
        obj: &'a ObjectRef,
    ) -> Result<AccessibleProxy<'a>, zbus::Error> {
        block_on(obj.as_accessible_proxy(conn))
    }

    /// All application root objects currently on the a11y bus.
    fn applications(conn: &Connection) -> Vec<ObjectRef> {
        let Ok(root) = registry_root(conn) else {
            return Vec::new();
        };
        block_on(root.get_children()).unwrap_or_default()
    }

    /// Best-effort PID for an application via its unique bus name.
    fn pid_for(conn: &Connection, app: &ObjectRef) -> Option<i64> {
        let proxy = block_on(zbus::fdo::DBusProxy::new(conn)).ok()?;
        let name: zbus::names::BusName<'_> = app.name.as_str().try_into().ok()?;
        block_on(proxy.get_connection_unix_process_id(name))
            .ok()
            .map(i64::from)
    }

    /// Map an AT-SPI [`Role`] to the stable lowercase string the tokenizer
    /// expects (matches libatspi's role names, e.g. "push button").
    fn role_string(role: Role) -> String {
        role.name().to_string()
    }

    /// Derive a [`ToggleState`] for checkable roles from the state set.
    fn toggle_state(set: StateSet, role: Role) -> Option<ToggleState> {
        let checkable = set.contains(State::Checkable)
            || matches!(
                role,
                Role::CheckBox
                    | Role::CheckMenuItem
                    | Role::RadioButton
                    | Role::RadioMenuItem
                    | Role::ToggleButton
            );
        if !checkable {
            return None;
        }
        Some(if set.contains(State::Indeterminate) {
            ToggleState::Mixed
        } else if set.contains(State::Checked) || set.contains(State::Pressed) {
            ToggleState::True
        } else {
            ToggleState::False
        })
    }

    /// Best-effort text: prefer the `Text` interface contents, fall back to the
    /// accessible `name`. Returns `None` when both are empty.
    fn text_of(conn: &Connection, obj: &ObjectRef, acc: &AccessibleProxy<'_>) -> Option<String> {
        if let Ok(text_proxy) = block_on(build_text_proxy(conn, obj)) {
            if let Ok(count) = block_on(text_proxy.character_count()) {
                if count > 0 {
                    if let Ok(s) = block_on(text_proxy.get_text(0, count)) {
                        let trimmed = s.trim();
                        if !trimmed.is_empty() {
                            return Some(trimmed.to_string());
                        }
                    }
                }
            }
        }
        match block_on(acc.name()) {
            Ok(name) if !name.trim().is_empty() => Some(name.trim().to_string()),
            _ => None,
        }
    }

    async fn build_text_proxy<'p>(
        conn: &Connection,
        obj: &ObjectRef,
    ) -> Result<TextProxy<'p>, zbus::Error> {
        TextProxy::builder(conn)
            .destination(obj.name.clone())?
            .path(obj.path.clone())?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
    }

    async fn build_component_proxy<'p>(
        conn: &Connection,
        obj: &ObjectRef,
    ) -> Result<ComponentProxy<'p>, zbus::Error> {
        ComponentProxy::builder(conn)
            .destination(obj.name.clone())?
            .path(obj.path.clone())?
            .cache_properties(zbus::proxy::CacheProperties::No)
            .build()
            .await
    }

    /// Screen-relative bounds via the `Component` interface. Missing component
    /// or a defunct object yields a zero rect.
    fn bounds_of(conn: &Connection, obj: &ObjectRef) -> Bounds {
        if let Ok(component) = block_on(build_component_proxy(conn, obj)) {
            if let Ok((x, y, w, h)) = block_on(component.get_extents(CoordType::Screen)) {
                return Bounds {
                    x: f64::from(x),
                    y: f64::from(y),
                    width: f64::from(w),
                    height: f64::from(h),
                };
            }
        }
        Bounds {
            x: 0.0,
            y: 0.0,
            width: 0.0,
            height: 0.0,
        }
    }

    /// A stable-ish identifier: prefer the toolkit `accessible_id`, else use the
    /// D-Bus object path which is unique within the application.
    fn identifier_of(obj: &ObjectRef, acc: &AccessibleProxy<'_>) -> Option<String> {
        if let Ok(id) = block_on(acc.accessible_id()) {
            if !id.trim().is_empty() {
                return Some(id);
            }
        }
        let path = obj.path.as_str();
        if path.is_empty() {
            None
        } else {
            Some(path.to_string())
        }
    }

    /// Convert one accessible into an [`AxElement`]. Returns `None` for defunct
    /// objects or when the proxy cannot be built.
    fn element_of(conn: &Connection, obj: &ObjectRef) -> Option<(AxElement, StateSet)> {
        let acc = accessible_for(conn, obj).ok()?;
        let state = block_on(acc.get_state()).unwrap_or_default();
        if state.contains(State::Defunct) {
            return None;
        }
        let role = block_on(acc.get_role()).unwrap_or(Role::Invalid);
        let element = AxElement {
            role: role_string(role),
            text: text_of(conn, obj, &acc),
            bounds: bounds_of(conn, obj),
            ax_identifier: identifier_of(obj, &acc),
            checked: toggle_state(state, role),
        };
        Some((element, state))
    }

    /// Recursively collect a subtree into `walk.out`, honouring depth/node/time
    /// caps. Defunct or unreadable nodes are skipped silently.
    fn collect_subtree(walk: &mut Walk<'_>, obj: &ObjectRef, depth: u32) {
        if depth > MAX_DEPTH || walk.exhausted() {
            return;
        }
        walk.nodes += 1;

        let Some((element, _state)) = element_of(walk.conn, obj) else {
            return;
        };
        walk.out.push(element);

        let Ok(acc) = accessible_for(walk.conn, obj) else {
            return;
        };
        let children = block_on(acc.get_children()).unwrap_or_default();
        for child in children {
            if walk.exhausted() {
                break;
            }
            collect_subtree(walk, &child, depth + 1);
        }
    }

    /// Top-level windows/frames of a single application.
    fn windows_of_app(conn: &Connection, app: &ObjectRef) -> Vec<ObjectRef> {
        match accessible_for(conn, app) {
            Ok(acc) => block_on(acc.get_children()).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Does this window/frame carry the `Active` state (i.e. is it frontmost)?
    fn is_active_window(conn: &Connection, win: &ObjectRef) -> bool {
        match accessible_for(conn, win) {
            Ok(acc) => block_on(acc.get_state())
                .map(|s| s.contains(State::Active))
                .unwrap_or(false),
            Err(_) => false,
        }
    }

    /// Find the active (frontmost) window across all applications, returning the
    /// owning application and the window object refs.
    fn active_window(conn: &Connection) -> Option<(ObjectRef, ObjectRef)> {
        for app in applications(conn) {
            for win in windows_of_app(conn, &app) {
                if is_active_window(conn, &win) {
                    return Some((app, win));
                }
            }
        }
        None
    }

    /// Pick the most appropriate window from a candidate list: title match
    /// first, then the active window, then the first available.
    fn select_window(
        conn: &Connection,
        windows: &[ObjectRef],
        target_title: Option<&str>,
    ) -> Option<ObjectRef> {
        if windows.is_empty() {
            return None;
        }
        if let Some(title) = target_title {
            let title = title.trim();
            if !title.is_empty() {
                for win in windows {
                    if let Ok(acc) = accessible_for(conn, win) {
                        if let Ok(name) = block_on(acc.name()) {
                            if name.trim() == title {
                                return Some(win.clone());
                            }
                        }
                    }
                }
            }
        }
        for win in windows {
            if is_active_window(conn, win) {
                return Some(win.clone());
            }
        }
        windows.first().cloned()
    }

    /// Depth-first search for the descendant carrying the `Focused` state.
    fn find_focused(conn: &Connection, obj: &ObjectRef, depth: u32) -> Option<AxElement> {
        if depth > MAX_DEPTH {
            return None;
        }
        let (element, state) = element_of(conn, obj)?;
        if state.contains(State::Focused) {
            return Some(element);
        }
        let acc = accessible_for(conn, obj).ok()?;
        let children = block_on(acc.get_children()).unwrap_or_default();
        for child in children {
            if let Some(found) = find_focused(conn, &child, depth + 1) {
                return Some(found);
            }
        }
        None
    }

    // -- public-facing operations ------------------------------------------

    pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
        let conn = connect()?;
        let bus = conn.connection();
        let Some((_app, win)) = active_window(bus) else {
            return Ok(Vec::new());
        };
        let mut walk = Walk::new(bus);
        collect_subtree(&mut walk, &win, 0);
        Ok(walk.out)
    }

    pub fn collect_window_elements(
        pid: i32,
        _native_window_id: u32,
        _target_window_bounds: Option<&Bounds>,
        target_window_title: Option<&str>,
    ) -> Result<Vec<AxElement>, AppError> {
        let conn = connect()?;
        let bus = conn.connection();

        // Resolve the application accessible by PID. Fall back to the active
        // window when the PID cannot be matched (e.g. the toolkit does not
        // expose a usable bus name).
        let mut target_app: Option<ObjectRef> = None;
        for app in applications(bus) {
            if pid_for(bus, &app) == Some(i64::from(pid)) {
                target_app = Some(app);
                break;
            }
        }

        let windows: Vec<ObjectRef> = match &target_app {
            Some(app) => windows_of_app(bus, app),
            None => match active_window(bus) {
                Some((_, win)) => vec![win],
                None => return Ok(Vec::new()),
            },
        };

        let Some(win) = select_window(bus, &windows, target_window_title) else {
            return Ok(Vec::new());
        };

        let mut walk = Walk::new(bus);
        collect_subtree(&mut walk, &win, 0);
        Ok(walk.out)
    }

    pub fn focused_frontmost_element() -> Result<Option<AxElement>, AppError> {
        let conn = connect()?;
        let bus = conn.connection();
        let Some((_app, win)) = active_window(bus) else {
            return Ok(None);
        };
        Ok(find_focused(bus, &win, 0))
    }

    pub fn focused_frontmost_window_bounds() -> Result<Option<Bounds>, AppError> {
        let conn = connect()?;
        let bus = conn.connection();
        let Some((_app, win)) = active_window(bus) else {
            return Ok(None);
        };
        Ok(Some(bounds_of(bus, &win)))
    }

    pub fn frontmost_app_pid() -> Option<i64> {
        let conn = connect().ok()?;
        let bus = conn.connection();
        let (app, _win) = active_window(bus)?;
        pid_for(bus, &app)
    }
}

// ---------------------------------------------------------------------------
// Public API (re-exported by ax/mod.rs).
// ---------------------------------------------------------------------------

pub fn collect_frontmost_window_elements() -> Result<Vec<AxElement>, AppError> {
    imp::collect_frontmost_window_elements()
}

pub fn collect_window_elements(
    pid: i32,
    native_window_id: u32,
    target_window_bounds: Option<&Bounds>,
    target_window_title: Option<&str>,
) -> Result<Vec<AxElement>, AppError> {
    imp::collect_window_elements(
        pid,
        native_window_id,
        target_window_bounds,
        target_window_title,
    )
}

pub fn focused_frontmost_element() -> Result<Option<AxElement>, AppError> {
    imp::focused_frontmost_element()
}

pub fn focused_frontmost_window_bounds() -> Result<Option<Bounds>, AppError> {
    imp::focused_frontmost_window_bounds()
}

pub fn frontmost_app_pid() -> Option<i64> {
    imp::frontmost_app_pid()
}

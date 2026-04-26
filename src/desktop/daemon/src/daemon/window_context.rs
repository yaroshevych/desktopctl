use super::*;

fn is_desktopctl_window_app(app: &str) -> bool {
    let app_lc = app.trim().to_ascii_lowercase();
    app_lc.contains("desktopctl")
}

fn is_restricted_window(window: &platform::windowing::WindowInfo) -> bool {
    is_desktopctl_window_app(&window.app)
}

fn is_targetable_window(window: &platform::windowing::WindowInfo) -> bool {
    window.visible
        && window.bounds.width > 8.0
        && window.bounds.height > 8.0
        && !is_restricted_window(window)
}

fn restricted_window_error() -> AppError {
    AppError::target_not_found("DesktopCtl windows cannot be targeted; focus another app window")
}

pub(super) fn enrich_window_refs(windows: &mut [platform::windowing::WindowInfo]) {
    for window in windows.iter_mut() {
        if window.window_ref.is_none() {
            window.window_ref = Some(window_refs::issue_for_window(window));
        }
    }
    let id_to_ref: std::collections::HashMap<String, String> = windows
        .iter()
        .filter_map(|window| window.window_ref.clone().map(|r| (window.id.clone(), r)))
        .collect();
    for window in windows.iter_mut() {
        if let Some(parent_internal) = window.parent_id.clone() {
            if let Some(parent_ref) = id_to_ref.get(&parent_internal) {
                window.parent_id = Some(parent_ref.clone());
            }
        }
    }
}

pub(super) fn resolve_active_window_target() -> Result<platform::windowing::WindowInfo, AppError> {
    // Fast-path: if frontmost app windows provide a viable candidate, return
    // immediately and avoid slower fallback heuristics.
    if let Ok(mut frontmost_windows) = window_target::list_frontmost_app_windows() {
        enrich_window_refs(&mut frontmost_windows);
        if frontmost_windows.iter().any(|window| {
            window.frontmost
                && window.visible
                && window.bounds.width > 8.0
                && window.bounds.height > 8.0
                && is_restricted_window(window)
        }) {
            trace::log("active_window_target:blocked_desktopctl_window");
            return Err(restricted_window_error());
        }
        let focused_window_bounds = platform::ax::focused_frontmost_window_bounds()
            .ok()
            .flatten();
        if let Some(selected) = frontmost_windows
            .iter()
            .filter(|window| {
                window.frontmost
                    && window.visible
                    && window.bounds.width > 8.0
                    && window.bounds.height > 8.0
                    && !is_restricted_window(window)
            })
            .max_by(|a, b| {
                let sa = active_window_candidate_score(a, focused_window_bounds.as_ref());
                let sb = active_window_candidate_score(b, focused_window_bounds.as_ref());
                sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
        {
            trace::log("active_window_target:short_circuit_frontmost_app");
            return Ok(selected);
        }
        if let Some(selected) = frontmost_windows
            .iter()
            .filter(|window| {
                window.visible
                    && window.bounds.width > 8.0
                    && window.bounds.height > 8.0
                    && !is_restricted_window(window)
            })
            .max_by(|a, b| {
                let sa = active_window_candidate_score(a, focused_window_bounds.as_ref());
                let sb = active_window_candidate_score(b, focused_window_bounds.as_ref());
                sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
            })
            .cloned()
        {
            trace::log("active_window_target:short_circuit_visible_frontmost_app");
            return Ok(selected);
        }
    }

    // Use normalized frontmost snapshot only when the quick path above did
    // not yield a candidate.
    let snapshot = window_target::resolve_frontmost_snapshot();
    let app_hint = snapshot.app.as_deref();
    let target_bounds = snapshot.bounds.as_ref();

    let mut windows = window_target::list_windows_basic()?;
    enrich_window_refs(&mut windows);

    let eligible: Vec<&platform::windowing::WindowInfo> = windows
        .iter()
        .filter(|window| {
            window.visible
                && window.bounds.width > 8.0
                && window.bounds.height > 8.0
                && !is_restricted_window(window)
        })
        .collect();
    let best_scored =
        |items: Vec<&platform::windowing::WindowInfo>| -> Option<platform::windowing::WindowInfo> {
            items
                .into_iter()
                .max_by(|a, b| {
                    let sa = active_window_candidate_score(a, target_bounds);
                    let sb = active_window_candidate_score(b, target_bounds);
                    sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
                })
                .cloned()
        };
    let selected = best_scored(
        eligible
            .iter()
            .copied()
            .filter(|window| {
                app_hint
                    .map(|app| window.app.eq_ignore_ascii_case(app))
                    .unwrap_or(true)
            })
            .collect(),
    )
    .or_else(|| {
        if let Some(target) = target_bounds {
            let overlap_selected = eligible
                .iter()
                .copied()
                .filter(|window| iou(&window.bounds, target) > 0.01)
                .max_by(|a, b| {
                    let sa = iou(&a.bounds, target);
                    let sb = iou(&b.bounds, target);
                    sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
                })
                .cloned();
            if overlap_selected.is_some() {
                trace::log("active_window_target:app_hint_fallback_to_overlap");
            }
            overlap_selected
        } else {
            None
        }
    })
    .or_else(|| {
        if app_hint.is_some() {
            trace::log("active_window_target:app_hint_fallback_to_any_visible_last_resort");
        }
        best_scored(eligible)
    });

    selected.ok_or_else(|| {
        AppError::target_not_found(
            "frontmost window not found; ensure a standard app window is focused",
        )
    })
}

fn active_window_candidate_score(
    window: &platform::windowing::WindowInfo,
    target_bounds: Option<&desktop_core::protocol::Bounds>,
) -> f64 {
    let area = (window.bounds.width.max(0.0) * window.bounds.height.max(0.0)).max(0.0);
    let overlap_bonus = target_bounds
        .map(|target| {
            // Prioritize geometric overlap with raw frontmost bounds. Avoid
            // rewarding large container windows, which hides modal dialogs.
            iou(&window.bounds, target) * 10.0
        })
        .unwrap_or(0.0);
    let frontmost_bonus = if window.frontmost { 0.5 } else { 0.0 };
    overlap_bonus + frontmost_bonus + area.sqrt() * 0.01
}

pub(super) fn assert_active_window_id_matches(
    reference: &str,
) -> Result<platform::windowing::WindowInfo, AppError> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_argument(
            "active window id must not be empty",
        ));
    }

    resolve_explicit_window_target(trimmed)
}

pub(super) fn resolve_explicit_window_target(
    reference: &str,
) -> Result<platform::windowing::WindowInfo, AppError> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_argument(
            "active window id must not be empty",
        ));
    }

    if let Ok(mut frontmost_windows) = window_target::list_frontmost_app_windows() {
        enrich_window_refs(&mut frontmost_windows);
        match select_explicit_window_target_from_windows(trimmed, &frontmost_windows) {
            Ok(window) => return Ok(window),
            Err(err) if matches!(err.code, desktop_core::error::ErrorCode::TargetNotFound) => {
                trace::log("active_window_id_match:frontmost_fastpath_miss");
            }
            Err(err) => return Err(err),
        }
    }

    let mut windows = window_target::list_windows()?;
    enrich_window_refs(&mut windows);
    select_explicit_window_target_from_windows(trimmed, &windows)
}

pub(super) fn native_window_id_for_capture(
    window: &platform::windowing::WindowInfo,
) -> Option<u32> {
    window
        .id
        .rsplit(':')
        .next()
        .and_then(|value| value.parse::<u32>().ok())
}

pub(super) fn explicit_background_capture_window_id(
    window: &platform::windowing::WindowInfo,
) -> Result<u32, AppError> {
    native_window_id_for_capture(window).ok_or_else(|| {
        AppError::backend_unavailable(
            "background window capture requires a native macOS window id; switch to frontmost mode",
        )
    })
}

pub(super) fn background_input_target_for_window(
    window: &platform::windowing::WindowInfo,
) -> Result<desktop_core::automation::BackgroundInputTarget, AppError> {
    Ok(desktop_core::automation::BackgroundInputTarget {
        pid: i32::try_from(window.pid).map_err(|_| {
            AppError::backend_unavailable(
                "background input target pid is out of range; switch to frontmost mode",
            )
        })?,
        window_id: explicit_background_capture_window_id(window)?,
        bounds: window.bounds.clone(),
    })
}

fn select_explicit_window_target_from_windows(
    reference: &str,
    windows: &[platform::windowing::WindowInfo],
) -> Result<platform::windowing::WindowInfo, AppError> {
    let trimmed = reference.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_argument(
            "active window id must not be empty",
        ));
    }

    if let Some((expected_pid, expected_window_id)) = window_refs::resolve_native_for_ref(trimmed) {
        for window in windows {
            if window.pid == expected_pid && window.id == expected_window_id {
                if is_restricted_window(window) {
                    return Err(restricted_window_error());
                }
                if is_targetable_window(window) {
                    trace::log("active_window_id_match:resolved_ref_all_windows");
                    return Ok(window.clone());
                }
            }
        }
        return Err(AppError::target_not_found(format!(
            "window id \"{trimmed}\" was not found or is not visible"
        )));
    }

    let exact_matches: Vec<&platform::windowing::WindowInfo> = windows
        .iter()
        .filter(|window| {
            window.id == trimmed
                || window.window_ref.as_deref() == Some(trimmed)
                || format!("{}:{}", window.pid, window.id) == trimmed
        })
        .collect();
    if exact_matches
        .iter()
        .any(|window| is_restricted_window(window))
    {
        return Err(restricted_window_error());
    }
    let exact_targetable: Vec<&platform::windowing::WindowInfo> = exact_matches
        .into_iter()
        .filter(|window| is_targetable_window(window))
        .collect();
    if exact_targetable.len() == 1 {
        trace::log("active_window_id_match:resolved_exact_all_windows");
        return Ok(exact_targetable[0].clone());
    }
    if exact_targetable.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple windows matched id \"{trimmed}\""
        ))
        .with_details(json!({
            "query": trimmed,
            "candidates": exact_targetable.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
        })));
    }

    let targetable: Vec<platform::windowing::WindowInfo> = windows
        .iter()
        .filter(|window| is_targetable_window(window))
        .cloned()
        .collect();
    window_target::select_window_candidate(&targetable, trimmed).cloned()
}

pub(super) fn resolve_active_window_for_guard(
    active_window: bool,
    active_window_id: Option<&str>,
) -> Result<Option<platform::windowing::WindowInfo>, AppError> {
    if active_window_id.is_some() && !active_window {
        return Err(AppError::invalid_argument(
            "active window id requires --active-window",
        ));
    }
    if !active_window {
        return Ok(None);
    }
    let mut target = if let Some(reference) = active_window_id {
        assert_active_window_id_matches(reference)?
    } else {
        resolve_active_window_target()?
    };
    if target.window_ref.is_none() {
        target.window_ref = Some(window_refs::issue_for_window(&target));
    }
    Ok(Some(target))
}

pub(super) fn attach_window_ref_to_payload(payload: &mut desktop_core::protocol::TokenizePayload) {
    let Some(first) = payload.windows.first_mut() else {
        return;
    };
    if first.window_ref.is_some() {
        return;
    }
    let Some(target_bounds) = first
        .os_bounds
        .as_ref()
        .cloned()
        .or_else(|| Some(first.bounds.clone()))
    else {
        return;
    };

    let mut windows = match window_target::list_frontmost_app_windows() {
        Ok(items) => items,
        Err(_) => return,
    };
    enrich_window_refs(&mut windows);
    let app_hint = first.app.as_deref();
    let title_hint = first.title.as_str();
    let selected = windows
        .iter()
        .filter(|window| window.bounds.width > 8.0 && window.bounds.height > 8.0 && window.visible)
        .max_by(|a, b| {
            let sa = window_match_score(a, &target_bounds, app_hint, title_hint);
            let sb = window_match_score(b, &target_bounds, app_hint, title_hint);
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        });
    if let Some(window) = selected {
        first.window_ref = window.window_ref.clone();
    }
}

fn window_match_score(
    window: &platform::windowing::WindowInfo,
    target_bounds: &desktop_core::protocol::Bounds,
    app_hint: Option<&str>,
    title_hint: &str,
) -> f64 {
    let overlap = iou(&window.bounds, target_bounds);
    let area_delta = ((window.bounds.width - target_bounds.width).abs()
        + (window.bounds.height - target_bounds.height).abs())
    .min(2000.0);
    let mut score = overlap * 10.0 - area_delta * 0.002;
    if let Some(app) = app_hint {
        if window.app.eq_ignore_ascii_case(app) {
            score += 1.0;
        }
    }
    if !title_hint.trim().is_empty() && window.title.eq_ignore_ascii_case(title_hint) {
        score += 0.4;
    }
    if window.frontmost {
        score += 0.25;
    }
    score
}

pub(super) fn backfill_tokenize_window_positions(
    payload: &mut desktop_core::protocol::TokenizePayload,
) {
    if payload.windows.is_empty()
        || payload
            .windows
            .iter()
            .all(|window| window.os_bounds.is_some())
    {
        return;
    }
    let Some(bounds) = window_target::frontmost_window_bounds() else {
        return;
    };
    let mut filled = 0usize;
    for window in &mut payload.windows {
        if window.os_bounds.is_none() {
            window.os_bounds = Some(bounds.clone());
            filled += 1;
        }
    }
    if filled > 0 {
        trace::log(format!(
            "screen_tokenize:backfill_os_bounds filled={} bounds=({:.1},{:.1},{:.1},{:.1})",
            filled, bounds.x, bounds.y, bounds.width, bounds.height
        ));
    }
}

pub(super) fn remap_tokenize_window_id_field(value: &mut Value) {
    let Some(windows) = value.get_mut("windows").and_then(Value::as_array_mut) else {
        return;
    };
    for window in windows {
        let Some(object) = window.as_object_mut() else {
            continue;
        };
        match object.remove("window_ref") {
            Some(Value::String(ref_id)) if !ref_id.trim().is_empty() => {
                object.insert("id".to_string(), Value::String(ref_id));
            }
            _ => {
                object.remove("id");
            }
        }
    }
}

pub(super) fn collect_tokenize_new_window_hint_snapshot(
    active_window_id: &str,
) -> Option<TokenizeHintSnapshot> {
    let active_window_id = active_window_id.trim();
    if active_window_id.is_empty() {
        return None;
    }
    let mut app_windows = window_target::list_frontmost_app_windows().ok()?;
    enrich_window_refs(&mut app_windows);
    collect_tokenize_new_window_hint_snapshot_from_windows(active_window_id, app_windows)
}

pub(super) fn collect_tokenize_new_window_hint_snapshot_from_windows(
    active_window_id: &str,
    app_windows: Vec<platform::windowing::WindowInfo>,
) -> Option<TokenizeHintSnapshot> {
    let active_window_id = active_window_id.trim();
    if active_window_id.is_empty() {
        return None;
    }
    let mut app_windows = app_windows;
    enrich_window_refs(&mut app_windows);
    let target_app = app_windows.iter().find_map(|window| {
        let ref_match = window.window_ref.as_deref() == Some(active_window_id);
        let native_match = window.id == active_window_id;
        let issued_ref_match = window_refs::resolve_native_for_ref(active_window_id)
            .is_some_and(|(pid, id)| window.pid == pid && window.id == id);
        if ref_match || native_match || issued_ref_match {
            Some(window.app.clone())
        } else {
            None
        }
    });
    let target_app = target_app?;
    let scoped_windows = app_windows
        .into_iter()
        .filter(|window| window.app.eq_ignore_ascii_case(&target_app))
        .collect::<Vec<_>>();
    let context = scoped_windows.iter().find_map(|window| {
        let app = window.app.trim();
        if app.is_empty() {
            None
        } else {
            Some(format!("app {app}"))
        }
    });
    let current_windows = scoped_windows
        .into_iter()
        .filter(|window| window.visible && window.bounds.width > 8.0 && window.bounds.height > 8.0)
        .filter_map(|window| {
            window.window_ref.map(|window_ref| {
                (
                    window_ref,
                    if window.title.trim().is_empty() {
                        "untitled".to_string()
                    } else {
                        window.title
                    },
                    window.modal.unwrap_or(false),
                    window.parent_id,
                )
            })
        })
        .collect::<Vec<TokenizeHintWindow>>();
    if current_windows.is_empty() {
        return None;
    }
    Some(TokenizeHintSnapshot {
        context,
        state_key: format!("active_window:{active_window_id}"),
        current_windows,
    })
}

pub(super) fn append_tokenize_new_window_hint(
    value: &mut Value,
    active_window_id: Option<&str>,
    precomputed: Option<TokenizeHintSnapshot>,
) {
    // This hint is intended for flows pinned to an explicit active window id.
    // Avoid extra window enumeration cost for generic tokenize calls.
    let Some(active_window_id) = active_window_id.map(str::trim).filter(|v| !v.is_empty()) else {
        return;
    };
    let Some(windows) = value.get("windows").and_then(Value::as_array) else {
        return;
    };
    if windows.is_empty() {
        return;
    }

    let mut payload_windows: Vec<TokenizeHintWindow> = Vec::new();
    let payload_app = windows.iter().find_map(|window| {
        window
            .as_object()
            .and_then(|obj| obj.get("app"))
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
    });
    for window in windows {
        let Some(obj) = window.as_object() else {
            continue;
        };
        let Some(id) = obj
            .get("id")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(|v| v.to_string())
        else {
            continue;
        };
        let title = obj
            .get("title")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or("untitled")
            .to_string();
        payload_windows.push((id, title, false, None));
    }

    let precomputed =
        precomputed.or_else(|| collect_tokenize_new_window_hint_snapshot(active_window_id));
    let mut hint_context: Option<String> =
        precomputed.as_ref().and_then(|snap| snap.context.clone());
    let mut state_key: Option<String> = precomputed.as_ref().map(|snap| snap.state_key.clone());
    let mut current_windows: Vec<TokenizeHintWindow> = precomputed
        .map(|snap| snap.current_windows)
        .unwrap_or_default();

    if current_windows.is_empty() {
        current_windows = payload_windows;
        if state_key.is_none() {
            if let Some(app) = payload_app {
                state_key = Some(format!("app:{}", app.to_lowercase()));
                hint_context = Some(format!("app {app}"));
            } else {
                state_key = Some("payload_windows".to_string());
            }
        }
        if hint_context.is_none() {
            hint_context = Some("current view".to_string());
        }
    }
    let current_ids: HashSet<String> = current_windows
        .iter()
        .map(|(id, _, _, _)| id.clone())
        .collect();
    if current_ids.is_empty() {
        return;
    }

    let Some(state_key) = state_key else {
        return;
    };
    let lock = TOKENIZE_WINDOW_HINT_STATE.get_or_init(|| Mutex::new(HashMap::new()));
    let mut state = match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let previous_ids = state.get(&state_key).cloned().unwrap_or_default();
    state.insert(state_key, current_ids.clone());
    drop(state);

    if previous_ids.is_empty() {
        return;
    }

    let mut new_windows: Vec<TokenizeHintWindow> = current_windows
        .into_iter()
        .filter(|(id, _, _, _)| !previous_ids.contains(id))
        .collect();
    if new_windows.is_empty() {
        return;
    }
    new_windows.sort_by(|a, b| a.1.to_lowercase().cmp(&b.1.to_lowercase()));

    let summary = new_windows
        .iter()
        .take(3)
        .map(|(id, title, is_modal, parent_id)| {
            if *is_modal {
                format!("{title} ({id}, modal)")
            } else if let Some(parent) = parent_id.as_deref().filter(|v| !v.trim().is_empty()) {
                format!("{title} ({id}, parent={parent})")
            } else {
                format!("{title} ({id})")
            }
        })
        .collect::<Vec<String>>()
        .join(", ");
    let suffix = if new_windows.len() > 3 {
        format!(" (+{} more)", new_windows.len() - 3)
    } else {
        String::new()
    };
    let context = hint_context.unwrap_or_else(|| "app".to_string());
    let hint = format!(
        "new window detected for {context}: {summary}{suffix}. If this is a modal dialog, target the new window id instead of the previous --active-window id."
    );

    if let Some(obj) = value.as_object_mut() {
        obj.insert("hint".to_string(), Value::String(hint));
    }
}

#[cfg(test)]
mod tests {
    use super::{
        background_input_target_for_window, is_desktopctl_window_app, native_window_id_for_capture,
        select_explicit_window_target_from_windows,
    };
    use crate::platform;
    use desktop_core::{error::ErrorCode, protocol::Bounds};

    fn test_window(
        id: &str,
        pid: i64,
        app: &str,
        title: &str,
        window_ref: Option<&str>,
        frontmost: bool,
    ) -> platform::windowing::WindowInfo {
        platform::windowing::WindowInfo {
            id: id.to_string(),
            window_ref: window_ref.map(str::to_string),
            parent_id: None,
            pid,
            index: 1,
            app: app.to_string(),
            title: title.to_string(),
            bounds: Bounds {
                x: 10.0,
                y: 20.0,
                width: 300.0,
                height: 200.0,
            },
            frontmost,
            visible: true,
            modal: None,
        }
    }

    #[test]
    fn desktopctl_app_windows_are_blocked() {
        assert!(is_desktopctl_window_app("DesktopCtl"));
        assert!(is_desktopctl_window_app("desktopctl helper"));
    }

    #[test]
    fn non_desktopctl_app_windows_are_allowed() {
        assert!(!is_desktopctl_window_app("Safari"));
        assert!(!is_desktopctl_window_app("Notes"));
    }

    #[test]
    fn explicit_window_target_resolves_window_ref_when_terminal_is_frontmost() {
        let windows = vec![
            test_window(
                "term-native",
                10,
                "Ghostty",
                "shell",
                Some("ghostty_111111"),
                true,
            ),
            test_window(
                "notes-native",
                20,
                "Notes",
                "Project",
                Some("notes_222222"),
                false,
            ),
        ];

        let selected = select_explicit_window_target_from_windows("notes_222222", &windows)
            .expect("background target should resolve");

        assert_eq!(selected.app, "Notes");
        assert_eq!(selected.id, "notes-native");
    }

    #[test]
    fn explicit_window_target_resolves_native_id() {
        let windows = vec![
            test_window("term-native", 10, "Ghostty", "shell", None, true),
            test_window("notes-native", 20, "Notes", "Project", None, false),
        ];

        let selected = select_explicit_window_target_from_windows("notes-native", &windows)
            .expect("native id should resolve");

        assert_eq!(selected.app, "Notes");
    }

    #[test]
    fn native_window_id_for_capture_parses_cg_window_number() {
        let window = test_window("123:456", 123, "Notes", "Project", None, false);
        assert_eq!(native_window_id_for_capture(&window), Some(456));
    }

    #[test]
    fn background_input_target_uses_pid_window_id_and_bounds() {
        let window = test_window("123:456", 123, "Notes", "Project", None, false);

        let target = background_input_target_for_window(&window).expect("target");

        assert_eq!(target.pid, 123);
        assert_eq!(target.window_id, 456);
        assert_eq!(target.bounds.x, 10.0);
        assert_eq!(target.bounds.y, 20.0);
    }

    #[test]
    fn explicit_window_target_reports_ambiguous_app_query() {
        let windows = vec![
            test_window("notes-1", 20, "Notes", "Project A", None, false),
            test_window("notes-2", 20, "Notes", "Project B", None, false),
        ];

        let err = select_explicit_window_target_from_windows("Notes", &windows)
            .expect_err("ambiguous app query should fail");

        assert_eq!(err.code, ErrorCode::AmbiguousTarget);
    }

    #[test]
    fn explicit_window_target_rejects_desktopctl_window() {
        let windows = vec![test_window(
            "desktopctl-native",
            30,
            "DesktopCtl",
            "DesktopCtl",
            Some("desktopctl_333333"),
            true,
        )];

        let err = select_explicit_window_target_from_windows("desktopctl_333333", &windows)
            .expect_err("DesktopCtl windows should be blocked");

        assert_eq!(err.code, ErrorCode::TargetNotFound);
        assert!(
            err.message
                .contains("DesktopCtl windows cannot be targeted")
        );
    }
}

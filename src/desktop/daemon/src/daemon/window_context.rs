use super::*;

fn is_desktopctl_window_app(app: &str) -> bool {
    let app_lc = app.trim().to_ascii_lowercase();
    app_lc.contains("desktopctl")
}

fn is_restricted_window(window: &platform::windowing::WindowInfo) -> bool {
    is_desktopctl_window_app(&window.app)
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
    if let Some((expected_pid, expected_window_id)) = window_refs::resolve_native_for_ref(trimmed) {
        if let Ok(mut current_app_windows) = window_target::list_frontmost_app_windows() {
            enrich_window_refs(&mut current_app_windows);
            if let Some(active) = current_app_windows.into_iter().find(|window| {
                window.visible
                    && window.bounds.width > 8.0
                    && window.bounds.height > 8.0
                    && window.pid == expected_pid
                    && window.id == expected_window_id
            }) {
                if is_restricted_window(&active) {
                    return Err(restricted_window_error());
                }
                trace::log("active_window_id_match:fastpath_hit");
                return Ok(active);
            }
            trace::log("active_window_id_match:fastpath_miss");
        }
    }

    let active = resolve_active_window_target()?;
    let active_ref = active
        .window_ref
        .clone()
        .ok_or_else(|| AppError::target_not_found("active window id is unavailable"))?;
    if active_ref != trimmed {
        return Err(AppError::target_not_found(format!(
            "active window does not match requested id \"{trimmed}\""
        )));
    }
    Ok(active)
}

pub(super) fn bind_active_window_reference(
    active_window: bool,
    active_window_id: Option<&str>,
) -> Result<Option<String>, AppError> {
    if active_window_id.is_some() && !active_window {
        return Err(AppError::invalid_argument(
            "active window id requires --active-window",
        ));
    }
    if !active_window {
        return Ok(None);
    }
    let target = if let Some(reference) = active_window_id {
        assert_active_window_id_matches(reference)?
    } else {
        resolve_active_window_target()?
    };
    let bound = target
        .window_ref
        .ok_or_else(|| AppError::target_not_found("active window id is unavailable"))?;
    Ok(Some(bound))
}

pub(super) fn resolve_observe_scope_bounds(
    active_window: bool,
    active_window_id: Option<&str>,
) -> Result<Option<desktop_core::protocol::Bounds>, AppError> {
    if active_window_id.is_some() && !active_window {
        return Err(AppError::invalid_argument(
            "active window id requires --active-window",
        ));
    }
    if !active_window {
        return Ok(None);
    }
    let target = if let Some(reference) = active_window_id {
        assert_active_window_id_matches(reference)?
    } else {
        resolve_active_window_target()?
    };
    Ok(Some(target.bounds))
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
    let context = app_windows.iter().find_map(|window| {
        let app = window.app.trim();
        if app.is_empty() {
            None
        } else {
            Some(format!("app {app}"))
        }
    });
    let current_windows = app_windows
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
    use super::is_desktopctl_window_app;

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
}

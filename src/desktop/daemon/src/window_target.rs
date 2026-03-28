use desktop_core::{error::AppError, protocol::Bounds};
use serde_json::{Value, json};

use crate::{
    platform,
    platform::windowing::{FrontmostWindowContext, WindowInfo},
    trace,
};

pub(crate) fn main_display_bounds() -> Option<Bounds> {
    platform::windowing::main_display_bounds()
}

fn frontmost_window_context() -> Option<FrontmostWindowContext> {
    platform::windowing::frontmost_window_context()
}

pub(crate) fn frontmost_window_bounds() -> Option<Bounds> {
    let context = frontmost_window_context();
    let direct = context.as_ref().and_then(|ctx| ctx.bounds.clone());
    if let Some(direct) = direct.as_ref() {
        if !is_tiny_window_bounds(direct) {
            return Some(direct.clone());
        }
    }

    let app_hint = context.as_ref().and_then(|ctx| ctx.app.as_deref());
    let listed = list_frontmost_app_windows()
        .ok()
        .and_then(|windows| {
            preferred_window_for_capture(&windows, app_hint).map(|window| window.bounds.clone())
        })
        .or_else(|| {
            list_windows().ok().and_then(|windows| {
                preferred_window_for_capture(&windows, app_hint).map(|window| window.bounds.clone())
            })
        });

    match (direct, listed) {
        (Some(direct), Some(listed))
            if is_tiny_window_bounds(&direct) && !is_tiny_window_bounds(&listed) =>
        {
            trace::log(format!(
                "frontmost_window_bounds:replace_tiny_direct direct=({:.1},{:.1},{:.1},{:.1}) listed=({:.1},{:.1},{:.1},{:.1})",
                direct.x,
                direct.y,
                direct.width,
                direct.height,
                listed.x,
                listed.y,
                listed.width,
                listed.height
            ));
            Some(listed)
        }
        (Some(direct), Some(listed))
            if !is_tiny_window_bounds(&listed)
                && window_area(&listed) > window_area(&direct).saturating_mul(4) =>
        {
            trace::log(format!(
                "frontmost_window_bounds:replace_small_direct direct=({:.1},{:.1},{:.1},{:.1}) listed=({:.1},{:.1},{:.1},{:.1})",
                direct.x,
                direct.y,
                direct.width,
                direct.height,
                listed.x,
                listed.y,
                listed.width,
                listed.height
            ));
            Some(listed)
        }
        (Some(direct), _) => Some(direct),
        (None, Some(listed)) => Some(listed),
        (None, None) => None,
    }
}

pub(crate) fn frontmost_app_name() -> Option<String> {
    frontmost_window_context().and_then(|ctx| ctx.app)
}

fn window_area(bounds: &Bounds) -> u64 {
    let area = bounds.width.max(0.0) * bounds.height.max(0.0);
    area.round().max(0.0) as u64
}

fn is_tiny_window_bounds(bounds: &Bounds) -> bool {
    bounds.width < 120.0 || bounds.height < 90.0 || window_area(bounds) < 30_000
}

fn preferred_window_for_capture<'a>(
    windows: &'a [WindowInfo],
    app_hint: Option<&str>,
) -> Option<&'a WindowInfo> {
    let eligible = |window: &&WindowInfo| {
        window.visible && window.bounds.width > 8.0 && window.bounds.height > 8.0
    };
    let app_matches = |window: &&WindowInfo| match app_hint {
        Some(app) => window.app.eq_ignore_ascii_case(app),
        None => true,
    };

    windows
        .iter()
        .filter(|window| eligible(window) && window.frontmost && app_matches(window))
        .max_by_key(|window| window_area(&window.bounds))
        .or_else(|| {
            windows
                .iter()
                .filter(|window| eligible(window) && app_matches(window))
                .max_by_key(|window| window_area(&window.bounds))
        })
        .or_else(|| {
            windows
                .iter()
                .filter(|window| eligible(window) && window.frontmost)
                .max_by_key(|window| window_area(&window.bounds))
        })
        .or_else(|| {
            windows
                .iter()
                .filter(eligible)
                .max_by_key(|window| window_area(&window.bounds))
        })
}

pub(crate) fn list_windows() -> Result<Vec<WindowInfo>, AppError> {
    platform::windowing::list_windows()
}

fn list_frontmost_app_windows() -> Result<Vec<WindowInfo>, AppError> {
    platform::windowing::list_frontmost_app_windows()
}

pub(crate) fn resolve_tokenize_window_target(
    windows: &[WindowInfo],
    query: Option<&str>,
) -> Result<WindowInfo, AppError> {
    if windows.is_empty() {
        return Err(AppError::target_not_found(
            "no windows available for screen tokenize",
        ));
    }

    if let Some(query) = query {
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(AppError::invalid_argument(
                "window id must not be empty for screen tokenize",
            ));
        }
        if let Some(found) = windows.iter().find(|w| w.id == trimmed) {
            return Ok(found.clone());
        }
        let selected = select_window_candidate(windows, trimmed)?;
        return Ok(selected.clone());
    }

    windows
        .iter()
        .find(|w| w.frontmost && w.visible && w.bounds.width > 8.0 && w.bounds.height > 8.0)
        .or_else(|| {
            windows
                .iter()
                .find(|w| w.visible && w.bounds.width > 8.0 && w.bounds.height > 8.0)
        })
        .cloned()
        .ok_or_else(|| {
            AppError::target_not_found("no visible window available for screen tokenize")
        })
}

pub(crate) fn select_window_candidate<'a>(
    windows: &'a [WindowInfo],
    query: &str,
) -> Result<&'a WindowInfo, AppError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(AppError::invalid_argument("window title must not be empty"));
    }

    let lower = query.to_lowercase();

    let exact_title: Vec<&WindowInfo> = windows
        .iter()
        .filter(|w| w.title.eq_ignore_ascii_case(query))
        .collect();
    if exact_title.len() == 1 {
        return Ok(exact_title[0]);
    }
    if exact_title.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple windows matched title \"{query}\""
        ))
        .with_details(json!({
            "query": query,
            "candidates": exact_title.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
        })));
    }

    let exact_app: Vec<&WindowInfo> = windows
        .iter()
        .filter(|w| w.app.eq_ignore_ascii_case(query))
        .collect();
    if exact_app.len() == 1 {
        return Ok(exact_app[0]);
    }
    if exact_app.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple windows matched app \"{query}\""
        ))
        .with_details(json!({
            "query": query,
            "candidates": exact_app.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
        })));
    }

    let partial: Vec<&WindowInfo> = windows
        .iter()
        .filter(|w| {
            w.title.to_lowercase().contains(&lower) || w.app.to_lowercase().contains(&lower)
        })
        .collect();
    if partial.len() == 1 {
        return Ok(partial[0]);
    }
    if partial.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple windows partially matched \"{query}\""
        ))
        .with_details(json!({
            "query": query,
            "candidates": partial.iter().map(|w| w.as_json()).collect::<Vec<Value>>()
        })));
    }

    Err(AppError::target_not_found(format!(
        "window \"{query}\" was not found"
    )))
}

#[cfg(test)]
mod tests {
    use desktop_core::{error::ErrorCode, protocol::Bounds};

    use super::{resolve_tokenize_window_target, select_window_candidate};
    use crate::platform::windowing::WindowInfo;

    fn test_window(pid: i64, index: u32, app: &str, title: &str) -> WindowInfo {
        WindowInfo {
            id: format!("{pid}:{index}"),
            pid,
            index,
            app: app.to_string(),
            title: title.to_string(),
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 100.0,
                height: 100.0,
            },
            frontmost: false,
            visible: true,
        }
    }

    #[test]
    fn select_window_prefers_exact_title() {
        let windows = vec![
            test_window(10, 1, "TextEdit", "Document 1"),
            test_window(11, 1, "Calculator", "Calculator"),
        ];
        let selected = select_window_candidate(&windows, "Calculator").expect("selected");
        assert_eq!(selected.app, "Calculator");
        assert_eq!(selected.title, "Calculator");
    }

    #[test]
    fn select_window_reports_ambiguous_app_matches() {
        let windows = vec![
            test_window(20, 1, "Safari", "Tab A"),
            test_window(20, 2, "Safari", "Tab B"),
        ];
        let err = select_window_candidate(&windows, "Safari").expect_err("must be ambiguous");
        assert_eq!(err.code, ErrorCode::AmbiguousTarget);
    }

    #[test]
    fn tokenize_target_prefers_explicit_window_id() {
        let mut windows = vec![
            test_window(20, 1, "Safari", "Tab A"),
            test_window(22, 2, "Calculator", "Calculator"),
        ];
        windows[0].frontmost = true;
        let selected = resolve_tokenize_window_target(&windows, Some("22:2")).expect("selected");
        assert_eq!(selected.id, "22:2");
        assert_eq!(selected.app, "Calculator");
    }

    #[test]
    fn tokenize_target_defaults_to_frontmost_visible() {
        let mut windows = vec![
            test_window(20, 1, "Safari", "Tab A"),
            test_window(22, 2, "Calculator", "Calculator"),
        ];
        windows[1].frontmost = true;
        let selected = resolve_tokenize_window_target(&windows, None).expect("selected frontmost");
        assert_eq!(selected.id, "22:2");
    }
}

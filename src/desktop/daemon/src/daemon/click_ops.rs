use super::*;

pub(super) fn click_text_target(
    query: &str,
    button: PointerButton,
    active_window: bool,
    active_window_id: Option<&str>,
    request_context: &RequestContext,
) -> Result<(Value, Option<Vec<Value>>), AppError> {
    if active_window_id.is_some() && !active_window {
        return Err(AppError::invalid_argument(
            "active window id requires --active-window",
        ));
    }
    if active_window {
        let explicit_target = active_window_id
            .map(assert_active_window_id_matches)
            .transpose()?;
        if explicit_target.is_none()
            && let Some(result) = try_click_text_active_window_ax(query, button)?
        {
            return Ok((result, None));
        }
        let bounds = explicit_target
            .as_ref()
            .map(|target| target.bounds.clone())
            .or_else(|| click_scope_window_bounds(request_context))
            .ok_or_else(|| {
                AppError::target_not_found("target window bounds unavailable for click --text")
            })?;
        let app = explicit_target
            .as_ref()
            .map(|target| target.app.clone())
            .or_else(|| request_frontmost_app(request_context));
        let title = explicit_target
            .as_ref()
            .map(|target| target.title.clone())
            .or_else(|| app.clone())
            .unwrap_or_else(|| "active_window".to_string());
        let id = explicit_target
            .as_ref()
            .map(|target| target.id.clone())
            .unwrap_or_else(|| "frontmost:1".to_string());
        let (native_window_id, capture_bounds) = if let Some(target) = explicit_target.as_ref() {
            (
                Some(explicit_background_capture_window_id(target)?),
                Some(target.bounds.clone()),
            )
        } else {
            (None, None)
        };
        let window_meta = vision::pipeline::TokenizeWindowMeta {
            id,
            title,
            app,
            bounds,
            pid: explicit_target
                .as_ref()
                .and_then(|target| i32::try_from(target.pid).ok()),
            native_window_id,
            capture_bounds,
        };
        let payload = vision::pipeline::tokenize_window(window_meta)?;
        let pre_click_tokens = observe_seed_tokens_from_tokenize_payload(&payload);
        let tokenize_texts = tokenize_payload_texts_for_click(&payload);
        if tokenize_texts.is_empty() {
            return Err(AppError::target_not_found(
                "no tokenize text detected in target window; cannot click target safely",
            ));
        }
        let target = select_text_candidate(&tokenize_texts, query)?;
        trace::log("ui_click_text:active_window source=tokenize");
        trace::log(format!(
            "ui_click_text:selected text=\"{}\" confidence={:.3} bounds=({}, {}, {}, {})",
            compact_for_log(&target.text),
            target.confidence,
            target.bounds.x,
            target.bounds.y,
            target.bounds.width,
            target.bounds.height
        ));
        perform_click_for_target_window(&target.bounds, button, explicit_target.as_ref())?;
        return Ok((
            json!({
                "snapshot_id": payload.snapshot_id,
                "click_target": {
                    "text": target.text,
                    "bounds": target.bounds
                }
            }),
            Some(pre_click_tokens),
        ));
    }

    permissions::ensure_screen_recording_permission()?;
    let capture = vision::pipeline::capture_and_update(None)?;
    let normalized_texts = normalize_snapshot_texts_to_display(
        &capture.snapshot.texts,
        capture.image.width(),
        capture.image.height(),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
    );
    let window_bounds = click_scope_window_bounds(request_context);
    let window_filtered = window_bounds
        .as_ref()
        .map(|bounds| filter_texts_to_window_progressive(&normalized_texts, bounds))
        .unwrap_or_else(|| normalized_texts.clone());
    trace::log(format!(
        "ui_click_text:candidates snapshot_id={} query=\"{}\" texts={} window_filtered={} display={}x{} focused_app={} frontmost_window={}",
        capture.snapshot.snapshot_id,
        query,
        normalized_texts.len(),
        window_filtered.len(),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
        capture.snapshot.focused_app.as_deref().unwrap_or("<none>"),
        window_bounds
            .as_ref()
            .map(|b| format!("({:.1},{:.1},{:.1},{:.1})", b.x, b.y, b.width, b.height))
            .unwrap_or_else(|| "null".to_string())
    ));
    let target = match select_text_candidate(&window_filtered, query) {
        Ok(target) => target,
        Err(primary_err) => {
            trace::log(format!(
                "ui_click_text:ocr_primary_failed code={:?} msg={}",
                primary_err.code, primary_err.message
            ));
            match tokenize_click_text_candidate(query, window_bounds.as_ref(), request_context) {
                Ok(fallback) => {
                    trace::log("ui_click_text:fallback source=tokenize");
                    fallback
                }
                Err(fallback_err) => {
                    trace::log(format!(
                        "ui_click_text:fallback_failed code={:?} msg={}",
                        fallback_err.code, fallback_err.message
                    ));
                    if window_bounds.is_some() && window_filtered.is_empty() {
                        return Err(AppError::target_not_found(
                            "no OCR/tokenize text detected in frontmost window; cannot click target safely",
                        ));
                    }
                    return Err(primary_err);
                }
            }
        }
    };
    trace::log(format!(
        "ui_click_text:selected text=\"{}\" confidence={:.3} bounds=({}, {}, {}, {})",
        compact_for_log(&target.text),
        target.confidence,
        target.bounds.x,
        target.bounds.y,
        target.bounds.width,
        target.bounds.height
    ));
    perform_click(&target.bounds, button)?;

    Ok((
        json!({
            "snapshot_id": capture.snapshot.snapshot_id,
            "click_target": {
                "text": target.text,
                "bounds": target.bounds
            }
        }),
        None,
    ))
}

pub(super) fn try_click_text_active_window_ax(
    query: &str,
    button: PointerButton,
) -> Result<Option<Value>, AppError> {
    let ax_elements = match platform::ax::collect_frontmost_window_elements() {
        Ok(items) => items,
        Err(err) => {
            trace::log(format!("ui_click_text:active_window_ax_warn {err}"));
            return Ok(None);
        }
    };
    if ax_elements.is_empty() {
        return Ok(None);
    }

    let texts: Vec<desktop_core::protocol::SnapshotText> = ax_elements
        .iter()
        .filter_map(|ax| {
            let text = ax.text.as_ref()?.trim();
            if text.is_empty() {
                return None;
            }
            Some(desktop_core::protocol::SnapshotText {
                text: text.to_string(),
                bounds: ax.bounds.clone(),
                confidence: 0.92,
            })
        })
        .collect();
    if texts.is_empty() {
        return Ok(None);
    }

    match select_text_candidate(&texts, query) {
        Ok(target) => {
            trace::log("ui_click_text:active_window source=ax");
            trace::log(format!(
                "ui_click_text:selected text=\"{}\" confidence={:.3} bounds=({}, {}, {}, {})",
                compact_for_log(&target.text),
                target.confidence,
                target.bounds.x,
                target.bounds.y,
                target.bounds.width,
                target.bounds.height
            ));
            perform_click(&target.bounds, button)?;
            Ok(Some(json!({
                "snapshot_id": 0,
                "click_target": {
                    "text": target.text,
                    "bounds": target.bounds
                }
            })))
        }
        Err(err) if matches!(err.code, desktop_core::error::ErrorCode::TargetNotFound) => Ok(None),
        Err(err) if matches!(err.code, desktop_core::error::ErrorCode::AmbiguousTarget) => Ok(None),
        Err(err) => Err(err),
    }
}

#[derive(Debug, Clone)]
pub(super) struct TokenizeClickElementCandidate {
    pub(super) id: String,
    pub(super) text: Option<String>,
    pub(super) bounds: desktop_core::protocol::Bounds,
    pub(super) source: String,
}

pub(super) fn click_element_id_target(
    id: &str,
    button: PointerButton,
    active_window: bool,
    active_window_id: Option<&str>,
    request_context: &RequestContext,
) -> Result<(Value, Option<Vec<Value>>), AppError> {
    if !active_window {
        return Err(AppError::invalid_argument(
            "pointer click --id requires --active-window",
        ));
    }
    let explicit_target = active_window_id
        .map(assert_active_window_id_matches)
        .transpose()?;
    permissions::ensure_screen_recording_permission()?;
    let needle = id.trim();
    if needle.is_empty() {
        return Err(AppError::invalid_argument("empty element id selector"));
    }
    if explicit_target.is_none() && is_ax_element_id(needle) {
        if let Some(result) = try_click_ax_element_id_target(needle, button)? {
            return Ok((result, None));
        }
    }
    let bounds = explicit_target
        .as_ref()
        .map(|target| target.bounds.clone())
        .or_else(|| click_scope_window_bounds(request_context))
        .ok_or_else(|| {
            AppError::target_not_found("target window bounds unavailable for click --id")
        })?;
    let app = explicit_target
        .as_ref()
        .map(|target| target.app.clone())
        .or_else(|| request_frontmost_app(request_context));
    let title = explicit_target
        .as_ref()
        .map(|target| target.title.clone())
        .or_else(|| app.clone())
        .unwrap_or_else(|| "active_window".to_string());
    let id = explicit_target
        .as_ref()
        .map(|target| target.id.clone())
        .unwrap_or_else(|| "frontmost:1".to_string());
    let (native_window_id, capture_bounds) = if let Some(target) = explicit_target.as_ref() {
        (
            Some(explicit_background_capture_window_id(target)?),
            Some(target.bounds.clone()),
        )
    } else {
        (None, None)
    };
    let window_meta = vision::pipeline::TokenizeWindowMeta {
        id,
        title,
        app,
        bounds,
        pid: explicit_target
            .as_ref()
            .and_then(|target| i32::try_from(target.pid).ok()),
        native_window_id,
        capture_bounds,
    };
    let payload = vision::pipeline::tokenize_window(window_meta)?;
    let pre_click_tokens = observe_seed_tokens_from_tokenize_payload(&payload);
    let candidates = tokenize_payload_elements_for_click(&payload);
    let total_candidates = candidates.len();
    let matches: Vec<TokenizeClickElementCandidate> = candidates
        .into_iter()
        .filter(|element| element.id == needle)
        .collect();
    trace::log(format!(
        "pointer_click_id:candidates id=\"{}\" total={} matched={}",
        compact_for_log(needle),
        total_candidates,
        matches.len()
    ));
    if matches.is_empty() {
        return Err(AppError::target_not_found(format!(
            "element id \"{needle}\" was not found in target window"
        )));
    }
    if matches.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple elements matched id \"{needle}\""
        )));
    }
    let target = &matches[0];
    trace::log(format!(
        "pointer_click_id:selected id=\"{}\" source={} bounds=({:.1}, {:.1}, {:.1}, {:.1}) text=\"{}\"",
        compact_for_log(&target.id),
        compact_for_log(&target.source),
        target.bounds.x,
        target.bounds.y,
        target.bounds.width,
        target.bounds.height,
        compact_for_log(target.text.as_deref().unwrap_or(""))
    ));
    perform_click_for_target_window(&target.bounds, button, explicit_target.as_ref())?;
    Ok((
        json!({
            "click_target": {
                "id": target.id.clone(),
                "text": target.text.clone(),
                "bounds": target.bounds.clone()
            }
        }),
        Some(pre_click_tokens),
    ))
}

pub(super) fn is_ax_element_id(id: &str) -> bool {
    id.starts_with("axid_") || id.starts_with("axp_")
}

pub(super) fn center_point(bounds: &desktop_core::protocol::Bounds) -> Point {
    let x = (bounds.x + bounds.width * 0.5).round().max(0.0) as u32;
    let y = (bounds.y + bounds.height * 0.5).round().max(0.0) as u32;
    Point::new(x, y)
}

pub(super) fn resolve_element_id_target(
    id: &str,
    active_window: bool,
    active_window_id: Option<&str>,
    request_context: &RequestContext,
) -> Result<TokenizeClickElementCandidate, AppError> {
    if active_window_id.is_some() && !active_window {
        return Err(AppError::invalid_argument(
            "active window id requires --active-window",
        ));
    }
    let explicit_target = active_window_id
        .map(assert_active_window_id_matches)
        .transpose()?;
    let needle = id.trim();
    if needle.is_empty() {
        return Err(AppError::invalid_argument("empty element id selector"));
    }

    if explicit_target.is_none()
        && let Some(target) = resolve_ax_element_id_target(needle)?
    {
        return Ok(target);
    }

    let resolved_target = if active_window {
        Some(explicit_target.unwrap_or(resolve_active_window_target()?))
    } else {
        None
    };
    let bounds = resolved_target
        .as_ref()
        .map(|target| target.bounds.clone())
        .or_else(|| click_scope_window_bounds(request_context))
        .ok_or_else(|| {
            AppError::target_not_found("target window bounds unavailable for element id lookup")
        })?;
    let app = resolved_target
        .as_ref()
        .map(|target| target.app.clone())
        .or_else(|| request_frontmost_app(request_context));
    let title = resolved_target
        .as_ref()
        .map(|target| target.title.clone())
        .or_else(|| app.clone())
        .unwrap_or_else(|| "active_window".to_string());
    let id = resolved_target
        .as_ref()
        .map(|target| target.id.clone())
        .unwrap_or_else(|| "frontmost:1".to_string());
    let (native_window_id, capture_bounds) = if active_window_id.is_some()
        && let Some(target) = resolved_target.as_ref()
    {
        (
            Some(explicit_background_capture_window_id(target)?),
            Some(target.bounds.clone()),
        )
    } else {
        (None, None)
    };
    let window_meta = vision::pipeline::TokenizeWindowMeta {
        id,
        title,
        app,
        bounds,
        pid: resolved_target
            .as_ref()
            .and_then(|target| i32::try_from(target.pid).ok()),
        native_window_id,
        capture_bounds,
    };
    let payload = vision::pipeline::tokenize_window(window_meta)?;
    let candidates = tokenize_payload_elements_for_click(&payload);
    let total_candidates = candidates.len();
    let matches: Vec<TokenizeClickElementCandidate> = candidates
        .into_iter()
        .filter(|element| element.id == needle)
        .collect();
    trace::log(format!(
        "element_id_lookup:candidates id=\"{}\" total={} matched={}",
        compact_for_log(needle),
        total_candidates,
        matches.len()
    ));
    if matches.is_empty() {
        return Err(AppError::target_not_found(format!(
            "element id \"{needle}\" was not found in target window"
        )));
    }
    if matches.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple elements matched id \"{needle}\""
        )));
    }
    Ok(matches[0].clone())
}

pub(super) fn resolve_ax_element_id_target(
    needle: &str,
) -> Result<Option<TokenizeClickElementCandidate>, AppError> {
    let ax_elements = match platform::ax::collect_frontmost_window_elements() {
        Ok(items) => items,
        Err(err) => {
            trace::log(format!("element_id_lookup:ax_warn {err}"));
            return Ok(None);
        }
    };
    if ax_elements.is_empty() {
        return Ok(None);
    }

    let mut elements: Vec<desktop_core::protocol::TokenizeElement> = ax_elements
        .iter()
        .map(|ax| {
            vision::element_normalizer::ElementBuilder::new()
                .id(vision::ax_merge::primary_id_for_ax(ax))
                .kind("")
                .bbox(ax.bounds.clone())
                .has_border(None)
                .text(ax.text.clone())
                .confidence(None)
                .checked(ax.checked)
                .source(format!("accessibility_ax:{}", ax.role))
                .build()
        })
        .collect();
    vision::element_normalizer::finalize_elements(&mut elements);
    let candidates: Vec<TokenizeClickElementCandidate> = elements
        .into_iter()
        .map(|element| TokenizeClickElementCandidate {
            id: element.id,
            text: element.text,
            bounds: desktop_core::protocol::Bounds {
                x: element.bbox[0],
                y: element.bbox[1],
                width: element.bbox[2],
                height: element.bbox[3],
            },
            source: element.source,
        })
        .collect();
    let total_candidates = candidates.len();
    let matches: Vec<TokenizeClickElementCandidate> = candidates
        .into_iter()
        .filter(|element| element.id == needle)
        .collect();
    trace::log(format!(
        "element_id_lookup:ax_candidates id=\"{}\" total={} matched={}",
        compact_for_log(needle),
        total_candidates,
        matches.len()
    ));
    if matches.is_empty() {
        return Ok(None);
    }
    if matches.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple AX elements matched id \"{needle}\""
        )));
    }
    Ok(Some(matches[0].clone()))
}

pub(super) fn try_click_ax_element_id_target(
    needle: &str,
    button: PointerButton,
) -> Result<Option<Value>, AppError> {
    let ax_elements = match platform::ax::collect_frontmost_window_elements() {
        Ok(items) => items,
        Err(err) => {
            trace::log(format!("pointer_click_id:ax_direct_warn {err}"));
            return Ok(None);
        }
    };
    if ax_elements.is_empty() {
        return Ok(None);
    }

    let mut elements: Vec<desktop_core::protocol::TokenizeElement> = ax_elements
        .iter()
        .map(|ax| {
            vision::element_normalizer::ElementBuilder::new()
                .id(vision::ax_merge::primary_id_for_ax(ax))
                .kind("")
                .bbox(ax.bounds.clone())
                .has_border(None)
                .text(ax.text.clone())
                .confidence(None)
                .checked(ax.checked)
                .source(format!("accessibility_ax:{}", ax.role))
                .build()
        })
        .collect();
    vision::element_normalizer::finalize_elements(&mut elements);
    let candidates: Vec<TokenizeClickElementCandidate> = elements
        .into_iter()
        .map(|element| TokenizeClickElementCandidate {
            id: element.id,
            text: element.text,
            bounds: desktop_core::protocol::Bounds {
                x: element.bbox[0],
                y: element.bbox[1],
                width: element.bbox[2],
                height: element.bbox[3],
            },
            source: element.source,
        })
        .collect();
    let total_candidates = candidates.len();
    let matches: Vec<TokenizeClickElementCandidate> = candidates
        .into_iter()
        .filter(|element| element.id == needle)
        .collect();
    trace::log(format!(
        "pointer_click_id:ax_direct_candidates id=\"{}\" total={} matched={}",
        compact_for_log(needle),
        total_candidates,
        matches.len()
    ));
    if matches.is_empty() {
        return Ok(None);
    }
    if matches.len() > 1 {
        return Err(AppError::ambiguous_target(format!(
            "multiple AX elements matched id \"{needle}\""
        )));
    }

    let target = &matches[0];
    trace::log(format!(
        "pointer_click_id:ax_direct_selected id=\"{}\" source={} bounds=({:.1}, {:.1}, {:.1}, {:.1}) text=\"{}\"",
        compact_for_log(&target.id),
        compact_for_log(&target.source),
        target.bounds.x,
        target.bounds.y,
        target.bounds.width,
        target.bounds.height,
        compact_for_log(target.text.as_deref().unwrap_or(""))
    ));
    perform_click(&target.bounds, button)?;
    Ok(Some(json!({
        "click_target": {
            "id": target.id.clone(),
            "text": target.text.clone(),
            "bounds": target.bounds.clone()
        }
    })))
}

pub(super) fn tokenize_click_text_candidate(
    query: &str,
    window_bounds: Option<&desktop_core::protocol::Bounds>,
    request_context: &RequestContext,
) -> Result<desktop_core::protocol::SnapshotText, AppError> {
    let bounds = window_bounds.cloned().ok_or_else(|| {
        AppError::target_not_found("frontmost window bounds unavailable for tokenize fallback")
    })?;
    let app = request_frontmost_app(request_context);
    let window_meta = vision::pipeline::TokenizeWindowMeta {
        id: "frontmost:1".to_string(),
        title: app.clone().unwrap_or_else(|| "active_window".to_string()),
        app,
        bounds,
        pid: None,
        native_window_id: None,
        capture_bounds: None,
    };
    let payload = vision::pipeline::tokenize_window(window_meta)?;
    let tokenize_texts = tokenize_payload_texts_for_click(&payload);
    if tokenize_texts.is_empty() {
        return Err(AppError::target_not_found(
            "tokenize fallback produced no text elements",
        ));
    }
    select_text_candidate(&tokenize_texts, query)
}

pub(super) fn tokenize_payload_texts_for_click(
    payload: &desktop_core::protocol::TokenizePayload,
) -> Vec<desktop_core::protocol::SnapshotText> {
    let mut out = Vec::new();
    let Some(image) = payload.image.as_ref() else {
        return out;
    };
    let image_w = image.width as f64;
    let image_h = image.height as f64;
    if image_w <= 0.0 || image_h <= 0.0 {
        return out;
    }

    for window in &payload.windows {
        let Some(os_bounds) = window.os_bounds.as_ref() else {
            continue;
        };
        for element in &window.elements {
            let text = element
                .text
                .as_ref()
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .map(ToString::to_string);
            let Some(text) = text else { continue };
            let Some(bounds) = tokenize_element_bbox_to_display(&element.bbox, os_bounds, image)
            else {
                continue;
            };
            let confidence =
                element
                    .confidence
                    .unwrap_or(if element.source.starts_with("accessibility_ax:") {
                        0.92
                    } else {
                        0.62
                    });
            out.push(desktop_core::protocol::SnapshotText {
                text,
                bounds,
                confidence,
            });
        }
    }
    out
}

pub(super) fn tokenize_payload_elements_for_click(
    payload: &desktop_core::protocol::TokenizePayload,
) -> Vec<TokenizeClickElementCandidate> {
    let mut out = Vec::new();
    let Some(image) = payload.image.as_ref() else {
        return out;
    };
    for window in &payload.windows {
        let Some(os_bounds) = window.os_bounds.as_ref() else {
            continue;
        };
        for element in &window.elements {
            let Some(bounds) = tokenize_element_bbox_to_display(&element.bbox, os_bounds, image)
            else {
                continue;
            };
            out.push(TokenizeClickElementCandidate {
                id: element.id.clone(),
                text: element
                    .text
                    .as_ref()
                    .map(|v| v.trim())
                    .filter(|v| !v.is_empty())
                    .map(ToString::to_string),
                bounds,
                source: element.source.clone(),
            });
        }
    }
    out
}

pub(super) fn tokenize_element_bbox_to_display(
    bbox: &[f64; 4],
    os_bounds: &desktop_core::protocol::Bounds,
    image: &desktop_core::protocol::TokenizeImage,
) -> Option<desktop_core::protocol::Bounds> {
    if image.width == 0 || image.height == 0 {
        return None;
    }
    if bbox[2] <= 0.0 || bbox[3] <= 0.0 {
        return None;
    }
    let sx = os_bounds.width / image.width as f64;
    let sy = os_bounds.height / image.height as f64;
    Some(desktop_core::protocol::Bounds {
        x: os_bounds.x + bbox[0] * sx,
        y: os_bounds.y + bbox[1] * sy,
        width: bbox[2] * sx,
        height: bbox[3] * sy,
    })
}

pub(super) fn perform_click(
    bounds: &desktop_core::protocol::Bounds,
    button: PointerButton,
) -> Result<Point, AppError> {
    let center_x = (bounds.x + bounds.width / 2.0).max(0.0).round() as u32;
    let center_y = (bounds.y + bounds.height / 2.0).max(0.0).round() as u32;
    trace::log(format!(
        "perform_click:point bounds=({}, {}, {}, {}) center=({}, {})",
        bounds.x, bounds.y, bounds.width, bounds.height, center_x, center_y
    ));
    perform_click_at(center_x, center_y, button)
}

fn perform_click_for_target_window(
    bounds: &desktop_core::protocol::Bounds,
    button: PointerButton,
    target_window: Option<&platform::windowing::WindowInfo>,
) -> Result<Point, AppError> {
    let center_x = (bounds.x + bounds.width / 2.0).max(0.0).round() as u32;
    let center_y = (bounds.y + bounds.height / 2.0).max(0.0).round() as u32;
    if background_input_enabled()
        && target_window.is_some()
        && !matches!(button, PointerButton::Left)
    {
        return Err(background_input_unsupported("right click"));
    }
    if background_input_enabled()
        && let Some(window) = target_window
    {
        let point = Point::new(center_x, center_y);
        let target = background_input_target_for_window(window)?;
        let backend = desktop_core::automation::new_background_input_backend()?;
        backend.left_click(&target, point)?;
        trace::log(format!(
            "background_input:click_target ok pid={} window_id={} point=({}, {})",
            target.pid, target.window_id, point.x, point.y
        ));
        return Ok(point);
    }
    perform_click_at(center_x, center_y, button)
}

pub(super) fn perform_click_at(x: u32, y: u32, button: PointerButton) -> Result<Point, AppError> {
    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    let point = Point::new(x, y);
    trace::log(format!("perform_click:move start center=({}, {})", x, y));
    backend.move_mouse(point)?;
    trace::log("perform_click:move ok");
    thread::sleep(Duration::from_millis(60));
    match button {
        PointerButton::Left => {
            trace::log("perform_click:left_click start");
            backend.left_click(point)?;
            trace::log("perform_click:left_click ok");
        }
        PointerButton::Right => {
            trace::log("perform_click:right_click start");
            backend.right_click(point)?;
            trace::log("perform_click:right_click ok");
        }
    }
    Ok(point)
}

pub(super) fn click_scope_window_bounds(
    request_context: &RequestContext,
) -> Option<desktop_core::protocol::Bounds> {
    #[cfg(target_os = "macos")]
    {
        if overlay::is_active() {
            if let Some(bounds) = overlay::tracked_window_bounds() {
                trace::log(format!(
                    "ui_click_text:window_scope source=overlay bounds=({:.1},{:.1},{:.1},{:.1})",
                    bounds.x, bounds.y, bounds.width, bounds.height
                ));
                return Some(bounds);
            }
        }
    }
    let bounds = request_frontmost_bounds(request_context);
    if let Some(b) = bounds.as_ref() {
        trace::log(format!(
            "ui_click_text:window_scope source=frontmost bounds=({:.1},{:.1},{:.1},{:.1})",
            b.x, b.y, b.width, b.height
        ));
    } else {
        trace::log("ui_click_text:window_scope source=none");
    }
    bounds
}

fn normalize_snapshot_texts_to_display(
    texts: &[desktop_core::protocol::SnapshotText],
    image_width: u32,
    image_height: u32,
    display_width: u32,
    display_height: u32,
) -> Vec<desktop_core::protocol::SnapshotText> {
    if image_width == 0 || image_height == 0 || display_width == 0 || display_height == 0 {
        return texts.to_vec();
    }
    let sx = image_width as f64 / display_width as f64;
    let sy = image_height as f64 / display_height as f64;
    if (sx - 1.0).abs() < 0.0001 && (sy - 1.0).abs() < 0.0001 {
        return texts.to_vec();
    }
    texts
        .iter()
        .cloned()
        .map(|mut text| {
            text.bounds = desktop_core::protocol::Bounds {
                x: (text.bounds.x / sx).max(0.0),
                y: (text.bounds.y / sy).max(0.0),
                width: (text.bounds.width / sx).max(0.0),
                height: (text.bounds.height / sy).max(0.0),
            };
            text
        })
        .collect()
}

fn filter_texts_to_window(
    texts: &[desktop_core::protocol::SnapshotText],
    window_bounds: &desktop_core::protocol::Bounds,
) -> Vec<desktop_core::protocol::SnapshotText> {
    texts
        .iter()
        .filter(|text| {
            let cx = text.bounds.x + text.bounds.width / 2.0;
            let cy = text.bounds.y + text.bounds.height / 2.0;
            cx >= window_bounds.x
                && cx <= window_bounds.x + window_bounds.width
                && cy >= window_bounds.y
                && cy <= window_bounds.y + window_bounds.height
        })
        .cloned()
        .collect()
}

fn filter_texts_to_window_progressive(
    texts: &[desktop_core::protocol::SnapshotText],
    window_bounds: &desktop_core::protocol::Bounds,
) -> Vec<desktop_core::protocol::SnapshotText> {
    const PAD_LEVELS: [f64; 4] = [4.0, 40.0, 96.0, 180.0];
    for pad in PAD_LEVELS {
        let filtered = filter_texts_to_window(texts, &inflate_bounds(window_bounds, pad));
        if !filtered.is_empty() {
            trace::log(format!(
                "ui_click_text:window_filter pad={pad:.1} hits={}",
                filtered.len()
            ));
            return filtered;
        }
    }
    Vec::new()
}

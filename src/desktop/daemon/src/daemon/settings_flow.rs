use super::*;

pub(super) fn screen_settings_map() -> Result<Value, AppError> {
    let capture = vision::pipeline::capture_and_update(None)?;
    let frame_image = load_rgba_image(&capture.image_path);
    let detected_regions_raw = frame_image
        .as_ref()
        .map(vision::regions::detect_settings_regions)
        .unwrap_or_default();
    let detected_regions = scale_regions_to_display(
        &detected_regions_raw,
        frame_image.as_ref().map(|img| img.width()),
        frame_image.as_ref().map(|img| img.height()),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
    );
    let inferred_window_bounds = infer_window_bounds_from_content(
        detected_regions.content_bounds.as_ref(),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
    )
    .or_else(|| detected_regions.window_bounds.clone());
    let heading = find_settings_heading(
        &capture.snapshot.texts,
        detected_regions.content_bounds.as_ref(),
    )
    .or_else(|| find_settings_heading(&capture.snapshot.texts, None));
    let instruction = find_settings_instruction(&capture.snapshot.texts, heading.as_ref());
    let rows = infer_settings_rows(&capture.snapshot.texts, heading.as_ref());
    let row_height = median_row_height(&rows).unwrap_or(14.0);
    let no_items = first_matching_text(&capture.snapshot.texts, "no items");
    let mut rows_bounds = bounds_from_texts(&rows).map(|b| desktop_core::protocol::Bounds {
        x: (b.x - 18.0).max(0.0),
        y: (b.y - 6.0).max(0.0),
        width: b.width + 56.0,
        height: b.height + 12.0,
    });
    if let (Some(list), Some(content)) = (&rows_bounds, &detected_regions.content_bounds) {
        let list_center_x = list.x + list.width / 2.0;
        let list_center_y = list.y + list.height / 2.0;
        let content_x2 = content.x + content.width;
        let content_y2 = content.y + content.height;
        let center_inside = list_center_x >= content.x
            && list_center_x <= content_x2
            && list_center_y >= content.y
            && list_center_y <= content_y2;
        if !center_inside || iou(list, content) < 0.06 {
            rows_bounds = None;
        }
    }
    if is_sidebar_like_rows(rows_bounds.as_ref(), heading.as_ref(), no_items.as_ref()) {
        rows_bounds = None;
    }

    let mut list_bounds = rows_bounds;
    if list_bounds.is_none() {
        list_bounds = infer_list_bounds_from_anchors(
            heading.as_ref(),
            no_items.as_ref(),
            detected_regions.content_bounds.as_ref(),
        );
    }
    if list_bounds.is_none() {
        list_bounds = detected_regions.table_bounds.clone();
    }

    let controls = infer_settings_controls_for_settings_pane(
        &capture.snapshot.texts,
        heading.as_ref(),
        no_items.as_ref(),
        instruction.as_ref(),
        list_bounds.as_ref(),
        detected_regions.content_bounds.as_ref(),
        row_height,
    );
    trace::log(format!(
        "screen_settings_map snapshot_id={} image={}x{} display={}x{} heading={} instruction={} no_items={} rows={} list={} regions.window={} regions.content={} regions.table={} controls={}",
        capture.snapshot.snapshot_id,
        frame_image
            .as_ref()
            .map(|img| img.width().to_string())
            .unwrap_or_else(|| "0".to_string()),
        frame_image
            .as_ref()
            .map(|img| img.height().to_string())
            .unwrap_or_else(|| "0".to_string()),
        capture.snapshot.display.width,
        capture.snapshot.display.height,
        fmt_bounds_opt(heading.as_ref().map(|h| &h.bounds)),
        fmt_bounds_opt(instruction.as_ref().map(|t| &t.bounds)),
        fmt_bounds_opt(no_items.as_ref().map(|t| &t.bounds)),
        rows.len(),
        fmt_bounds_opt(list_bounds.as_ref()),
        fmt_bounds_opt(inferred_window_bounds.as_ref()),
        fmt_bounds_opt(detected_regions.content_bounds.as_ref()),
        fmt_bounds_opt(detected_regions.table_bounds.as_ref()),
        controls
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "null".to_string())
    ));

    let row_entries = rows
        .iter()
        .map(|row| {
            let toggle = list_bounds.as_ref().map(|list| {
                bounds_from_center(
                    list.x + list.width - 18.0,
                    row.bounds.y + row.bounds.height / 2.0,
                    28.0,
                    16.0,
                )
            });
            let toggle_state = toggle
                .as_ref()
                .map(|bounds| {
                    estimate_toggle_state(
                        frame_image.as_ref(),
                        bounds,
                        capture.snapshot.display.width,
                        capture.snapshot.display.height,
                    )
                })
                .unwrap_or_else(|| "unknown".to_string());
            json!({
                "text": row.text,
                "bounds": row.bounds,
                "confidence": row.confidence,
                "toggle_bounds": toggle,
                "toggle_click": toggle.as_ref().map(center_point),
                "toggle_state": toggle_state
            })
        })
        .collect::<Vec<_>>();

    Ok(json!({
        "snapshot_id": capture.snapshot.snapshot_id,
        "timestamp": capture.snapshot.timestamp,
        "display": capture.snapshot.display,
        "focused_app": capture.snapshot.focused_app,
        "heading": heading,
        "instruction": instruction,
        "no_items": no_items,
        "list_bounds": list_bounds,
        "regions": {
            "settings_window": inferred_window_bounds,
            "sidebar": detected_regions.sidebar_bounds,
            "content": detected_regions.content_bounds,
            "table": detected_regions.table_bounds
        },
        "rows": row_entries,
        "controls": controls
    }))
}

pub(super) fn is_sidebar_like_rows(
    rows_bounds: Option<&desktop_core::protocol::Bounds>,
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
) -> bool {
    let Some(rows) = rows_bounds else {
        return false;
    };
    if rows.height >= 240.0 {
        return true;
    }
    if let Some(no_items) = no_items {
        if rows.y > no_items.bounds.y + 56.0 {
            return true;
        }
        if rows.x + rows.width < no_items.bounds.x + 20.0 {
            return true;
        }
    }
    if let Some(heading) = heading {
        if rows.y > heading.bounds.y + 190.0 {
            return true;
        }
        if rows.x + rows.width < heading.bounds.x - 8.0 {
            return true;
        }
    }
    false
}

fn fmt_bounds_opt(bounds: Option<&desktop_core::protocol::Bounds>) -> String {
    match bounds {
        Some(b) => format!("({:.1},{:.1},{:.1},{:.1})", b.x, b.y, b.width, b.height),
        None => "null".to_string(),
    }
}

pub(super) fn scale_regions_to_display(
    regions: &vision::regions::SettingsRegions,
    image_width: Option<u32>,
    image_height: Option<u32>,
    display_width: u32,
    display_height: u32,
) -> vision::regions::SettingsRegions {
    let Some(img_w) = image_width else {
        return regions.clone();
    };
    let Some(img_h) = image_height else {
        return regions.clone();
    };
    if img_w == 0 || img_h == 0 || display_width == 0 || display_height == 0 {
        return regions.clone();
    }
    let sx = display_width as f64 / img_w as f64;
    let sy = display_height as f64 / img_h as f64;
    if (sx - 1.0).abs() < 0.0001 && (sy - 1.0).abs() < 0.0001 {
        return regions.clone();
    }

    let scale = |b: &desktop_core::protocol::Bounds| desktop_core::protocol::Bounds {
        x: (b.x * sx).max(0.0),
        y: (b.y * sy).max(0.0),
        width: (b.width * sx).max(0.0),
        height: (b.height * sy).max(0.0),
    };

    vision::regions::SettingsRegions {
        window_bounds: regions.window_bounds.as_ref().map(scale),
        sidebar_bounds: regions.sidebar_bounds.as_ref().map(scale),
        content_bounds: regions.content_bounds.as_ref().map(scale),
        table_bounds: regions.table_bounds.as_ref().map(scale),
    }
}

pub(super) fn infer_window_bounds_from_content(
    content: Option<&desktop_core::protocol::Bounds>,
    display_width: u32,
    display_height: u32,
) -> Option<desktop_core::protocol::Bounds> {
    let content = content?;
    if content.width <= 0.0 || content.height <= 0.0 {
        return None;
    }
    let sidebar_w = (content.width * 0.31).clamp(150.0, 340.0);
    let title_h = (content.height * 0.085).clamp(30.0, 56.0);
    let x0 = (content.x - sidebar_w).max(0.0);
    let y0 = (content.y - title_h).max(0.0);
    let x1 = (content.x + content.width).min(display_width as f64);
    let y1 = (content.y + content.height).min(display_height as f64);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(desktop_core::protocol::Bounds {
        x: x0,
        y: y0,
        width: (x1 - x0).max(0.0),
        height: (y1 - y0).max(0.0),
    })
}

pub(super) fn infer_settings_controls_for_settings_pane(
    texts: &[desktop_core::protocol::SnapshotText],
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
    instruction: Option<&desktop_core::protocol::SnapshotText>,
    list_bounds: Option<&desktop_core::protocol::Bounds>,
    content_bounds: Option<&desktop_core::protocol::Bounds>,
    row_height: f64,
) -> Option<Value> {
    if let Some(controls) =
        infer_settings_controls_from_ocr_symbols(texts, heading, no_items, instruction, content_bounds)
    {
        return Some(controls);
    }
    if let Some(controls) = infer_settings_controls_from_anchor(heading, no_items) {
        return Some(controls);
    }
    // In VM panes with populated rows, list bounds inferred from row labels can be
    // horizontally biased to the right. Instruction anchor tends to align better
    // with the +/- controls row in those cases.
    if let Some(controls) = infer_settings_controls_from_instruction_anchor(
        instruction,
        heading,
        content_bounds,
        list_bounds,
        row_height,
    ) {
        return Some(controls);
    }
    if let Some(controls) =
        infer_settings_controls_from_list_bounds(list_bounds, heading, row_height)
    {
        return Some(controls);
    }
    None
}

fn infer_settings_controls_from_ocr_symbols(
    texts: &[desktop_core::protocol::SnapshotText],
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
    instruction: Option<&desktop_core::protocol::SnapshotText>,
    content_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Option<Value> {
    let heading = heading?;
    let y_min = heading.bounds.y + heading.bounds.height + 8.0;
    let y_max = heading.bounds.y + 360.0;
    let mut x_min = heading.bounds.x - 100.0;
    let mut x_max = heading.bounds.x + 320.0;
    if let Some(content) = content_bounds {
        x_min = x_min.max(content.x - 8.0);
        x_max = x_max.min(content.x + content.width * 0.55);
    }
    if let Some(no_items) = no_items {
        x_max = x_max.min(no_items.bounds.x - 8.0);
    }
    if x_max <= x_min {
        return None;
    }

    let mut pluses = Vec::new();
    let mut minuses = Vec::new();
    for text in texts {
        if let Some((plus_bounds, minus_bounds)) =
            split_compound_control_token_bounds(&text.text, &text.bounds)
        {
            let mut plus_bounds = plus_bounds;
            let mut minus_bounds = minus_bounds;
            if let Some(instruction) = instruction {
                let anchored_plus_cx = instruction.bounds.x + 4.0;
                let token_plus_cx = plus_bounds.x + plus_bounds.width / 2.0;
                if (anchored_plus_cx - token_plus_cx).abs() <= 72.0 {
                    let cy = plus_bounds.y + plus_bounds.height / 2.0;
                    plus_bounds = bounds_from_center(anchored_plus_cx, cy, 14.0, 14.0);
                    minus_bounds = bounds_from_center(anchored_plus_cx + 18.0, cy, 14.0, 14.0);
                }
            }

            let plus_cx = plus_bounds.x + plus_bounds.width / 2.0;
            let plus_cy = plus_bounds.y + plus_bounds.height / 2.0;
            let minus_cx = minus_bounds.x + minus_bounds.width / 2.0;
            let minus_cy = minus_bounds.y + minus_bounds.height / 2.0;
            let in_scope = |cx: f64, cy: f64| -> bool {
                if cx < x_min || cx > x_max || cy < y_min || cy > y_max {
                    return false;
                }
                if let Some(content) = content_bounds {
                    let cx2 = content.x + content.width;
                    let cy2 = content.y + content.height;
                    if cx < content.x || cx > cx2 || cy < content.y || cy > cy2 {
                        return false;
                    }
                }
                true
            };
            if in_scope(plus_cx, plus_cy) && in_scope(minus_cx, minus_cy) {
                pluses.push(desktop_core::protocol::SnapshotText {
                    text: "+".to_string(),
                    bounds: plus_bounds,
                    confidence: text.confidence,
                });
                minuses.push(desktop_core::protocol::SnapshotText {
                    text: "-".to_string(),
                    bounds: minus_bounds,
                    confidence: text.confidence,
                });
                continue;
            }
        }

        let Some(symbol) = normalize_control_symbol(&text.text) else {
            continue;
        };
        let cx = text.bounds.x + text.bounds.width / 2.0;
        let cy = text.bounds.y + text.bounds.height / 2.0;
        if cx < x_min || cx > x_max || cy < y_min || cy > y_max {
            continue;
        }
        if let Some(content) = content_bounds {
            let cx2 = content.x + content.width;
            let cy2 = content.y + content.height;
            if cx < content.x || cx > cx2 || cy < content.y || cy > cy2 {
                continue;
            }
        }
        match symbol {
            '+' => pluses.push(text.clone()),
            '-' => minuses.push(text.clone()),
            _ => {}
        }
    }

    let mut best: Option<(
        desktop_core::protocol::SnapshotText,
        desktop_core::protocol::SnapshotText,
        f64,
    )> = None;
    for plus in &pluses {
        let plus_cx = plus.bounds.x + plus.bounds.width / 2.0;
        let plus_cy = plus.bounds.y + plus.bounds.height / 2.0;
        for minus in &minuses {
            let minus_cx = minus.bounds.x + minus.bounds.width / 2.0;
            let minus_cy = minus.bounds.y + minus.bounds.height / 2.0;
            let dx = minus_cx - plus_cx;
            let dy = (minus_cy - plus_cy).abs();
            if !(8.0..=36.0).contains(&dx) || dy > 12.0 {
                continue;
            }
            let score = (dx - 18.0).abs() + dy * 2.0;
            if best.as_ref().map(|(_, _, s)| score < *s).unwrap_or(true) {
                best = Some((plus.clone(), minus.clone(), score));
            }
        }
    }
    let (plus, minus, _) = best?;
    let plus_center_x = plus.bounds.x + plus.bounds.width / 2.0;
    let plus_center_y = plus.bounds.y + plus.bounds.height / 2.0;
    let minus_center_x = minus.bounds.x + minus.bounds.width / 2.0;
    let footer = desktop_core::protocol::Bounds {
        x: (plus_center_x - 18.0).max(0.0),
        y: (plus_center_y - 12.0).max(0.0),
        width: (minus_center_x - plus_center_x + 36.0).max(36.0),
        height: 24.0,
    };
    Some(json!({
        "source": "ocr_symbols",
        "add_button_bounds": plus.bounds,
        "remove_button_bounds": minus.bounds,
        "add_click": center_point(&plus.bounds),
        "remove_click": center_point(&minus.bounds),
        "footer_bounds": footer
    }))
}

fn normalize_control_symbol(text: &str) -> Option<char> {
    let value = text.trim();
    if value == "+" {
        return Some('+');
    }
    if matches!(value, "-" | "−" | "–" | "—") {
        return Some('-');
    }
    None
}

fn split_compound_control_token_bounds(
    text: &str,
    bounds: &desktop_core::protocol::Bounds,
) -> Option<(desktop_core::protocol::Bounds, desktop_core::protocol::Bounds)> {
    let value = text.trim();
    let has_plus = value.contains('+');
    let has_minus = value.chars().any(|ch| matches!(ch, '-' | '−' | '–' | '—'));
    if !has_plus || !has_minus || bounds.width < 24.0 || bounds.height < 8.0 {
        return None;
    }

    let cy = bounds.y + bounds.height / 2.0;
    let min_x = bounds.x + 6.0;
    let max_x = bounds.x + bounds.width - 6.0;
    if max_x - min_x < 18.0 {
        return None;
    }

    let plus_cx = (bounds.x + 10.0).clamp(min_x, max_x - 18.0);
    let minus_cx = (plus_cx + 18.0).clamp(plus_cx + 10.0, max_x);
    let plus = bounds_from_center(plus_cx, cy, 14.0, 14.0);
    let minus = bounds_from_center(minus_cx, cy, 14.0, 14.0);
    Some((plus, minus))
}

fn infer_settings_controls_from_anchor(
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
) -> Option<Value> {
    let no_items = no_items?;
    if let Some(heading) = heading {
        if no_items.bounds.x + no_items.bounds.width < heading.bounds.x + 20.0 {
            return None;
        }
        if no_items.bounds.y < heading.bounds.y + heading.bounds.height + 12.0 {
            return None;
        }
        if no_items.bounds.y > heading.bounds.y + 260.0 {
            return None;
        }
    }
    let center_x = no_items.bounds.x + no_items.bounds.width / 2.0;
    let center_y = no_items.bounds.y + no_items.bounds.height / 2.0;
    let add_x = center_x - 182.0;
    let remove_x = center_x - 164.0;
    let controls_y = center_y + 20.0;
    let plus = bounds_from_center(add_x, controls_y, 14.0, 14.0);
    let minus = bounds_from_center(remove_x, controls_y, 14.0, 14.0);
    let footer = desktop_core::protocol::Bounds {
        x: (add_x - 14.0).max(0.0),
        y: (controls_y - 12.0).max(0.0),
        width: 48.0,
        height: 24.0,
    };
    Some(json!({
        "source": "no_items_anchor",
        "add_button_bounds": plus,
        "remove_button_bounds": minus,
        "add_click": center_point(&plus),
        "remove_click": center_point(&minus),
        "footer_bounds": footer
    }))
}

fn infer_settings_controls_from_instruction_anchor(
    instruction: Option<&desktop_core::protocol::SnapshotText>,
    heading: Option<&desktop_core::protocol::SnapshotText>,
    content_bounds: Option<&desktop_core::protocol::Bounds>,
    list_bounds: Option<&desktop_core::protocol::Bounds>,
    row_height: f64,
) -> Option<Value> {
    let instruction = instruction?;
    if let Some(heading) = heading {
        if instruction.bounds.y < heading.bounds.y + heading.bounds.height + 4.0 {
            return None;
        }
        if instruction.bounds.y > heading.bounds.y + 200.0 {
            return None;
        }
    }
    let mut add_x = instruction.bounds.x + 4.0;
    let mut remove_x = add_x + 18.0;
    let instruction_bottom = instruction.bounds.y + instruction.bounds.height;
    let mut controls_y = instruction_bottom + 44.0;
    let mut y_source = "instruction_offset";
    if let Some(list) = list_bounds {
        let list_controls_y = list.y + list.height + row_height.clamp(10.0, 24.0);
        let delta = list_controls_y - instruction_bottom;
        // Keep instruction-derived X (more stable) but use list-derived Y when the
        // inferred footer row is plausibly below the instruction line.
        if (24.0..=120.0).contains(&delta) {
            controls_y = list_controls_y;
            y_source = "list_bounds";
        }
    }
    if let Some(content) = content_bounds {
        add_x = add_x.max(content.x + 8.0);
        remove_x = remove_x.max(add_x + 16.0);
        controls_y = controls_y.clamp(content.y + 18.0, content.y + content.height - 8.0);
    }
    let plus = bounds_from_center(add_x, controls_y, 14.0, 14.0);
    let minus = bounds_from_center(remove_x, controls_y, 14.0, 14.0);
    let footer = desktop_core::protocol::Bounds {
        x: (plus.x - 8.0).max(0.0),
        y: (plus.y - 6.0).max(0.0),
        width: 52.0,
        height: 24.0,
    };
    Some(json!({
        "source": "instruction_anchor",
        "y_source": y_source,
        "add_button_bounds": plus,
        "remove_button_bounds": minus,
        "add_click": center_point(&plus),
        "remove_click": center_point(&minus),
        "footer_bounds": footer
    }))
}

pub(super) fn settings_click_from_no_items_anchor(
    control: &str,
    payload: &Value,
) -> Option<(u32, u32)> {
    let (nx, ny, nw, nh) = bounds_tuple_from_value(&payload["no_items"]["bounds"])?;
    if let Some((hx, hy, hw, hh)) = payload
        .get("heading")
        .and_then(|v| v.get("bounds"))
        .and_then(bounds_tuple_from_value)
    {
        if nx + nw < hx + 20.0 {
            return None;
        }
        if ny < hy + hh + 12.0 || ny > hy + 260.0 {
            return None;
        }
        if hx + hw < nx - 340.0 {
            return None;
        }
    }
    // Prefer instruction-line X anchor when available:
    // "+" is aligned under the first letter in "Allow the applications below...".
    let instruction_anchor_x = payload
        .get("instruction")
        .and_then(|v| v.get("bounds"))
        .and_then(bounds_tuple_from_value)
        .and_then(|(ix, iy, iw, ih)| {
            let instruction_bottom = iy + ih;
            let plausible = instruction_bottom < ny
                && (ny - instruction_bottom) <= 220.0
                && ix + iw >= nx - 120.0;
            if plausible { Some(ix + 4.0) } else { None }
        });

    // Fallback to left-edge "No Items" anchor. OCR width for this label can vary
    // a lot and shifts center-based clicks, so avoid using its center.
    let no_items_anchor_x = nx;
    let center_y = ny + nh / 2.0;
    let x = match control {
        "add" => instruction_anchor_x.unwrap_or(no_items_anchor_x - 153.5),
        "remove" => instruction_anchor_x
            .map(|x| x + 18.0)
            .unwrap_or(no_items_anchor_x - 135.5),
        _ => return None,
    };
    let y = (center_y + 20.0).round().max(0.0) as u32;
    Some((x.round().max(0.0) as u32, y))
}

pub(super) fn click_settings_control(
    control: &str,
    row_text: Option<&str>,
    timeout_ms: u64,
) -> Result<Value, AppError> {
    permissions::ensure_screen_recording_permission()?;
    let payload = screen_settings_map()?;
    let (x, y, details) = match control {
        "add" => {
            if let Some((x, y)) = settings_click_from_no_items_anchor("add", &payload) {
                (
                    x,
                    y,
                    json!({
                        "control": "add",
                        "click": { "x": x, "y": y },
                        "derived_from": "no_items_anchor"
                    }),
                )
            } else {
                let click = payload["controls"]["add_click"].clone();
                let (x, y) = point_from_value(&click).ok_or_else(|| {
                    AppError::target_not_found("settings add (+) button was not found")
                })?;
                (x, y, json!({ "control": "add", "click": click }))
            }
        }
        "remove" => {
            if let Some((x, y)) = settings_click_from_no_items_anchor("remove", &payload) {
                (
                    x,
                    y,
                    json!({
                        "control": "remove",
                        "click": { "x": x, "y": y },
                        "derived_from": "no_items_anchor"
                    }),
                )
            } else {
                let click = payload["controls"]["remove_click"].clone();
                let (x, y) = point_from_value(&click).ok_or_else(|| {
                    AppError::target_not_found("settings remove (-) button was not found")
                })?;
                (x, y, json!({ "control": "remove", "click": click }))
            }
        }
        "toggle" => {
            let needle = row_text
                .map(|s| s.trim().to_lowercase())
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AppError::invalid_argument("settings toggle requires row text"))?;
            let rows = payload["rows"]
                .as_array()
                .ok_or_else(|| AppError::internal("invalid settings rows payload"))?;
            let matched = rows.iter().find(|row| {
                row["text"]
                    .as_str()
                    .map(|text| text.to_lowercase().contains(&needle))
                    .unwrap_or(false)
            });
            let row = matched.ok_or_else(|| {
                AppError::target_not_found(format!(
                    "settings row \"{}\" was not found",
                    row_text.unwrap_or_default()
                ))
            })?;
            let click = row["toggle_click"].clone();
            let (x, y) = point_from_value(&click).ok_or_else(|| {
                AppError::target_not_found(format!(
                    "toggle target for row \"{}\" was not found",
                    row_text.unwrap_or_default()
                ))
            })?;
            (
                x,
                y,
                json!({
                    "control": "toggle",
                    "row_text": row["text"],
                    "click": click,
                    "toggle_bounds": row["toggle_bounds"]
                }),
            )
        }
        _ => {
            return Err(AppError::invalid_argument(format!(
                "unsupported settings control: {control}"
            )));
        }
    };

    let display_w = payload["display"]["width"]
        .as_u64()
        .map(|v| v as u32)
        .unwrap_or(u32::MAX);
    let display_h = payload["display"]["height"]
        .as_u64()
        .map(|v| v as u32)
        .unwrap_or(u32::MAX);
    if x >= display_w || y >= display_h {
        return Err(AppError::target_not_found(format!(
            "settings {control} click target ({x},{y}) is outside display bounds {display_w}x{display_h}"
        )));
    }
    if matches!(control, "add" | "remove") {
        if let Some((nx, ny, _nw, _nh)) = bounds_tuple_from_value(&payload["no_items"]["bounds"]) {
            let xf = x as f64;
            let yf = y as f64;
            let x_ok = xf >= (nx - 520.0) && xf <= (nx - 20.0);
            let y_ok = yf >= (ny - 90.0) && yf <= (ny + 90.0);
            if !x_ok || !y_ok {
                return Err(AppError::target_not_found(format!(
                    "rejected unsafe settings {control} click ({x},{y}); anchor mismatch with No Items at ({:.1},{:.1})",
                    nx, ny
                )));
            }
        }
        if payload.get("heading").map(|v| v.is_null()).unwrap_or(true)
            && payload["regions"]
                .get("settings_window")
                .map(|v| v.is_null())
                .unwrap_or(true)
        {
            return Err(AppError::target_not_found(
                "settings pane is not detected in the current frame",
            ));
        }
        if let Some((wx, wy, ww, wh)) = payload["regions"]
            .get("settings_window")
            .and_then(bounds_tuple_from_value)
        {
            let inside_window = (x as f64) >= wx
                && (x as f64) <= wx + ww
                && (y as f64) >= wy
                && (y as f64) <= wy + wh;
            if !inside_window {
                return Err(AppError::target_not_found(format!(
                    "settings {control} click target ({x},{y}) is outside detected settings window"
                )));
            }
        }
        let mut validated_by_controls_band = false;
        if let Some((tx, ty, tw, th)) = payload["controls"]
            .get("footer_bounds")
            .and_then(bounds_tuple_from_value)
        {
            let margin = 26.0;
            let inside_table_band = (x as f64) >= tx - margin
                && (x as f64) <= tx + tw + margin
                && (y as f64) >= ty - margin
                && (y as f64) <= ty + th + margin;
            if !inside_table_band {
                return Err(AppError::target_not_found(format!(
                    "settings {control} click target ({x},{y}) is outside inferred controls band"
                )));
            }
            validated_by_controls_band = true;
        }
        // Heading guard is a fallback safety net. If we already validated click
        // against an inferred controls footer band, do not reject by heading x-range.
        if !validated_by_controls_band {
            if let Some((hx, hy, _hw, hh)) = payload
                .get("heading")
                .and_then(|h| h.get("bounds"))
                .and_then(bounds_tuple_from_value)
            {
                let y_f = y as f64;
                let x_f = x as f64;
                let y_min = hy - hh.max(14.0);
                let y_max = hy + 320.0;
                let x_min = hx - 90.0;
                if y_f < y_min || y_f > y_max || x_f < x_min {
                    return Err(AppError::target_not_found(format!(
                        "settings {control} click target ({x},{y}) is outside expected pane near heading"
                    )));
                }
            }
        }
    }

    trace::log(format!(
        "ui_click_settings_control control={} point=({}, {})",
        control, x, y
    ));
    perform_click_at(x, y)?;
    thread::sleep(Duration::from_millis(timeout_ms.min(500).max(40)));
    Ok(json!({
        "snapshot_id": payload["snapshot_id"],
        "timestamp": payload["timestamp"],
        "result": details
    }))
}

pub(super) fn settings_ensure_enabled(row_text: &str, timeout_ms: u64) -> Result<Value, AppError> {
    let before = screen_settings_map()?;
    let before_row = find_settings_row(&before, row_text)?;
    let before_state = before_row["toggle_state"].as_str().unwrap_or("unknown");
    if before_state == "on" {
        return Ok(json!({
            "row_text": before_row["text"],
            "state_before": before_state,
            "state_after": before_state,
            "changed": false
        }));
    }

    let _ = click_settings_control("toggle", Some(row_text), timeout_ms)?;
    let after = screen_settings_map()?;
    let after_row = find_settings_row(&after, row_text)?;
    let after_state = after_row["toggle_state"].as_str().unwrap_or("unknown");
    if after_state == "on" {
        return Ok(json!({
            "row_text": after_row["text"],
            "state_before": before_state,
            "state_after": after_state,
            "changed": true
        }));
    }

    if unlock_prompt_visible()? {
        return Err(AppError::permission_denied(
            "settings requires authentication; run `desktopctl ui settings unlock --password <password>` and retry",
        ));
    }

    Err(AppError::postcondition_failed(format!(
        "failed to enable settings row \"{row_text}\" (state remained \"{after_state}\")"
    )))
}

pub(super) fn settings_unlock(password: &str, timeout_ms: u64) -> Result<Value, AppError> {
    let candidates = ["Use Password...", "Use Password", "Unlock"];
    for label in candidates {
        let _ = click_text_once(label, 400);
    }

    let start = Instant::now();
    let mut field_clicked = false;
    while start.elapsed().as_millis() as u64 <= timeout_ms.max(600) {
        let capture = vision::pipeline::capture_and_update(None)?;
        if let Ok(password_label) = select_text_candidate(&capture.snapshot.texts, "Password") {
            let x = (password_label.bounds.x + password_label.bounds.width + 120.0)
                .max(0.0)
                .round() as u32;
            let y = (password_label.bounds.y + password_label.bounds.height / 2.0)
                .max(0.0)
                .round() as u32;
            perform_click_at(x, y)?;
            field_clicked = true;
            break;
        }
        thread::sleep(Duration::from_millis(120));
    }
    if !field_clicked {
        return Err(AppError::target_not_found(
            "password field was not found in settings unlock prompt",
        ));
    }

    let backend = new_backend()?;
    backend.check_accessibility_permission()?;
    backend.type_text(password)?;
    backend.press_enter()?;
    thread::sleep(Duration::from_millis(260));

    Ok(json!({
        "unlocked": true
    }))
}

fn click_text_once(query: &str, timeout_ms: u64) -> Result<Value, AppError> {
    permissions::ensure_screen_recording_permission()?;
    let start = Instant::now();
    while start.elapsed().as_millis() as u64 <= timeout_ms.max(250) {
        let capture = vision::pipeline::capture_and_update(None)?;
        if let Ok(target) = select_text_candidate(&capture.snapshot.texts, query) {
            perform_click(&target.bounds)?;
            return Ok(json!({
                "snapshot_id": capture.snapshot.snapshot_id,
                "text": target.text,
                "bounds": target.bounds
            }));
        }
        thread::sleep(Duration::from_millis(80));
    }
    Err(AppError::target_not_found(format!(
        "text target \"{query}\" was not found"
    )))
}

fn find_settings_row<'a>(payload: &'a Value, needle: &str) -> Result<&'a Value, AppError> {
    let rows = payload["rows"]
        .as_array()
        .ok_or_else(|| AppError::internal("invalid settings rows payload"))?;
    let needle = needle.trim().to_lowercase();
    rows.iter()
        .find(|row| {
            row["text"]
                .as_str()
                .map(|text| text.to_lowercase().contains(&needle))
                .unwrap_or(false)
        })
        .ok_or_else(|| {
            AppError::target_not_found(format!("settings row \"{needle}\" was not found"))
        })
}

fn unlock_prompt_visible() -> Result<bool, AppError> {
    let capture = vision::pipeline::capture_and_update(None)?;
    let found = capture.snapshot.texts.iter().any(|text| {
        let lower = text.text.to_lowercase();
        lower.contains("password")
            || lower.contains("unlock")
            || lower.contains("use password")
            || lower.contains("touch id")
    });
    Ok(found)
}

pub(super) fn find_settings_heading(
    texts: &[desktop_core::protocol::SnapshotText],
    content_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Option<desktop_core::protocol::SnapshotText> {
    let keys = [
        "screen & system audio recording",
        "screen recording",
        "accessibility",
    ];
    let mut candidates = texts
        .iter()
        .filter_map(|text| {
            let lower = text.text.to_lowercase();
            let matched = keys.iter().any(|key| lower.contains(key));
            if !matched {
                return None;
            }
            if let Some(content) = content_bounds {
                let center_x = text.bounds.x + text.bounds.width / 2.0;
                let center_y = text.bounds.y + text.bounds.height / 2.0;
                let x2 = content.x + content.width;
                let y2 = content.y + content.height;
                let inside = center_x >= content.x
                    && center_x <= x2
                    && center_y >= content.y
                    && center_y <= y2;
                if !inside || text.bounds.y > content.y + content.height * 0.45 {
                    return None;
                }
            }
            Some(text.clone())
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|a, b| {
        a.bounds
            .y
            .total_cmp(&b.bounds.y)
            .then_with(|| b.bounds.x.total_cmp(&a.bounds.x))
            .then_with(|| b.confidence.total_cmp(&a.confidence))
            .then_with(|| b.bounds.width.total_cmp(&a.bounds.width))
    });
    candidates.into_iter().next()
}

pub(super) fn infer_settings_rows(
    texts: &[desktop_core::protocol::SnapshotText],
    heading: Option<&desktop_core::protocol::SnapshotText>,
) -> Vec<desktop_core::protocol::SnapshotText> {
    let mut rows: Vec<desktop_core::protocol::SnapshotText> = texts
        .iter()
        .filter_map(|text| {
            if !is_probable_settings_row_label(&text.text, text.confidence) {
                return None;
            }
            if let Some(heading) = heading {
                let min_y = heading.bounds.y + heading.bounds.height + 8.0;
                let max_y = heading.bounds.y + 360.0;
                if text.bounds.y < min_y || text.bounds.y > max_y {
                    return None;
                }
                if text.bounds.x + text.bounds.width < heading.bounds.x - 120.0 {
                    return None;
                }
            }
            Some(text.clone())
        })
        .collect();
    rows.sort_by(|a, b| a.bounds.y.total_cmp(&b.bounds.y));
    rows
}

pub(super) fn first_matching_text(
    texts: &[desktop_core::protocol::SnapshotText],
    needle: &str,
) -> Option<desktop_core::protocol::SnapshotText> {
    let needle = needle.trim().to_lowercase();
    texts
        .iter()
        .find(|t| t.text.to_lowercase().contains(&needle))
        .cloned()
}

pub(super) fn find_settings_instruction(
    texts: &[desktop_core::protocol::SnapshotText],
    heading: Option<&desktop_core::protocol::SnapshotText>,
) -> Option<desktop_core::protocol::SnapshotText> {
    let mut candidates: Vec<_> = texts
        .iter()
        .filter(|text| {
            let lower = text.text.to_lowercase();
            lower.contains("allow the applications below")
                || lower.contains("allow applications below")
                || lower.contains("system audio recording only")
        })
        .cloned()
        .collect();
    if let Some(heading) = heading {
        candidates.retain(|text| {
            text.bounds.y >= heading.bounds.y + heading.bounds.height
                && text.bounds.y <= heading.bounds.y + 180.0
                && text.bounds.x + text.bounds.width >= heading.bounds.x - 20.0
        });
    }
    candidates.sort_by(|a, b| {
        a.bounds
            .y
            .total_cmp(&b.bounds.y)
            .then_with(|| a.bounds.x.total_cmp(&b.bounds.x))
    });
    candidates.into_iter().next()
}

pub(super) fn infer_list_bounds_from_anchors(
    heading: Option<&desktop_core::protocol::SnapshotText>,
    no_items: Option<&desktop_core::protocol::SnapshotText>,
    content_bounds: Option<&desktop_core::protocol::Bounds>,
) -> Option<desktop_core::protocol::Bounds> {
    let (mut x0, mut y0, mut x1, mut y1) = match (heading, no_items) {
        (Some(h), Some(no)) => (
            (h.bounds.x - 24.0).max(0.0),
            (h.bounds.y + h.bounds.height + 8.0).max(0.0),
            no.bounds.x + no.bounds.width + 170.0,
            no.bounds.y + no.bounds.height + 24.0,
        ),
        (Some(h), None) => (
            (h.bounds.x - 24.0).max(0.0),
            (h.bounds.y + h.bounds.height + 8.0).max(0.0),
            h.bounds.x + h.bounds.width + 220.0,
            h.bounds.y + h.bounds.height + 84.0,
        ),
        (None, Some(no)) => (
            (no.bounds.x - 170.0).max(0.0),
            (no.bounds.y - 22.0).max(0.0),
            no.bounds.x + no.bounds.width + 170.0,
            no.bounds.y + no.bounds.height + 24.0,
        ),
        (None, None) => return None,
    };

    if let Some(content) = content_bounds {
        let cx2 = content.x + content.width;
        let cy2 = content.y + content.height;
        x0 = x0.max(content.x + 4.0);
        y0 = y0.max(content.y + 20.0);
        x1 = x1.min(cx2 - 6.0);
        y1 = y1.min(cy2 - 4.0);
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let width = x1 - x0;
    let height = y1 - y0;
    if width < 120.0 || height < 36.0 {
        return None;
    }

    Some(desktop_core::protocol::Bounds {
        x: x0,
        y: y0,
        width,
        height,
    })
}

pub(super) fn infer_settings_controls_from_list_bounds(
    list_bounds: Option<&desktop_core::protocol::Bounds>,
    heading: Option<&desktop_core::protocol::SnapshotText>,
    row_height: f64,
) -> Option<Value> {
    let list = list_bounds?;
    if let Some(heading) = heading {
        if list.x + list.width < heading.bounds.x - 32.0 {
            return None;
        }
        if list.y > heading.bounds.y + 320.0 {
            return None;
        }
    }
    let control_y = list.y + list.height + row_height.max(10.0).min(24.0);
    let plus = bounds_from_center(list.x + 12.0, control_y, 14.0, 14.0);
    let minus = bounds_from_center(list.x + 30.0, control_y, 14.0, 14.0);
    let footer = desktop_core::protocol::Bounds {
        x: (plus.x - 8.0).max(0.0),
        y: (plus.y - 6.0).max(0.0),
        width: 52.0,
        height: 24.0,
    };
    Some(json!({
        "source": "list_bounds",
        "add_button_bounds": plus,
        "remove_button_bounds": minus,
        "add_click": center_point(&plus),
        "remove_click": center_point(&minus),
        "footer_bounds": footer
    }))
}

fn is_probable_settings_row_label(text: &str, confidence: f32) -> bool {
    let value = text.trim();
    if confidence < 0.35 || value.is_empty() || value.len() > 48 {
        return false;
    }
    if !value.chars().any(|c| c.is_alphanumeric()) {
        return false;
    }
    let lower = value.to_lowercase();
    let blocked = [
        "allow the applications",
        "system audio recording only",
        "no items",
        "privacy",
    ];
    !blocked.iter().any(|word| lower.contains(word))
}

fn median_row_height(rows: &[desktop_core::protocol::SnapshotText]) -> Option<f64> {
    if rows.is_empty() {
        return None;
    }
    let mut heights = rows
        .iter()
        .map(|row| row.bounds.height)
        .filter(|h| *h > 0.0)
        .collect::<Vec<_>>();
    if heights.is_empty() {
        return None;
    }
    heights.sort_by(|a, b| a.total_cmp(b));
    Some(heights[heights.len() / 2])
}

fn bounds_from_center(x: f64, y: f64, width: f64, height: f64) -> desktop_core::protocol::Bounds {
    desktop_core::protocol::Bounds {
        x: (x - width / 2.0).max(0.0),
        y: (y - height / 2.0).max(0.0),
        width,
        height,
    }
}

fn center_point(bounds: &desktop_core::protocol::Bounds) -> serde_json::Value {
    json!({
        "x": (bounds.x + bounds.width / 2.0).round().max(0.0) as u32,
        "y": (bounds.y + bounds.height / 2.0).round().max(0.0) as u32
    })
}

pub(super) fn point_from_value(value: &serde_json::Value) -> Option<(u32, u32)> {
    Some((
        value.get("x")?.as_u64()? as u32,
        value.get("y")?.as_u64()? as u32,
    ))
}

pub(super) fn bounds_tuple_from_value(value: &serde_json::Value) -> Option<(f64, f64, f64, f64)> {
    Some((
        value.get("x")?.as_f64()?,
        value.get("y")?.as_f64()?,
        value.get("width")?.as_f64()?,
        value.get("height")?.as_f64()?,
    ))
}

pub(super) fn load_rgba_image(path: &std::path::Path) -> Option<RgbaImage> {
    image::open(path).ok().map(|img| img.to_rgba8())
}

pub(super) fn estimate_toggle_state(
    image: Option<&RgbaImage>,
    bounds: &desktop_core::protocol::Bounds,
    display_width: u32,
    display_height: u32,
) -> String {
    let image = match image {
        Some(img) => img,
        None => return "unknown".to_string(),
    };
    let width = image.width();
    let height = image.height();
    if width == 0 || height == 0 || display_width == 0 || display_height == 0 {
        return "unknown".to_string();
    }
    let (x0, y0, x1, y1) =
        match logical_bounds_to_image_rect(bounds, width, height, display_width, display_height) {
            Some(rect) => rect,
            None => return "unknown".to_string(),
        };
    let width = width as i32;
    let height = height as i32;
    if width <= 0 || height <= 0 {
        return "unknown".to_string();
    }

    let mut r_sum = 0f64;
    let mut g_sum = 0f64;
    let mut b_sum = 0f64;
    let mut count = 0f64;
    for y in y0..y1 {
        for x in x0..x1 {
            let px = image.get_pixel(x as u32, y as u32).0;
            r_sum += px[0] as f64;
            g_sum += px[1] as f64;
            b_sum += px[2] as f64;
            count += 1.0;
        }
    }
    if count <= 0.0 {
        return "unknown".to_string();
    }
    let r = r_sum / count;
    let g = g_sum / count;
    let b = b_sum / count;
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let chroma = max - min;

    if chroma < 12.0 {
        "off".to_string()
    } else if b > g + 8.0 && b > r + 18.0 && b > 90.0 {
        "on".to_string()
    } else {
        "unknown".to_string()
    }
}

pub(super) fn logical_bounds_to_image_rect(
    bounds: &desktop_core::protocol::Bounds,
    image_width: u32,
    image_height: u32,
    display_width: u32,
    display_height: u32,
) -> Option<(i32, i32, i32, i32)> {
    if image_width == 0 || image_height == 0 || display_width == 0 || display_height == 0 {
        return None;
    }
    let sx = image_width as f64 / display_width as f64;
    let sy = image_height as f64 / display_height as f64;
    let x0 = (bounds.x * sx).floor().max(0.0) as i32;
    let y0 = (bounds.y * sy).floor().max(0.0) as i32;
    let x1 = ((bounds.x + bounds.width) * sx).ceil().max(0.0) as i32;
    let y1 = ((bounds.y + bounds.height) * sy).ceil().max(0.0) as i32;
    let max_x = image_width as i32;
    let max_y = image_height as i32;
    if max_x <= 0 || max_y <= 0 {
        return None;
    }
    let x0 = x0.clamp(0, max_x - 1);
    let y0 = y0.clamp(0, max_y - 1);
    let x1 = x1.clamp(x0 + 1, max_x);
    let y1 = y1.clamp(y0 + 1, max_y);
    Some((x0, y0, x1, y1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use desktop_core::protocol::{Bounds, SnapshotText};

    fn text(text: &str, bounds: Bounds) -> SnapshotText {
        SnapshotText {
            text: text.to_string(),
            bounds,
            confidence: 0.9,
        }
    }

    #[test]
    fn instruction_anchor_uses_list_y_when_available() {
        let heading = text(
            "Screen Recording",
            Bounds {
                x: 1021.3,
                y: 202.7,
                width: 94.0,
                height: 19.2,
            },
        );
        let instruction = text(
            "Allow the applications below",
            Bounds {
                x: 965.8,
                y: 247.5,
                width: 329.0,
                height: 15.0,
            },
        );
        let content = Bounds {
            x: 720.0,
            y: 184.0,
            width: 715.0,
            height: 625.0,
        };
        let list = Bounds {
            x: 997.3,
            y: 229.9,
            width: 338.0,
            height: 76.0,
        };

        let controls = infer_settings_controls_from_instruction_anchor(
            Some(&instruction),
            Some(&heading),
            Some(&content),
            Some(&list),
            14.0,
        )
        .expect("controls should be inferred");

        assert_eq!(controls["source"], "instruction_anchor");
        assert_eq!(controls["y_source"], "list_bounds");

        let add = point_from_value(&controls["add_click"]).expect("add click");
        assert_eq!(add, (970, 320));
    }

    #[test]
    fn instruction_anchor_falls_back_to_instruction_offset_without_list() {
        let heading = text(
            "Screen Recording",
            Bounds {
                x: 1021.3,
                y: 202.7,
                width: 94.0,
                height: 19.2,
            },
        );
        let instruction = text(
            "Allow the applications below",
            Bounds {
                x: 965.8,
                y: 247.5,
                width: 329.0,
                height: 15.0,
            },
        );
        let content = Bounds {
            x: 720.0,
            y: 184.0,
            width: 715.0,
            height: 625.0,
        };

        let controls = infer_settings_controls_from_instruction_anchor(
            Some(&instruction),
            Some(&heading),
            Some(&content),
            None,
            14.0,
        )
        .expect("controls should be inferred");

        assert_eq!(controls["source"], "instruction_anchor");
        assert_eq!(controls["y_source"], "instruction_offset");

        let add = point_from_value(&controls["add_click"]).expect("add click");
        assert_eq!(add, (970, 307));
    }
}

use desktop_core::protocol::{Bounds, TokenizeElement};
use std::collections::HashMap;

use super::{ax::AxElement, coord_map::CoordMap, element_normalizer::ElementBuilder};

#[derive(Debug, Default, Clone, Copy)]
pub struct AxMergeMetrics {
    pub ax_seen: usize,
    pub ax_added: usize,
    pub ax_replaced: usize,
    pub ax_dropped_bounds: usize,
    pub ax_text_filled: usize,
}

pub fn merge_elements(
    elements: &mut Vec<TokenizeElement>,
    ax_elements: &[AxElement],
    coord_map: &CoordMap,
) -> AxMergeMetrics {
    let mut metrics = AxMergeMetrics::default();
    let mut fallback_id_counts: HashMap<String, usize> = HashMap::new();

    for ax in ax_elements {
        metrics.ax_seen += 1;

        let Some(local) = coord_map.logical_to_image_bounds_clamped(&ax.bounds) else {
            metrics.ax_dropped_bounds += 1;
            continue;
        };

        let (merged_text, filled_from_ocr) =
            merged_ax_text(&ax.role, &local, elements, ax.text.as_deref());
        let ax_primary_id =
            primary_id_for_ax(ax).or_else(|| Some(fallback_id_for_ax(ax, &mut fallback_id_counts)));
        if filled_from_ocr {
            metrics.ax_text_filled += 1;
        }
        let ax_text = merged_text.as_deref().unwrap_or("").trim();
        if should_prioritize_ax_text_region(&ax.role, ax_text) {
            drop_ocr_text_inside_ax_region(elements, &local);
        }

        let mut replace_idx: Option<usize> = None;
        let mut replace_score = 0.0f64;
        for (idx, existing) in elements.iter().enumerate() {
            let eb = Bounds {
                x: existing.bbox[0],
                y: existing.bbox[1],
                width: existing.bbox[2],
                height: existing.bbox[3],
            };
            let overlap = overlap_area(&local, &eb);
            let min_area = (local.width * local.height)
                .min(eb.width * eb.height)
                .max(1.0);
            let overlap_ratio = overlap / min_area;
            let same_region = overlap_ratio >= 0.70;
            let existing_text = existing.text.as_deref().unwrap_or("").trim();
            let same_text =
                !ax_text.is_empty() && !existing_text.is_empty() && ax_text == existing_text;
            let same_text_near = same_text && overlap_ratio >= 0.20;
            if same_region || same_text_near {
                let score = overlap_ratio + if same_text { 0.25 } else { 0.0 };
                if score > replace_score {
                    replace_score = score;
                    replace_idx = Some(idx);
                }
            }
        }

        if let Some(idx) = replace_idx {
            let existing_text = elements[idx].text.clone();
            elements[idx] = ElementBuilder::new()
                .id(ax_primary_id.clone())
                .kind("")
                .bbox(local)
                .has_border(None)
                .text(merged_text.or(existing_text))
                .confidence(None)
                .source(format!("accessibility_ax:{}", ax.role))
                .build();
            metrics.ax_replaced += 1;
            continue;
        }

        elements.push(
            ElementBuilder::new()
                .id(ax_primary_id)
                .kind("")
                .bbox(local)
                .has_border(None)
                .text(merged_text)
                .confidence(None)
                .source(format!("accessibility_ax:{}", ax.role))
                .build(),
        );
        metrics.ax_added += 1;
    }

    metrics
}

pub(crate) fn primary_id_for_ax(ax: &AxElement) -> Option<String> {
    if let Some(identifier) = ax
        .ax_identifier
        .as_deref()
        .and_then(sanitize_ax_id_component)
    {
        return Some(format!("axid_{identifier}"));
    }
    None
}

fn fallback_id_for_ax(ax: &AxElement, counts: &mut HashMap<String, usize>) -> String {
    let role = normalize_ax_role_name(&ax.role);
    let text = ax
        .text
        .as_deref()
        .and_then(sanitize_ax_id_component)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_component(&s, 20));
    let base = match text {
        Some(label) => format!("{role}_{label}"),
        None => role.to_string(),
    };
    let next = counts
        .entry(base.clone())
        .and_modify(|n| *n += 1)
        .or_insert(1usize);
    if *next == 1 {
        base
    } else {
        format!("{base}_{next}")
    }
}

fn normalize_ax_role_name(role: &str) -> &str {
    match role {
        "AXButton" => "button",
        "AXCheckBox" => "checkbox",
        "AXRadioButton" => "radiobutton",
        "AXPopUpButton" => "popup",
        "AXTextField" => "textfield",
        "AXTextArea" => "textarea",
        "AXMenuButton" => "menubutton",
        _ => "element",
    }
}

fn truncate_component(value: &str, max_len: usize) -> String {
    if value.len() <= max_len {
        value.to_string()
    } else {
        value[..max_len].to_string()
    }
}

fn sanitize_ax_id_component(raw: &str) -> Option<String> {
    let mut out = String::new();
    let mut prev_sep = false;
    for ch in raw.trim().chars() {
        let c = ch.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_sep = false;
        } else if !prev_sep {
            out.push('_');
            prev_sep = true;
        }
        if out.len() >= 48 {
            break;
        }
    }
    let normalized = out.trim_matches('_').to_string();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

fn should_prioritize_ax_text_region(role: &str, ax_text: &str) -> bool {
    !ax_text.is_empty() && matches!(role, "AXTextField" | "AXTextArea")
}

fn drop_ocr_text_inside_ax_region(elements: &mut Vec<TokenizeElement>, ax_bounds: &Bounds) {
    elements.retain(|el| {
        if el.source.starts_with("accessibility_ax:") {
            return true;
        }
        let eb = Bounds {
            x: el.bbox[0],
            y: el.bbox[1],
            width: el.bbox[2],
            height: el.bbox[3],
        };
        let overlap = overlap_area(&eb, ax_bounds);
        let ea = (eb.width * eb.height).max(1.0);
        let mostly_inside = overlap / ea >= 0.85;
        let center_in = center_inside(&eb, ax_bounds);
        !(mostly_inside || center_in)
    });
}

fn merged_ax_text(
    role: &str,
    ax_bounds: &Bounds,
    elements: &[TokenizeElement],
    ax_text: Option<&str>,
) -> (Option<String>, bool) {
    let primary = ax_text.and_then(|text| normalize_ax_primary_text(role, text));
    if let Some(text) = primary {
        return (Some(text), false);
    }

    let mut candidates: Vec<(f64, f64, f64, String)> = Vec::new();
    for el in elements {
        if el.source.starts_with("accessibility_ax:") {
            continue;
        }
        let Some(text) = el.text.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) else {
            continue;
        };
        let eb = Bounds {
            x: el.bbox[0],
            y: el.bbox[1],
            width: el.bbox[2],
            height: el.bbox[3],
        };
        let ea = (eb.width * eb.height).max(1.0);
        let overlap_ratio = overlap_area(&eb, ax_bounds) / ea;
        if overlap_ratio < 0.60 && !center_inside(&eb, ax_bounds) {
            continue;
        }
        candidates.push((overlap_ratio, eb.y, eb.x, text.to_string()));
    }

    if candidates.is_empty() {
        return (None, false);
    }
    candidates.sort_by(|a, b| {
        b.0.partial_cmp(&a.0)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a.2.partial_cmp(&b.2).unwrap_or(std::cmp::Ordering::Equal))
    });
    (Some(candidates[0].3.clone()), true)
}

fn normalize_ax_primary_text(role: &str, text: &str) -> Option<String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if is_uninformative_ax_text(role, trimmed) {
        return None;
    }
    Some(trimmed.to_string())
}

fn is_uninformative_ax_text(role: &str, text: &str) -> bool {
    if role != "AXScrollArea" {
        return false;
    }
    matches!(
        text.to_ascii_lowercase().as_str(),
        "input" | "output" | "calculator"
    )
}

fn overlap_area(a: &Bounds, b: &Bounds) -> f64 {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    let iw = (ix2 - ix1).max(0.0);
    let ih = (iy2 - iy1).max(0.0);
    iw * ih
}

fn center_inside(inner: &Bounds, outer: &Bounds) -> bool {
    let cx = inner.x + inner.width * 0.5;
    let cy = inner.y + inner.height * 0.5;
    cx >= outer.x && cx <= outer.x + outer.width && cy >= outer.y && cy <= outer.y + outer.height
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_element(source: &str, text: Option<&str>, bbox: [f64; 4]) -> TokenizeElement {
        TokenizeElement {
            id: String::new(),
            kind: String::new(),
            bbox,
            has_border: None,
            text: text.map(ToString::to_string),
            text_truncated: None,
            confidence: None,
            scrollable: None,
            source: source.to_string(),
        }
    }

    #[test]
    fn prioritizes_ax_text_field_region_over_ocr_fragments() {
        let mut elements = vec![
            make_element("vision_ocr", Some("hello"), [10.0, 10.0, 80.0, 20.0]),
            make_element("vision_ocr", Some("world"), [10.0, 35.0, 70.0, 20.0]),
            make_element("vision_ocr", Some("outside"), [220.0, 220.0, 40.0, 20.0]),
        ];
        let ax_bounds = Bounds {
            x: 0.0,
            y: 0.0,
            width: 120.0,
            height: 80.0,
        };
        assert!(should_prioritize_ax_text_region(
            "AXTextArea",
            "hello world"
        ));
        drop_ocr_text_inside_ax_region(&mut elements, &ax_bounds);
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0].text.as_deref(), Some("outside"));
    }

    #[test]
    fn merge_elements_prefers_ax_text_for_text_area_and_keeps_outside_ocr() {
        let mut elements = vec![
            make_element("vision_ocr", Some("hello"), [10.0, 10.0, 80.0, 20.0]),
            make_element("vision_ocr", Some("world"), [10.0, 35.0, 70.0, 20.0]),
            make_element("vision_ocr", Some("outside"), [220.0, 220.0, 40.0, 20.0]),
        ];
        let ax_elements = vec![AxElement {
            role: "AXTextArea".to_string(),
            text: Some("hello world from ax".to_string()),
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 80.0,
            },
            ax_identifier: None,
        }];
        let coord_map = CoordMap::new(
            Bounds {
                x: 0.0,
                y: 0.0,
                width: 300.0,
                height: 300.0,
            },
            300,
            300,
        );

        let metrics = merge_elements(&mut elements, &ax_elements, &coord_map);
        assert_eq!(metrics.ax_seen, 1);
        assert_eq!(metrics.ax_added, 1);
        assert_eq!(metrics.ax_replaced, 0);
        assert_eq!(metrics.ax_text_filled, 0);

        assert_eq!(elements.len(), 2);
        let outside = elements
            .iter()
            .find(|el| el.text.as_deref() == Some("outside"))
            .expect("outside OCR element should remain");
        assert_eq!(outside.source, "vision_ocr");

        let ax = elements
            .iter()
            .find(|el| el.source == "accessibility_ax:AXTextArea")
            .expect("AX element should be present");
        assert_eq!(ax.text.as_deref(), Some("hello world from ax"));
    }

    #[test]
    fn merge_elements_fills_missing_ax_text_from_overlapping_ocr() {
        let mut elements = vec![make_element(
            "vision_ocr",
            Some("draft"),
            [10.0, 10.0, 80.0, 20.0],
        )];
        let ax_elements = vec![AxElement {
            role: "AXTextField".to_string(),
            text: None,
            bounds: Bounds {
                x: 8.0,
                y: 8.0,
                width: 90.0,
                height: 30.0,
            },
            ax_identifier: None,
        }];
        let coord_map = CoordMap::new(
            Bounds {
                x: 0.0,
                y: 0.0,
                width: 120.0,
                height: 60.0,
            },
            120,
            60,
        );

        let metrics = merge_elements(&mut elements, &ax_elements, &coord_map);
        assert_eq!(metrics.ax_seen, 1);
        assert_eq!(metrics.ax_text_filled, 1);
        assert_eq!(metrics.ax_added, 1);
        assert_eq!(metrics.ax_replaced, 0);

        assert_eq!(elements.len(), 1);
        let only = &elements[0];
        assert_eq!(only.source, "accessibility_ax:AXTextField");
        assert_eq!(only.text.as_deref(), Some("draft"));
        assert!(only.id.starts_with("textfield"));
    }

    #[test]
    fn merge_elements_ignores_generic_scrollarea_label_and_uses_ocr_display() {
        let mut elements = vec![make_element(
            "vision_ocr",
            Some("7777878"),
            [12.0, 12.0, 100.0, 28.0],
        )];
        let ax_elements = vec![AxElement {
            role: "AXScrollArea".to_string(),
            text: Some("Input".to_string()),
            bounds: Bounds {
                x: 8.0,
                y: 8.0,
                width: 120.0,
                height: 40.0,
            },
            ax_identifier: None,
        }];
        let coord_map = CoordMap::new(
            Bounds {
                x: 0.0,
                y: 0.0,
                width: 200.0,
                height: 80.0,
            },
            200,
            80,
        );

        let metrics = merge_elements(&mut elements, &ax_elements, &coord_map);
        assert_eq!(metrics.ax_seen, 1);
        assert_eq!(metrics.ax_text_filled, 1);
        assert_eq!(elements.len(), 1);
        let only = &elements[0];
        assert_eq!(only.source, "accessibility_ax:AXScrollArea");
        assert_eq!(only.text.as_deref(), Some("7777878"));
    }

    #[test]
    fn ax_primary_id_prefers_identifier_over_path() {
        let ax = AxElement {
            role: "AXButton".to_string(),
            text: Some("Save".to_string()),
            bounds: Bounds {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            ax_identifier: Some("SaveButtonMain".to_string()),
        };
        let id = primary_id_for_ax(&ax).expect("id");
        assert_eq!(id, "axid_savebuttonmain");
    }
}

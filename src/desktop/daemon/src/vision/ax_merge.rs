use desktop_core::protocol::{Bounds, TokenizeElement};

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

    for ax in ax_elements {
        metrics.ax_seen += 1;

        let Some(local) = coord_map.logical_to_image_bounds_clamped(&ax.bounds) else {
            metrics.ax_dropped_bounds += 1;
            continue;
        };

        let (merged_text, filled_from_ocr) = merged_ax_text(&local, elements, ax.text.as_deref());
        if filled_from_ocr {
            metrics.ax_text_filled += 1;
        }
        let ax_text = merged_text.as_deref().unwrap_or("").trim();

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

fn merged_ax_text(
    ax_bounds: &Bounds,
    elements: &[TokenizeElement],
    ax_text: Option<&str>,
) -> (Option<String>, bool) {
    let primary = ax_text
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string);
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

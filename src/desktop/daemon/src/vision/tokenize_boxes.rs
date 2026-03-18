use desktop_core::protocol::Bounds;
use image::RgbaImage;

const GLYPH_MIN_W: usize = 6;
const GLYPH_MIN_H: usize = 6;
const GLYPH_MIN_AREA: usize = 24;
const GLYPH_MAX_AREA: usize = 1800;
const GLYPH_MIN_ASPECT: f64 = 0.2;
const GLYPH_MAX_ASPECT: f64 = 5.0;
const GLYPH_MAX_SIZE_PX: f64 = 52.0;
pub(crate) const GLYPH_IOU_TEXT_OVERLAP_MAX: f64 = 0.08;
const GLYPH_DEDUPE_IOU: f64 = 0.72;
const GLYPH_CAP: usize = 140;
const TEXT_ANCHORED_BOX_CAP: usize = 220;
// Sparse/noisy OCR seeds frequently degrade text-anchored boxes on icon-heavy UIs
// (e.g. Calculator/Music toolbar), so fall back to raw geometry when below this count.
const TEXT_SEED_MIN_COUNT: usize = 4;
// Reject implausibly-large OCR seeds (often full-pane OCR noise) before grouping.
const TEXT_SEED_MAX_REL_AREA: f64 = 0.06;

#[allow(dead_code)]
pub fn detect_ui_boxes(image: &RgbaImage) -> Vec<Bounds> {
    detect_ui_boxes_with_text(image, &[])
}

pub fn detect_ui_boxes_with_text(image: &RgbaImage, text_bounds: &[Bounds]) -> Vec<Bounds> {
    let raw = detect_ui_boxes_raw(image);
    if text_bounds.is_empty() {
        return raw;
    }
    let width = image.width() as f64;
    let height = image.height() as f64;
    let clean_text_bounds = sanitize_text_seeds(text_bounds, width, height);
    if !should_use_text_anchors(&clean_text_bounds) {
        return raw;
    }
    let text_groups = group_text_bounds(&clean_text_bounds, width, height);
    if text_groups.is_empty() {
        return raw;
    }
    let mut anchored = text_groups
        .iter()
        .map(|group| local_text_box(group, width, height))
        .collect::<Vec<_>>();
    for group in &text_groups {
        if let Some(candidate) = best_container_for_group(group, &raw, width, height) {
            anchored.push(candidate);
        }
    }
    anchored.extend(multi_text_panel_candidates(
        &raw,
        &text_groups,
        width,
        height,
    ));
    let mut deduped = dedupe_boxes(anchored, 0.84);
    if deduped.len() > TEXT_ANCHORED_BOX_CAP {
        deduped.truncate(TEXT_ANCHORED_BOX_CAP);
    }
    deduped
}

fn sanitize_text_seeds(text_bounds: &[Bounds], image_w: f64, image_h: f64) -> Vec<Bounds> {
    let image_area = (image_w * image_h).max(1.0);
    let mut out: Vec<Bounds> = text_bounds
        .iter()
        .filter_map(|text| clamp_bounds(text, image_w, image_h))
        .filter(|text| {
            let area = text.width * text.height;
            area >= 8.0
                && area <= image_area * TEXT_SEED_MAX_REL_AREA
                && text.width <= image_w * 0.90
                && text.height <= image_h * 0.28
        })
        .collect();
    out = dedupe_boxes(out, 0.96);
    out
}

fn should_use_text_anchors(text_bounds: &[Bounds]) -> bool {
    text_bounds.len() >= TEXT_SEED_MIN_COUNT
}

fn detect_ui_boxes_raw(image: &RgbaImage) -> Vec<Bounds> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width < 24 || height < 24 {
        return Vec::new();
    }

    let gray = grayscale(image);
    let mean_luma = gray.iter().map(|v| *v as f64).sum::<f64>() / (gray.len() as f64);

    let mut boxes = Vec::new();
    boxes.extend(edge_component_boxes(&gray, width, height));
    boxes.extend(text_component_boxes(&gray, width, height, mean_luma));
    boxes.extend(grid_cell_boxes(&gray, width, height));
    let structural_panels = structural_panel_boxes(&gray, width, height);
    boxes.extend(structural_panels.iter().cloned());
    boxes.extend(structural_row_boxes(
        &gray,
        width,
        height,
        &structural_panels,
    ));

    let mut deduped = dedupe_boxes(boxes, 0.88);
    // Keep a broad candidate set for pseudo-label recall, but prevent runaway noise.
    if deduped.len() > 320 {
        deduped.truncate(320);
    }
    deduped
}

pub fn detect_glyphs(image: &RgbaImage, text_bounds: &[Bounds]) -> Vec<Bounds> {
    let width = image.width() as usize;
    let height = image.height() as usize;
    if width < 16 || height < 16 {
        return Vec::new();
    }

    let gray = grayscale(image);
    let mean_luma = gray.iter().map(|v| *v as f64).sum::<f64>() / (gray.len() as f64);
    let mut mask = vec![false; width * height];
    if mean_luma >= 128.0 {
        for i in 0..mask.len() {
            mask[i] = gray[i] <= 110;
        }
    } else {
        for i in 0..mask.len() {
            mask[i] = gray[i] >= 164;
        }
    }
    let mask = dilate_rect(&mask, width, height, 1, 1);
    let mask = erode_rect(&mask, width, height, 1, 1);
    let mut glyphs = connected_component_boxes(
        &mask,
        width,
        height,
        GLYPH_MIN_W,
        GLYPH_MIN_H,
        GLYPH_MIN_AREA,
        GLYPH_MAX_AREA,
        GLYPH_MIN_ASPECT,
        GLYPH_MAX_ASPECT,
    );
    glyphs.retain(|glyph| {
        glyph.width <= GLYPH_MAX_SIZE_PX
            && glyph.height <= GLYPH_MAX_SIZE_PX
            && !overlaps_text(glyph, text_bounds)
            && !inside_text_padding(glyph, text_bounds, 2.0)
    });
    let mut deduped = dedupe_boxes(glyphs, GLYPH_DEDUPE_IOU);
    if deduped.len() > GLYPH_CAP {
        deduped.truncate(GLYPH_CAP);
    }
    deduped
}

fn grayscale(image: &RgbaImage) -> Vec<u8> {
    image
        .pixels()
        .map(|p| {
            let [r, g, b, _a] = p.0;
            (0.299 * (r as f32) + 0.587 * (g as f32) + 0.114 * (b as f32)).round() as u8
        })
        .collect()
}

fn edge_component_boxes(gray: &[u8], width: usize, height: usize) -> Vec<Bounds> {
    let mut grad = vec![0u16; width * height];
    let mut grad_sum: u64 = 0;
    for y in 1..height {
        for x in 1..width {
            let idx = y * width + x;
            let gx = (gray[idx] as i16 - gray[idx - 1] as i16).unsigned_abs();
            let gy = (gray[idx] as i16 - gray[idx - width] as i16).unsigned_abs();
            let g = gx + gy;
            grad[idx] = g;
            grad_sum += g as u64;
        }
    }
    let mean_grad = (grad_sum as f64) / ((width * height).max(1) as f64);
    let threshold = mean_grad.clamp(18.0, 52.0) as u16;
    let mut mask = vec![false; width * height];
    for i in 0..mask.len() {
        mask[i] = grad[i] >= threshold;
    }
    let mask = dilate(&mask, width, height, 1);
    let mask = erode(&mask, width, height, 1);
    connected_component_boxes(
        &mask,
        width,
        height,
        16,
        14,
        160,
        (width * height) / 2,
        0.08,
        24.0,
    )
}

fn text_component_boxes(gray: &[u8], width: usize, height: usize, mean_luma: f64) -> Vec<Bounds> {
    let mut mask = vec![false; width * height];
    if mean_luma >= 128.0 {
        // Light theme: text/icons are mostly darker than background.
        for i in 0..mask.len() {
            mask[i] = gray[i] <= 118;
        }
    } else {
        // Dark theme: text/icons are mostly brighter than background.
        for i in 0..mask.len() {
            mask[i] = gray[i] >= 150;
        }
    }

    // Merge adjacent glyph strokes into word/control-level groups.
    let mask = dilate_rect(&mask, width, height, 6, 2);
    let mask = erode_rect(&mask, width, height, 2, 1);
    connected_component_boxes(
        &mask,
        width,
        height,
        10,
        8,
        70,
        width * height / 6,
        0.2,
        30.0,
    )
}

fn grid_cell_boxes(gray: &[u8], width: usize, height: usize) -> Vec<Bounds> {
    let mut h_edges = vec![0usize; height];
    let mut v_edges = vec![0usize; width];
    for y in 1..height {
        for x in 1..width {
            let idx = y * width + x;
            let gx = (gray[idx] as i16 - gray[idx - 1] as i16).unsigned_abs() as usize;
            let gy = (gray[idx] as i16 - gray[idx - width] as i16).unsigned_abs() as usize;
            if gx + gy >= 34 {
                h_edges[y] += 1;
                v_edges[x] += 1;
            }
        }
    }

    let h_lines = compress_indices(
        &(0..height)
            .filter(|y| h_edges[*y] >= (width as f64 * 0.20) as usize)
            .collect::<Vec<_>>(),
        3,
    );
    let v_lines = compress_indices(
        &(0..width)
            .filter(|x| v_edges[*x] >= (height as f64 * 0.16) as usize)
            .collect::<Vec<_>>(),
        3,
    );
    if h_lines.len() < 6 || v_lines.len() < 4 {
        return Vec::new();
    }

    let mut boxes = Vec::new();
    for y_pair in h_lines.windows(2) {
        if h_edges[y_pair[0]] < (width as f64 * 0.24) as usize
            || h_edges[y_pair[1]] < (width as f64 * 0.24) as usize
        {
            continue;
        }
        let y0 = y_pair[0] as f64;
        let y1 = y_pair[1] as f64;
        let h = y1 - y0;
        if !(28.0..=420.0).contains(&h) {
            continue;
        }
        for x_pair in v_lines.windows(2) {
            if v_edges[x_pair[0]] < (height as f64 * 0.20) as usize
                || v_edges[x_pair[1]] < (height as f64 * 0.20) as usize
            {
                continue;
            }
            let x0 = x_pair[0] as f64;
            let x1 = x_pair[1] as f64;
            let w = x1 - x0;
            if !(28.0..=620.0).contains(&w) {
                continue;
            }
            if (w * h) > (width as f64 * height as f64 * 0.24) {
                continue;
            }
            boxes.push(Bounds {
                x: x0,
                y: y0,
                width: w,
                height: h,
            });
        }
    }
    boxes
}

fn connected_component_boxes(
    mask: &[bool],
    width: usize,
    height: usize,
    min_w: usize,
    min_h: usize,
    min_area: usize,
    max_area: usize,
    min_aspect: f64,
    max_aspect: f64,
) -> Vec<Bounds> {
    let mut visited = vec![false; mask.len()];
    let mut boxes = Vec::new();

    for y in 0..height {
        for x in 0..width {
            let start = y * width + x;
            if visited[start] || !mask[start] {
                continue;
            }

            let mut queue = std::collections::VecDeque::from([(x, y)]);
            visited[start] = true;
            let mut min_x = x;
            let mut max_x = x;
            let mut min_y = y;
            let mut max_y = y;
            let mut pixels = 0usize;

            while let Some((cx, cy)) = queue.pop_front() {
                pixels += 1;
                min_x = min_x.min(cx);
                max_x = max_x.max(cx);
                min_y = min_y.min(cy);
                max_y = max_y.max(cy);

                if cx + 1 < width {
                    let n = cy * width + (cx + 1);
                    if !visited[n] && mask[n] {
                        visited[n] = true;
                        queue.push_back((cx + 1, cy));
                    }
                }
                if cx > 0 {
                    let n = cy * width + (cx - 1);
                    if !visited[n] && mask[n] {
                        visited[n] = true;
                        queue.push_back((cx - 1, cy));
                    }
                }
                if cy + 1 < height {
                    let n = (cy + 1) * width + cx;
                    if !visited[n] && mask[n] {
                        visited[n] = true;
                        queue.push_back((cx, cy + 1));
                    }
                }
                if cy > 0 {
                    let n = (cy - 1) * width + cx;
                    if !visited[n] && mask[n] {
                        visited[n] = true;
                        queue.push_back((cx, cy - 1));
                    }
                }
            }

            let box_w = max_x - min_x + 1;
            let box_h = max_y - min_y + 1;
            let area = box_w * box_h;
            if box_w < min_w || box_h < min_h {
                continue;
            }
            if area < min_area || area > max_area {
                continue;
            }
            let aspect = box_w as f64 / box_h as f64;
            if !(min_aspect..=max_aspect).contains(&aspect) {
                continue;
            }
            if pixels < min_area / 4 {
                continue;
            }
            // Suppress very large sparse outlines that often appear in light-mode split panes.
            if area > (width * height) / 6 && pixels < area / 8 {
                continue;
            }
            boxes.push(Bounds {
                x: min_x as f64,
                y: min_y as f64,
                width: box_w as f64,
                height: box_h as f64,
            });
        }
    }
    boxes
}

fn dilate(mask: &[bool], width: usize, height: usize, radius: usize) -> Vec<bool> {
    dilate_rect(mask, width, height, radius, radius)
}

fn erode(mask: &[bool], width: usize, height: usize, radius: usize) -> Vec<bool> {
    erode_rect(mask, width, height, radius, radius)
}

fn dilate_rect(mask: &[bool], width: usize, height: usize, rx: usize, ry: usize) -> Vec<bool> {
    let mut out = vec![false; mask.len()];
    for y in 0..height {
        for x in 0..width {
            let y0 = y.saturating_sub(ry);
            let y1 = (y + ry).min(height - 1);
            let x0 = x.saturating_sub(rx);
            let x1 = (x + rx).min(width - 1);
            let mut hit = false;
            'outer: for yy in y0..=y1 {
                for xx in x0..=x1 {
                    if mask[yy * width + xx] {
                        hit = true;
                        break 'outer;
                    }
                }
            }
            out[y * width + x] = hit;
        }
    }
    out
}

fn erode_rect(mask: &[bool], width: usize, height: usize, rx: usize, ry: usize) -> Vec<bool> {
    let mut out = vec![false; mask.len()];
    for y in 0..height {
        for x in 0..width {
            let y0 = y.saturating_sub(ry);
            let y1 = (y + ry).min(height - 1);
            let x0 = x.saturating_sub(rx);
            let x1 = (x + rx).min(width - 1);
            let mut all_set = true;
            'outer: for yy in y0..=y1 {
                for xx in x0..=x1 {
                    if !mask[yy * width + xx] {
                        all_set = false;
                        break 'outer;
                    }
                }
            }
            out[y * width + x] = all_set;
        }
    }
    out
}

fn compress_indices(indices: &[usize], max_gap: usize) -> Vec<usize> {
    if indices.is_empty() {
        return Vec::new();
    }
    let mut out = Vec::new();
    let mut run_start = indices[0];
    let mut prev = indices[0];
    for idx in indices.iter().skip(1) {
        if *idx - prev <= max_gap {
            prev = *idx;
            continue;
        }
        out.push((run_start + prev) / 2);
        run_start = *idx;
        prev = *idx;
    }
    out.push((run_start + prev) / 2);
    out
}

fn structural_panel_boxes(gray: &[u8], width: usize, height: usize) -> Vec<Bounds> {
    if width < 320 || height < 220 {
        return Vec::new();
    }

    let toolbar_bottom = infer_toolbar_bottom(gray, width, height);
    let mut v_edges = vec![0usize; width];
    for y in toolbar_bottom.max(1)..height {
        for x in 1..width {
            let idx = y * width + x;
            let gx = (gray[idx] as i16 - gray[idx - 1] as i16).unsigned_abs() as usize;
            if gx >= 24 {
                v_edges[x] += 1;
            }
        }
    }

    let mut split_candidates: Vec<usize> = (0..width)
        .filter(|x| {
            let nx = *x as f64 / width as f64;
            (0.06..0.94).contains(&nx)
                && v_edges[*x] >= ((height - toolbar_bottom) as f64 * 0.58) as usize
        })
        .collect();
    split_candidates = compress_indices(&split_candidates, 5);
    if split_candidates.is_empty() {
        return Vec::new();
    }

    let mut boundaries = vec![0usize];
    boundaries.extend(split_candidates);
    boundaries.push(width - 1);

    let mut panels = Vec::new();
    for pair in boundaries.windows(2) {
        let x0 = pair[0];
        let x1 = pair[1];
        if x1 <= x0 + 8 {
            continue;
        }
        let w = x1 - x0;
        if w < 140 || w > (width as f64 * 0.7) as usize {
            continue;
        }
        panels.push(Bounds {
            x: x0 as f64,
            y: toolbar_bottom as f64,
            width: w as f64,
            height: (height - toolbar_bottom) as f64,
        });
    }
    panels
}

fn structural_row_boxes(
    gray: &[u8],
    width: usize,
    height: usize,
    panels: &[Bounds],
) -> Vec<Bounds> {
    if panels.is_empty() {
        return Vec::new();
    }
    let mut rows = Vec::new();
    for panel in panels {
        let panel_w = panel.width as usize;
        // Focus row proposals on sidebar/list panes; skip broad editor/content panes.
        if panel_w < 180 || panel_w > (width as f64 * 0.42) as usize {
            continue;
        }
        let x0 = panel.x.max(0.0).floor() as usize;
        let x1 = (panel.x + panel.width).min(width as f64).ceil() as usize;
        let y0 = panel.y.max(1.0).floor() as usize;
        let y1 = (panel.y + panel.height).min(height as f64).ceil() as usize;
        if x1 <= x0 + 8 || y1 <= y0 + 8 {
            continue;
        }
        let mut h_edges = vec![0usize; y1 - y0];
        for y in y0.max(1)..y1 {
            let mut hits = 0usize;
            for x in x0..x1 {
                let idx = y * width + x;
                let gy = (gray[idx] as i16 - gray[idx - width] as i16).unsigned_abs() as usize;
                if gy >= 14 {
                    hits += 1;
                }
            }
            h_edges[y - y0] = hits;
        }
        let lines: Vec<usize> = (0..h_edges.len())
            .filter(|i| h_edges[*i] >= ((x1 - x0) as f64 * 0.33) as usize)
            .map(|i| y0 + i)
            .collect();
        let lines = compress_indices(&lines, 2);
        if lines.len() < 2 {
            continue;
        }
        for pair in lines.windows(2) {
            let yy0 = pair[0];
            let yy1 = pair[1];
            if yy1 <= yy0 + 4 {
                continue;
            }
            let h = yy1 - yy0;
            if !(20..=120).contains(&h) {
                continue;
            }
            rows.push(Bounds {
                x: x0 as f64 + 2.0,
                y: yy0 as f64,
                width: (x1 - x0).saturating_sub(4) as f64,
                height: h as f64,
            });
        }
    }
    rows
}

fn infer_toolbar_bottom(gray: &[u8], width: usize, height: usize) -> usize {
    let probe_max = height.min(260);
    let mut best_y = (height as f64 * 0.08) as usize;
    for y in 2..probe_max {
        let mut hits = 0usize;
        for x in 0..width {
            let idx = y * width + x;
            let gy = (gray[idx] as i16 - gray[idx - width] as i16).unsigned_abs() as usize;
            if gy >= 18 {
                hits += 1;
            }
        }
        if hits >= (width as f64 * 0.45) as usize {
            best_y = y;
            break;
        }
    }
    best_y.clamp(40, height.saturating_sub(1))
}

fn group_text_bounds(text_bounds: &[Bounds], image_w: f64, image_h: f64) -> Vec<Bounds> {
    let normalized: Vec<Bounds> = text_bounds
        .iter()
        .filter_map(|text| clamp_bounds(text, image_w, image_h))
        .collect();
    if normalized.is_empty() {
        return Vec::new();
    }
    let n = normalized.len();
    let mut parent: Vec<usize> = (0..n).collect();

    fn find(parent: &mut [usize], idx: usize) -> usize {
        if parent[idx] != idx {
            let root = find(parent, parent[idx]);
            parent[idx] = root;
        }
        parent[idx]
    }

    fn union(parent: &mut [usize], a: usize, b: usize) {
        let ra = find(parent, a);
        let rb = find(parent, b);
        if ra != rb {
            parent[rb] = ra;
        }
    }

    for i in 0..n {
        for j in (i + 1)..n {
            if text_boxes_connected(&normalized[i], &normalized[j], image_w, image_h) {
                union(&mut parent, i, j);
            }
        }
    }

    let mut groups = std::collections::HashMap::<usize, Bounds>::new();
    for (idx, item) in normalized.iter().enumerate() {
        let root = find(&mut parent, idx);
        groups
            .entry(root)
            .and_modify(|acc| *acc = union_bounds(acc, item))
            .or_insert_with(|| item.clone());
    }

    let mut out: Vec<Bounds> = groups.into_values().collect();
    out.retain(|group| group.width >= 2.0 && group.height >= 2.0);
    out = dedupe_boxes(out, 0.92);
    out
}

fn text_boxes_connected(a: &Bounds, b: &Bounds, image_w: f64, image_h: f64) -> bool {
    let pad_a = (a.height * 0.50).clamp(2.0, 14.0);
    let pad_b = (b.height * 0.50).clamp(2.0, 14.0);
    let ea = expand_bounds(a, pad_a, pad_a, image_w, image_h);
    let eb = expand_bounds(b, pad_b, pad_b, image_w, image_h);
    rect_intersects(&ea, &eb)
}

fn local_text_box(group: &Bounds, image_w: f64, image_h: f64) -> Bounds {
    let pad_x = (group.height * 0.70).clamp(4.0, 20.0);
    let pad_y = (group.height * 0.45).clamp(3.0, 16.0);
    expand_bounds(group, pad_x, pad_y, image_w, image_h)
}

fn best_container_for_group(
    group: &Bounds,
    candidates: &[Bounds],
    image_w: f64,
    image_h: f64,
) -> Option<Bounds> {
    let group_area = (group.width * group.height).max(1.0);
    let expansion_cap = (group.height * 1.10).clamp(8.0, 20.0);
    let mut best: Option<(f64, Bounds)> = None;

    for candidate in candidates {
        if candidate.width < 2.0 || candidate.height < 2.0 {
            continue;
        }
        if !rect_contains(candidate, group, 1.0) {
            continue;
        }
        let left = (group.x - candidate.x).max(0.0);
        let right = ((candidate.x + candidate.width) - (group.x + group.width)).max(0.0);
        let top = (group.y - candidate.y).max(0.0);
        let bottom = ((candidate.y + candidate.height) - (group.y + group.height)).max(0.0);
        let max_side = left.max(right).max(top).max(bottom);
        let row_like = is_row_like(candidate, group, image_w);
        let panel_like = is_panel_like(candidate, group, image_w, image_h);
        if max_side > expansion_cap && !row_like && !panel_like {
            continue;
        }
        let extra_area = (candidate.width * candidate.height) / group_area;
        let mean_expand = (left + right + top + bottom) / 4.0;
        let mut score = extra_area + (mean_expand / expansion_cap).min(4.0);
        // Row/panel relaxers allow candidates beyond strict expansion caps,
        // but we still mildly penalize broad structural regions to prefer tighter control boxes.
        if row_like {
            score += 0.30;
        }
        if panel_like {
            score += 0.80;
        }
        match &best {
            Some((best_score, _)) if *best_score <= score => {}
            _ => best = Some((score, candidate.clone())),
        }
    }
    best.map(|(_, candidate)| candidate)
}

fn multi_text_panel_candidates(
    candidates: &[Bounds],
    groups: &[Bounds],
    image_w: f64,
    image_h: f64,
) -> Vec<Bounds> {
    let mut out = Vec::new();
    for candidate in candidates {
        if candidate.width <= image_w * 0.52
            && candidate.height >= image_h * 0.20
            && candidate.height <= image_h * 0.98
        {
            let covered = groups
                .iter()
                .filter(|group| {
                    let cx = group.x + group.width / 2.0;
                    let cy = group.y + group.height / 2.0;
                    point_in_bounds(candidate, cx, cy)
                })
                .count();
            if covered >= 3 {
                out.push(candidate.clone());
            }
        }
    }
    out
}

fn is_row_like(candidate: &Bounds, group: &Bounds, image_w: f64) -> bool {
    let gh = group.height.max(1.0);
    candidate.height >= gh * 1.3
        && candidate.height <= gh * 6.2
        && candidate.width >= group.width * 1.4
        && candidate.width <= image_w * 0.68
        && candidate.height <= 140.0
}

fn is_panel_like(candidate: &Bounds, group: &Bounds, image_w: f64, image_h: f64) -> bool {
    candidate.width >= group.width * 1.2
        && candidate.width <= image_w * 0.48
        && candidate.height >= image_h * 0.20
        && candidate.height <= image_h * 0.96
}

fn union_bounds(a: &Bounds, b: &Bounds) -> Bounds {
    let x1 = a.x.min(b.x);
    let y1 = a.y.min(b.y);
    let x2 = (a.x + a.width).max(b.x + b.width);
    let y2 = (a.y + a.height).max(b.y + b.height);
    Bounds {
        x: x1,
        y: y1,
        width: (x2 - x1).max(0.0),
        height: (y2 - y1).max(0.0),
    }
}

fn expand_bounds(bounds: &Bounds, pad_x: f64, pad_y: f64, image_w: f64, image_h: f64) -> Bounds {
    let x1 = (bounds.x - pad_x).clamp(0.0, image_w.max(1.0));
    let y1 = (bounds.y - pad_y).clamp(0.0, image_h.max(1.0));
    let x2 = (bounds.x + bounds.width + pad_x).clamp(0.0, image_w.max(1.0));
    let y2 = (bounds.y + bounds.height + pad_y).clamp(0.0, image_h.max(1.0));
    Bounds {
        x: x1,
        y: y1,
        width: (x2 - x1).max(0.0),
        height: (y2 - y1).max(0.0),
    }
}

fn clamp_bounds(bounds: &Bounds, image_w: f64, image_h: f64) -> Option<Bounds> {
    let x1 = bounds.x.clamp(0.0, image_w.max(1.0));
    let y1 = bounds.y.clamp(0.0, image_h.max(1.0));
    let x2 = (bounds.x + bounds.width).clamp(0.0, image_w.max(1.0));
    let y2 = (bounds.y + bounds.height).clamp(0.0, image_h.max(1.0));
    let width = (x2 - x1).max(0.0);
    let height = (y2 - y1).max(0.0);
    if width < 2.0 || height < 2.0 {
        return None;
    }
    Some(Bounds {
        x: x1,
        y: y1,
        width,
        height,
    })
}

fn rect_contains(outer: &Bounds, inner: &Bounds, tolerance: f64) -> bool {
    outer.x <= inner.x + tolerance
        && outer.y <= inner.y + tolerance
        && outer.x + outer.width >= inner.x + inner.width - tolerance
        && outer.y + outer.height >= inner.y + inner.height - tolerance
}

fn rect_intersects(a: &Bounds, b: &Bounds) -> bool {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;
    ax2 >= b.x && bx2 >= a.x && ay2 >= b.y && by2 >= a.y
}

fn point_in_bounds(bounds: &Bounds, x: f64, y: f64) -> bool {
    x >= bounds.x && x <= bounds.x + bounds.width && y >= bounds.y && y <= bounds.y + bounds.height
}

fn iou(a: &Bounds, b: &Bounds) -> f64 {
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
    let inter = iw * ih;
    if inter <= 0.0 {
        return 0.0;
    }
    let union = (a.width * a.height) + (b.width * b.height) - inter;
    if union <= 0.0 { 0.0 } else { inter / union }
}

fn dedupe_boxes(mut boxes: Vec<Bounds>, iou_threshold: f64) -> Vec<Bounds> {
    boxes.sort_by(|a, b| {
        let aa = a.width * a.height;
        let bb = b.width * b.height;
        bb.partial_cmp(&aa).unwrap_or(std::cmp::Ordering::Equal)
    });

    let mut kept = Vec::new();
    'next: for candidate in boxes {
        if candidate.width < 2.0 || candidate.height < 2.0 {
            continue;
        }
        for existing in &kept {
            if iou(&candidate, existing) >= iou_threshold {
                continue 'next;
            }
        }
        kept.push(candidate);
    }
    kept.sort_by(|a, b| {
        a.y.partial_cmp(&b.y)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal))
    });
    kept
}

fn overlaps_text(candidate: &Bounds, texts: &[Bounds]) -> bool {
    texts
        .iter()
        .any(|text| iou(candidate, text) >= GLYPH_IOU_TEXT_OVERLAP_MAX)
}

fn inside_text_padding(candidate: &Bounds, texts: &[Bounds], padding: f64) -> bool {
    // Centroid gating drops small icon fragments still "inside" text labels even with tiny IoU.
    let cx = candidate.x + (candidate.width / 2.0);
    let cy = candidate.y + (candidate.height / 2.0);
    texts.iter().any(|text| {
        cx >= text.x - padding
            && cx <= text.x + text.width + padding
            && cy >= text.y - padding
            && cy <= text.y + text.height + padding
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_text_bounds_merges_close_lines_only() {
        let text = vec![
            Bounds {
                x: 100.0,
                y: 100.0,
                width: 80.0,
                height: 16.0,
            },
            Bounds {
                x: 184.0,
                y: 101.0,
                width: 64.0,
                height: 16.0,
            },
            Bounds {
                x: 102.0,
                y: 170.0,
                width: 92.0,
                height: 16.0,
            },
        ];
        let groups = group_text_bounds(&text, 800.0, 600.0);
        assert_eq!(
            groups.len(),
            2,
            "first line should merge, distant line separate"
        );
    }

    #[test]
    fn best_container_prefers_row_over_large_panel() {
        let group = Bounds {
            x: 120.0,
            y: 220.0,
            width: 90.0,
            height: 18.0,
        };
        let row_candidate = Bounds {
            x: 92.0,
            y: 205.0,
            width: 320.0,
            height: 52.0,
        };
        let panel_candidate = Bounds {
            x: 0.0,
            y: 100.0,
            width: 430.0,
            height: 520.0,
        };
        let chosen = best_container_for_group(
            &group,
            &[panel_candidate.clone(), row_candidate.clone()],
            1200.0,
            900.0,
        )
        .expect("expected a candidate");
        assert!(
            iou(&chosen, &row_candidate) > 0.80,
            "row candidate should win over broad panel candidate"
        );
    }

    #[test]
    fn local_text_box_respects_image_bounds() {
        let group = Bounds {
            x: 4.0,
            y: 5.0,
            width: 20.0,
            height: 12.0,
        };
        let local = local_text_box(&group, 40.0, 30.0);
        assert!(local.x >= 0.0 && local.y >= 0.0);
        assert!(local.x + local.width <= 40.0);
        assert!(local.y + local.height <= 30.0);
    }

    #[test]
    fn text_anchor_gate_rejects_sparse_or_oversized_seed() {
        let seeds = vec![
            Bounds {
                x: 0.0,
                y: 0.0,
                width: 350.0,
                height: 1402.0,
            },
            Bounds {
                x: 991.0,
                y: 28.0,
                width: 36.0,
                height: 47.0,
            },
            Bounds {
                x: 1943.0,
                y: 29.0,
                width: 197.0,
                height: 67.0,
            },
            Bounds {
                x: 535.0,
                y: 39.0,
                width: 104.0,
                height: 25.0,
            },
        ];
        let clean = sanitize_text_seeds(&seeds, 2440.0, 1402.0);
        assert!(
            clean.len() < TEXT_SEED_MIN_COUNT,
            "sparse/noisy seeds should fail text-anchor gate"
        );
        assert!(!should_use_text_anchors(&clean));
    }

    #[test]
    fn text_anchor_gate_accepts_notes_like_seed() {
        let seeds = vec![
            Bounds {
                x: 90.0,
                y: 158.0,
                width: 82.0,
                height: 22.0,
            },
            Bounds {
                x: 94.0,
                y: 246.0,
                width: 126.0,
                height: 20.0,
            },
            Bounds {
                x: 94.0,
                y: 294.0,
                width: 80.0,
                height: 20.0,
            },
            Bounds {
                x: 94.0,
                y: 342.0,
                width: 70.0,
                height: 20.0,
            },
            Bounds {
                x: 488.0,
                y: 213.0,
                width: 228.0,
                height: 24.0,
            },
            Bounds {
                x: 488.0,
                y: 324.0,
                width: 204.0,
                height: 22.0,
            },
        ];
        let clean = sanitize_text_seeds(&seeds, 2000.0, 1312.0);
        assert!(should_use_text_anchors(&clean));
    }
}

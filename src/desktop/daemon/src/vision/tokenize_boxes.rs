use desktop_core::protocol::Bounds;
use image::RgbaImage;

const GLYPH_MIN_W: usize = 6;
const GLYPH_MIN_H: usize = 6;
const GLYPH_MIN_AREA: usize = 24;
const GLYPH_MAX_AREA: usize = 1800;
const GLYPH_MIN_ASPECT: f64 = 0.2;
const GLYPH_MAX_ASPECT: f64 = 5.0;
const GLYPH_MAX_SIZE_PX: f64 = 44.0;
pub(crate) const GLYPH_IOU_TEXT_OVERLAP_MAX: f64 = 0.08;
const GLYPH_DEDUPE_IOU: f64 = 0.72;
const GLYPH_CAP: usize = 140;
const TEXT_ANCHORED_BOX_CAP: usize = 220;
const RAW_BOX_CAP: usize = 180;
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
    let gray = grayscale(image);
    let image_w_px = image.width() as usize;
    let image_h_px = image.height() as usize;
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
    anchored.extend(aligned_list_candidates(&text_groups, &raw, width, height));
    anchored.extend(text_control_candidates(
        &gray,
        image_w_px,
        image_h_px,
        &text_groups,
        &raw,
    ));
    let mut deduped = dedupe_boxes(anchored, 0.84);
    deduped = prune_nested_glitch_boxes(deduped);
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
    boxes.extend(text_row_and_paragraph_boxes(&gray, width, height, &boxes));

    let mut deduped = dedupe_boxes(boxes, 0.86);
    deduped = prune_nested_glitch_boxes(deduped);
    // Keep a broad candidate set for pseudo-label recall, but prevent runaway noise.
    if deduped.len() > RAW_BOX_CAP {
        deduped.truncate(RAW_BOX_CAP);
    }
    deduped
}

fn text_row_and_paragraph_boxes(
    gray: &[u8],
    width: usize,
    height: usize,
    seed_boxes: &[Bounds],
) -> Vec<Bounds> {
    let words: Vec<Bounds> = seed_boxes
        .iter()
        .filter(|b| {
            b.width >= 12.0
                && b.width <= 460.0
                && b.height >= 10.0
                && b.height <= 44.0
                && (b.width * b.height) <= 14000.0
                && (b.width / b.height.max(1.0)) <= 22.0
        })
        .cloned()
        .collect();
    if words.len() < 3 {
        return Vec::new();
    }

    let line_components = cluster_bounds(&words, |a, b| {
        let min_h = a.height.min(b.height).max(1.0);
        let vertical_overlap = overlap_1d(a.y, a.y + a.height, b.y, b.y + b.height);
        let vertical_ratio = vertical_overlap / min_h;
        let hgap = axis_gap(a.x, a.x + a.width, b.x, b.x + b.width);
        vertical_ratio >= 0.40 && hgap <= (min_h * 2.0).clamp(12.0, 64.0)
    });

    let mut line_boxes = Vec::new();
    for line in line_components {
        if line.is_empty() {
            continue;
        }
        let merged = line
            .iter()
            .skip(1)
            .fold(line[0].clone(), |acc, b| union_bounds(&acc, b));
        if line.len() == 1 && merged.width < 88.0 {
            continue;
        }
        line_boxes.push(expand_bounds(
            &merged,
            (merged.height * 0.26).clamp(2.0, 10.0),
            (merged.height * 0.16).clamp(1.0, 6.0),
            width as f64,
            height as f64,
        ));
    }

    let para_components = cluster_bounds(&line_boxes, |a, b| {
        let min_h = a.height.min(b.height).max(1.0);
        let vgap = axis_gap(a.y, a.y + a.height, b.y, b.y + b.height);
        if vgap > (min_h * 2.1).clamp(10.0, 42.0) {
            return false;
        }
        let hoverlap = overlap_1d(a.x, a.x + a.width, b.x, b.x + b.width);
        let min_w = a.width.min(b.width).max(1.0);
        let hoverlap_ratio = hoverlap / min_w;
        let left_align = (a.x - b.x).abs() <= (min_h * 1.6).clamp(8.0, 26.0);
        hoverlap_ratio >= 0.22 || left_align
    });

    let mut out = Vec::new();
    out.extend(line_boxes.iter().cloned());
    for paragraph in para_components {
        if paragraph.len() < 2 {
            continue;
        }
        let merged = paragraph
            .iter()
            .skip(1)
            .fold(paragraph[0].clone(), |acc, b| union_bounds(&acc, b));
        if merged.width < 120.0 || merged.height < 28.0 {
            continue;
        }
        out.push(expand_bounds(
            &merged,
            (merged.height * 0.20).clamp(4.0, 12.0),
            (merged.height * 0.14).clamp(3.0, 10.0),
            width as f64,
            height as f64,
        ));
    }

    for line in &line_boxes {
        let cx = line.x + line.width / 2.0;
        let cy = line.y + line.height / 2.0;
        let mut best: Option<(f64, Bounds)> = None;
        for (sx, sy) in [(1.6, 1.6), (2.3, 1.9), (3.2, 2.1)] {
            let candidate = expand_from_center(line, cx, cy, sx, sy, width as f64, height as f64);
            if candidate.width < 70.0 || candidate.width > width as f64 * 0.75 {
                continue;
            }
            if candidate.height < 22.0 || candidate.height > 88.0 {
                continue;
            }
            let border = rect_border_energy(gray, width, height, &candidate);
            let inner = rect_inner_energy(gray, width, height, &candidate);
            let score = border - (inner * 0.68);
            if score < 8.8 {
                continue;
            }
            match &best {
                Some((best_score, _)) if *best_score >= score => {}
                _ => best = Some((score, candidate)),
            }
        }
        if let Some((_, c)) = best {
            out.push(c);
        }
    }
    dedupe_boxes(out, 0.86)
}

fn cluster_bounds<F>(items: &[Bounds], mut connected: F) -> Vec<Vec<Bounds>>
where
    F: FnMut(&Bounds, &Bounds) -> bool,
{
    if items.is_empty() {
        return Vec::new();
    }
    let n = items.len();
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
            if connected(&items[i], &items[j]) {
                union(&mut parent, i, j);
            }
        }
    }

    let mut groups = std::collections::HashMap::<usize, Vec<Bounds>>::new();
    for (idx, item) in items.iter().enumerate() {
        let root = find(&mut parent, idx);
        groups.entry(root).or_default().push(item.clone());
    }
    let mut out: Vec<Vec<Bounds>> = groups.into_values().collect();
    out.sort_by_key(|group| group.len());
    out
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
    // Opening pass suppresses isolated JPEG-like speckles before CC extraction.
    let mask = erode_rect(&mask, width, height, 1, 1);
    let mask = dilate_rect(&mask, width, height, 1, 1);
    // Mild closing keeps icon strokes connected after opening.
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
            && near_text(glyph, text_bounds)
            && mask_fill_density(&mask, width, glyph) >= 0.16
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
    let min_h = a.height.min(b.height).max(1.0);
    let min_w = a.width.min(b.width).max(1.0);
    let vertical_overlap = overlap_1d(a.y, a.y + a.height, b.y, b.y + b.height);
    let horizontal_overlap = overlap_1d(a.x, a.x + a.width, b.x, b.x + b.width);
    let vertical_overlap_ratio = vertical_overlap / min_h;
    let horizontal_overlap_ratio = horizontal_overlap / min_w;
    let horizontal_gap = axis_gap(a.x, a.x + a.width, b.x, b.x + b.width);
    let vertical_gap = axis_gap(a.y, a.y + a.height, b.y, b.y + b.height);

    // Word-level grouping on the same baseline.
    let same_line_gap = (min_h * 2.4).clamp(10.0, 72.0);
    if vertical_overlap_ratio >= 0.45 && horizontal_gap <= same_line_gap {
        return true;
    }

    // Paragraph-level grouping across nearby lines.
    let multiline_gap = (min_h * 1.8).clamp(8.0, 42.0);
    if horizontal_overlap_ratio >= 0.35 && vertical_gap <= multiline_gap {
        return true;
    }

    // Small geometric fallback for near-miss OCR boxes.
    let pad_a = (a.height * 0.80).clamp(3.0, 20.0);
    let pad_b = (b.height * 0.80).clamp(3.0, 20.0);
    let ea = expand_bounds(a, pad_a, pad_a, image_w, image_h);
    let eb = expand_bounds(b, pad_b, pad_b, image_w, image_h);
    rect_intersects(&ea, &eb)
}

fn overlap_1d(a1: f64, a2: f64, b1: f64, b2: f64) -> f64 {
    (a2.min(b2) - a1.max(b1)).max(0.0)
}

fn axis_gap(a1: f64, a2: f64, b1: f64, b2: f64) -> f64 {
    if a2 < b1 {
        b1 - a2
    } else if b2 < a1 {
        a1 - b2
    } else {
        0.0
    }
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

fn aligned_list_candidates(
    groups: &[Bounds],
    candidates: &[Bounds],
    image_w: f64,
    image_h: f64,
) -> Vec<Bounds> {
    if groups.len() < 4 {
        return Vec::new();
    }
    let mut rows: Vec<Bounds> = groups
        .iter()
        .filter(|g| g.width >= 8.0 && g.height >= 8.0)
        .cloned()
        .collect();
    if rows.len() < 4 {
        return Vec::new();
    }
    rows.sort_by(|a, b| {
        a.x.partial_cmp(&b.x)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
    });

    let avg_h = rows.iter().map(|r| r.height).sum::<f64>() / rows.len() as f64;
    let left_tol = (avg_h * 0.9).clamp(8.0, 24.0);
    let mut clusters: Vec<Vec<Bounds>> = Vec::new();
    for row in rows {
        if let Some(cluster) = clusters.iter_mut().find(|cluster| {
            let cx = cluster.iter().map(|r| r.x).sum::<f64>() / cluster.len() as f64;
            (row.x - cx).abs() <= left_tol
        }) {
            cluster.push(row);
        } else {
            clusters.push(vec![row]);
        }
    }

    let mut out = Vec::new();
    for mut cluster in clusters {
        if cluster.len() < 4 {
            continue;
        }
        cluster.sort_by(|a, b| a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal));

        let min_left = cluster.iter().map(|r| r.x).fold(f64::INFINITY, f64::min);
        let max_left = cluster
            .iter()
            .map(|r| r.x)
            .fold(f64::NEG_INFINITY, f64::max);
        if (max_left - min_left) > left_tol * 1.2 {
            continue;
        }

        let centers: Vec<f64> = cluster.iter().map(|r| r.y + (r.height / 2.0)).collect();
        let gaps: Vec<f64> = centers.windows(2).map(|w| (w[1] - w[0]).max(0.0)).collect();
        if gaps.len() < 3 {
            continue;
        }
        let mean_gap = gaps.iter().sum::<f64>() / gaps.len() as f64;
        if mean_gap <= 2.0 {
            continue;
        }
        let var_gap = gaps
            .iter()
            .map(|g| {
                let d = *g - mean_gap;
                d * d
            })
            .sum::<f64>()
            / gaps.len() as f64;
        let std_gap = var_gap.sqrt();
        let cv_gap = std_gap / mean_gap.max(1.0);
        let min_gap = gaps.iter().copied().fold(f64::INFINITY, f64::min);
        let max_gap = gaps.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        if cv_gap > 0.42 || min_gap < mean_gap * 0.34 || max_gap > mean_gap * 1.85 {
            continue;
        }

        let list_union = cluster
            .iter()
            .skip(1)
            .fold(cluster[0].clone(), |acc, row| union_bounds(&acc, row));
        let span_h = list_union.height;
        if span_h < avg_h * 2.5 {
            continue;
        }
        let padded = expand_bounds(
            &list_union,
            (avg_h * 1.2).clamp(6.0, 26.0),
            (avg_h * 0.9).clamp(4.0, 20.0),
            image_w,
            image_h,
        );
        if let Some(best) = best_list_container(&padded, &cluster, candidates, image_w) {
            out.push(best);
        } else {
            out.push(padded);
        }
    }
    dedupe_boxes(out, 0.88)
}

fn best_list_container(
    list_bounds: &Bounds,
    rows: &[Bounds],
    candidates: &[Bounds],
    image_w: f64,
) -> Option<Bounds> {
    let mut best: Option<(f64, Bounds)> = None;
    for candidate in candidates {
        if candidate.width < 2.0 || candidate.height < 2.0 {
            continue;
        }
        if candidate.width > image_w * 0.75 {
            continue;
        }
        if !rect_contains(candidate, list_bounds, 1.0) {
            continue;
        }
        let covered = rows
            .iter()
            .filter(|row| {
                let cx = row.x + (row.width / 2.0);
                let cy = row.y + (row.height / 2.0);
                point_in_bounds(candidate, cx, cy)
            })
            .count();
        if covered < rows.len().saturating_sub(1) {
            continue;
        }
        let area = (candidate.width * candidate.height).max(1.0);
        match &best {
            Some((best_area, _)) if *best_area <= area => {}
            _ => best = Some((area, candidate.clone())),
        }
    }
    best.map(|(_, c)| c)
}

fn text_control_candidates(
    gray: &[u8],
    width: usize,
    height: usize,
    groups: &[Bounds],
    raw_candidates: &[Bounds],
) -> Vec<Bounds> {
    if groups.is_empty() || width < 32 || height < 32 {
        return Vec::new();
    }
    let mut out = Vec::new();
    let image_w = width as f64;
    let image_h = height as f64;
    for group in groups {
        if group.width < 22.0 || group.width > 520.0 || group.height < 10.0 || group.height > 42.0 {
            continue;
        }

        let cx = group.x + group.width / 2.0;
        let cy = group.y + group.height / 2.0;

        // Prefer a raw control candidate when it already contains text with plausible field padding.
        let mut best_raw: Option<(f64, Bounds)> = None;
        for candidate in raw_candidates {
            if !rect_contains(candidate, group, 1.0) {
                continue;
            }
            if candidate.height < 20.0
                || candidate.height > (group.height * 4.2).clamp(28.0, 110.0)
                || candidate.width > image_w * 0.86
                || candidate.width < (group.width + 14.0).max(46.0)
            {
                continue;
            }
            let edge_contrast = control_edge_contrast(gray, width, height, candidate);
            let padding_ratio = horizontal_padding_ratio(candidate, group);
            let aspect = candidate.width / candidate.height.max(1.0);
            if edge_contrast < 7.4 || padding_ratio < 0.18 || aspect < 1.8 {
                continue;
            }
            let oversize_penalty = ((candidate.width / group.width.max(1.0)) - 6.0).max(0.0) * 2.4;
            let score = edge_contrast + (padding_ratio * 9.0) - oversize_penalty;
            match &best_raw {
                Some((best_score, _)) if *best_score >= score => {}
                _ => best_raw = Some((score, candidate.clone())),
            }
        }

        // If raw candidates already include a strong field boundary, avoid synthesizing duplicates.
        if best_raw
            .as_ref()
            .is_some_and(|(_, c)| is_good_existing_text_field_candidate(c, group))
        {
            out.push(best_raw.expect("checked is_some").1);
            continue;
        }

        let mut best_synth: Option<(f64, Bounds)> = None;
        for (sx, sy) in [(2.2, 1.8), (3.2, 2.0), (4.4, 2.2), (5.8, 2.4), (7.0, 2.7)] {
            let candidate = expand_from_center(group, cx, cy, sx, sy, image_w, image_h);
            if candidate.width < (group.width + 18.0).max(50.0)
                || candidate.width > image_w.min(group.width * 7.8 + 52.0)
            {
                continue;
            }
            if candidate.height < 20.0 || candidate.height > 88.0 {
                continue;
            }
            if (candidate.width / candidate.height.max(1.0)) < 1.8 {
                continue;
            }
            let edge_contrast = control_edge_contrast(gray, width, height, &candidate);
            let padding_ratio = horizontal_padding_ratio(&candidate, group);
            if edge_contrast < 7.4 || padding_ratio < 0.18 {
                continue;
            }
            let border = rect_border_energy(gray, width, height, &candidate);
            let inner = rect_inner_energy(gray, width, height, &candidate);
            let score = edge_contrast + (padding_ratio * 9.0) - ((inner / 28.0).min(1.8));
            // Prefer candidates where most border energy is concentrated on horizontal edges,
            // a common signal for text inputs/search fields.
            let horizontal_border_bias = border / (inner + 1.0);
            let final_score = score + horizontal_border_bias.min(2.4);
            match &best_synth {
                Some((best_score, _)) if *best_score >= final_score => {}
                _ => best_synth = Some((final_score, candidate)),
            }
        }
        if let Some((score, candidate)) =
            edge_scan_text_field_candidate(gray, width, height, group, image_w, image_h)
        {
            match &best_synth {
                Some((best_score, _)) if *best_score >= score => {}
                _ => best_synth = Some((score, candidate)),
            }
        }

        let chosen = match (best_raw, best_synth) {
            (Some((raw_score, raw)), Some((synth_score, synth))) => {
                if raw_score >= synth_score - 0.8 {
                    Some(raw)
                } else {
                    Some(synth)
                }
            }
            (Some((_, raw)), None) => Some(raw),
            (None, Some((_, synth))) => Some(synth),
            (None, None) => None,
        };
        if let Some(candidate) = chosen {
            let widened =
                maybe_widen_control_candidate(gray, width, height, &candidate, group, image_w);
            let padded = maybe_enforce_toolbar_field_padding(
                gray, width, height, &widened, group, image_w, image_h,
            );
            out.push(padded);
        }
    }
    dedupe_boxes(out, 0.88)
}

fn edge_scan_text_field_candidate(
    gray: &[u8],
    width: usize,
    height: usize,
    group: &Bounds,
    image_w: f64,
    image_h: f64,
) -> Option<(f64, Bounds)> {
    let gx1 = group.x.max(1.0).floor() as usize;
    let gy1 = group.y.max(1.0).floor() as usize;
    let gx2 = (group.x + group.width).min(image_w - 1.0).ceil() as usize;
    let gy2 = (group.y + group.height).min(image_h - 1.0).ceil() as usize;
    if gx2 <= gx1 + 4 || gy2 <= gy1 + 3 {
        return None;
    }

    let side_span = (group.height * 9.0).clamp(48.0, 220.0) as usize;
    let v_span = (group.height * 2.8).clamp(12.0, 42.0) as usize;
    let y0 = gy1.saturating_sub((group.height * 0.8).clamp(4.0, 16.0) as usize);
    let y1 = (gy2 + (group.height * 0.9).clamp(6.0, 22.0) as usize).min(height.saturating_sub(1));
    if y1 <= y0 + 2 {
        return None;
    }

    let left_start = gx1.saturating_sub(side_span);
    let left_end = gx1.saturating_sub(2);
    let right_start = (gx2 + 2).min(width.saturating_sub(2));
    let right_end = (gx2 + side_span).min(width.saturating_sub(2));
    if left_end <= left_start || right_end <= right_start {
        return None;
    }

    let mut best_left: Option<(f64, usize)> = None;
    for x in left_start..=left_end {
        let s = col_edge_strength(gray, width, height, x, y0, y1);
        if s < 5.8 {
            continue;
        }
        match best_left {
            Some((best_s, _)) if best_s >= s => {}
            _ => best_left = Some((s, x)),
        }
    }
    let mut best_right: Option<(f64, usize)> = None;
    for x in right_start..=right_end {
        let s = col_edge_strength(gray, width, height, x, y0, y1);
        if s < 5.8 {
            continue;
        }
        match best_right {
            Some((best_s, _)) if best_s >= s => {}
            _ => best_right = Some((s, x)),
        }
    }
    let (left_score, left_x) = best_left?;
    let (right_score, right_x) = best_right?;
    if right_x <= left_x + 20 {
        return None;
    }

    let x0 = left_x.max(1);
    let x1 = right_x.min(width.saturating_sub(2));
    if x1 <= x0 + 20 {
        return None;
    }

    let top_start = gy1.saturating_sub(v_span);
    let top_end = gy1.saturating_sub(1);
    let bottom_start = (gy2 + 1).min(height.saturating_sub(2));
    let bottom_end = (gy2 + v_span).min(height.saturating_sub(2));
    if top_end <= top_start || bottom_end <= bottom_start {
        return None;
    }

    let mut best_top: Option<(f64, usize)> = None;
    for y in top_start..=top_end {
        let s = row_edge_strength(gray, width, height, y, x0, x1);
        if s < 5.0 {
            continue;
        }
        match best_top {
            Some((best_s, _)) if best_s >= s => {}
            _ => best_top = Some((s, y)),
        }
    }
    let mut best_bottom: Option<(f64, usize)> = None;
    for y in bottom_start..=bottom_end {
        let s = row_edge_strength(gray, width, height, y, x0, x1);
        if s < 5.0 {
            continue;
        }
        match best_bottom {
            Some((best_s, _)) if best_s >= s => {}
            _ => best_bottom = Some((s, y)),
        }
    }
    let (top_score, top_y) = best_top?;
    let (bottom_score, bottom_y) = best_bottom?;
    if bottom_y <= top_y + 8 {
        return None;
    }

    let candidate = Bounds {
        x: x0 as f64,
        y: top_y as f64,
        width: (x1 - x0 + 1) as f64,
        height: (bottom_y - top_y + 1) as f64,
    };
    if !rect_contains(&candidate, group, 1.0) {
        return None;
    }
    if candidate.height < 20.0 || candidate.height > 92.0 {
        return None;
    }
    if candidate.width < (group.width + 14.0).max(48.0)
        || candidate.width > image_w.min(group.width * 8.0 + 62.0)
    {
        return None;
    }
    let aspect = candidate.width / candidate.height.max(1.0);
    if aspect < 1.8 {
        return None;
    }
    let edge_contrast = control_edge_contrast(gray, width, height, &candidate);
    let padding_ratio = horizontal_padding_ratio(&candidate, group);
    if edge_contrast < 6.8 || padding_ratio < 0.20 {
        return None;
    }
    let side_pair_score = (left_score + right_score) * 0.35;
    let top_bottom_score = (top_score + bottom_score) * 0.25;
    let score = edge_contrast + (padding_ratio * 8.0) + side_pair_score + top_bottom_score;
    Some((score, candidate))
}

fn col_edge_strength(
    gray: &[u8],
    width: usize,
    _height: usize,
    x: usize,
    y0: usize,
    y1: usize,
) -> f64 {
    if x == 0 || x + 1 >= width || y1 <= y0 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for y in y0..=y1 {
        let idx = y * width + x;
        let left = gray[idx - 1] as f64;
        let center = gray[idx] as f64;
        let right = gray[idx + 1] as f64;
        sum += (center - left).abs() + (center - right).abs();
        count += 1;
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

fn row_edge_strength(
    gray: &[u8],
    width: usize,
    height: usize,
    y: usize,
    x0: usize,
    x1: usize,
) -> f64 {
    if y == 0 || y + 1 >= height || x1 <= x0 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    for x in x0..=x1 {
        let idx = y * width + x;
        let up = gray[idx - width] as f64;
        let center = gray[idx] as f64;
        let down = gray[idx + width] as f64;
        sum += (center - up).abs() + (center - down).abs();
        count += 1;
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

fn maybe_widen_control_candidate(
    gray: &[u8],
    width: usize,
    height: usize,
    candidate: &Bounds,
    group: &Bounds,
    image_w: f64,
) -> Bounds {
    let base_pad = horizontal_padding_ratio(candidate, group);
    if base_pad >= 0.28 || candidate.width >= image_w * 0.85 {
        return candidate.clone();
    }
    let x1 = candidate.x.max(1.0).floor() as usize;
    let x2 = (candidate.x + candidate.width).min(image_w - 1.0).ceil() as usize;
    let y0 = candidate.y.max(1.0).floor() as usize;
    let y1 = (candidate.y + candidate.height)
        .min(height as f64 - 1.0)
        .ceil() as usize;
    if x2 <= x1 + 6 || y1 <= y0 + 2 {
        return candidate.clone();
    }
    let probe_y0 = (y0 + 1).min(height.saturating_sub(2));
    let probe_y1 = y1.saturating_sub(1).min(height.saturating_sub(2));
    if probe_y1 <= probe_y0 + 1 {
        return candidate.clone();
    }

    let max_extra = (group.height * 3.5).clamp(16.0, 96.0) as usize;
    let left_start = x1.saturating_sub(max_extra);
    let left_end = x1.saturating_sub(1);
    let right_start = (x2 + 1).min(width.saturating_sub(2));
    let right_end = (x2 + max_extra).min(width.saturating_sub(2));
    if left_end <= left_start || right_end <= right_start {
        return candidate.clone();
    }

    let mut best_left: Option<(f64, usize)> = None;
    for x in left_start..=left_end {
        let dist = (x1.saturating_sub(x)) as f64;
        let s = col_edge_strength(gray, width, height, x, probe_y0, probe_y1) + (dist * 0.04);
        if s < 4.0 {
            continue;
        }
        match best_left {
            Some((best_s, _)) if best_s >= s => {}
            _ => best_left = Some((s, x)),
        }
    }
    let mut best_right: Option<(f64, usize)> = None;
    for x in right_start..=right_end {
        let dist = (x.saturating_sub(x2)) as f64;
        let s = col_edge_strength(gray, width, height, x, probe_y0, probe_y1) + (dist * 0.04);
        if s < 4.0 {
            continue;
        }
        match best_right {
            Some((best_s, _)) if best_s >= s => {}
            _ => best_right = Some((s, x)),
        }
    }
    let Some((_, left_x)) = best_left else {
        return candidate.clone();
    };
    let Some((_, right_x)) = best_right else {
        return candidate.clone();
    };
    if right_x <= left_x + 24 {
        return candidate.clone();
    }

    let widened = Bounds {
        x: left_x as f64,
        y: candidate.y,
        width: (right_x.saturating_sub(left_x) + 1) as f64,
        height: candidate.height,
    };
    if !rect_contains(&widened, group, 1.0) {
        return candidate.clone();
    }
    if widened.width > image_w * 0.86 {
        return candidate.clone();
    }
    let widened_pad = horizontal_padding_ratio(&widened, group);
    if widened_pad < base_pad + 0.08 {
        return candidate.clone();
    }
    widened
}

fn maybe_enforce_toolbar_field_padding(
    _gray: &[u8],
    _width: usize,
    _height: usize,
    candidate: &Bounds,
    group: &Bounds,
    image_w: f64,
    image_h: f64,
) -> Bounds {
    if group.width < 160.0 || group.height > 34.0 {
        return candidate.clone();
    }
    if group.y > image_h * 0.22 || candidate.height > 64.0 {
        return candidate.clone();
    }
    let cur_total_pad = (candidate.width - group.width).max(0.0);
    let target_total_pad = (group.width * 0.28)
        .max(group.height * 2.4)
        .clamp(38.0, 160.0);
    if cur_total_pad >= target_total_pad {
        return candidate.clone();
    }
    let needed = (target_total_pad - cur_total_pad).max(0.0);
    let expand_each = (needed / 2.0).clamp(8.0, 48.0);
    let expanded = Bounds {
        x: (candidate.x - expand_each).max(0.0),
        y: candidate.y,
        width: ((candidate.x + candidate.width + expand_each).min(image_w)
            - (candidate.x - expand_each).max(0.0))
        .max(0.0),
        height: candidate.height,
    };
    if expanded.width > image_w * 0.90 {
        return candidate.clone();
    }
    if !rect_contains(&expanded, group, 1.0) {
        return candidate.clone();
    }
    expanded
}

fn horizontal_padding_ratio(candidate: &Bounds, group: &Bounds) -> f64 {
    let left = (group.x - candidate.x).max(0.0);
    let right = ((candidate.x + candidate.width) - (group.x + group.width)).max(0.0);
    (left + right) / group.width.max(1.0)
}

fn is_good_existing_text_field_candidate(candidate: &Bounds, group: &Bounds) -> bool {
    if !rect_contains(candidate, group, 1.0) {
        return false;
    }
    let left = (group.x - candidate.x).max(0.0);
    let right = ((candidate.x + candidate.width) - (group.x + group.width)).max(0.0);
    let top = (group.y - candidate.y).max(0.0);
    let bottom = ((candidate.y + candidate.height) - (group.y + group.height)).max(0.0);
    let total_h = left + right;
    let total_v = top + bottom;
    let min_total_h = (group.height * 0.95).clamp(8.0, 26.0);
    let min_total_v = (group.height * 0.20).clamp(2.0, 10.0);
    let aspect = candidate.width / candidate.height.max(1.0);
    total_h >= min_total_h
        && total_v >= min_total_v
        && candidate.width >= group.width + min_total_h
        && candidate.height >= (group.height * 1.10)
        && aspect >= 1.8
}

fn control_edge_contrast(gray: &[u8], width: usize, height: usize, rect: &Bounds) -> f64 {
    let border = rect_border_energy(gray, width, height, rect);
    let inner = rect_inner_energy(gray, width, height, rect);
    border - (inner * 0.68)
}

fn expand_from_center(
    seed: &Bounds,
    cx: f64,
    cy: f64,
    scale_x: f64,
    scale_y: f64,
    image_w: f64,
    image_h: f64,
) -> Bounds {
    let target_w = (seed.width * scale_x).max(seed.width + 24.0);
    let target_h = (seed.height * scale_y).max(seed.height + 8.0);
    let x1 = (cx - target_w / 2.0).clamp(0.0, image_w.max(1.0));
    let y1 = (cy - target_h / 2.0).clamp(0.0, image_h.max(1.0));
    let x2 = (cx + target_w / 2.0).clamp(0.0, image_w.max(1.0));
    let y2 = (cy + target_h / 2.0).clamp(0.0, image_h.max(1.0));
    Bounds {
        x: x1,
        y: y1,
        width: (x2 - x1).max(0.0),
        height: (y2 - y1).max(0.0),
    }
}

fn rect_border_energy(gray: &[u8], width: usize, height: usize, rect: &Bounds) -> f64 {
    let (x1, y1, x2, y2) = quantize_rect(rect, width, height);
    if x2 <= x1 + 2 || y2 <= y1 + 2 {
        return 0.0;
    }
    let mut sum = 0.0;
    let mut count = 0usize;
    let top = y1;
    let bottom = y2 - 1;
    for x in x1 + 1..x2 - 1 {
        sum += pixel_grad(gray, width, height, x, top);
        sum += pixel_grad(gray, width, height, x, bottom);
        count += 2;
    }
    let left = x1;
    let right = x2 - 1;
    for y in y1 + 1..y2 - 1 {
        sum += pixel_grad(gray, width, height, left, y);
        sum += pixel_grad(gray, width, height, right, y);
        count += 2;
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

fn rect_inner_energy(gray: &[u8], width: usize, height: usize, rect: &Bounds) -> f64 {
    let (x1, y1, x2, y2) = quantize_rect(rect, width, height);
    if x2 <= x1 + 6 || y2 <= y1 + 6 {
        return 0.0;
    }
    let ix1 = x1 + 2;
    let iy1 = y1 + 2;
    let ix2 = x2 - 2;
    let iy2 = y2 - 2;
    let mut sum = 0.0;
    let mut count = 0usize;
    for y in iy1..iy2 {
        for x in ix1..ix2 {
            sum += pixel_grad(gray, width, height, x, y);
            count += 1;
        }
    }
    if count == 0 { 0.0 } else { sum / count as f64 }
}

fn quantize_rect(rect: &Bounds, width: usize, height: usize) -> (usize, usize, usize, usize) {
    let x1 = rect.x.max(0.0).floor() as usize;
    let y1 = rect.y.max(0.0).floor() as usize;
    let x2 = (rect.x + rect.width).min(width as f64).ceil() as usize;
    let y2 = (rect.y + rect.height).min(height as f64).ceil() as usize;
    (x1.min(width), y1.min(height), x2.min(width), y2.min(height))
}

fn pixel_grad(gray: &[u8], width: usize, height: usize, x: usize, y: usize) -> f64 {
    if width == 0 || height == 0 {
        return 0.0;
    }
    let xm = x.saturating_sub(1);
    let xp = (x + 1).min(width - 1);
    let ym = y.saturating_sub(1);
    let yp = (y + 1).min(height - 1);
    let gx = (gray[y * width + xp] as f64 - gray[y * width + xm] as f64).abs();
    let gy = (gray[yp * width + x] as f64 - gray[ym * width + x] as f64).abs();
    gx + gy
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

fn prune_nested_glitch_boxes(mut boxes: Vec<Bounds>) -> Vec<Bounds> {
    boxes.sort_by(|a, b| {
        let aa = a.width * a.height;
        let bb = b.width * b.height;
        aa.partial_cmp(&bb).unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut kept: Vec<Bounds> = Vec::new();
    'next: for candidate in boxes {
        for existing in &kept {
            if is_immediately_nested(existing, &candidate) {
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

fn is_immediately_nested(inner: &Bounds, outer: &Bounds) -> bool {
    if !rect_contains(outer, inner, 1.0) {
        return false;
    }
    let inner_area = (inner.width * inner.height).max(1.0);
    let outer_area = (outer.width * outer.height).max(1.0);
    let ratio = inner_area / outer_area;
    if ratio < 0.70 {
        return false;
    }
    let left = (inner.x - outer.x).max(0.0);
    let right = ((outer.x + outer.width) - (inner.x + inner.width)).max(0.0);
    let top = (inner.y - outer.y).max(0.0);
    let bottom = ((outer.y + outer.height) - (inner.y + inner.height)).max(0.0);
    let max_gap = left.max(right).max(top).max(bottom);
    max_gap <= (inner.height * 0.95).clamp(8.0, 26.0)
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

fn near_text(candidate: &Bounds, texts: &[Bounds]) -> bool {
    if texts.is_empty() {
        return false;
    }
    let cx = candidate.x + (candidate.width / 2.0);
    let cy = candidate.y + (candidate.height / 2.0);
    texts.iter().any(|text| {
        let margin = (text.height * 2.4).clamp(18.0, 120.0);
        let x1 = text.x - margin;
        let y1 = text.y - margin;
        let x2 = text.x + text.width + margin;
        let y2 = text.y + text.height + margin;
        cx >= x1 && cx <= x2 && cy >= y1 && cy <= y2
    })
}

fn mask_fill_density(mask: &[bool], width: usize, bounds: &Bounds) -> f64 {
    if width == 0 {
        return 0.0;
    }
    let height = mask.len() / width;
    if height == 0 {
        return 0.0;
    }
    let x1 = bounds.x.floor().max(0.0) as usize;
    let y1 = bounds.y.floor().max(0.0) as usize;
    let x2 = (bounds.x + bounds.width).ceil().min(width as f64) as usize;
    let y2 = (bounds.y + bounds.height).ceil().min(height as f64) as usize;
    if x2 <= x1 || y2 <= y1 {
        return 0.0;
    }
    let mut on = 0usize;
    for y in y1..y2 {
        let row = y * width;
        for x in x1..x2 {
            if mask[row + x] {
                on += 1;
            }
        }
    }
    let area = (x2 - x1) * (y2 - y1);
    on as f64 / area.max(1) as f64
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
    fn group_text_bounds_merges_word_boxes_into_phrase() {
        let text = vec![
            Bounds {
                x: 40.0,
                y: 48.0,
                width: 56.0,
                height: 18.0,
            },
            Bounds {
                x: 122.0,
                y: 49.0,
                width: 72.0,
                height: 18.0,
            },
            Bounds {
                x: 220.0,
                y: 47.0,
                width: 65.0,
                height: 19.0,
            },
        ];
        let groups = group_text_bounds(&text, 640.0, 240.0);
        assert_eq!(
            groups.len(),
            1,
            "same-line words should merge into one phrase"
        );
        let merged = &groups[0];
        assert!(merged.width >= 240.0, "merged phrase should span all words");
    }

    #[test]
    fn group_text_bounds_merges_multiline_paragraph_blocks() {
        let text = vec![
            Bounds {
                x: 60.0,
                y: 90.0,
                width: 220.0,
                height: 20.0,
            },
            Bounds {
                x: 68.0,
                y: 123.0,
                width: 240.0,
                height: 21.0,
            },
            Bounds {
                x: 420.0,
                y: 40.0,
                width: 80.0,
                height: 18.0,
            },
        ];
        let groups = group_text_bounds(&text, 900.0, 500.0);
        assert_eq!(
            groups.len(),
            2,
            "paragraph lines should merge; distant label separate"
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
    fn aligned_list_candidates_detect_uniform_left_aligned_rows() {
        let groups = vec![
            Bounds {
                x: 72.0,
                y: 120.0,
                width: 120.0,
                height: 20.0,
            },
            Bounds {
                x: 74.0,
                y: 168.0,
                width: 132.0,
                height: 19.0,
            },
            Bounds {
                x: 71.0,
                y: 216.0,
                width: 125.0,
                height: 20.0,
            },
            Bounds {
                x: 73.0,
                y: 264.0,
                width: 136.0,
                height: 19.0,
            },
        ];
        let list = aligned_list_candidates(&groups, &[], 1280.0, 800.0);
        assert!(
            !list.is_empty(),
            "expected aligned list container candidate"
        );
        let union = groups
            .iter()
            .skip(1)
            .fold(groups[0].clone(), |acc, row| super::union_bounds(&acc, row));
        assert!(
            list.iter()
                .any(|candidate| super::rect_contains(candidate, &union, 3.0)),
            "at least one list candidate should contain grouped rows"
        );
    }

    #[test]
    fn prune_nested_glitch_boxes_discards_immediate_outer_duplicate() {
        let inner = Bounds {
            x: 102.0,
            y: 84.0,
            width: 280.0,
            height: 88.0,
        };
        let outer = Bounds {
            x: 94.0,
            y: 76.0,
            width: 296.0,
            height: 104.0,
        };
        let kept = prune_nested_glitch_boxes(vec![outer.clone(), inner.clone()]);
        assert_eq!(
            kept.len(),
            1,
            "expected one nested glitch box to be dropped"
        );
        assert!(
            iou(&kept[0], &inner) > 0.95,
            "smaller inner container should be retained"
        );
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

    #[test]
    fn text_row_and_paragraph_boxes_merge_fragmented_words() {
        let width = 520usize;
        let height = 320usize;
        let mut gray = vec![230u8; width * height];
        // Draw a simple input-like border around the first line.
        for x in 40..360 {
            gray[60 * width + x] = 110;
            gray[94 * width + x] = 110;
        }
        for y in 60..95 {
            gray[y * width + 40] = 110;
            gray[y * width + 359] = 110;
        }
        let seed = vec![
            Bounds {
                x: 72.0,
                y: 68.0,
                width: 42.0,
                height: 16.0,
            },
            Bounds {
                x: 126.0,
                y: 68.0,
                width: 58.0,
                height: 16.0,
            },
            Bounds {
                x: 197.0,
                y: 69.0,
                width: 38.0,
                height: 16.0,
            },
            Bounds {
                x: 70.0,
                y: 122.0,
                width: 108.0,
                height: 18.0,
            },
            Bounds {
                x: 186.0,
                y: 123.0,
                width: 116.0,
                height: 18.0,
            },
        ];
        let merged = text_row_and_paragraph_boxes(&gray, width, height, &seed);
        assert!(
            merged.iter().any(|b| b.width >= 240.0 && b.height <= 60.0),
            "expected at least one merged line/control-sized box"
        );
        assert!(
            merged.iter().any(|b| b.width >= 250.0 && b.height >= 50.0),
            "expected paragraph-level merged candidate"
        );
    }

    #[test]
    fn text_control_candidates_expand_past_tight_raw_box() {
        let width = 420usize;
        let height = 180usize;
        let mut gray = vec![235u8; width * height];
        // Draw a low-contrast input border.
        for x in 92..350 {
            gray[48 * width + x] = 128;
            gray[82 * width + x] = 128;
        }
        for y in 48..83 {
            gray[y * width + 92] = 128;
            gray[y * width + 349] = 128;
        }

        let text_group = Bounds {
            x: 188.0,
            y: 58.0,
            width: 72.0,
            height: 16.0,
        };
        let too_tight_raw = Bounds {
            x: 186.0,
            y: 56.0,
            width: 76.0,
            height: 20.0,
        };
        let out = text_control_candidates(
            &gray,
            width,
            height,
            &[text_group.clone()],
            &[too_tight_raw],
        );
        assert!(!out.is_empty(), "expected synthesized control candidate");
        assert!(
            out.iter().any(|candidate| {
                rect_contains(candidate, &text_group, 1.0)
                    && candidate.width >= 150.0
                    && candidate.height >= 24.0
                    && candidate.x <= 130.0
                    && (candidate.x + candidate.width) >= 320.0
            }),
            "expected expanded field boundary candidate; got {out:?}"
        );
    }

    #[test]
    fn text_control_candidates_keep_good_raw_field_box() {
        let width = 420usize;
        let height = 180usize;
        let mut gray = vec![235u8; width * height];
        for x in 92..350 {
            gray[48 * width + x] = 124;
            gray[82 * width + x] = 124;
        }
        for y in 48..83 {
            gray[y * width + 92] = 124;
            gray[y * width + 349] = 124;
        }
        let text_group = Bounds {
            x: 188.0,
            y: 58.0,
            width: 72.0,
            height: 16.0,
        };
        let good_raw = Bounds {
            x: 92.0,
            y: 48.0,
            width: 258.0,
            height: 34.0,
        };
        let out = text_control_candidates(&gray, width, height, &[text_group], &[good_raw.clone()]);
        assert!(!out.is_empty(), "expected a control candidate");
        assert!(
            out.iter()
                .any(|candidate| iou(candidate, &good_raw) >= 0.70),
            "expected raw field boundary to be retained; got {out:?}"
        );
    }

    #[test]
    fn text_control_candidates_detect_low_contrast_boundary() {
        let width = 420usize;
        let height = 180usize;
        let mut gray = vec![232u8; width * height];
        // Very low contrast input field border.
        for x in 110..332 {
            gray[58 * width + x] = 150;
            gray[88 * width + x] = 150;
        }
        for y in 58..89 {
            gray[y * width + 110] = 150;
            gray[y * width + 331] = 150;
        }

        let text_group = Bounds {
            x: 198.0,
            y: 66.0,
            width: 56.0,
            height: 15.0,
        };
        let out = text_control_candidates(&gray, width, height, &[text_group.clone()], &[]);
        assert!(
            !out.is_empty(),
            "expected low-contrast text-field candidate"
        );
        let candidate = out
            .iter()
            .find(|candidate| rect_contains(candidate, &text_group, 1.0))
            .cloned()
            .expect("expected candidate containing text group");
        assert!(
            candidate.width >= 190.0 && candidate.height >= 24.0,
            "candidate too small: {candidate:?}"
        );
    }

    #[test]
    fn maybe_widen_control_candidate_expands_horizontally_when_padding_too_small() {
        let width = 520usize;
        let height = 220usize;
        let mut gray = vec![230u8; width * height];
        // Input border with significant horizontal padding around OCR text.
        for x in 120..420 {
            gray[70 * width + x] = 126;
            gray[102 * width + x] = 126;
        }
        for y in 70..103 {
            gray[y * width + 120] = 126;
            gray[y * width + 419] = 126;
        }
        let group = Bounds {
            x: 186.0,
            y: 78.0,
            width: 176.0,
            height: 16.0,
        };
        let narrow = Bounds {
            x: 165.0,
            y: 72.0,
            width: 220.0,
            height: 30.0,
        };
        let widened =
            maybe_widen_control_candidate(&gray, width, height, &narrow, &group, width as f64);
        assert!(
            widened.width >= narrow.width + 28.0,
            "expected widening for insufficient text padding: {widened:?}"
        );
        assert!(rect_contains(&widened, &group, 1.0));
    }

    #[test]
    fn maybe_enforce_toolbar_field_padding_widens_top_search_candidate() {
        let width = 1600usize;
        let height = 900usize;
        let gray = vec![126u8; width * height];
        let group = Bounds {
            x: 1116.0,
            y: 41.0,
            width: 268.0,
            height: 27.0,
        };
        let candidate = Bounds {
            x: 1098.0,
            y: 30.0,
            width: 305.0,
            height: 51.0,
        };
        let out = maybe_enforce_toolbar_field_padding(
            &gray,
            width,
            height,
            &candidate,
            &group,
            width as f64,
            height as f64,
        );
        assert!(
            out.width > candidate.width,
            "top toolbar search candidate should widen"
        );
        assert!(rect_contains(&out, &group, 1.0));
    }
}

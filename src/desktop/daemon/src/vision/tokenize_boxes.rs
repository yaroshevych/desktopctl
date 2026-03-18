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

pub fn detect_ui_boxes(image: &RgbaImage) -> Vec<Bounds> {
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
    boxes.extend(structural_row_boxes(&gray, width, height, &structural_panels));

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
            (0.06..0.94).contains(&nx) && v_edges[*x] >= ((height - toolbar_bottom) as f64 * 0.58) as usize
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

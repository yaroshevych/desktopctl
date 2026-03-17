use desktop_core::protocol::Bounds;
use image::RgbaImage;

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

    let mut deduped = dedupe_boxes(boxes, 0.88);
    // Keep a broad candidate set for pseudo-label recall, but prevent runaway noise.
    if deduped.len() > 320 {
        deduped.truncate(320);
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
    if h_lines.len() < 4 || v_lines.len() < 3 {
        return Vec::new();
    }

    let mut boxes = Vec::new();
    for y_pair in h_lines.windows(2) {
        let y0 = y_pair[0] as f64;
        let y1 = y_pair[1] as f64;
        let h = y1 - y0;
        if !(28.0..=420.0).contains(&h) {
            continue;
        }
        for x_pair in v_lines.windows(2) {
            let x0 = x_pair[0] as f64;
            let x1 = x_pair[1] as f64;
            let w = x1 - x0;
            if !(28.0..=620.0).contains(&w) {
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

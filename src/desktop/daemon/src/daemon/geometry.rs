pub(super) fn merge_bounds(
    existing: Option<&desktop_core::protocol::Bounds>,
    incoming: &desktop_core::protocol::Bounds,
) -> desktop_core::protocol::Bounds {
    let Some(existing) = existing else {
        return incoming.clone();
    };
    let x1 = existing.x.min(incoming.x);
    let y1 = existing.y.min(incoming.y);
    let x2 = (existing.x + existing.width).max(incoming.x + incoming.width);
    let y2 = (existing.y + existing.height).max(incoming.y + incoming.height);
    desktop_core::protocol::Bounds {
        x: x1,
        y: y1,
        width: (x2 - x1).max(1.0),
        height: (y2 - y1).max(1.0),
    }
}

pub(super) fn iou(a: &desktop_core::protocol::Bounds, b: &desktop_core::protocol::Bounds) -> f64 {
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

pub(super) fn bounds_intersect(
    a: &desktop_core::protocol::Bounds,
    b: &desktop_core::protocol::Bounds,
) -> bool {
    let ax2 = a.x + a.width;
    let ay2 = a.y + a.height;
    let bx2 = b.x + b.width;
    let by2 = b.y + b.height;
    let ix1 = a.x.max(b.x);
    let iy1 = a.y.max(b.y);
    let ix2 = ax2.min(bx2);
    let iy2 = ay2.min(by2);
    (ix2 - ix1) > 0.0 && (iy2 - iy1) > 0.0
}

pub(super) fn inflate_bounds(
    bounds: &desktop_core::protocol::Bounds,
    pad: f64,
) -> desktop_core::protocol::Bounds {
    desktop_core::protocol::Bounds {
        x: (bounds.x - pad).max(0.0),
        y: (bounds.y - pad).max(0.0),
        width: bounds.width + pad * 2.0,
        height: bounds.height + pad * 2.0,
    }
}

use desktop_core::protocol::Bounds;

#[derive(Debug, Clone)]
pub struct CoordMap {
    pub logical_window_bounds: Bounds,
    pub image_width: u32,
    pub image_height: u32,
}

impl CoordMap {
    pub fn new(logical_window_bounds: Bounds, image_width: u32, image_height: u32) -> Self {
        Self {
            logical_window_bounds,
            image_width,
            image_height,
        }
    }

    pub fn logical_to_image_bounds_clamped(&self, bounds: &Bounds) -> Option<Bounds> {
        if self.image_width == 0 || self.image_height == 0 {
            return None;
        }
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return None;
        }
        if self.logical_window_bounds.width <= 0.0 || self.logical_window_bounds.height <= 0.0 {
            return None;
        }

        let sx = self.image_width as f64 / self.logical_window_bounds.width;
        let sy = self.image_height as f64 / self.logical_window_bounds.height;
        let local_x = bounds.x - self.logical_window_bounds.x;
        let local_y = bounds.y - self.logical_window_bounds.y;

        let x0 = (local_x * sx).floor();
        let y0 = (local_y * sy).floor();
        let x1 = ((local_x + bounds.width) * sx).ceil();
        let y1 = ((local_y + bounds.height) * sy).ceil();

        let x0 = x0.clamp(0.0, self.image_width as f64);
        let y0 = y0.clamp(0.0, self.image_height as f64);
        let x1 = x1.clamp(0.0, self.image_width as f64);
        let y1 = y1.clamp(0.0, self.image_height as f64);

        let width = x1 - x0;
        let height = y1 - y0;
        if width <= 1.0 || height <= 1.0 {
            return None;
        }

        Some(Bounds {
            x: x0,
            y: y0,
            width,
            height,
        })
    }

    pub fn logical_to_image_rect_u32(&self, bounds: &Bounds) -> Option<(u32, u32, u32, u32)> {
        let mapped = self.logical_to_image_bounds_clamped(bounds)?;
        Some((
            mapped.x as u32,
            mapped.y as u32,
            mapped.width as u32,
            mapped.height as u32,
        ))
    }
}

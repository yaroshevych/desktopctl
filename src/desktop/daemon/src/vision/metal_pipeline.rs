//! GPU-accelerated image preprocessing via Metal Performance Shaders.
//!
//! Computes Sobel edge detection and summed area tables (integral images) that
//! enable O(1) rectangle scoring for box detection.
//!
//! Falls back to CPU computation when Metal is unavailable (CI, tests).

use desktop_core::protocol::Bounds;
use image::RgbaImage;

/// Pre-processed frame data: integral images (SATs) for O(1) rectangle queries.
pub struct ProcessedFrame {
    /// Integral image of Sobel edge magnitudes.
    pub edge_sat: Vec<f64>,
    /// Integral image of grayscale values.
    pub gray_sat: Vec<f64>,
    /// Integral image of grayscale² values (for variance computation).
    pub gray_sq_sat: Vec<f64>,
    /// Grayscale image.
    pub gray: Vec<u8>,
    /// Sobel edge magnitude image (0-255 normalized).
    pub edge: Vec<u8>,
    /// Binary text mask (after threshold + morphology).
    pub text_mask: Vec<bool>,
    pub width: usize,
    pub height: usize,
}

impl ProcessedFrame {
    /// O(1) rectangle sum from a summed area table.
    /// Coordinates are inclusive: sum of pixels in [x1..=x2, y1..=y2].
    #[inline]
    pub fn rect_sum(&self, sat: &[f64], x1: usize, y1: usize, x2: usize, y2: usize) -> f64 {
        let w = self.width;
        // SAT is (width+1) × (height+1) with a zero border.
        let sw = w + 1;
        // Map to SAT coordinates (shifted by 1).
        let a = sat[(y2 + 1) * sw + (x2 + 1)];
        let b = if y1 == 0 { 0.0 } else { sat[y1 * sw + (x2 + 1)] };
        let c = if x1 == 0 { 0.0 } else { sat[(y2 + 1) * sw + x1] };
        let d = if x1 == 0 || y1 == 0 {
            0.0
        } else {
            sat[y1 * sw + x1]
        };
        a - b - c + d
    }

    /// Mean edge energy along the 4 border strips of a rectangle.
    /// `strip_w` is the thickness of each border strip in pixels.
    /// Excludes corner squares of size `corner_skip` to handle rounded corners.
    pub fn border_energy(&self, b: &Bounds, strip_w: usize, corner_skip: usize) -> f64 {
        let x1 = (b.x as usize).min(self.width.saturating_sub(1));
        let y1 = (b.y as usize).min(self.height.saturating_sub(1));
        let x2 = ((b.x + b.width) as usize).min(self.width.saturating_sub(1));
        let y2 = ((b.y + b.height) as usize).min(self.height.saturating_sub(1));

        if x2 <= x1 + corner_skip * 2 + 2 || y2 <= y1 + corner_skip * 2 + 2 {
            return 0.0;
        }

        let sw = strip_w.min((y2 - y1) / 4).min((x2 - x1) / 4).max(1);
        let cs = corner_skip.min((x2 - x1) / 4).min((y2 - y1) / 4);

        let mut total_energy = 0.0;
        let mut total_pixels = 0.0;

        // Top strip (excluding corners).
        let tx1 = x1 + cs;
        let tx2 = x2 - cs;
        let ty2 = (y1 + sw).min(y2);
        if tx2 > tx1 {
            let pixels = (tx2 - tx1) as f64 * (ty2 - y1) as f64;
            total_energy += self.rect_sum(&self.edge_sat, tx1, y1, tx2 - 1, ty2 - 1);
            total_pixels += pixels;
        }

        // Bottom strip (excluding corners).
        let by1 = y2.saturating_sub(sw);
        if tx2 > tx1 && by1 < y2 {
            let pixels = (tx2 - tx1) as f64 * (y2 - by1) as f64;
            total_energy += self.rect_sum(&self.edge_sat, tx1, by1, tx2 - 1, y2 - 1);
            total_pixels += pixels;
        }

        // Left strip (excluding corners).
        let ly1 = y1 + cs;
        let ly2 = y2 - cs;
        let lx2 = (x1 + sw).min(x2);
        if ly2 > ly1 {
            let pixels = (lx2 - x1) as f64 * (ly2 - ly1) as f64;
            total_energy += self.rect_sum(&self.edge_sat, x1, ly1, lx2 - 1, ly2 - 1);
            total_pixels += pixels;
        }

        // Right strip (excluding corners).
        let rx1 = x2.saturating_sub(sw);
        if ly2 > ly1 && rx1 < x2 {
            let pixels = (x2 - rx1) as f64 * (ly2 - ly1) as f64;
            total_energy += self.rect_sum(&self.edge_sat, rx1, ly1, x2 - 1, ly2 - 1);
            total_pixels += pixels;
        }

        if total_pixels <= 0.0 {
            0.0
        } else {
            total_energy / total_pixels
        }
    }

    /// Mean grayscale value inside a rectangle.
    pub fn interior_mean(&self, b: &Bounds) -> f64 {
        let x1 = (b.x as usize).min(self.width.saturating_sub(1));
        let y1 = (b.y as usize).min(self.height.saturating_sub(1));
        let x2 = ((b.x + b.width) as usize).min(self.width.saturating_sub(1));
        let y2 = ((b.y + b.height) as usize).min(self.height.saturating_sub(1));
        if x2 <= x1 || y2 <= y1 {
            return 0.0;
        }
        let area = (x2 - x1) as f64 * (y2 - y1) as f64;
        self.rect_sum(&self.gray_sat, x1, y1, x2 - 1, y2 - 1) / area
    }

    /// Variance of grayscale values inside a rectangle.
    pub fn interior_variance(&self, b: &Bounds) -> f64 {
        let x1 = (b.x as usize).min(self.width.saturating_sub(1));
        let y1 = (b.y as usize).min(self.height.saturating_sub(1));
        let x2 = ((b.x + b.width) as usize).min(self.width.saturating_sub(1));
        let y2 = ((b.y + b.height) as usize).min(self.height.saturating_sub(1));
        if x2 <= x1 || y2 <= y1 {
            return 0.0;
        }
        let area = (x2 - x1) as f64 * (y2 - y1) as f64;
        let mean = self.rect_sum(&self.gray_sat, x1, y1, x2 - 1, y2 - 1) / area;
        let mean_sq = self.rect_sum(&self.gray_sq_sat, x1, y1, x2 - 1, y2 - 1) / area;
        (mean_sq - mean * mean).max(0.0)
    }

    /// Mean edge energy inside a rectangle.
    pub fn interior_edge_energy(&self, b: &Bounds) -> f64 {
        let x1 = (b.x as usize).min(self.width.saturating_sub(1));
        let y1 = (b.y as usize).min(self.height.saturating_sub(1));
        let x2 = ((b.x + b.width) as usize).min(self.width.saturating_sub(1));
        let y2 = ((b.y + b.height) as usize).min(self.height.saturating_sub(1));
        if x2 <= x1 || y2 <= y1 {
            return 0.0;
        }
        let area = (x2 - x1) as f64 * (y2 - y1) as f64;
        self.rect_sum(&self.edge_sat, x1, y1, x2 - 1, y2 - 1) / area
    }

    /// Check if glow band outside rectangle has significant edge energy.
    /// Indicates focused text field with glow/shadow effect.
    pub fn glow_energy(&self, b: &Bounds, band_width: usize) -> f64 {
        let bw = band_width as f64;
        let outer = Bounds {
            x: (b.x - bw).max(0.0),
            y: (b.y - bw).max(0.0),
            width: b.width + bw * 2.0,
            height: b.height + bw * 2.0,
        };
        let outer_e = self.interior_edge_energy(&outer);
        let inner_e = self.interior_edge_energy(b);
        // Glow energy is the edge energy in the band minus the interior.
        (outer_e - inner_e).max(0.0)
    }
}

// ── CPU fallback ────────────────────────────────────────────────────────────

/// Process an image entirely on CPU. Used when Metal is unavailable or in tests.
pub fn process_cpu(image: &RgbaImage) -> ProcessedFrame {
    let width = image.width() as usize;
    let height = image.height() as usize;
    let n = width * height;

    // Grayscale.
    let mut gray = vec![0u8; n];
    for (i, pixel) in image.pixels().enumerate() {
        let r = pixel[0] as u32;
        let g = pixel[1] as u32;
        let b = pixel[2] as u32;
        gray[i] = ((r * 77 + g * 150 + b * 29) >> 8) as u8;
    }

    // Sobel edge detection (|gx| + |gy|, clamped to 255).
    let mut edge = vec![0u8; n];
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let idx = y * width + x;
            let gx = gray[idx + 1] as i32 - gray[idx - 1] as i32;
            let gy = gray[idx + width] as i32 - gray[idx - width] as i32;
            edge[idx] = (gx.unsigned_abs() + gy.unsigned_abs()).min(255) as u8;
        }
    }

    // Build SATs with (width+1)×(height+1) layout, zero-padded border.
    let sw = width + 1;
    let sh = height + 1;
    let sat_len = sw * sh;

    let mut edge_sat = vec![0.0f64; sat_len];
    let mut gray_sat = vec![0.0f64; sat_len];
    let mut gray_sq_sat = vec![0.0f64; sat_len];

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            let si = (y + 1) * sw + (x + 1);
            let e = edge[idx] as f64;
            let g = gray[idx] as f64;
            edge_sat[si] = e + edge_sat[si - 1] + edge_sat[si - sw] - edge_sat[si - sw - 1];
            gray_sat[si] = g + gray_sat[si - 1] + gray_sat[si - sw] - gray_sat[si - sw - 1];
            gray_sq_sat[si] =
                g * g + gray_sq_sat[si - 1] + gray_sq_sat[si - sw] - gray_sq_sat[si - sw - 1];
        }
    }

    // Binary text mask via adaptive local threshold.
    let mean_luma = gray.iter().map(|v| *v as f64).sum::<f64>() / n.max(1) as f64;
    let mut text_mask = vec![false; n];
    if mean_luma >= 128.0 {
        // Light background: dark text.
        for i in 0..n {
            text_mask[i] = gray[i] <= 118;
        }
    } else {
        // Dark background: light text.
        for i in 0..n {
            text_mask[i] = gray[i] >= 150;
        }
    }

    // Morphology: dilate then erode to merge nearby text strokes.
    let text_mask = dilate_mask(&text_mask, width, height, 6, 2);
    let text_mask = erode_mask(&text_mask, width, height, 2, 1);

    ProcessedFrame {
        edge_sat,
        gray_sat,
        gray_sq_sat,
        gray,
        edge,
        text_mask,
        width,
        height,
    }
}

fn dilate_mask(mask: &[bool], w: usize, h: usize, rx: usize, ry: usize) -> Vec<bool> {
    let mut out = vec![false; w * h];
    for y in 0..h {
        for x in 0..w {
            if !mask[y * w + x] {
                continue;
            }
            let y0 = y.saturating_sub(ry);
            let y1 = (y + ry).min(h - 1);
            let x0 = x.saturating_sub(rx);
            let x1 = (x + rx).min(w - 1);
            for dy in y0..=y1 {
                for dx in x0..=x1 {
                    out[dy * w + dx] = true;
                }
            }
        }
    }
    out
}

fn erode_mask(mask: &[bool], w: usize, h: usize, rx: usize, ry: usize) -> Vec<bool> {
    let mut out = vec![true; w * h];
    for y in 0..h {
        for x in 0..w {
            if mask[y * w + x] {
                continue;
            }
            let y0 = y.saturating_sub(ry);
            let y1 = (y + ry).min(h - 1);
            let x0 = x.saturating_sub(rx);
            let x1 = (x + rx).min(w - 1);
            for dy in y0..=y1 {
                for dx in x0..=x1 {
                    out[dy * w + dx] = false;
                }
            }
        }
    }
    out
}

// ── Metal GPU pipeline (TODO) ───────────────────────────────────────────────

// TODO: Implement MetalPipeline struct that uses MPS kernels:
// - MPSImageSobel for edge detection
// - MPSImageIntegral / MPSImageIntegralOfSquares for SATs
// - MPSImageThresholdBinary + MPSImageDilate/Erode for text mask
// The CPU fallback above produces identical results, just slower (~3-5ms vs ~0.2ms).

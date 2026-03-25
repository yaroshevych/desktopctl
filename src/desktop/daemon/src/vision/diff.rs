use desktop_core::protocol::Bounds;
use image::{DynamicImage, GrayImage, RgbaImage, imageops::FilterType};

#[derive(Debug, Clone)]
pub struct GrayThumbnail {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThumbRegion {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
}

pub fn thumbnail_from_rgba(image: &RgbaImage, width: u32, height: u32) -> GrayThumbnail {
    let resized = image::imageops::resize(image, width, height, FilterType::Triangle);
    let gray: GrayImage = DynamicImage::ImageRgba8(resized).to_luma8();
    GrayThumbnail {
        width,
        height,
        pixels: gray.into_raw(),
    }
}

pub fn diff_region(
    prev: &GrayThumbnail,
    curr: &GrayThumbnail,
    threshold: u8,
) -> Option<ThumbRegion> {
    if prev.width != curr.width
        || prev.height != curr.height
        || prev.pixels.len() != curr.pixels.len()
    {
        return Some(ThumbRegion {
            x: 0,
            y: 0,
            width: curr.width.max(1),
            height: curr.height.max(1),
        });
    }

    let mut min_x = curr.width;
    let mut min_y = curr.height;
    let mut max_x = 0_u32;
    let mut max_y = 0_u32;
    let mut changed = false;

    for y in 0..curr.height {
        for x in 0..curr.width {
            let idx = (y * curr.width + x) as usize;
            let a = prev.pixels[idx];
            let b = curr.pixels[idx];
            if a.abs_diff(b) > threshold {
                changed = true;
                min_x = min_x.min(x);
                min_y = min_y.min(y);
                max_x = max_x.max(x);
                max_y = max_y.max(y);
            }
        }
    }

    if !changed {
        None
    } else {
        Some(ThumbRegion {
            x: min_x,
            y: min_y,
            width: (max_x - min_x + 1).max(1),
            height: (max_y - min_y + 1).max(1),
        })
    }
}

pub fn changed_pixel_count(prev: &GrayThumbnail, curr: &GrayThumbnail, threshold: u8) -> usize {
    if prev.width != curr.width
        || prev.height != curr.height
        || prev.pixels.len() != curr.pixels.len()
    {
        return curr.pixels.len();
    }

    let mut changed = 0usize;
    for idx in 0..curr.pixels.len() {
        if prev.pixels[idx].abs_diff(curr.pixels[idx]) > threshold {
            changed += 1;
        }
    }
    changed
}

pub fn upscale_region(
    region: ThumbRegion,
    full_width: u32,
    full_height: u32,
    thumb_width: u32,
    thumb_height: u32,
) -> Bounds {
    let sx = full_width as f64 / thumb_width.max(1) as f64;
    let sy = full_height as f64 / thumb_height.max(1) as f64;
    Bounds {
        x: region.x as f64 * sx,
        y: region.y as f64 * sy,
        width: region.width as f64 * sx,
        height: region.height as f64 * sy,
    }
}

#[cfg(test)]
mod tests {
    use super::{GrayThumbnail, ThumbRegion, changed_pixel_count, diff_region};

    #[test]
    fn detects_changed_region() {
        let prev = GrayThumbnail {
            width: 4,
            height: 4,
            pixels: vec![0; 16],
        };
        let mut curr_pixels = vec![0; 16];
        curr_pixels[6] = 255; // x=2,y=1
        let curr = GrayThumbnail {
            width: 4,
            height: 4,
            pixels: curr_pixels,
        };
        let region = diff_region(&prev, &curr, 8).expect("expected change");
        assert_eq!(
            region,
            ThumbRegion {
                x: 2,
                y: 1,
                width: 1,
                height: 1
            }
        );
    }

    #[test]
    fn no_change_returns_none() {
        let prev = GrayThumbnail {
            width: 4,
            height: 4,
            pixels: vec![12; 16],
        };
        let curr = GrayThumbnail {
            width: 4,
            height: 4,
            pixels: vec![12; 16],
        };
        assert!(diff_region(&prev, &curr, 3).is_none());
    }

    #[test]
    fn changed_pixel_count_reports_sparse_changes() {
        let prev = GrayThumbnail {
            width: 4,
            height: 4,
            pixels: vec![0; 16],
        };
        let mut curr_pixels = vec![0; 16];
        curr_pixels[0] = 12;
        curr_pixels[15] = 20;
        let curr = GrayThumbnail {
            width: 4,
            height: 4,
            pixels: curr_pixels,
        };
        assert_eq!(changed_pixel_count(&prev, &curr, 8), 2);
    }
}

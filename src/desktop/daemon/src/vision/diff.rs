use desktop_core::protocol::Bounds;
use image::{DynamicImage, GrayImage, RgbaImage, imageops::FilterType};

mod gpu;

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
    if let Some(gpu) = gpu::thumbnail_from_rgba_gpu(image, width, height) {
        return gpu;
    }
    thumbnail_from_rgba_cpu(image, width, height)
}

fn thumbnail_from_rgba_cpu(image: &RgbaImage, width: u32, height: u32) -> GrayThumbnail {
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
    let regions = diff_regions(prev, curr, threshold);
    if regions.is_empty() {
        return None;
    }
    let mut min_x = u32::MAX;
    let mut min_y = u32::MAX;
    let mut max_x = 0_u32;
    let mut max_y = 0_u32;
    for region in &regions {
        min_x = min_x.min(region.x);
        min_y = min_y.min(region.y);
        max_x = max_x.max(region.x + region.width.saturating_sub(1));
        max_y = max_y.max(region.y + region.height.saturating_sub(1));
    }
    Some(ThumbRegion {
        x: min_x,
        y: min_y,
        width: max_x.saturating_sub(min_x).saturating_add(1).max(1),
        height: max_y.saturating_sub(min_y).saturating_add(1).max(1),
    })
}

pub fn diff_regions(prev: &GrayThumbnail, curr: &GrayThumbnail, threshold: u8) -> Vec<ThumbRegion> {
    if prev.width != curr.width
        || prev.height != curr.height
        || prev.pixels.len() != curr.pixels.len()
    {
        return vec![ThumbRegion {
            x: 0,
            y: 0,
            width: curr.width.max(1),
            height: curr.height.max(1),
        }];
    }

    let width = curr.width as usize;
    let height = curr.height as usize;
    let mut changed = vec![false; curr.pixels.len()];
    for idx in 0..curr.pixels.len() {
        if prev.pixels[idx].abs_diff(curr.pixels[idx]) > threshold {
            changed[idx] = true;
        }
    }
    let mut visited = vec![false; changed.len()];
    let mut regions = Vec::new();
    let mut queue: std::collections::VecDeque<(usize, usize)> = std::collections::VecDeque::new();

    for y in 0..height {
        for x in 0..width {
            let idx = y * width + x;
            if !changed[idx] || visited[idx] {
                continue;
            }
            visited[idx] = true;
            queue.push_back((x, y));

            let mut min_x = x as u32;
            let mut min_y = y as u32;
            let mut max_x = x as u32;
            let mut max_y = y as u32;
            while let Some((cx, cy)) = queue.pop_front() {
                min_x = min_x.min(cx as u32);
                min_y = min_y.min(cy as u32);
                max_x = max_x.max(cx as u32);
                max_y = max_y.max(cy as u32);
                for (nx, ny) in neighbors4(cx, cy, width, height) {
                    let nidx = ny * width + nx;
                    if changed[nidx] && !visited[nidx] {
                        visited[nidx] = true;
                        queue.push_back((nx, ny));
                    }
                }
            }
            regions.push(ThumbRegion {
                x: min_x,
                y: min_y,
                width: max_x.saturating_sub(min_x).saturating_add(1).max(1),
                height: max_y.saturating_sub(min_y).saturating_add(1).max(1),
            });
        }
    }
    regions
}

fn neighbors4(x: usize, y: usize, width: usize, height: usize) -> [(usize, usize); 4] {
    let left = (x.saturating_sub(1), y);
    let up = (x, y.saturating_sub(1));
    let right = ((x + 1).min(width.saturating_sub(1)), y);
    let down = (x, (y + 1).min(height.saturating_sub(1)));
    [left, up, right, down]
}

#[cfg(test)]
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
    use super::{GrayThumbnail, ThumbRegion, changed_pixel_count, diff_region, diff_regions};

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

    #[test]
    fn detects_multiple_changed_regions() {
        let prev = GrayThumbnail {
            width: 6,
            height: 4,
            pixels: vec![0; 24],
        };
        let mut curr_pixels = vec![0; 24];
        curr_pixels[1] = 255; // x=1,y=0
        curr_pixels[22] = 255; // x=4,y=3
        let curr = GrayThumbnail {
            width: 6,
            height: 4,
            pixels: curr_pixels,
        };
        let mut regions = diff_regions(&prev, &curr, 8);
        regions.sort_by_key(|r| (r.y, r.x));
        assert_eq!(regions.len(), 2);
        assert_eq!(
            regions[0],
            ThumbRegion {
                x: 1,
                y: 0,
                width: 1,
                height: 1
            }
        );
        assert_eq!(
            regions[1],
            ThumbRegion {
                x: 4,
                y: 3,
                width: 1,
                height: 1
            }
        );
    }
}

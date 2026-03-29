use desktop_core::protocol::Bounds;
use image::{DynamicImage, GrayImage, RgbaImage, imageops::FilterType};
#[cfg(target_os = "macos")]
use std::sync::OnceLock;

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
    #[cfg(target_os = "macos")]
    if let Some(gpu) = thumbnail_from_rgba_gpu(image, width, height) {
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

#[cfg(target_os = "macos")]
fn thumbnail_from_rgba_gpu(image: &RgbaImage, width: u32, height: u32) -> Option<GrayThumbnail> {
    if width == 0 || height == 0 {
        return None;
    }

    use metal::{
        CommandBufferRef, CompileOptions, ComputeCommandEncoderRef, MTLOrigin, MTLPixelFormat,
        MTLRegion, MTLSize, MTLStorageMode, MTLTextureType, MTLTextureUsage, TextureDescriptor,
    };

    const SHADER: &str = r#"
        #include <metal_stdlib>
        using namespace metal;

        kernel void downscale_luma(
            texture2d<float, access::sample> src [[texture(0)]],
            texture2d<float, access::write> dst [[texture(1)]],
            uint2 gid [[thread_position_in_grid]]
        ) {
            uint out_w = dst.get_width();
            uint out_h = dst.get_height();
            if (gid.x >= out_w || gid.y >= out_h) { return; }

            constexpr sampler s(address::clamp_to_edge, filter::linear);
            float2 uv = (float2(gid) + 0.5f) / float2(out_w, out_h);
            float4 c = src.sample(s, uv);
            float luma = dot(c.rgb, float3(0.299f, 0.587f, 0.114f));
            dst.write(float4(luma, 0.0f, 0.0f, 1.0f), gid);
        }
    "#;

    struct ThumbnailGpuPipeline {
        device: metal::Device,
        queue: metal::CommandQueue,
        pso: metal::ComputePipelineState,
    }

    impl ThumbnailGpuPipeline {
        fn new() -> Option<Self> {
            let device = metal::Device::system_default()?;
            let options = CompileOptions::new();
            let library = device.new_library_with_source(SHADER, &options).ok()?;
            let func = library.get_function("downscale_luma", None).ok()?;
            let pso = device
                .new_compute_pipeline_state_with_function(&func)
                .ok()?;
            let queue = device.new_command_queue();
            Some(Self { device, queue, pso })
        }
    }

    fn pipeline() -> Option<&'static ThumbnailGpuPipeline> {
        static PIPELINE: OnceLock<Option<ThumbnailGpuPipeline>> = OnceLock::new();
        PIPELINE.get_or_init(ThumbnailGpuPipeline::new).as_ref()
    }

    fn make_input_texture(device: &metal::Device, image: &RgbaImage) -> Option<metal::Texture> {
        let desc = TextureDescriptor::new();
        desc.set_texture_type(MTLTextureType::D2);
        desc.set_pixel_format(MTLPixelFormat::RGBA8Unorm);
        desc.set_width(image.width() as u64);
        desc.set_height(image.height() as u64);
        desc.set_storage_mode(MTLStorageMode::Managed);
        desc.set_usage(MTLTextureUsage::ShaderRead);
        let tex = device.new_texture(&desc);
        let region = MTLRegion {
            origin: MTLOrigin { x: 0, y: 0, z: 0 },
            size: MTLSize {
                width: image.width() as u64,
                height: image.height() as u64,
                depth: 1,
            },
        };
        tex.replace_region(
            region,
            0,
            image.as_raw().as_ptr().cast(),
            (image.width() * 4) as u64,
        );
        Some(tex)
    }

    fn make_output_texture(
        device: &metal::Device,
        width: u32,
        height: u32,
    ) -> Option<metal::Texture> {
        let desc = TextureDescriptor::new();
        desc.set_texture_type(MTLTextureType::D2);
        desc.set_pixel_format(MTLPixelFormat::R8Unorm);
        desc.set_width(width as u64);
        desc.set_height(height as u64);
        desc.set_storage_mode(MTLStorageMode::Managed);
        desc.set_usage(MTLTextureUsage::ShaderWrite | MTLTextureUsage::ShaderRead);
        Some(device.new_texture(&desc))
    }

    fn encode_dispatch(
        cmd: &CommandBufferRef,
        encoder: &ComputeCommandEncoderRef,
        width: u32,
        height: u32,
    ) {
        let tg_w = 16_u64.min(width.max(1) as u64);
        let tg_h = 16_u64.min(height.max(1) as u64);
        let threads_per_group = MTLSize {
            width: tg_w,
            height: tg_h,
            depth: 1,
        };
        let groups = MTLSize {
            width: (width as u64).div_ceil(tg_w),
            height: (height as u64).div_ceil(tg_h),
            depth: 1,
        };
        encoder.dispatch_thread_groups(groups, threads_per_group);
        encoder.end_encoding();
        cmd.commit();
        cmd.wait_until_completed();
    }

    let pipeline = pipeline()?;
    let src_tex = make_input_texture(&pipeline.device, image)?;
    let dst_tex = make_output_texture(&pipeline.device, width, height)?;
    let cmd = pipeline.queue.new_command_buffer();
    let encoder = cmd.new_compute_command_encoder();
    encoder.set_compute_pipeline_state(&pipeline.pso);
    encoder.set_texture(0, Some(&src_tex));
    encoder.set_texture(1, Some(&dst_tex));
    encode_dispatch(&cmd, &encoder, width, height);

    let mut pixels = vec![0_u8; (width * height) as usize];
    let region = MTLRegion {
        origin: MTLOrigin { x: 0, y: 0, z: 0 },
        size: MTLSize {
            width: width as u64,
            height: height as u64,
            depth: 1,
        },
    };
    dst_tex.get_bytes(pixels.as_mut_ptr().cast(), width as u64, region, 0);
    Some(GrayThumbnail {
        width,
        height,
        pixels,
    })
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

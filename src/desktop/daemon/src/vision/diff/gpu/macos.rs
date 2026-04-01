use std::sync::OnceLock;

use image::RgbaImage;

use super::GrayThumbnail;

pub(super) fn thumbnail_from_rgba_gpu(
    image: &RgbaImage,
    width: u32,
    height: u32,
) -> Option<GrayThumbnail> {
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

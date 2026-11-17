// SPDX-License-Identifier: GPL-3.0-only
//! ci-wgpu-probe – feasibility probe for sharing a Core Image RAW rendering with wgpu on macOS.
//!
//! Usage: ci-wgpu-probe FILE [--decoder V] [--runs N]
//!
//! 1. creates a wgpu device (Metal backend) and takes its MTLDevice,
//! 2. creates a CIContext on that same MTLDevice and decodes FILE with CIRAWFilter (neutral settings:
//!    noise reduction, sharpening, local tone map, boost, lens correction off),
//! 3. renders the CIImage directly into an MTLTexture (RGBA16Float, linear Rec. 2020) on a Metal command
//!    buffer, waits for it, and wraps the texture as a wgpu::Texture (wgpu-hal `texture_from_raw`,
//!    `create_texture_from_hal`) – no copy through the CPU,
//! 4. for checking only: copies the wgpu texture into a buffer, reads it back and compares it bit by bit
//!    with a CPU-side render of the same image (`render:toBitmap:`), and prints timings.
//!
//! Not part of the app. Objective-C calls via objc2 `msg_send!` (Core Image has no objc2 0.6 bindings in use).

use std::ffi::c_void;
use std::time::Instant;

use objc2::rc::Retained;
use objc2::runtime::{AnyObject, ProtocolObject};
use objc2::{class, msg_send};
use objc2_foundation::{NSRect, NSString, NSURL};
use objc2_metal::{MTLCommandBuffer, MTLCommandQueue, MTLDevice, MTLPixelFormat, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage};

#[link(name = "CoreImage", kind = "framework")]
extern "C" {}
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGColorSpaceCreateWithName(name: *const c_void) -> *mut c_void;
    static kCGColorSpaceLinearITUR_2020: *const c_void;
}
// CIFormat kCIFormatRGBAh
#[link(name = "CoreImage", kind = "framework")]
extern "C" {
    static kCIFormatRGBAh: i32;
}

#[repr(transparent)]
#[derive(Clone, Copy)]
struct ColorSpaceRef(*mut c_void);
// Core Graphics colour space as Objective-C argument (`^{CGColorSpace=}`)
unsafe impl objc2::encode::Encode for ColorSpaceRef {
    const ENCODING: objc2::encode::Encoding = objc2::encode::Encoding::Pointer(&objc2::encode::Encoding::Struct("CGColorSpace", &[]));
}

fn raw_filter(path: &str, decoder: Option<&str>) -> Retained<AnyObject> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(path));
    let f: Option<Retained<AnyObject>> = unsafe { msg_send![class!(CIRAWFilter), filterWithImageURL: &*url] };
    let f = f.expect("CIRAWFilter: file not supported");
    unsafe {
        if let Some(v) = decoder {
            let _: () = msg_send![&*f, setDecoderVersion: &*NSString::from_str(v)];
        }
        let _: () = msg_send![&*f, setLuminanceNoiseReductionAmount: 0.0f32];
        let _: () = msg_send![&*f, setColorNoiseReductionAmount: 0.0f32];
        let _: () = msg_send![&*f, setSharpnessAmount: 0.0f32];
        let _: () = msg_send![&*f, setContrastAmount: 0.0f32];
        let _: () = msg_send![&*f, setDetailAmount: 0.0f32];
        let _: () = msg_send![&*f, setMoireReductionAmount: 0.0f32];
        let _: () = msg_send![&*f, setLocalToneMapAmount: 0.0f32];
        let _: () = msg_send![&*f, setBoostAmount: 0.0f32];
        let _: () = msg_send![&*f, setBoostShadowAmount: 0.0f32];
        let _: () = msg_send![&*f, setLensCorrectionEnabled: false];
    }
    f
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let file = args.get(1).expect("usage: ci-wgpu-probe FILE [--decoder V] [--runs N]");
    let opt = |n: &str| args.iter().position(|a| a == n).and_then(|i| args.get(i + 1)).cloned();
    let decoder = opt("--decoder");
    let runs: usize = opt("--runs").and_then(|v| v.parse().ok()).unwrap_or(3);

    // 1. wgpu device on Metal
    let t = Instant::now();
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::METAL;
    let instance = wgpu::Instance::new(desc);
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions::default())).expect("no Metal adapter");
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default())).expect("device");
    let mtl: Retained<ProtocolObject<dyn MTLDevice>> = unsafe { device.as_hal::<wgpu::hal::api::Metal>() }.expect("not Metal").raw_device().clone();
    println!("wgpu device on {} ({:.0?})", mtl.name(), t.elapsed());

    // 2. CIContext on the same MTLDevice
    let ctx: Retained<AnyObject> = unsafe { msg_send![class!(CIContext), contextWithMTLDevice: &*mtl] };
    let cs = ColorSpaceRef(unsafe { CGColorSpaceCreateWithName(kCGColorSpaceLinearITUR_2020) });
    let ci_queue = mtl.newCommandQueue().expect("command queue");

    for run in 0..runs {
        let t0 = Instant::now();
        let f = raw_filter(file, decoder.as_deref());
        let img: Option<Retained<AnyObject>> = unsafe { msg_send![&*f, outputImage] };
        let img = img.expect("no output image");
        let ext: NSRect = unsafe { msg_send![&*img, extent] };
        let (w, h) = (ext.size.width as usize, ext.size.height as usize);

        // 3. render into a private MTLTexture on our command buffer
        let desc = unsafe { MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(MTLPixelFormat::RGBA16Float, w, h, false) };
        desc.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::ShaderWrite | MTLTextureUsage::RenderTarget);
        desc.setStorageMode(MTLStorageMode::Private);
        let tex = mtl.newTextureWithDescriptor(&desc).expect("texture");
        let cb = ci_queue.commandBuffer().expect("command buffer");
        unsafe {
            let _: () = msg_send![&*ctx, render: &*img, toMTLTexture: &*tex, commandBuffer: &*cb, bounds: ext, colorSpace: cs];
        }
        cb.commit();
        cb.waitUntilCompleted();
        let t_gpu = t0.elapsed();

        // wrap as wgpu texture (no copy)
        let hal_tex = unsafe {
            wgpu::hal::metal::Device::texture_from_raw(
                tex.clone(),
                wgpu::TextureFormat::Rgba16Float,
                MTLTextureType::Type2D,
                1,
                1,
                wgpu::hal::CopyExtent { width: w as u32, height: h as u32, depth: 1 },
                None,
            )
        };
        let size = wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 };
        let wtex = unsafe {
            device.create_texture_from_hal::<wgpu::hal::api::Metal>(
                hal_tex,
                &wgpu::TextureDescriptor {
                    label: Some("core-image-raw"),
                    size,
                    mip_level_count: 1,
                    sample_count: 1,
                    dimension: wgpu::TextureDimension::D2,
                    format: wgpu::TextureFormat::Rgba16Float,
                    usage: wgpu::TextureUsages::COPY_SRC | wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::STORAGE_BINDING,
                    view_formats: &[],
                },
                wgpu::TextureUses::RESOURCE,
            )
        };
        let t_wrap = t0.elapsed();

        // 4. check: read the wgpu texture back and compare with a CPU-side render of the same image
        let bpr = (w * 8).div_ceil(256) * 256;
        let buf = device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (bpr * h) as u64, usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ, mapped_at_creation: false });
        let mut enc = device.create_command_encoder(&Default::default());
        enc.copy_texture_to_buffer(
            wgpu::TexelCopyTextureInfo { texture: &wtex, mip_level: 0, origin: wgpu::Origin3d::ZERO, aspect: wgpu::TextureAspect::All },
            wgpu::TexelCopyBufferInfo { buffer: &buf, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(bpr as u32), rows_per_image: None } },
            size,
        );
        queue.submit([enc.finish()]);
        let slice = buf.slice(..);
        slice.map_async(wgpu::MapMode::Read, |_| {});
        device.poll(wgpu::PollType::wait_indefinitely()).expect("poll");
        let gpu: Vec<u8> = {
            let m = slice.get_mapped_range().expect("mapped range");
            (0..h).flat_map(|y| m[y * bpr..y * bpr + w * 8].to_vec()).collect()
        };
        buf.unmap();
        let t_read = t0.elapsed();

        let t1 = Instant::now();
        let mut cpu = vec![0u8; w * h * 8];
        unsafe {
            let _: () = msg_send![&*ctx, render: &*img, toBitmap: cpu.as_mut_ptr() as *mut c_void, rowBytes: (w * 8) as isize, bounds: ext, format: kCIFormatRGBAh, colorSpace: cs];
        }
        let t_cpu = t1.elapsed();
        // Core Image renders into a Metal texture with the origin at the bottom: compare row-flipped
        let same_flipped = (0..h).all(|y| gpu[y * w * 8..(y + 1) * w * 8] == cpu[(h - 1 - y) * w * 8..(h - y) * w * 8]);
        let same = gpu == cpu;
        let nonzero = gpu.iter().filter(|&&b| b != 0).count();
        println!(
            "run {run}: {w}x{h} · CIRAWFilter→MTLTexture {:.0?} · wrapped as wgpu texture {:.1?} · wgpu readback {:.0?} · CPU bitmap render {:.0?} · identical: {} (row-flipped: {}) · non-zero bytes {:.1} %",
            t_gpu,
            t_wrap - t_gpu,
            t_read - t_wrap,
            t_cpu,
            same,
            same_flipped,
            100.0 * nonzero as f64 / gpu.len() as f64
        );
        let _ = tex.width();
    }
}

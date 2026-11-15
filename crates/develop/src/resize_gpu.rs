//! Export scaling ([`crate::resize`]) on the GPU (measurement path, see `resize::lanczos3`): the 16-bit image goes up once, the vertical and the horizontal
//! Lanczos3 pass run on the graphics card, the small image comes back. Own small wgpu device (like the masks), only
//! when the setting "Grafikkarte verwenden" allows it (`pools::gpu_allowed`). `None` everywhere: the processor computes.

use std::sync::{Mutex, OnceLock};

use wgpu::util::DeviceExt;

use crate::Rgb16;

struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    vertical: wgpu::ComputePipeline,
    horizontal: wgpu::ComputePipeline,
    max_buffer: u64,
    /// one job at a time (the buffers are large; exports run one after the other anyway)
    lock: Mutex<()>,
}

static GPU: OnceLock<Option<Gpu>> = OnceLock::new();

/// Workgroup size of both kernels (1D, see `resize.wgsl`)
const WG: u32 = 256;

impl Gpu {
    fn new() -> Option<Gpu> {
        let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
        desc.backends = wgpu::Backends::PRIMARY;
        let instance = wgpu::Instance::new(desc);
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            compatible_surface: None,
            force_fallback_adapter: false,
            apply_limit_buckets: false,
        }))
        .ok()?;
        // software renderers (e.g. WARP in a virtual machine): slower than the processor and unreliable
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let al = adapter.limits();
        let mut limits = wgpu::Limits::default();
        limits.max_storage_buffer_binding_size = al.max_storage_buffer_binding_size;
        limits.max_buffer_size = al.max_buffer_size;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("graineo export scaling"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        // wgpu ends the thread with a panic on an uncaught GPU error: count it instead (the job falls back to the
        // processor, see `pools::gpu_errors`); a lost device switches every GPU path off
        device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| pools::gpu_error("export scaling", &e.to_string())));
        device.set_device_lost_callback(|reason, message| {
            if reason != wgpu::DeviceLostReason::Destroyed {
                pools::gpu_lost("export scaling", &message);
            }
        });

        let entries: Vec<_> = (0..6u32)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: binding < 4 }, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("resize"), entries: &entries });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        // compile inside an error scope: if the driver cannot build the shader, the processor computes
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("resize"), source: wgpu::ShaderSource::Wgsl(include_str!("resize.wgsl").into()) });
        let pipe = |entry: &str| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor { label: Some(entry), layout: Some(&pl), module: &module, entry_point: Some(entry), compilation_options: Default::default(), cache: None })
        };
        let (vertical, horizontal) = (pipe("vertical"), pipe("horizontal"));
        if let Some(e) = pollster::block_on(scope.pop()) {
            eprintln!("export scaling: shader not usable, the processor computes: {e}");
            return None;
        }
        let max_buffer = (al.max_storage_buffer_binding_size as u64).min(al.max_buffer_size);
        Some(Gpu { device, queue, layout, vertical, horizontal, max_buffer, lock: Mutex::new(()) })
    }
}

fn gpu() -> Option<&'static Gpu> {
    if !pools::gpu_allowed() || std::env::var_os("GRAINEO_RESIZE_CPU").is_some() {
        return None;
    }
    GPU.get_or_init(Gpu::new).as_ref()
}

/// Number of workgroups for `n` threads as a 2D grid (x at most 65 535 groups).
fn groups(n: usize) -> (u32, u32) {
    let g = n.div_ceil(WG as usize).max(1);
    let x = g.min(65_535);
    (x as u32, g.div_ceil(x) as u32)
}

/// Lanczos3 `img` → `nw`×`nh` on the GPU. `None`: no GPU, not allowed, image too small or too large for the buffers.
pub fn lanczos3(img: &Rgb16, nw: usize, nh: usize) -> Option<Rgb16> {
    let (w, h) = (img.width, img.height);
    if w == 0 || h == 0 || nw == 0 || nh == 0 || img.data.len() != w * h * 3 {
        return None;
    }
    let g = gpu()?;
    let src_bytes = (w * h * 3).div_ceil(2) * 4;
    let tmp_bytes = w * nh * 3 * 4;
    let dst_words = (nw * nh * 3).div_ceil(2);
    if [src_bytes, tmp_bytes, dst_words * 4].iter().any(|&b| b as u64 > g.max_buffer) {
        return None;
    }
    let _one = g.lock.lock().unwrap_or_else(|e| e.into_inner());
    let errors_before = pools::gpu_errors();
    let vt = crate::resize::taps(h, nh);
    let ht = crate::resize::taps(w, nw);
    // tables: per output position [first source index (as bits), n weights]
    let table = |t: &crate::resize::Taps| -> Vec<f32> {
        let mut v = Vec::with_capacity(t.left.len() * (t.n + 1));
        for (k, &l) in t.left.iter().enumerate() {
            v.push(f32::from_bits(l));
            v.extend_from_slice(&t.w[k * t.n..(k + 1) * t.n]);
        }
        v
    };
    let storage = wgpu::BufferUsages::STORAGE;
    let init = |data: &[u8]| g.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: data, usage: storage });
    let (vx, vy) = groups(w * nh);
    let (hx, hy) = groups(dst_words);
    let cfg: [u32; 8] = [w as u32, h as u32, nw as u32, nh as u32, vt.n as u32, ht.n as u32, vx * WG, hx * WG];
    let cfg_buf = init(bytemuck::cast_slice(&cfg));
    // source: 16-bit values, two per word (the last word padded with 0)
    let src = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("resize source"), size: src_bytes as u64, usage: storage, mapped_at_creation: true });
    {
        let mut m = src.slice(..).get_mapped_range_mut().ok()?;
        let bytes: &[u8] = bytemuck::cast_slice(&img.data);
        m.slice(..bytes.len()).copy_from_slice(bytes); // the padding stays 0 (mapped at creation = zeroed)
    }
    src.unmap();
    let vbuf = init(bytemuck::cast_slice(&table(&vt)));
    let hbuf = init(bytemuck::cast_slice(&table(&ht)));
    let tmp = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("resize tmp"), size: tmp_bytes as u64, usage: storage, mapped_at_creation: false });
    let dst = g.device.create_buffer(&wgpu::BufferDescriptor { label: Some("resize out"), size: (dst_words * 4) as u64, usage: storage | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
    let readback = g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (dst_words * 4) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
    let bind = g.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: None,
        layout: &g.layout,
        entries: &[&cfg_buf, &src, &vbuf, &hbuf, &tmp, &dst].iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect::<Vec<_>>(),
    });
    let scope = g.device.push_error_scope(wgpu::ErrorFilter::Validation);
    let mut enc = g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("resize") });
    {
        let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("resize"), timestamp_writes: None });
        pass.set_bind_group(0, &bind, &[]);
        pass.set_pipeline(&g.vertical);
        pass.dispatch_workgroups(vx, vy, 1);
        pass.set_pipeline(&g.horizontal);
        pass.dispatch_workgroups(hx, hy, 1);
    }
    enc.copy_buffer_to_buffer(&dst, 0, &readback, 0, (dst_words * 4) as u64);
    g.queue.submit([enc.finish()]);
    let sl = readback.slice(..);
    let (tx, rx) = std::sync::mpsc::channel();
    sl.map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
    if let Some(e) = pollster::block_on(scope.pop()) {
        eprintln!("export scaling on the GPU failed, the processor computes: {e}");
        return None;
    }
    rx.recv().ok()?.ok()?;
    if pools::gpu_errors() != errors_before {
        return None;
    }
    let data = sl.get_mapped_range().ok()?;
    let out: Vec<u16> = bytemuck::cast_slice::<u8, u16>(&data).get(..nw * nh * 3)?.to_vec();
    drop(data);
    readback.unmap();
    Some(Rgb16 { width: nw, height: nh, data: out })
}

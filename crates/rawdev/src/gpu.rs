//! Full resolution on the GPU (wgpu/WGSL): noise reduction, base sharpening and lens correction –
//! the most expensive steps after demosaicing (together ≈ 2 s at 24 MP on the CPU). The image goes
//! up once and back once; everything in between stays on the GPU.
//!
//! The CPU version in `crates/raw` remains the reference (tests: [`compare`]). Without a GPU – or with
//! `GRAINEO_GPU=0` – the CPU computes automatically. Not used for the Sigma path (SPP hashes untouched).

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use wgpu::util::DeviceExt;

pub mod false_color;
pub mod xtrans;

pub struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipelines: Mutex<HashMap<&'static str, wgpu::ComputePipeline>>,
    module_main: wgpu::ShaderModule,
    module_output: wgpu::ShaderModule,
    module_xtrans: wgpu::ShaderModule,
    pub name: String,
    max_buffer: u64,
    /// unified memory (Apple Silicon): write and read image buffers directly
    direct: bool,
    /// timestamp queries available (profiling per kernel)
    timestamps: bool,
}

static GPU: OnceLock<Option<Gpu>> = OnceLock::new();

/// The GPU, if present and allowed (`GRAINEO_GPU=0` disables it).
pub fn gpu() -> Option<&'static Gpu> {
    // setting "Grafikkarte verwenden" (and `GRAINEO_GPU=0`): processor path
    if !pools::gpu_allowed() {
        return None;
    }
    if std::env::var("GRAINEO_GPU_STAGES").as_deref() == Ok("1") {
        STAGE_TIMING.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    GPU.get_or_init(Gpu::new).as_ref()
}

/// Names of the compute entry points (`@compute … fn name(`) of a shader.
fn entry_points(source: &'static str) -> impl Iterator<Item = &'static str> {
    source.split("@compute").skip(1).filter_map(|t| {
        let t = &t[t.find("fn ")? + 3..];
        Some(t[..t.find('(')?].trim())
    })
}

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
        // Software renderers (e.g. WARP on Windows in a virtual machine): slower than the CPU and unreliable
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let al = adapter.limits();
        let mut limits = wgpu::Limits::default();
        limits.max_storage_buffer_binding_size = al.max_storage_buffer_binding_size;
        limits.max_buffer_size = al.max_buffer_size;
        limits.max_storage_buffers_per_shader_stage = al.max_storage_buffers_per_shader_stage.max(8);
        let info = adapter.get_info();
        let timestamps = adapter.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let direct = info.device_type == wgpu::DeviceType::IntegratedGpu && adapter.features().contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS);
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("rawdev"),
            required_features: (if direct { wgpu::Features::MAPPABLE_PRIMARY_BUFFERS } else { wgpu::Features::empty() }) | (adapter.features() & wgpu::Features::TIMESTAMP_QUERY),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        // wgpu ends the thread with a panic on an uncaught GPU error: count it instead (the job falls back to the
        // processor, see `pools::gpu_errors`); a lost device switches every GPU path off
        device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| pools::gpu_error("rawdev", &e.to_string())));
        device.set_device_lost_callback(|reason, message| {
            if reason != wgpu::DeviceLostReason::Destroyed {
                pools::gpu_lost("rawdev", &message);
            }
        });

        let module_main = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("rawdev"), source: wgpu::ShaderSource::Wgsl(include_str!("gpu.wgsl").into()) });
        let module_output = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("rawdev-ausgabe"), source: wgpu::ShaderSource::Wgsl(include_str!("gpu_out.wgsl").into()) });
        let module_xtrans = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("rawdev-xtrans"), source: wgpu::ShaderSource::Wgsl(include_str!("gpu_xtrans.wgsl").into()) });
        let entry = |binding: u32, read_only_flag: bool| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: read_only_flag }, has_dynamic_offset: false, min_binding_size: None },
            count: None,
        };
        let entries: Vec<_> = (0..7).map(|i| entry(i, i == 0)).collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("rawdev"), entries: &entries });
        let max_buffer = (al.max_storage_buffer_binding_size as u64).min(al.max_buffer_size);
        let name = adapter.get_info().name;
        let gpu = Gpu { device, queue, layout, pipelines: Mutex::new(HashMap::new()), module_main, module_output, module_xtrans, name, max_buffer, direct, timestamps };
        // Compile all shaders right here: if one fails (e.g. in the driver's compiler), the CPU computes –
        // instead of crashing later in the middle of developing with a validation error.
        let scope = gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        for e in entry_points(include_str!("gpu.wgsl")).chain(entry_points(include_str!("gpu_out.wgsl"))).chain(entry_points(include_str!("gpu_xtrans.wgsl"))) {
            gpu.pipeline(e);
        }
        if let Some(f) = pollster::block_on(scope.pop()) {
            eprintln!("GPU shaders not usable ({}), development computes on the CPU: {f}", gpu.name);
            return None;
        }
        Some(gpu)
    }

    fn pipeline(&self, entry: &'static str) -> wgpu::ComputePipeline {
        let mut m = self.pipelines.lock().unwrap();
        if let Some(p) = m.get(entry) {
            return p.clone();
        }
        let pl = self.device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&self.layout)], immediate_size: 0 });
        let p = self.device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: Some(&pl),
            module: if entry.starts_with("aus_") {
                &self.module_output
            } else if entry.starts_with("xt_") {
                &self.module_xtrans
            } else {
                &self.module_main
            },
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        });
        m.insert(entry, p.clone());
        p
    }

    fn buffer(&self, n: usize) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: (n.max(1) * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        })
    }

    /// Image buffer: with unified memory directly writable and readable (no copy via staging buffers).
    fn image_buffer(&self, data: Option<&[f32]>, n: usize) -> wgpu::Buffer {
        if !self.direct {
            return match data {
                Some(d) => self.buffer_with(d),
                None => self.buffer(n),
            };
        }
        let b = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("image"),
            size: (n.max(1) * 4) as u64,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: data.is_some(),
        });
        if let Some(d) = data {
            b.slice(..).get_mapped_range_mut().expect("wgpu: image buffer").copy_from_slice(bytemuck::cast_slice(d));
            b.unmap();
        }
        b
    }

    /// Buffer with 16-bit values, two per word (as the output buffer of [`output_kernels`]); the last word is padded with 0.
    fn packed_u16_buffer(&self, data: &[u16]) -> wgpu::Buffer {
        let words = data.len().div_ceil(2).max(1);
        let usage = if self.direct {
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST
        } else {
            wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST
        };
        let b = self.device.create_buffer(&wgpu::BufferDescriptor { label: Some("image u16"), size: (words * 4) as u64, usage, mapped_at_creation: true });
        {
            let mut m = b.slice(..).get_mapped_range_mut().expect("wgpu: image buffer");
            let bytes: &[u8] = bytemuck::cast_slice(data);
            m.slice(..bytes.len()).copy_from_slice(bytes); // the padding stays 0 (mapped at creation = zeroed)
        }
        b.unmap();
        b
    }

    fn buffer_with(&self, data: &[f32]) -> wgpu::Buffer {
        self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(if data.is_empty() { &[0f32] } else { data }),
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::COPY_DST,
        })
    }
}

/// One graphics adapter of this computer (performance report).
#[derive(Clone, Debug)]
pub struct AdapterSummary {
    pub name: String,
    /// "Metal", "Vulkan", "Dx12", "Gl"
    pub backend: String,
    /// "IntegratedGpu", "DiscreteGpu", "VirtualGpu", "Cpu" (software renderer), "Other"
    pub device_type: String,
    pub driver: String,
    pub driver_info: String,
}

/// All graphics adapters wgpu finds on any backend (a fresh instance; does not open a device). For the
/// performance report only: costs a few hundred milliseconds on some systems.
pub fn adapters() -> Vec<AdapterSummary> {
    let mut desc = wgpu::InstanceDescriptor::new_without_display_handle();
    desc.backends = wgpu::Backends::all();
    let instance = wgpu::Instance::new(desc);
    pollster::block_on(instance.enumerate_adapters(wgpu::Backends::all()))
        .into_iter()
        .map(|a| {
            let i = a.get_info();
            AdapterSummary { name: i.name, backend: format!("{:?}", i.backend), device_type: format!("{:?}", i.device_type), driver: i.driver, driver_info: i.driver_info }
        })
        .collect()
}

/// Name of the adapter the development uses, if one is open already (never opens one).
pub fn adapter_in_use() -> Option<String> {
    GPU.get().and_then(|g| g.as_ref()).map(|g| g.name.clone())
}

/// Largest storage buffer (bytes) a kernel can bind.
pub fn max_buffer() -> Option<u64> {
    gpu().map(|g| g.max_buffer)
}

/// Open the GPU and compile all kernels (in the background at startup; otherwise the first image
/// at full resolution costs 1–2 s more).
pub fn warm_up() {
    if let Some(g) = gpu() {
        for k in ["yuv_vor", "yuv_rueck", "filt_h", "filt_v", "nr_v_detail", "nr_v_schrumpfen", "addieren", "umlegen", "gauss_h", "gauss_v", "luminanz", "teilen", "mal_begrenzt", "schaerfen", "vignette", "geometrie", "zuschneiden", "aus_mask_summe", "aus_box_h", "aus_box_v", "aus_pixel", "aus_szene", "aus_bb_klein", "aus_bb_box_h", "aus_bb_box_v", "aus_bb_ab", "aus_bb_basis", "xt_fuellen", "xt_gminmax", "xt_gruen", "xt_gruen2_a", "xt_gruen2_b", "xt_rb1", "xt_rb2", "xt_g22", "xt_kopie", "xt_drv", "xt_homo", "xt_h5", "xt_v5", "xt_fertig", "aus_fc_yuv", "aus_fc_median"] {
            let _ = g.pipeline(k);
        }
    }
}

/// A job: many kernels in sequence in one command buffer.
struct Run<'a> {
    g: &'a Gpu,
    enc: wgpu::CommandEncoder,
    w: usize,
    h: usize,
    empty: wgpu::Buffer,
    params: Vec<wgpu::Buffer>,
    /// Start of the running stage (measurement with `GRAINEO_GPU_STAGES=1`)
    started: std::time::Instant,
    /// Timestamps per kernel (`GRAINEO_GPU_PROFILE=1`, if the GPU supports it)
    timestamp_qs: Option<wgpu::QuerySet>,
    timer_names: Vec<&'static str>,
    /// one open compute pass for many kernels in a row (saves ≈ 1 ms of pass switching per kernel)
    cpass: Option<wgpu::ComputePass<'static>>,
    /// GPU errors caught before the job ([`pools::gpu_errors`]); one more at the end = discard the result
    errors_before: u64,
}

/// This many kernels per job are recorded when profiling.
const MAX_TIMESTAMPS: usize = 1024;

/// `GRAINEO_GPU_STAGES=1`: submit and wait after each stage so that the time per stage can be measured
/// (the total time rises slightly; for measurements only).
fn stage_timing() -> bool {
    STAGE_TIMING.load(std::sync::atomic::Ordering::Relaxed)
}

fn profiling() -> bool {
    static P: OnceLock<bool> = OnceLock::new();
    *P.get_or_init(|| std::env::var("GRAINEO_GPU_PROFILE").as_deref() == Ok("1"))
}

static STAGE_TIMING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Switch stage timing on/off (measurements; default off or `GRAINEO_GPU_STAGES=1`).
pub fn set_stage_timing(on: bool) {
    STAGE_TIMING.store(on, std::sync::atomic::Ordering::Relaxed);
}

impl<'a> Run<'a> {
    fn new(g: &'a Gpu, w: usize, h: usize) -> Self {
        let enc = g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rawdev") });
        let timestamp_qs = (profiling() && g.timestamps).then(|| g.device.create_query_set(&wgpu::QuerySetDescriptor { label: Some("time"), ty: wgpu::QueryType::Timestamp, count: (2 * MAX_TIMESTAMPS) as u32 }));
        let errors_before = pools::gpu_errors();
        Run { g, enc, w, h, empty: g.buffer(1), params: Vec::new(), started: std::time::Instant::now(), timestamp_qs, timer_names: Vec::new(), cpass: None, errors_before }
    }

    /// Kernel `name` with parameters (p[0..2] = width, height are set) on buffers a…f.
    fn dispatch(&mut self, name: &'static str, extra: &[(usize, f32)], bufs: &[&wgpu::Buffer]) {
        self.dispatch_dim(name, (self.w, self.h), extra, bufs);
    }

    /// Kernel with a finished parameter vector and its own work size (x, y, z).
    fn dispatch_p(&mut self, name: &'static str, p: &[f32], (dx, dy, dz): (usize, usize, usize), bufs: &[&wgpu::Buffer]) {
        // workgroup size: X-Trans kernels 32×8 (wide, contiguous rows), the others 16×16
        let (wgx, wgy) = if name.starts_with("xt_") { (32, 8) } else { (16, 16) };
        let pb = self.g.buffer_with(p);
        let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: pb.as_entire_binding() }];
        for i in 0..6 {
            let b = bufs.get(i).copied().unwrap_or(&self.empty);
            entries.push(wgpu::BindGroupEntry { binding: i as u32 + 1, resource: b.as_entire_binding() });
        }
        let bg = self.g.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &self.g.layout, entries: &entries });
        let pl = self.g.pipeline(name);
        // measurement per kernel (`GRAINEO_GPU_PROFILE=1`): GPU timestamps at the start and end of the pass
        let tw = self.timestamp_qs.as_ref().filter(|_| self.timer_names.len() < MAX_TIMESTAMPS).map(|qs| {
            let k = self.timer_names.len() as u32;
            wgpu::ComputePassTimestampWrites { query_set: qs, beginning_of_pass_write_index: Some(2 * k), end_of_pass_write_index: Some(2 * k + 1) }
        });
        let timed = tw.is_some();
        if timed {
            // measurement: one separate pass with timestamps per kernel
            self.cpass = None;
            let mut pass = self.enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(name), timestamp_writes: tw });
            pass.set_pipeline(&pl);
            pass.set_bind_group(0, &bg, &[]);
            pass.dispatch_workgroups(dx.div_ceil(wgx) as u32, dy.div_ceil(wgy) as u32, dz.max(1) as u32);
            drop(pass);
            self.timer_names.push(name);
        } else {
            if self.cpass.is_none() && std::env::var_os("GRAINEO_GPU_SINGLE").is_none() {
                self.cpass = Some(self.enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some("rawdev"), timestamp_writes: None }).forget_lifetime());
            }
            match self.cpass.as_mut() {
                Some(pass) => {
                    pass.set_pipeline(&pl);
                    pass.set_bind_group(0, &bg, &[]);
                    pass.dispatch_workgroups(dx.div_ceil(wgx) as u32, dy.div_ceil(wgy) as u32, dz.max(1) as u32);
                }
                None => {
                    // fallback (`GRAINEO_GPU_SINGLE=1`): one pass per kernel
                    let mut pass = self.enc.begin_compute_pass(&wgpu::ComputePassDescriptor { label: Some(name), timestamp_writes: None });
                    pass.set_pipeline(&pl);
                    pass.set_bind_group(0, &bg, &[]);
                    pass.dispatch_workgroups(dx.div_ceil(wgx) as u32, dy.div_ceil(wgy) as u32, dz.max(1) as u32);
                }
            }
        }
        self.params.push(pb);
    }

    /// End the open pass (before any other work on the command encoder).
    fn end_pass(&mut self) {
        self.cpass = None;
    }

    /// Fill a buffer with 0 (between two kernels).
    fn clear(&mut self, b: &wgpu::Buffer, bytes: u64) {
        self.end_pass();
        self.enc.clear_buffer(b, 0, Some(bytes));
    }

    /// Write the timestamp results (GPU time per kernel) into the collector; call before submitting.
    fn resolve_timestamps(&mut self) -> Option<(wgpu::Buffer, usize)> {
        self.end_pass();
        let qs = self.timestamp_qs.as_ref()?;
        let n = self.timer_names.len();
        if n == 0 {
            return None;
        }
        let large = (2 * n * 8) as u64;
        let res = self.g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: large, usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        let readback = self.g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: large, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        self.enc.resolve_query_set(qs, 0..(2 * n) as u32, &res, 0);
        self.enc.copy_buffer_to_buffer(&res, 0, &readback, 0, large);
        Some((readback, n))
    }

    /// After waiting: fetch the times from the readback buffer.
    fn read_timestamps(g: &Gpu, names: &[&'static str], readback: wgpu::Buffer, n: usize) {
        let sl = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        sl.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = g.device.poll(wgpu::PollType::wait_indefinitely());
        if !matches!(rx.recv(), Ok(Ok(()))) {
            return;
        }
        let Ok(data) = sl.get_mapped_range() else { return };
        let t: &[u64] = bytemuck::cast_slice(&data);
        let period = g.queue.get_timestamp_period() as f64; // ns per tick
        for k in 0..n {
            let (a, b) = (t[2 * k], t[2 * k + 1]);
            let ns = b.saturating_sub(a) as f64 * period;
            crate::timings::lap(&format!("kern {}", names[k]), std::time::Duration::from_nanos(ns as u64));
        }
        drop(data);
        readback.unmap();
    }

    /// Like [`Run::dispatch`], but with its own work size (e.g. the small grid of the luminance map).
    fn dispatch_dim(&mut self, name: &'static str, (dw, dh): (usize, usize), extra: &[(usize, f32)], bufs: &[&wgpu::Buffer]) {
        let mut p = vec![0f32; 72];
        p[0] = dw as f32;
        p[1] = dh as f32;
        for &(i, v) in extra {
            p[i] = v;
        }
        self.dispatch_p(name, &p, (dw, dh, 1), bufs);
    }

    /// Stage boundary for measurements: submit, wait, record time (only with `GRAINEO_GPU_STAGES=1`).
    fn stage_boundary(&mut self, name: &'static str) {
        if !stage_timing() {
            return;
        }
        self.end_pass();
        let new = self.g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rawdev") });
        let prev = std::mem::replace(&mut self.enc, new);
        self.g.queue.submit([prev.finish()]);
        let _ = self.g.device.poll(wgpu::PollType::wait_indefinitely());
        crate::timings::lap(name, self.started.elapsed());
        self.started = std::time::Instant::now();
    }

    /// Submit what is recorded so far, wait and read back the first `n` values of `buf`; the job continues with a
    /// new command buffer (buffers stay on the GPU). For small intermediate results the CPU needs mid-job.
    /// `None`: the GPU did not deliver (device lost, out of memory) – the caller falls back to the CPU.
    fn read_back(&mut self, buf: &wgpu::Buffer, n: usize) -> Option<Vec<f32>> {
        self.end_pass();
        let bytes = (n.max(1) * 4) as u64;
        let readback = self.g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: bytes, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let new = self.g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("rawdev") });
        let mut prev = std::mem::replace(&mut self.enc, new);
        prev.copy_buffer_to_buffer(buf, 0, &readback, 0, bytes);
        self.g.queue.submit([prev.finish()]);
        let sl = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        sl.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.g.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok()?.ok()?;
        // a GPU error during the job (caught instead of a panic): the result is not trustworthy
        if pools::gpu_errors() != self.errors_before {
            return None;
        }
        let data = sl.get_mapped_range().ok()?;
        let v: Vec<f32> = bytemuck::cast_slice::<u8, f32>(&data).get(..n)?.to_vec();
        drop(data);
        readback.unmap();
        Some(v)
    }

    /// Submit and read back the buffer `rgb` (3·w·h).
    fn finish_rgb(mut self, rgb: &wgpu::Buffer) -> Option<Vec<[f32; 3]>> {
        let n = self.w * self.h * 3;
        let log = std::env::var_os("GRAINEO_LOG").is_some();
        let t = std::time::Instant::now();
        let zt = self.resolve_timestamps();
        self.g.queue.submit([self.enc.finish()]);
        let _ = self.g.device.poll(wgpu::PollType::wait_indefinitely());
        if let Some((l, n)) = zt {
            Run::read_timestamps(self.g, &self.timer_names, l, n);
        }
        if !stage_timing() {
            crate::timings::lap("GPU Rechnen (Detail, ein Auftrag)", self.started.elapsed());
        }
        if log {
            eprintln!("gpu: compute {:.0?}", t.elapsed());
        }
        let t = std::time::Instant::now();
        let readback = self.g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (n * 4) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = self.g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_buffer_to_buffer(rgb, 0, &readback, 0, (n * 4) as u64);
        self.g.queue.submit([enc.finish()]);
        let sl = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        sl.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.g.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok()?.ok()?;
        // a GPU error during the job (caught instead of a panic): the result is not trustworthy
        if pools::gpu_errors() != self.errors_before {
            return None;
        }
        if log {
            eprintln!("gpu: copy back {:.0?}", t.elapsed());
        }
        let t = std::time::Instant::now();
        let data = sl.get_mapped_range().ok()?;
        let out: Vec<[f32; 3]> = bytemuck::cast_slice::<u8, [f32; 3]>(&data).to_vec();
        drop(data);
        readback.unmap();
        if log {
            eprintln!("gpu: read out {:.0?}", t.elapsed());
        }
        Some(out)
    }
}

impl Run<'_> {
    /// Submit and read back the 16-bit output buffer (two values per word) as 3·n u16.
    fn finish_u16(self, out: &wgpu::Buffer, n: usize) -> Option<Vec<u16>> {
        let v = self.finish_words(out, (n * 3).div_ceil(2))?;
        Some(bytemuck::cast_slice::<u32, u16>(&v)[..n * 3].to_vec())
    }

    /// Submit and read back the output: 16-bit encoded, or the scene (f32, three per pixel) for the film.
    fn finish_out(self, out: &wgpu::Buffer, n: usize, scene: bool) -> Option<Developed> {
        if scene {
            let v = self.finish_words(out, n * 3)?;
            Some(Developed::Scene(v.into_iter().map(f32::from_bits).collect()))
        } else {
            Some(Developed::Encoded(self.finish_u16(out, n)?))
        }
    }

    /// Submit and read back the first `words` 32-bit words of `out`.
    fn finish_words(mut self, out: &wgpu::Buffer, words: usize) -> Option<Vec<u32>> {
        let log = std::env::var_os("GRAINEO_LOG").is_some();
        let t = std::time::Instant::now();
        let zt = self.resolve_timestamps();
        self.g.queue.submit([self.enc.finish()]);
        let _ = self.g.device.poll(wgpu::PollType::wait_indefinitely());
        if let Some((l, n)) = zt {
            Run::read_timestamps(self.g, &self.timer_names, l, n);
        }
        if !stage_timing() {
            crate::timings::lap("GPU Rechnen (Detail + Ausgabe, ein Auftrag)", self.started.elapsed());
        }
        if log {
            eprintln!("gpu: compute {:.0?}", t.elapsed());
        }
        let t = std::time::Instant::now();
        let readback = self.g.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: (words * 4) as u64, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let mut enc = self.g.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("readback") });
        enc.copy_buffer_to_buffer(out, 0, &readback, 0, (words * 4) as u64);
        self.g.queue.submit([enc.finish()]);
        let sl = readback.slice(..);
        let (tx, rx) = std::sync::mpsc::channel();
        sl.map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        let _ = self.g.device.poll(wgpu::PollType::wait_indefinitely());
        rx.recv().ok()?.ok()?;
        // a GPU error during the job (caught instead of a panic): the result is not trustworthy
        if pools::gpu_errors() != self.errors_before {
            return None;
        }
        let data = sl.get_mapped_range().ok()?;
        let v: Vec<u32> = bytemuck::cast_slice::<u8, u32>(&data).get(..words)?.to_vec();
        drop(data);
        readback.unmap();
        crate::timings::lap("GPU Zurücklesen", t.elapsed());
        if log {
            eprintln!("gpu: copy back+read out {:.0?}", t.elapsed());
        }
        Some(v)
    }
}

/// What [`detail`] should do.
pub struct Job<'a> {
    pub noise: Option<raw::noise::Parameter>,
    /// Base sharpening: (strength, PSF kernel, iterations, threshold, clip)
    pub sharpen: Option<(f32, Vec<f32>, usize, f32, f32)>,
    pub lens: Option<&'a raw::lens::Grid>,
}

const B3: [f32; 5] = [1.0 / 16.0, 4.0 / 16.0, 6.0 / 16.0, 4.0 / 16.0, 1.0 / 16.0];
const BOX: [f32; 5] = [0.2; 5];

/// Queue the kernels of the detail stage (noise reduction, base sharpening, lens correction) into job `l`.
/// `rgb` is the image (interleaved, 3·w·h); returns: buffer with the result (may be a new one).
fn detail_kernels(g: &Gpu, l: &mut Run, rgb: wgpu::Buffer, w: usize, h: usize, a: &Job) -> wgpu::Buffer {
    let n = w * h;
    let only = std::env::var("GRAINEO_GPU_ONLY").unwrap_or_default();
    let part = |t: &str| only.is_empty() || only == t;
    // --- Noise reduction (rauschen.rs: anwenden / ebene_entrauschen) ---
    if let Some(r) = a.noise.as_ref().filter(|_| part("nr")) {
        let ans = [(3usize, r.a), (4, r.s2), (5, if r.gauss.is_some() { 1.0 } else { 0.0 }), (6, r.gauss.unwrap_or(1.0))];
        let planes = [g.buffer(n), g.buffer(n), g.buffer(n)];
        l.dispatch("yuv_vor", &ans, &[&rgb, &planes[0], &planes[1], &planes[2]]);
        let (cur, next, tmp, en) = (g.buffer(n), g.buffer(n), g.buffer(n), g.buffer(n));
        for (k, thresholds) in [&r.y, &r.u, &r.v].into_iter().enumerate() {
            if thresholds.is_empty() {
                continue;
            }
            let p = &planes[k];
            // cur = p, p = 0
            l.dispatch("umlegen", &[], &[p, &cur]);
            let (mut cur, mut next) = (&cur, &next);
            for (lv, &t) in thresholds.iter().enumerate() {
                let step = (1usize << lv) as f32;
                let taps = |kk: &[f32; 5], extra: &[(usize, f32)]| -> Vec<(usize, f32)> {
                    let mut v = vec![(2usize, step)];
                    v.extend(kk.iter().enumerate().map(|(i, &x)| (8 + i, x)));
                    v.extend_from_slice(extra);
                    v
                };
                // smoothed = B3 horizontal, then vertical; detail and energy in the same pass
                l.dispatch("filt_h", &taps(&B3, &[]), &[cur, &tmp]);
                l.dispatch("nr_v_detail", &taps(&B3, &[(3, if t > 0.0 { 1.0 } else { 0.0 })]), &[&tmp, cur, next, &en, p]);
                if t > 0.0 {
                    l.dispatch("filt_h", &taps(&BOX, &[]), &[&en, &tmp]);
                    l.dispatch("nr_v_schrumpfen", &taps(&BOX, &[(3, t * t)]), &[&tmp, cur, p]);
                }
                std::mem::swap(&mut cur, &mut next);
            }
            l.dispatch("addieren", &[], &[cur, &en, p]);
        }
        l.dispatch("yuv_rueck", &ans, &[&rgb, &planes[0], &planes[1], &planes[2]]);
        l.stage_boundary("GPU Rauschminderung");
    }
    // --- Base sharpening (schaerfe.rs: anwenden) ---
    if let Some((strength, kernel, iter, threshold, clip)) = a.sharpen.as_ref().filter(|_| part("schaerfe")) {
        let (y, est, tmp, bb, b2) = (g.buffer(n), g.buffer(n), g.buffer(n), g.buffer(n), g.buffer(n));
        let mut kp = vec![(4usize, kernel.len() as f32)];
        kp.extend(kernel.iter().enumerate().map(|(i, &x)| (16 + i, x)));
        l.dispatch("luminanz", &[], &[&rgb, &y, &est]);
        for _ in 0..*iter {
            l.dispatch("gauss_h", &kp, &[&est, &tmp]);
            l.dispatch("gauss_v", &kp, &[&tmp, &bb]);
            l.dispatch("teilen", &[], &[&y, &bb]);
            l.dispatch("gauss_h", &kp, &[&bb, &tmp]);
            l.dispatch("gauss_v", &kp, &[&tmp, &b2]);
            l.dispatch("mal_begrenzt", &[], &[&est, &b2]);
        }
        l.dispatch("schaerfen", &[(5, *strength), (6, *threshold), (7, *clip)], &[&rgb, &y, &est]);
        l.stage_boundary("GPU Schärfe");
    }
    // --- Lens correction (objektiv.rs: anwenden_skaliert, full resolution) ---
    let mut result_buf = rgb;
    if let Some(gi) = a.lens.filter(|_| part("objektiv")) {
        let gp = [(3usize, gi.advance as f32), (4, gi.gw as f32), (5, gi.gh as f32), (6, gi.split[0].0 as f32), (7, gi.split[0].1 as f32), (8, gi.split[1].0 as f32), (9, gi.split[1].1 as f32)];
        if let Some(fk) = &gi.factor {
            let fb = g.buffer_with(fk);
            l.dispatch("vignette", &gp, &[&result_buf, &fb]);
        }
        if let Some(q) = &gi.source {
            let qf: Vec<f32> = q.iter().flatten().copied().collect();
            let qb = g.buffer_with(&qf);
            let lz: Vec<f32> = raw::lens::lanczos_weights().iter().flatten().copied().collect();
            let lb = g.buffer_with(&lz);
            let dest = g.image_buffer(None, n * 3);
            l.dispatch("geometrie", &gp, &[&result_buf, &qb, &dest, &lb]);
            result_buf = dest;
        }
        l.stage_boundary("GPU Objektiv");
    }
    result_buf
}

/// Noise reduction, base sharpening, lens correction on the GPU. None: no GPU or image too large
/// (then the CPU computes).
pub fn detail(px: &[[f32; 3]], w: usize, h: usize, a: &Job) -> Option<Vec<[f32; 3]>> {
    let g = gpu()?;
    let n = w * h;
    if (n * 3 * 4) as u64 > g.max_buffer || w < 16 || h < 16 || !pools::gpu_fits_memory(n) {
        return None;
    }
    let log = std::env::var_os("GRAINEO_LOG").is_some();
    let t0 = std::time::Instant::now();
    let flatten_table: &[f32] = bytemuck::cast_slice(px);
    let rgb = g.image_buffer(Some(flatten_table), n * 3);
    let mut l = Run::new(g, w, h);
    crate::timings::lap("GPU Hochladen", t0.elapsed());
    l.started = std::time::Instant::now();
    if log {
        eprintln!("gpu: upload {:.0?}", t0.elapsed());
    }
    let result_buf = detail_kernels(g, &mut l, rgb, w, h, a);
    if log {
        eprintln!("gpu: built {:.0?} ({} kernels)", t0.elapsed(), l.params.len());
    }
    let r = l.finish_rgb(&result_buf)?;
    if log {
        eprintln!("gpu: done {:.0?}", t0.elapsed());
    }
    Some(r)
}

/// Mosaic (X-Trans) → demosaic → crop → detail stage → output stage in one job, without the
/// demosaiced image ever coming back to the CPU. `None`: no GPU, pattern/buffers do not fit – the CPU computes.
pub fn full_mosaic(m: &raw::development::MosaicPart, a: &Job, out_data: &OutputData) -> Option<Developed> {
    let g = gpu()?;
    if m.sensor != raw::Sensor::XTrans || m.norm.len() != m.w * m.h {
        return None;
    }
    let r = m.crop;
    let n = r.w * r.h;
    if (n * 3 * 4) as u64 > g.max_buffer || r.w < 16 || r.h < 16 || !pools::gpu_fits_memory(n) {
        return None;
    }
    let log = std::env::var_os("GRAINEO_LOG").is_some();
    let t0 = std::time::Instant::now();
    let mos = g.image_buffer(Some(&m.norm), m.w * m.h);
    crate::timings::lap("GPU Hochladen", t0.elapsed());
    let mut l = Run::new(g, r.w, r.h);
    l.started = std::time::Instant::now();
    let full = xtrans::kernels(g, &mut l, &mos, m.w, m.h, m.pat)?;
    l.stage_boundary("GPU Demosaik");
    let rgb = if (r.x, r.y, r.w, r.h) == (0, 0, m.w, m.h) {
        full
    } else {
        let z = g.image_buffer(None, n * 3);
        l.dispatch("zuschneiden", &[(3, r.x as f32), (4, r.y as f32), (5, m.w as f32)], &[&full, &z]);
        z
    };
    let result_buf = detail_kernels(g, &mut l, rgb, r.w, r.h, a);
    let out = output_kernels(g, &mut l, &result_buf, r.w, r.h, out_data)?;
    if log {
        eprintln!("gpu: built {:.0?} ({} kernels)", t0.elapsed(), l.params.len());
    }
    let res = l.finish_out(&out, n, out_data.scene)?;
    if log {
        eprintln!("gpu: done {:.0?}", t0.elapsed());
    }
    Some(res)
}

/// Everything the output stage needs from the sliders (see `render::Params`).
pub struct OutputData {
    /// camera → working space incl. white balance and exposure (row by row)
    pub m: [[f32; 3]; 3],
    /// luminance weights of the working space
    pub lum: [f32; 3],
    /// working space → linear sRGB
    pub ws_srgb: [[f32; 3]; 3],
    /// luminance weights on the camera RGB (luminance map)
    pub camera_lum: [f32; 3],
    /// tone curve (8193 values) and its hue/saturation parameters
    pub tone: Vec<f32>,
    pub hue: f32,
    pub curve_saturation: f32,
    pub ev: f32,
    pub shadows: f32,
    pub highlights: f32,
    pub whites: f32,
    /// highlight roll of a lowered Whites slider: reach in stops (0 = off) and scene white
    pub roll_reach: f32,
    pub roll_white: f32,
    pub blacks: f32,
    pub saturation: f32,
    pub vibrance: f32,
    pub mono: bool,
    /// standard look (33³ × 3, absolute)
    pub std_look: Option<Vec<f32>>,
    /// camera look: edge length and differences (n³ × 3)
    pub look: Option<(usize, Vec<f32>)>,
    pub look_amount: f32,
    /// tetrahedral instead of trilinear interpolation of the camera look (learned looks)
    pub look_tetrahedral: bool,
    /// Dehaze: map (width × height, whole image), airlight, ω, gain, removes haze, lower bound of t
    pub haze: Option<GpuHaze>,
    /// Noise brake (render.rs `prepare`): the base layer is computed on the GPU from the image of this job
    pub brake: Option<GpuBrake>,
    /// Film: deliver the scene-linear sRGB (render.rs `scene_linear`) instead of the encoded 16-bit image
    pub scene: bool,
}

/// Result of the output stage on the GPU.
pub enum Developed {
    /// finished 16-bit sRGB (3·w·h)
    Encoded(Vec<u16>),
    /// scene-linear sRGB for the film simulation (3·w·h, [`OutputData::scene`])
    Scene(Vec<f32>),
}

impl Developed {
    pub fn encoded(self) -> Option<Vec<u16>> {
        match self {
            Developed::Encoded(v) => Some(v),
            Developed::Scene(_) => None,
        }
    }
    pub fn scene(self) -> Option<Vec<f32>> {
        match self {
            Developed::Scene(v) => Some(v),
            Developed::Encoded(_) => None,
        }
    }
}

/// Noise-brake parameters for the GPU output stage (see `develop::lift::Brake`).
#[derive(Clone, Copy)]
pub struct GpuBrake {
    /// exposure slider in EV (the part of the brightening that comes from exposure)
    pub exposure: f32,
    pub luma_exp: f32,
    pub chroma_exp: f32,
}

/// Dehaze parameters for the GPU output stage (see `develop::dehaze`).
pub struct GpuHaze {
    pub dark: Vec<f32>,
    pub w: usize,
    pub h: usize,
    pub a: [f32; 3],
    pub omega: f32,
    pub gain: f32,
    pub remove: bool,
    pub t0: f32,
}

/// Detail stage and output stage in one job: the image goes up once, what comes back is the finished
/// 16-bit sRGB (3·w·h values). None: no GPU, image too large – then the CPU computes.
pub fn full(px: &[[f32; 3]], w: usize, h: usize, a: &Job, out_data: &OutputData) -> Option<Developed> {
    let g = gpu()?;
    let n = w * h;
    if (n * 3 * 4) as u64 > g.max_buffer || w < 16 || h < 16 || !pools::gpu_fits_memory(n) {
        return None;
    }
    let log = std::env::var_os("GRAINEO_LOG").is_some();
    let t0 = std::time::Instant::now();
    let flatten_table: &[f32] = bytemuck::cast_slice(px);
    let rgb = g.image_buffer(Some(flatten_table), n * 3);
    let mut l = Run::new(g, w, h);
    if log {
        eprintln!("gpu: upload {:.0?}", t0.elapsed());
    }
    crate::timings::lap("GPU Hochladen", t0.elapsed());
    l.started = std::time::Instant::now();
    let result_buf = detail_kernels(g, &mut l, rgb, w, h, a);
    let out = output_kernels(g, &mut l, &result_buf, w, h, out_data)?;
    if log {
        eprintln!("gpu: built {:.0?} ({} kernels)", t0.elapsed(), l.params.len());
    }
    let r = l.finish_out(&out, n, out_data.scene)?;
    if log {
        eprintln!("gpu: done {:.0?}", t0.elapsed());
    }
    Some(r)
}

/// Output stage only, for a detail stage that is already computed (cached stage, zoomed view): the same kernels as
/// in [`full`], so the result equals the export bit for bit when the stage came from [`detail`]. None: no GPU.
pub fn output(px: &[[f32; 3]], w: usize, h: usize, out_data: &OutputData) -> Option<Developed> {
    let g = gpu()?;
    let n = w * h;
    if (n * 3 * 4) as u64 > g.max_buffer || w < 16 || h < 16 || !pools::gpu_fits_memory(n) {
        return None;
    }
    let t0 = std::time::Instant::now();
    let rgb = g.image_buffer(Some(bytemuck::cast_slice(px)), n * 3);
    crate::timings::lap("GPU Hochladen", t0.elapsed());
    output_from(g, &rgb, w, h, out_data, None)
}

/// A stage kept on the GPU (the editable preview while sliders move): each slider step then only uploads the
/// parameters and tables instead of the whole image. The output kernels only read it.
pub struct Resident {
    buf: wgpu::Buffer,
    pub w: usize,
    pub h: usize,
    /// tone table + dehaze map of this stage (the map is ≈ 11 MB at preview size and only depends on the stage):
    /// key (map size, airlight), buffer. While dehaze is active only the tone table (32 KB) is written per step;
    /// the lock is held until the result is back, so that concurrent renders do not overwrite each other's table.
    haze: Mutex<Option<((usize, usize, [u32; 3]), wgpu::Buffer)>>,
}

/// Upload a stage once for [`output_resident`]. None: no GPU or image too large.
pub fn upload(px: &[[f32; 3]], w: usize, h: usize) -> Option<Resident> {
    let g = gpu()?;
    if (w * h * 3 * 4) as u64 > g.max_buffer || w < 16 || h < 16 || px.len() != w * h || !pools::gpu_fits_memory(w * h) {
        return None;
    }
    let t0 = std::time::Instant::now();
    let buf = g.image_buffer(Some(bytemuck::cast_slice(px)), w * h * 3);
    crate::timings::lap("GPU Hochladen (einmal je Stufe)", t0.elapsed());
    Some(Resident { buf, w, h, haze: Mutex::new(None) })
}

/// Like [`output`] on a stage already on the GPU ([`upload`]): the same kernels, the same result.
pub fn output_resident(r: &Resident, out_data: &OutputData) -> Option<Developed> {
    let g = gpu()?;
    let Some(hz) = &out_data.haze else { return output_from(g, &r.buf, r.w, r.h, out_data, None) };
    let key = (hz.w, hz.h, hz.a.map(f32::to_bits));
    let mut cached = r.haze.lock().unwrap_or_else(|e| e.into_inner());
    match cached.as_ref() {
        Some((k, b)) if *k == key && b.size() == ((out_data.tone.len() + hz.dark.len()) * 4) as u64 => {
            g.queue.write_buffer(b, 0, bytemuck::cast_slice(&out_data.tone));
        }
        _ => *cached = Some((key, g.buffer_with(&tone_table(out_data)))),
    }
    let buf = cached.as_ref().map(|(_, b)| b.clone());
    output_from(g, &r.buf, r.w, r.h, out_data, buf.as_ref())
}

fn output_from(g: &Gpu, rgb: &wgpu::Buffer, w: usize, h: usize, out_data: &OutputData, tone_buf: Option<&wgpu::Buffer>) -> Option<Developed> {
    let mut l = Run::new(g, w, h);
    l.started = std::time::Instant::now();
    let out = output_kernels_with(g, &mut l, rgb, w, h, out_data, tone_buf)?;
    l.finish_out(&out, w * h, out_data.scene)
}

/// Base layer of the noise brake on the GPU (`develop::lift::base_layer`, radius 6, k 3; above 4 MP at half size
/// with radius 3). The noise model is estimated on the CPU from the small luminance (one short readback mid-job).
/// Returns: buffer (4 values per pixel), width, height, factor.
fn brake_base_kernels(g: &Gpu, l: &mut Run, rgb: &wgpu::Buffer, w: usize, h: usize) -> Option<(wgpu::Buffer, usize, usize, usize)> {
    let half = w * h > develop::lift::BASE_HALF_ABOVE;
    let (f, r) = if half { (2, 3usize) } else { (1, 6usize) };
    let (sw, sh) = (w / f, h / f);
    let sn = sw * sh;
    let (sb, qb, tb, yb) = (g.buffer(sn * 4), g.buffer(sn * 4), g.buffer(sn * 4), g.buffer(sn));
    let dim = (sw, sh);
    l.dispatch_dim("aus_bb_klein", dim, &[(2, w as f32), (3, f as f32)], &[rgb, &sb, &qb, &yb]);
    // noise model on the CPU from the small luminance (exactly as `develop::lift`; reading back costs a few ms)
    let t = std::time::Instant::now();
    let yv = l.read_back(&yb, sn)?;
    let (na, nb) = develop::lift::noise_model(&yv, sw, sh);
    crate::timings::lap("GPU Rauschbremse: Rauschmodell (CPU)", t.elapsed());
    if std::env::var_os("GRAINEO_LIFT_INFO").is_some() {
        eprintln!("GPU base layer {sw}×{sh}: noise var(Y) = {na:.4e}·Y + {nb:.4e}");
    }
    let boxed = |l: &mut Run, b: &wgpu::Buffer| {
        l.dispatch_dim("aus_bb_box_h", dim, &[(2, r as f32)], &[b, &tb]);
        l.dispatch_dim("aus_bb_box_v", dim, &[(2, r as f32)], &[&tb, b]);
    };
    boxed(l, &sb);
    boxed(l, &qb);
    l.dispatch_dim("aus_bb_ab", dim, &[(2, 3.0), (3, na), (4, nb)], &[&sb, &qb]);
    boxed(l, &sb);
    boxed(l, &qb);
    l.dispatch_dim("aus_bb_basis", dim, &[], &[&sb, &qb, &yb]);
    l.stage_boundary("GPU Rauschbremse (Basisebene)");
    Some((sb, sw, sh, f))
}

/// Luminance map and pixel kernel of the output stage; returns: output buffer (16 bit, two per word).
fn output_kernels(g: &Gpu, l: &mut Run, rgb: &wgpu::Buffer, w: usize, h: usize, out_data: &OutputData) -> Option<wgpu::Buffer> {
    output_kernels_with(g, l, rgb, w, h, out_data, None)
}

/// Tone table followed by the dehaze map (if any): the contents of the tone buffer of [`output_kernels`].
fn tone_table(out_data: &OutputData) -> Vec<f32> {
    match &out_data.haze {
        Some(hz) => out_data.tone.iter().chain(hz.dark.iter()).copied().collect(),
        None => out_data.tone.clone(),
    }
}

/// [`output_kernels`] with an optional finished tone buffer ([`tone_table`]).
/// `None`: the GPU did not deliver the noise model's readback (device lost) – the CPU computes.
fn output_kernels_with(g: &Gpu, l: &mut Run, rgb: &wgpu::Buffer, w: usize, h: usize, out_data: &OutputData, tone_buf: Option<&wgpu::Buffer>) -> Option<wgpu::Buffer> {
    let n = w * h;
    // luminance map (doc.rs: Stufe::maske): block mean of log2(luminance), 3× box filter
    let k = (w.max(h) / 128).max(1);
    let (sw, sh) = (w.div_ceil(k), h.div_ceil(k));
    let (g1, g2) = (g.buffer(sw * sh), g.buffer(sw * sh));
    let kl = out_data.camera_lum;
    // work size = grid; image width/height are therefore given explicitly in p[0], p[1] (they override the work size)
    l.dispatch_dim("aus_mask_summe", (sw, sh), &[(0, w as f32), (1, h as f32), (2, k as f32), (3, sw as f32), (4, sh as f32), (5, kl[0]), (6, kl[1]), (7, kl[2])], &[rgb, &g1]);
    for _ in 0..3 {
        l.dispatch_dim("aus_box_h", (sw, sh), &[], &[&g1, &g2]);
        l.dispatch_dim("aus_box_v", (sw, sh), &[], &[&g2, &g1]);
    }
    l.stage_boundary("GPU Helligkeitskarte");
    // tables
    // tone table, followed by the dehaze map (if any) in the same buffer (prepared by the caller for a stage kept on the GPU)
    let tone = match tone_buf {
        Some(b) => b.clone(),
        None => g.buffer_with(&tone_table(out_data)),
    };
    // standard look, followed by the camera look in the same buffer
    let std_part: &[f32] = out_data.std_look.as_deref().unwrap_or(&[0.0]);
    let look_offset = std_part.len();
    let looks: Vec<f32> = std_part.iter().chain(out_data.look.as_ref().map(|l| l.1.as_slice()).unwrap_or(&[0.0])).copied().collect();
    let looks = g.buffer_with(&looks);
    let brake = match out_data.brake {
        Some(b) => Some((b, brake_base_kernels(g, l, rgb, w, h)?)),
        None => None,
    };
    let out = g.buffer(if out_data.scene { n * 3 } else { (n * 3).div_ceil(2) });
    l.end_pass();
    l.enc.clear_buffer(&out, 0, None);
    let mut e: Vec<(usize, f32)> = vec![
        (2, k as f32),
        (3, sw as f32),
        (4, sh as f32),
        (5, kl[0]),
        (6, kl[1]),
        (7, kl[2]),
        (8, out_data.ev),
        (9, out_data.shadows),
        (10, out_data.highlights),
        (11, out_data.whites),
        (12, out_data.blacks),
        (13, out_data.saturation),
        (14, out_data.vibrance),
        (15, out_data.mono as u8 as f32),
        (16, out_data.look.as_ref().map_or(0.0, |l| l.0 as f32)),
        (17, out_data.look_amount),
        (18, out_data.std_look.is_some() as u8 as f32),
        (19, out_data.hue),
        (20, out_data.curve_saturation),
        (21, out_data.lum[0]),
        (22, out_data.lum[1]),
        (23, out_data.lum[2]),
        (61, look_offset as f32),
        (62, out_data.look_tetrahedral as u8 as f32),
        (63, out_data.roll_reach),
        (64, out_data.roll_white),
    ];
    if let Some((b, (_, bw, bh, f))) = &brake {
        e.extend_from_slice(&[(54, 1.0), (55, b.luma_exp), (56, b.chroma_exp), (57, b.exposure), (58, *f as f32), (59, *bw as f32), (60, *bh as f32)]);
    }
    if let Some(hz) = &out_data.haze {
        e.extend_from_slice(&[(43, 1.0), (44, hz.w as f32), (45, hz.h as f32), (46, hz.a[0]), (47, hz.a[1]), (48, hz.a[2]), (49, hz.omega), (50, hz.gain), (51, hz.remove as u8 as f32), (52, hz.t0), (53, out_data.tone.len() as f32)]);
    }
    for r in 0..3 {
        for c in 0..3 {
            e.push((24 + r * 3 + c, out_data.m[r][c]));
            e.push((33 + r * 3 + c, out_data.ws_srgb[r][c]));
        }
    }
    let base_buf = brake.as_ref().map_or(&l.empty, |(_, (buf, ..))| buf).clone();
    l.dispatch(if out_data.scene { "aus_szene" } else { "aus_pixel" }, &e, &[rgb, &g1, &tone, &looks, &base_buf, &out]);
    l.stage_boundary("GPU Ausgabe");
    Some(out)
}

/// Largest and mean deviation of two images (for tests and measurements).
pub fn compare(a: &[[f32; 3]], b: &[[f32; 3]]) -> (f32, f32) {
    let mut max = 0f32;
    let mut sum = 0f64;
    for (p, q) in a.iter().zip(b) {
        for c in 0..3 {
            let d = (p[c] - q[c]).abs();
            max = max.max(d);
            sum += d as f64;
        }
    }
    (max, (sum / (a.len() * 3).max(1) as f64) as f32)
}

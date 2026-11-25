//! The two heaviest parts of the high-ISO chain on the graphics card: the range filter of the first stage
//! (`window.wgsl`, windows up to 31 × 31 cells) and the reconstruction of the full resolution (`upscale.wgsl`:
//! smoothing of the top layer and the joint filter). Each returns `None` when the card is not allowed, not
//! there, too small for the image, or does not answer in time; the processor computes then.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use wgpu::util::DeviceExt;

use super::Px;

/// May the chain use the graphics card? (`GRAINEO_QUATTRO_GPU=0`: processor only.)
pub fn allowed() -> bool {
    pools::gpu_allowed() && std::env::var("GRAINEO_QUATTRO_GPU").map_or(true, |v| v != "0") && !GAVE_UP.load(Ordering::Relaxed)
}

/// Is a usable graphics card there (and allowed)?
pub fn available() -> bool {
    card().is_some()
}

/// Set once the card failed to answer in time: every later job goes to the processor.
static GAVE_UP: AtomicBool = AtomicBool::new(false);

/// Longest wait for one job.
const PATIENCE: Duration = Duration::from_secs(20);

/// Edge of a workgroup (see `window.wgsl`).
const GROUP: u32 = 16;

struct Card {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    range_mean: wgpu::ComputePipeline,
    upscale_layout: wgpu::BindGroupLayout,
    /// smoothing of the top layer, joint filter
    upscale: [wgpu::ComputePipeline; 2],
    largest_buffer: u64,
    /// one job at a time
    busy: Mutex<()>,
}

static CARD: OnceLock<Option<Card>> = OnceLock::new();

fn card() -> Option<&'static Card> {
    if !allowed() {
        return None;
    }
    CARD.get_or_init(Card::open).as_ref()
}

impl Card {
    fn open() -> Option<Card> {
        const NAME: &str = "Quattro high-ISO chain";
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
        // a software renderer is slower than the processor path
        if adapter.get_info().device_type == wgpu::DeviceType::Cpu {
            return None;
        }
        let offered = adapter.limits();
        let mut limits = wgpu::Limits::default();
        limits.max_storage_buffer_binding_size = offered.max_storage_buffer_binding_size;
        limits.max_buffer_size = offered.max_buffer_size;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some(NAME),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        // count errors instead of wgpu's default panic; jobs compare the count and fall back
        device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| pools::gpu_error(NAME, &e.to_string())));
        device.set_device_lost_callback(|reason, message| {
            if reason != wgpu::DeviceLostReason::Destroyed {
                pools::gpu_lost(NAME, &message);
            }
        });
        let entries: Vec<wgpu::BindGroupLayoutEntry> = (0..5u32)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: binding < 4 }, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some(NAME), entries: &entries });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&layout)], immediate_size: 0 });
        let scope = device.push_error_scope(wgpu::ErrorFilter::Validation);
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some(NAME), source: wgpu::ShaderSource::Wgsl(include_str!("window.wgsl").into()) });
        let range_mean = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("range_mean"),
            layout: Some(&pipeline_layout),
            module: &module,
            entry_point: Some("range_mean"),
            compilation_options: Default::default(),
            cache: None,
        });
        let upscale_entries: Vec<wgpu::BindGroupLayoutEntry> = (0..7u32)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer { ty: wgpu::BufferBindingType::Storage { read_only: binding < 5 }, has_dynamic_offset: false, min_binding_size: None },
                count: None,
            })
            .collect();
        let upscale_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor { label: Some("upscale"), entries: &upscale_entries });
        let upscale_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor { label: None, bind_group_layouts: &[Some(&upscale_layout)], immediate_size: 0 });
        let upscale_module = device.create_shader_module(wgpu::ShaderModuleDescriptor { label: Some("upscale"), source: wgpu::ShaderSource::Wgsl(include_str!("upscale.wgsl").into()) });
        let upscale = ["calm_top", "join"].map(|entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&upscale_pipeline_layout),
                module: &upscale_module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        });
        if let Some(e) = pollster::block_on(scope.pop()) {
            eprintln!("{NAME}: shader not usable, the processor computes: {e}");
            return None;
        }
        let largest_buffer = (offered.max_storage_buffer_binding_size as u64).min(offered.max_buffer_size);
        Some(Card { device, queue, layout, range_mean, upscale_layout, upscale, largest_buffer, busy: Mutex::new(()) })
    }

    /// Waits for the submitted job and the mapping of `slice`, at most [`PATIENCE`]. `None` on any failure.
    fn wait_for(&self, slice: &wgpu::BufferSlice, scope: wgpu::ErrorScopeGuard, errors_before: u64) -> Option<()> {
        let (done, mapped) = std::sync::mpsc::channel();
        slice.map_async(wgpu::MapMode::Read, move |r| {
            let _ = done.send(r);
        });
        if self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(PATIENCE) }).is_err() {
            // the driver hangs or the device is gone: no more jobs on the card in this run
            GAVE_UP.store(true, Ordering::Relaxed);
            eprintln!("Quattro high-ISO chain: the graphics card did not answer within {PATIENCE:?}, the processor computes");
            return None;
        }
        if let Some(e) = pollster::block_on(scope.pop()) {
            eprintln!("Quattro high-ISO chain: job on the graphics card failed, the processor computes: {e}");
            return None;
        }
        mapped.recv_timeout(PATIENCE).ok()?.ok()?;
        (pools::gpu_errors() == errors_before).then_some(())
    }

    /// The range filter over a `w × h` image: `image` and `factors` hold three values per cell, `curve` is the
    /// weight curve. Returns three values per cell.
    fn run(&self, w: usize, h: usize, radius: usize, image: &[Px], factors: &[Px], curve: &[f32]) -> Option<Vec<Px>> {
        let cells = w * h;
        let bytes = (cells * 12) as u64;
        if cells == 0 || image.len() != cells || factors.len() != cells || curve.is_empty() || bytes > self.largest_buffer || !fits_memory(4 * cells) {
            return None;
        }
        let _one = self.busy.lock().unwrap_or_else(|e| e.into_inner());
        let errors_before = pools::gpu_errors();
        let storage = wgpu::BufferUsages::STORAGE;
        let filled = |data: &[u8]| self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: data, usage: storage });
        let job = self.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&[w as u32, h as u32, radius as u32, 0u32]),
            usage: storage | wgpu::BufferUsages::COPY_DST,
        });
        let (image_buffer, factor_buffer, curve_buffer) = (filled(bytemuck::cast_slice(image)), filled(bytemuck::cast_slice(factors)), filled(bytemuck::cast_slice(curve)));
        let result = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: bytes, usage: storage | wgpu::BufferUsages::COPY_SRC, mapped_at_creation: false });
        let readback = self.device.create_buffer(&wgpu::BufferDescriptor { label: None, size: bytes, usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST, mapped_at_creation: false });
        let buffers = [&job, &image_buffer, &factor_buffer, &curve_buffer, &result];
        let entries: Vec<wgpu::BindGroupEntry> = buffers.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect();
        let bind = self.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &self.layout, entries: &entries });
        let scope = self.device.push_error_scope(wgpu::ErrorFilter::Validation);
        // bands of rows, one submit each: no single job may outlast the driver's watchdog on a slow card
        let band = range_band_rows(w, radius);
        let timed = std::env::var_os("GRAINEO_QUATTRO_TIMING").is_some();
        let mut longest = 0f64;
        for first in (0..h).step_by(band) {
            let rows = band.min(h - first);
            self.queue.write_buffer(&job, 12, bytemuck::cast_slice(&[first as u32]));
            let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
            {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
                pass.set_bind_group(0, &bind, &[]);
                pass.set_pipeline(&self.range_mean);
                pass.dispatch_workgroups((w as u32).div_ceil(GROUP), (rows as u32).div_ceil(GROUP), 1);
            }
            if first + rows >= h {
                encoder.copy_buffer_to_buffer(&result, 0, &readback, 0, bytes);
            }
            self.queue.submit([encoder.finish()]);
            if timed {
                let started = std::time::Instant::now();
                let _ = self.device.poll(wgpu::PollType::Wait { submission_index: None, timeout: Some(PATIENCE) });
                longest = longest.max(started.elapsed().as_secs_f64());
            }
        }
        if timed {
            eprintln!("quattro timing: range filter r={radius} on the card: {} bands of {band} rows, longest submit {:.1} ms", h.div_ceil(band), longest * 1e3);
        }
        let slice = readback.slice(..);
        self.wait_for(&slice, scope, errors_before)?;
        let view = slice.get_mapped_range().ok()?;
        let out: Vec<Px> = bytemuck::cast_slice::<u8, Px>(&view).to_vec();
        drop(view);
        readback.unmap();
        // give the memory back now, not when the driver gets round to it
        for buffer in buffers.into_iter().chain([&readback]) {
            buffer.destroy();
        }
        let _ = self.device.poll(wgpu::PollType::Poll);
        Some(out)
    }
}

/// Expected peak memory of developing one photo through the chain with the card, per full-resolution pixel: the
/// prepared file, the output, the buffers of the chain on the processor and those on the card (measured: a peak
/// footprint of 4.1 GB for 25.6 million pixels; the processor path peaks at 3.2 GB).
const PEAK_BYTES_PER_PIXEL: u64 = 170;
/// Share of the physical memory that peak may take; above it the processor path runs, which needs less.
const PEAK_MEMORY_SHARE: f64 = 0.5;

/// Does a photo of `pixels` full-resolution pixels fit the memory with the card in use? Unknown memory: yes.
fn fits_memory(pixels: usize) -> bool {
    fits(pixels, pools::physical_memory())
}

fn fits(pixels: usize, memory: Option<u64>) -> bool {
    memory.is_none_or(|m| (pixels as u64).saturating_mul(PEAK_BYTES_PER_PIXEL) as f64 <= m as f64 * PEAK_MEMORY_SHARE)
}

/// Window taps (cells × window area) the range filter computes per job: ≈ 20 ms on an M1 Pro, so even a card ten to
/// fifty times slower stays well inside the driver's watchdog (Windows TDR ≈ 2 s). The bands are separate submits.
const RANGE_TAPS_PER_JOB: usize = 1 << 27;

/// Rows of the grid per range-filter job for a grid `w` wide and a window of `radius`: a multiple of [`GROUP`].
fn range_band_rows(w: usize, radius: usize) -> usize {
    let window = (2 * radius + 1) * (2 * radius + 1);
    let rows = RANGE_TAPS_PER_JOB / (w.max(1) * window);
    (rows / GROUP as usize).max(1) * GROUP as usize
}

/// Rows of the full-resolution image the joint filter computes per job: bounds the buffers on the card.
const BAND_ROWS: usize = 512;

/// Range filter of the first stage ([`super::grid_denoise`]) on the graphics card.
pub(super) fn range_filter(img: &[Px], factors: &[Px], w: usize, h: usize, r: usize, curve: &[f32]) -> Option<Vec<Px>> {
    card()?.run(w, h, r, img, factors, curve)
}

/// Inputs of [`upscale`] beyond the images: the settings and tables of `upscale.wgsl` in one list.
pub(super) struct UpscaleNumbers(pub Vec<f32>);

/// Position of the band's first row among the settings.
const BAND_SETTING: usize = 6;

/// Step 3 on the graphics card ([`super::upscale`]): `sharp` = top layer with its spikes clamped (`2gw × 2gh`).
/// The top layer is smoothed in one job, the joint filter follows in bands of [`BAND_ROWS`] rows. Returns the colour
/// (three values per pixel) and the brightness ratio per pixel.
pub(super) fn upscale(denoised: &[Px], plain: &[Px], colour: &[Px], gw: usize, gh: usize, sharp: &[f32], numbers: &UpscaleNumbers) -> Option<(Vec<f32>, Vec<f32>)> {
    use rayon::prelude::*;
    let c = card()?;
    let mut clock = super::Clock::start();
    let (w, h) = (2 * gw, 2 * gh);
    let pixels = w * h;
    let band_bytes = (w * BAND_ROWS * 16) as u64;
    if pixels == 0 || sharp.len() != pixels || (pixels * 4) as u64 > c.largest_buffer || band_bytes > c.largest_buffer || !fits_memory(pixels) {
        return None;
    }
    let _one = c.busy.lock().unwrap_or_else(|e| e.into_inner());
    let errors_before = pools::gpu_errors();
    let storage = wgpu::BufferUsages::STORAGE;
    let filled = |data: &[u8], usage: wgpu::BufferUsages| c.device.create_buffer_init(&wgpu::util::BufferInitDescriptor { label: None, contents: data, usage });
    let empty = |size: u64, usage: wgpu::BufferUsages| c.device.create_buffer(&wgpu::BufferDescriptor { label: None, size, usage, mapped_at_creation: false });
    let buffers = [
        filled(bytemuck::cast_slice(&numbers.0), storage | wgpu::BufferUsages::COPY_DST),
        filled(bytemuck::cast_slice(denoised), storage),
        filled(bytemuck::cast_slice(plain), storage),
        filled(bytemuck::cast_slice(colour), storage),
        filled(bytemuck::cast_slice(sharp), storage),
        empty((pixels * 4) as u64, storage),
        empty(band_bytes, storage | wgpu::BufferUsages::COPY_SRC),
    ];
    let readback = empty(band_bytes, wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST);
    let entries: Vec<wgpu::BindGroupEntry> = buffers.iter().enumerate().map(|(i, b)| wgpu::BindGroupEntry { binding: i as u32, resource: b.as_entire_binding() }).collect();
    let bind = c.device.create_bind_group(&wgpu::BindGroupDescriptor { label: None, layout: &c.upscale_layout, entries: &entries });
    let mut rgb = vec![0f32; pixels * 3];
    let mut ratio = vec![0f32; pixels];
    clock.lap("upscale on the card: inputs sent");
    for (band, (rgb_band, ratio_band)) in rgb.chunks_mut(w * BAND_ROWS * 3).zip(ratio.chunks_mut(w * BAND_ROWS)).enumerate() {
        let rows = ratio_band.len() / w;
        c.queue.write_buffer(&buffers[0], (BAND_SETTING * 4) as u64, bytemuck::cast_slice(&[(band * BAND_ROWS) as f32]));
        let scope = c.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let mut encoder = c.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        if band == 0 {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_bind_group(0, &bind, &[]);
            pass.set_pipeline(&c.upscale[0]);
            pass.dispatch_workgroups((w as u32).div_ceil(GROUP), (h as u32).div_ceil(GROUP), 1);
        }
        {
            let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor { label: None, timestamp_writes: None });
            pass.set_bind_group(0, &bind, &[]);
            pass.set_pipeline(&c.upscale[1]);
            pass.dispatch_workgroups((w as u32).div_ceil(GROUP), (rows as u32).div_ceil(GROUP), 1);
        }
        let bytes = (w * rows * 16) as u64;
        encoder.copy_buffer_to_buffer(&buffers[6], 0, &readback, 0, bytes);
        c.queue.submit([encoder.finish()]);
        let slice = readback.slice(..bytes);
        c.wait_for(&slice, scope, errors_before)?;
        {
            let view = slice.get_mapped_range().ok()?;
            let values: &[[f32; 4]] = bytemuck::cast_slice(&view);
            rgb_band.par_chunks_mut(3).zip(ratio_band.par_iter_mut()).zip(values.par_iter()).for_each(|((c, r), v)| {
                c.copy_from_slice(&v[..3]);
                *r = v[3];
            });
        }
        readback.unmap();
    }
    for buffer in buffers.iter().chain([&readback]) {
        buffer.destroy();
    }
    let _ = c.device.poll(wgpu::PollType::Poll);
    clock.lap("upscale on the card: computed and read");
    Some((rgb, ratio))
}

#[cfg(test)]
mod tests {
    #[test]
    fn card_only_when_the_whole_development_fits_half_the_memory() {
        // sd Quattro H, 25.6 million pixels: 4.4 GB expected; dp Quattro, 19.6 million: 3.3 GB
        assert!(super::fits(25_600_000, Some(16 << 30)));
        assert!(!super::fits(25_600_000, Some(8 << 30)));
        assert!(super::fits(19_600_000, Some(8 << 30)));
        assert!(super::fits(25_600_000, None));
    }
}

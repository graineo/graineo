//! wgpu CPU/GPU reference backend: mask rasterization on CPU, dense filters and corrections on GPU.
use super::*;
use std::sync::{Mutex, OnceLock};
use wgpu::util::DeviceExt;
struct Gpu {
    device: wgpu::Device,
    queue: wgpu::Queue,
    layout: wgpu::BindGroupLayout,
    pipelines: [wgpu::ComputePipeline; 3],
    limit: u64,
    lock: Mutex<()>,
}
static GPU: OnceLock<Option<Gpu>> = OnceLock::new();
impl Gpu {
    fn new() -> Option<Self> {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
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
        let mut limits = wgpu::Limits::default();
        let al = adapter.limits();
        limits.max_storage_buffer_binding_size = al.max_storage_buffer_binding_size;
        limits.max_buffer_size = al.max_buffer_size;
        limits.max_storage_buffers_per_shader_stage = 8;
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("graineo masks"),
            required_features: wgpu::Features::empty(),
            required_limits: limits,
            memory_hints: wgpu::MemoryHints::Performance,
            experimental_features: wgpu::ExperimentalFeatures::disabled(),
            trace: wgpu::Trace::Off,
        }))
        .ok()?;
        // wgpu ends the thread with a panic on an uncaught GPU error: count it instead (the job falls back to the
        // processor, see `pools::gpu_errors`); a lost device switches every GPU path off
        device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| pools::gpu_error("masks", &e.to_string())));
        device.set_device_lost_callback(|reason, message| {
            if reason != wgpu::DeviceLostReason::Destroyed {
                pools::gpu_lost("masks", &message);
            }
        });

        let entries: Vec<_> = (0..7)
            .map(|binding| wgpu::BindGroupLayoutEntry {
                binding,
                visibility: wgpu::ShaderStages::COMPUTE,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage {
                        read_only: binding != 6,
                    },
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            })
            .collect();
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: None,
            entries: &entries,
        });
        let pl = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: None,
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("masks"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu.wgsl").into()),
        });
        let pipelines = ["box_h", "box_v", "correct"].map(|entry| {
            device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pl),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            })
        });
        Some(Self {
            device,
            queue,
            layout,
            pipelines,
            limit: (al.max_storage_buffer_binding_size as u64).min(al.max_buffer_size),
            lock: Mutex::new(()),
        })
    }
    fn buffer(&self, data: &[f32]) -> wgpu::Buffer {
        self.device
            .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                label: None,
                contents: bytemuck::cast_slice(data),
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            })
    }
    fn empty(&self, bytes: u64) -> wgpu::Buffer {
        self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
            mapped_at_creation: false,
        })
    }
    fn layer(&self, image: Image<'_>, weights: &[f32], a: Adjustments, view: View) -> Result<Vec<f32>> {
        let _guard = self.lock.lock().map_err(|_| "GPU-Sperre".to_owned())?;
        let errors_before = pools::gpu_errors();
        let bytes = image.data.len() as u64 * 4;
        if bytes > self.limit {
            return Err("GPU-Puffergrenze überschritten".into());
        }
        let src = self.buffer(image.data);
        let mask = self.buffer(weights);
        let out = self.empty(bytes);
        let temp = self.empty(bytes);
        let spatial =
            a.clarity != 0.0 || a.texture != 0.0 || a.sharpness != 0.0 || a.noise_reduction != 0.0;
        let needed = [
            a.noise_reduction != 0.0 || a.sharpness != 0.0,
            a.texture != 0.0,
            a.clarity != 0.0,
        ];
        let planes: Vec<_> = (0..3)
            .map(|i| self.empty(if needed[i] { bytes } else { 12 }))
            .collect();
        let dummy = self.buffer(&[0.0; 3]);
        let mut enc = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        let base = [
            image.width as f32,
            image.height as f32,
            1.0,
            2.0,
            a.exposure,
            a.contrast,
            a.highlights,
            a.shadows,
            a.temperature,
            a.tint,
            a.saturation,
            a.clarity,
            a.texture,
            a.sharpness,
            a.noise_reduction,
            0.0,
        ];
        let dispatch = |enc: &mut wgpu::CommandEncoder,
                        k: usize,
                        params: &[f32],
                        input: &wgpu::Buffer,
                        output: &wgpu::Buffer,
                        ps: [&wgpu::Buffer; 3]| {
            let p = self.buffer(params);
            let buffers = [&p, input, &mask, ps[0], ps[1], ps[2], output];
            let entries: Vec<_> = buffers
                .iter()
                .enumerate()
                .map(|(i, b)| wgpu::BindGroupEntry {
                    binding: i as u32,
                    resource: b.as_entire_binding(),
                })
                .collect();
            let group = self.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: None,
                layout: &self.layout,
                entries: &entries,
            });
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: None,
                timestamp_writes: None,
            });
            pass.set_pipeline(&self.pipelines[k]);
            pass.set_bind_group(0, &group, &[]);
            if k == 0 {
                pass.dispatch_workgroups((image.height * 3).div_ceil(64) as u32, 1, 1);
            } else if k == 1 {
                pass.dispatch_workgroups((image.width * 3).div_ceil(64) as u32, 1, 1);
            } else {
                pass.dispatch_workgroups(
                    image.width.div_ceil(16) as u32,
                    image.height.div_ceil(16) as u32,
                    1,
                );
            }
        };
        if spatial {
            for (i, r) in crate::correction::radii(view.short()).map(|r| r as f32).iter().enumerate() {
                if !needed[i] {
                    continue;
                }
                let mut p = base;
                p[2] = *r;
                dispatch(&mut enc, 0, &p, &src, &temp, [&dummy; 3]);
                dispatch(&mut enc, 1, &p, &temp, &planes[i], [&dummy; 3]);
            }
        }
        // No spatial parameters: shader still reads planes, so bind the original image as neutral planes.
        let ps = if spatial {
            [
                if needed[0] { &planes[0] } else { &src },
                if needed[1] { &planes[1] } else { &src },
                if needed[2] { &planes[2] } else { &src },
            ]
        } else {
            [&src; 3]
        };
        dispatch(&mut enc, 2, &base, &src, &out, ps);
        let read = self.device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        enc.copy_buffer_to_buffer(&out, 0, &read, 0, bytes);
        self.queue.submit([enc.finish()]);
        let (tx, rx) = std::sync::mpsc::channel();
        read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = tx.send(r);
        });
        self.device
            .poll(wgpu::PollType::wait_indefinitely())
            .map_err(|e| e.to_string())?;
        rx.recv()
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        if pools::gpu_errors() != errors_before {
            return Err("GPU-Fehler".into());
        }
        let mapped = read
            .slice(..)
            .get_mapped_range()
            .map_err(|e| e.to_string())?;
        let result: Vec<f32> = bytemuck::cast_slice(&mapped).to_vec();
        drop(mapped);
        read.unmap();
        if result.iter().any(|v| !v.is_finite()) {
            return Err("Nichtendliches GPU-Ergebnis".into());
        }
        Ok(result)
    }
}
/// None: no GPU or legacy document; errors can be handled by the caller without losing settings.
pub fn apply(image: Image<'_>, document: &Document) -> Result<Option<Vec<f32>>> {
    apply_view(image, document, View::whole(&image))
}
/// Like [`apply`] for a crop of a larger image (see [`View`]).
pub fn apply_view(image: Image<'_>, document: &Document, view: View) -> Result<Option<Vec<f32>>> {
    document.validate()?;
    if document.version != 2 || !pools::gpu_allowed() {
        return Ok(None);
    }
    let Some(gpu) = GPU.get_or_init(Gpu::new).as_ref() else {
        return Ok(None);
    };
    let mut data = image.data.to_vec();
    for layer in &document.layers {
        if layer.adjustments == Adjustments::default() {
            continue;
        }
        let im = Image::new(image.width, image.height, &data)?;
        let weights = rasterize_view(im, &layer.mask, view)?;
        data = gpu.layer(im, &weights, layer.adjustments, view)?;
    }
    Ok(Some(data))
}
pub fn apply_auto(image: Image<'_>, document: &Document) -> Result<Vec<f32>> {
    apply_auto_view(image, document, View::whole(&image))
}
/// Like [`apply_auto`] for a crop of a larger image (see [`View`]).
pub fn apply_auto_view(image: Image<'_>, document: &Document, view: View) -> Result<Vec<f32>> {
    match apply_view(image, document, view) {
        Ok(Some(v)) => Ok(v),
        Ok(None) => super::apply_view(image, document, view),
        Err(e) => {
            eprintln!("local adjustments: GPU fallback ({e})");
            super::apply_view(image, document, view)
        }
    }
}

//! graineo addition (not part of x3fuse-core): SPP tile noise reduction
//! (tiled path: `Pipeline::process_tile` + blending + sharpening) as a
//! wgpu/WGSL compute pipeline.
//!
//! Structure: the tile structure of the CPU version (8×8 tiles with a 40-pixel halo) is
//! kept so that tile borders, counter peculiarities and band-wise column means
//! behave exactly the same. Tiles are sent through all stages in batches (`BATCH` tiles, `gid.z`);
//! each stage is a separate kernel in `gpu.wgsl`. The
//! tile setup (`TileLayout::source_of`) and the blending
//! (`TileLayout::blend_pixel`) run over the whole image per pixel.
//!
//! Deviations from the CPU: f32 instead of f64 in the weight and mixing formulas, different
//! rounding of individual additions (FMA). The result is not bit-identical but
//! very close to the CPU version.
//!
//! At internal scale `<= 0.54` (high ISO) the whole-image prefilter runs first (reduced image:
//! resample, bilateral, edge map, local mean), and each tile takes its share of the result in
//! place of the per-tile filter stages, as on the CPU.

use std::collections::HashMap;
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use super::params::NoiseParams;
use super::pipeline::Pipeline;
use super::tiling::TileLayout;
use super::ResampleParams;
use crate::{Control, Error, Result};

const SHADER: &str = include_str!("gpu.wgsl");
const ENTRIES: &[&str] = &[
    "init_tiles",
    "resample_down",
    "resample_up",
    "kmap_mean",
    "kmap_radius",
    "bilateral",
    "border_copy",
    "edge_median",
    "edge_energy",
    "edge_final",
    "iir_rows",
    "iir_cols",
    "luma",
    "diffuse_nm",
    "diffuse_k",
    "osf_ratio",
    "mix_mono",
    "mix_color",
    "matrix_dq",
    "sharp_l",
    "apply_mono",
    "apply_color",
    "compose",
    "bilateral_image",
    "copy_prefiltered",
    "ca_shift",
    "copy_rgb",
    "restore_chroma",
];
/// Tiles per batch (default); adjustable via `X3F_SPP_GPU_BATCH`.
const BATCH: usize = 8;
/// Spacing of the parameter blocks per batch in the parameter buffer (binding alignment).
const PARAM_STRIDE: usize = 1024;
/// Maximum number of 40-row bands per tile.
const MAX_BANDS: usize = 16;
/// Spare table entries (see `Context::buffers`).
const TABS_RESERVE: usize = 1 << 20;

fn gpu_error(error: impl std::fmt::Display) -> Error {
    Error::Gpu(error.to_string())
}

/// Parameter block, field by field like `struct Params` in `gpu.wgsl` (4-byte fields only).
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Params {
    cols: u32,
    rows: u32,
    fcols: u32,
    frows: u32,
    core_w: u32,
    core_h: u32,
    halo_x: u32,
    halo_y: u32,
    grid_x: u32,
    grid_y: u32,
    img_w: u32,
    img_h: u32,
    img_stride: u32,
    tile_base: u32,
    batch: u32,
    bil_start: u32,
    kstep: u32,
    kcols: u32,
    krows: u32,
    subsample: u32,
    filter_half: u32,
    scaled: u32,
    smooth_edge: u32,
    accumulate: u32,
    match_first0: u32,
    match_last_cut: u32,
    db_radius: u32,
    db_end: u32,
    corr_radius: u32,
    color_mode: u32,
    perm_half: u32,
    perm_len: u32,
    dq_mask: u32,
    blend_border: u32,
    /// graineo: `rcps_on`, `power` (and `rcps_*`, `perm_*`, `match_*`, `db_*`, `corr_radius`) are no longer read by
    /// `gpu.wgsl` since the column matching and the pattern suppression were removed; they stay in the block so
    /// that its layout (shared with `gpu.wgsl`) is unchanged
    rcps_on: u32,
    power: u32,
    d_taps: u32,
    d_phases: u32,
    d_tail: i32,
    d_half: u32,
    u_taps: u32,
    u_phases: u32,
    u_tail: i32,
    u_half: u32,
    off_exp: u32,
    off_edge: u32,
    off_dq: u32,
    off_perm: u32,
    off_dtab: u32,
    off_dhcol: u32,
    off_dhph: u32,
    off_dvrow: u32,
    off_utab: u32,
    off_uhcol: u32,
    off_uhph: u32,
    off_uvrow: u32,
    iso: f32,
    lut_scale: f32,
    edge_bias: f32,
    edge_max: f32,
    edge_factor: f32,
    db_weight: f32,
    w0: f32,
    w1: f32,
    w2: f32,
    epsilon: f32,
    nm0: f32,
    nm1: f32,
    nm2: f32,
    lambda: f32,
    kscale: f32,
    rcps_thr: f32,
    rcps_map_thr: f32,
    rcps_blend: f32,
    mixing: f32,
    color_mixing: f32,
    strength: f32,
    sharp_limit: f32,
    sharpen_amount: f32,
    /// graineo: blend after the bilateral filter (colour slider step 0/1), otherwise 1
    alpha: f32,
    model: [f32; 9],
    edge_model: [f32; 6],
    matrix: [f32; 9],
    /// `1 − alpha`, computed in f64
    alpha_rest: f32,
    /// Remap border of the edge map (8 per tile, 0 for the prefilter)
    edge_border: u32,
    /// Border of the local mean (8 per tile, 4 for the prefilter)
    lm_border: u32,
    /// Prefiltered image and its tile geometry (`copy_prefiltered`)
    pre_w: u32,
    pre_h: u32,
    pre_core_x: u32,
    pre_core_y: u32,
    pre_halo_x: u32,
    pre_halo_y: u32,
    /// graineo: colour slider "Off" (`restore_chroma`), 0/1
    chroma_off: u32,
    /// graineo: CA correction (`ca_shift`), 0/1, and its values (`CaTransform::gpu_values`)
    ca_on: u32,
    ca_v: [f32; 8],
    ca_flags: [f32; 6],
    ca_c: [f32; 12],
}

// SAFETY: only u32/i32/f32 fields and arrays of f32 fields, `repr(C)`, no padding.
unsafe impl bytemuck::Zeroable for Params {}
unsafe impl bytemuck::Pod for Params {}

/// Process-wide shared GPU device with compiled pipelines and buffer cache.
struct Context {
    device: wgpu::Device,
    queue: wgpu::Queue,
    pipelines: HashMap<&'static str, wgpu::ComputePipeline>,
    layout0: wgpu::BindGroupLayout,
    layout1: wgpu::BindGroupLayout,
    dummies: Vec<wgpu::Buffer>,
    failure: Arc<Mutex<Option<String>>>,
    buffers: Option<(Key, Buffers)>,
    /// Work buffers of the whole-image prefilter (reduced size), only while needed.
    pre_buffers: Option<([usize; 2], PreBuffers)>,
    name: String,
    timestamps: bool,
    /// Image buffers directly mappable (shared memory), otherwise staging + readback buffer.
    mappable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Key {
    image: usize,
    tiles: usize,
    tile_pixels: usize,
    batch: usize,
    cols: usize,
    rows: usize,
    sharp: usize,
}

struct Buffers {
    params: wgpu::Buffer,
    tabs: wgpu::Buffer,
    tabs_len: usize,
    image: wgpu::Buffer,
    readback: Option<wgpu::Buffer>,
    store: wgpu::Buffer,
    sharp: wgpu::Buffer,
    work: wgpu::Buffer,
    smooth: wgpu::Buffer,
    filt: wgpu::Buffer,
    tmp: wgpu::Buffer,
    mono: wgpu::Buffer,
    mono2: wgpu::Buffer,
    aux: wgpu::Buffer,
    edge: wgpu::Buffer,
    nf: wgpu::Buffer,
    kbuf: wgpu::Buffer,
    /// graineo: unsmoothed tile B for the colour slider "Off"
    raw: wgpu::Buffer,
}

/// Buffers of the whole-image prefilter (RGB or mono at the reduced size).
struct PreBuffers {
    down: wgpu::Buffer,
    filt: wgpu::Buffer,
    tmp: wgpu::Buffer,
    aux: wgpu::Buffer,
    edge: wgpu::Buffer,
    nf: wgpu::Buffer,
}

type Shared = Mutex<Option<std::result::Result<Context, String>>>;
static GPU: Shared = Mutex::new(None);

impl Context {
    fn new() -> std::result::Result<Self, String> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: wgpu::Backends::METAL | wgpu::Backends::VULKAN | wgpu::Backends::DX12,
            ..Default::default()
        });
        let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            compatible_surface: None,
        }))
        .map_err(|e| e.to_string())?;
        let info = adapter.get_info();
        if info.device_type == wgpu::DeviceType::Cpu {
            return Err("adapter is a software renderer".into());
        }
        let supported = adapter.limits();
        let limits = wgpu::Limits {
            max_buffer_size: supported.max_buffer_size,
            max_storage_buffer_binding_size: supported.max_storage_buffer_binding_size,
            max_storage_buffers_per_shader_stage: 8,
            ..wgpu::Limits::default()
        };
        let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
            label: Some("SPP noise reduction"),
            // Timestamps only for the measuring mode (`X3F_SPP_GPU_PROFILE`); mappable
            // main buffers allow upload/readback without intermediate buffers with shared
            // memory (Apple).
            required_features: adapter.features()
                & (wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::MAPPABLE_PRIMARY_BUFFERS),
            required_limits: limits,
            ..Default::default()
        }))
        .map_err(|e| e.to_string())?;
        let failure = Arc::new(Mutex::new(None));
        let errors = failure.clone();
        device.on_uncaptured_error(Arc::new(move |error: wgpu::Error| {
            errors
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get_or_insert_with(|| error.to_string());
        }));
        let lost = failure.clone();
        device.set_device_lost_callback(move |reason, message| {
            *lost.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(format!("device lost ({reason:?}): {message}"));
        });
        let storage = |binding, read_only| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::COMPUTE,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only },
                has_dynamic_offset: false,
                min_binding_size: None,
            },
            count: None,
        };
        let layout0 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("SPP parameters"),
            entries: &[storage(0, true), storage(1, true)],
        });
        let layout1 = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("SPP work buffers"),
            entries: &(0..6).map(|i| storage(i, false)).collect::<Vec<_>>(),
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("SPP"),
            bind_group_layouts: &[&layout0, &layout1],
            push_constant_ranges: &[],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SPP gpu.wgsl"),
            source: wgpu::ShaderSource::Wgsl(SHADER.into()),
        });
        let check = |failure: &Arc<Mutex<Option<String>>>| match failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        };
        check(&failure)?;
        let mut pipelines = HashMap::new();
        for &entry in ENTRIES {
            let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
                label: Some(entry),
                layout: Some(&pipeline_layout),
                module: &module,
                entry_point: Some(entry),
                compilation_options: Default::default(),
                cache: None,
            });
            check(&failure)?;
            pipelines.insert(entry, pipeline);
        }
        let dummies = (0..6)
            .map(|i| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(&format!("SPP placeholder {i}")),
                    size: 16,
                    usage: wgpu::BufferUsages::STORAGE,
                    mapped_at_creation: false,
                })
            })
            .collect();
        let timestamps = device.features().contains(wgpu::Features::TIMESTAMP_QUERY);
        let mappable = device
            .features()
            .contains(wgpu::Features::MAPPABLE_PRIMARY_BUFFERS)
            && info.device_type == wgpu::DeviceType::IntegratedGpu;
        Ok(Self {
            device,
            queue,
            pipelines,
            layout0,
            layout1,
            dummies,
            failure,
            buffers: None,
            pre_buffers: None,
            name: format!("{} ({:?})", info.name, info.backend),
            timestamps,
            mappable,
        })
    }

    fn check(&self) -> Result<()> {
        match self
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            Some(error) => Err(gpu_error(error)),
            None => Ok(()),
        }
    }

    fn buffers(&mut self, key: Key, tabs_len: usize) -> &Buffers {
        let fits = matches!(&self.buffers, Some((k, b)) if *k == key && b.tabs_len >= tabs_len);
        if !fits {
            // Free old buffers first (memory!).
            self.buffers = None;
            let device = &self.device;
            let mappable = self.mappable;
            let make = |label: &str, floats: usize, usage: wgpu::BufferUsages| {
                device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: (floats.max(4) * 4) as u64,
                    usage,
                    mapped_at_creation: false,
                })
            };
            use wgpu::BufferUsages as U;
            let rw = U::STORAGE | U::COPY_DST | U::COPY_SRC;
            let tile_rgb = key.batch * key.tile_pixels * 3;
            let tile_mono = key.batch * key.tile_pixels;
            let kmap = 2 * key.batch * (key.cols.div_ceil(32) * key.rows.div_ceil(32));
            let buffers = Buffers {
                params: make(
                    "SPP parameters",
                    // one block per batch (at most 64) plus one for the prefilter
                    65 * PARAM_STRIDE / 4,
                    U::STORAGE | U::COPY_DST,
                ),
                // Room to spare: with and without prefilter the table lengths differ, and a
                // change must not rebuild all work buffers.
                tabs: make(
                    "SPP tables",
                    tabs_len + TABS_RESERVE,
                    U::STORAGE | U::COPY_DST,
                ),
                tabs_len: tabs_len + TABS_RESERVE,
                image: make(
                    "SPP image",
                    key.image,
                    if mappable {
                        U::STORAGE | U::MAP_WRITE | U::COPY_SRC
                    } else {
                        rw
                    },
                ),
                // Readback via a separate buffer: reading from the mapped image buffer
                // itself was measured to be considerably slower (≈ 70 instead of ≈ 10 ms).
                readback: Some(make("SPP readback", key.image, U::MAP_READ | U::COPY_DST)),
                store: make("SPP tile store", key.tiles * key.tile_pixels * 3, rw),
                sharp: make("SPP sharpening image", key.sharp, rw),
                work: make("SPP work", tile_rgb, rw),
                smooth: make("SPP smooth", tile_rgb, rw),
                filt: make("SPP filt", tile_rgb, rw),
                tmp: make("SPP tmp", tile_rgb, rw),
                mono: make("SPP mono", tile_mono, rw),
                mono2: make("SPP mono2", tile_mono, rw),
                aux: make("SPP aux", tile_mono, rw),
                edge: make("SPP edge", tile_mono, rw),
                nf: make("SPP nf", tile_mono, rw),
                kbuf: make("SPP kernel map", kmap, rw),
                raw: make("SPP raw (colour off)", tile_rgb, rw),
            };
            self.buffers = Some((key, buffers));
        }
        &self.buffers.as_ref().unwrap().1
    }

    /// Prepares the prefilter buffers for the reduced size (kept between calls).
    fn pre_buffers(&mut self, size: [usize; 2]) {
        if matches!(&self.pre_buffers, Some((s, _)) if *s == size) {
            return;
        }
        self.pre_buffers = None;
        let device = &self.device;
        let [w, h] = size;
        let make = |label: &str, floats: usize| {
            device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (floats.max(4) * 4) as u64,
                usage: wgpu::BufferUsages::STORAGE
                    | wgpu::BufferUsages::COPY_DST
                    | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            })
        };
        let buffers = PreBuffers {
            down: make("SPP prefilter down", w * h * 3),
            filt: make("SPP prefilter result", w * h * 3),
            tmp: make("SPP prefilter tmp", w * h * 3),
            aux: make("SPP prefilter aux", w * h),
            edge: make("SPP prefilter edge", w * h),
            nf: make("SPP prefilter nf", w * h),
        };
        self.pre_buffers = Some((size, buffers));
    }
}

/// One kernel invocation: entry point, buffer assignment b0…b5, grid size.
struct Step<'a> {
    entry: &'static str,
    slots: Vec<&'a wgpu::Buffer>,
    groups: [u32; 3],
}

fn step<'a>(
    entry: &'static str,
    slots: Vec<&'a wgpu::Buffer>,
    groups: [u32; 3],
    stage: &'static str,
) -> (Step<'a>, &'static str) {
    (
        Step {
            entry,
            slots,
            groups,
        },
        stage,
    )
}

fn groups2(w: usize, h: usize, batch: usize) -> [u32; 3] {
    [w.div_ceil(16) as u32, h.div_ceil(16) as u32, batch as u32]
}

fn groups1(n: usize, batch: usize) -> [u32; 3] {
    [n.div_ceil(64) as u32, 1, batch as u32]
}

/// Precomputed column/row starts of one `resample_image` direction.
struct ResampleTables {
    hcol: Vec<f32>,
    hph: Vec<f32>,
    vrow: Vec<f32>,
}

fn resample_tables(params: &ResampleParams, width: usize, rows: usize) -> ResampleTables {
    let taps = params.kernel_x as usize;
    let phases = params.kernel_y as usize;
    let half = taps / 2;
    let mut hcol = Vec::with_capacity((half + 1) * width);
    let mut hph = Vec::with_capacity((half + 1) * width);
    // Kind 0: normal row (phase 0); kind k ≥ 1: k-th initial row whose phase
    // continues across row boundaries as in the CPU version.
    for kind in 0..=half {
        let mut phase = if kind == 0 {
            0
        } else {
            ((kind - 1) * width) % phases
        };
        let mut column = i64::from(params.tail);
        for _ in 0..width {
            hcol.push(column as f32);
            hph.push(phase as f32);
            column += i64::from(params.offsets[phase]);
            phase = (phase + 1) % phases;
        }
    }
    let mut vrow = Vec::with_capacity(rows);
    let mut start = i64::from(params.tail);
    for y in 0..rows {
        vrow.push(start as f32);
        start += i64::from(params.offsets[y % phases]);
    }
    ResampleTables { hcol, hph, vrow }
}

fn scaled_dimension(dimension: usize, scale: f32) -> usize {
    (((dimension as f32) as f64 * f64::from(scale)).ceil() as usize + 1) & !1
}

/// Large `memcpy` (upload/readback). Runs on the calling thread on purpose: it is called while the global GPU lock is
/// held, and a rayon parallel loop there lets the waiting thread steal another job that wants the same lock – the
/// thread then waits for itself (deadlock seen with two offline copies and a foreground image at once).
fn parallel_copy(destination: &mut [u8], source: &[u8]) {
    destination.copy_from_slice(source);
}

/// Is the GPU path implemented for these parameters?
pub(crate) fn supports(noise: &NoiseParams) -> bool {
    // graineo: all parameter combinations are implemented: the whole-image prefilter
    // (`scale <= 0.54`), the blend after the bilateral filter (colour slider step 0/1,
    // `alpha ≠ 1`), colour slider "Off" (`chroma_off`, kernel `restore_chroma`) and the CA
    // correction (`ca_shift`).
    // graineo: the alternative CA form (test switch) runs on the CPU only
    noise.alpha.is_finite() && !crate::merrill_new::active("ca")
}

/// Name of the GPU adapter or error message (for diagnostics and the measuring tool).
pub(crate) fn adapter() -> std::result::Result<String, String> {
    let mut guard = GPU.lock().unwrap_or_else(|e| e.into_inner());
    match guard.get_or_insert_with(Context::new) {
        Ok(ctx) => Ok(ctx.name.clone()),
        Err(e) => Err(e.clone()),
    }
}

/// Frees the cached work buffers (≈ 0.9 GB at 4928×3264);
/// device and compiled shaders are kept.
pub(crate) fn release() {
    let mut guard = GPU.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(Ok(ctx)) = guard.as_mut() {
        ctx.buffers = None;
        ctx.pre_buffers = None;
    }
}

/// GPU version of `process::spp::denoise_image` (same contract: `data` is only
/// overwritten on success).
#[allow(clippy::too_many_arguments)]
pub(crate) fn denoise_image(
    data: &mut [f32],
    stride: usize,
    size: [usize; 2],
    active: [usize; 4],
    noise: &NoiseParams,
    pipeline: &Pipeline,
    control: Control<'_>,
) -> Result<Vec<i16>> {
    control.check()?;
    if !supports(noise) {
        return Err(gpu_error(
            "these parameters are not implemented on the GPU",
        ));
    }
    let timing = super::prof::enabled();
    let profile = std::env::var_os("X3F_SPP_GPU_PROFILE").is_some();
    let t0 = Instant::now();
    let mut guard = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let fresh = guard.is_none();
    let ctx = guard
        .get_or_insert_with(Context::new)
        .as_mut()
        .map_err(|e| gpu_error(e.clone()))?;
    if timing && fresh {
        eprintln!(
            "SPP GPU   initialisation (device + compiling {} shaders): {:.1} ms [{}]",
            ENTRIES.len(),
            t0.elapsed().as_secs_f64() * 1e3,
            ctx.name
        );
    }
    ctx.check()?;
    let view = pipeline.gpu_view();
    let p = view.params;
    let layout = TileLayout::new(size, [8, 8], [40, 40])?;
    let [cols, rows] = layout.tile;
    let scaled = p.scale != 1.0;
    let (fcols, frows) = if scaled {
        (
            scaled_dimension(cols, p.scale),
            scaled_dimension(rows, p.scale),
        )
    } else {
        (cols, rows)
    };
    let half_filter = p.filter_size as usize / 2;
    if cols <= p.filter_size as usize + 1 || rows / 40 > MAX_BANDS || stride * size[1] > data.len()
    {
        return Err(gpu_error("tile geometry not supported"));
    }
    // Whole-image prefilter, same condition as on the CPU: reduced size and tile halo.
    let prefilter = f64::from(p.scale) <= 0.54;
    let pre_size = size.map(|n| scaled_dimension(n, p.scale));
    let pre_halo = layout.halo.map(|h| (h as f64 * f64::from(p.scale)) as usize);
    let pre_core = [pre_size[0] / layout.grid[0], pre_size[1] / layout.grid[1]];
    if prefilter {
        let tile = [fcols, frows];
        let invalid = stride % 3 != 0
            || pre_size[1] < 5
            || p.filter_size as usize / 2 >= pre_size[0]
            || (0..2).any(|i| {
                pre_halo[i] > pre_core[i]
                    || pre_core[i] == 0
                    || tile[i] < pre_core[i] + 2 * pre_halo[i]
            });
        if invalid {
            return Err(gpu_error("prefilter geometry not supported"));
        }
    }
    let batch = std::env::var("X3F_SPP_GPU_BATCH")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(BATCH)
        .clamp(1, 64);
    let tiles = layout.grid[0] * layout.grid[1];

    // Tables: exp LUT, edge LUT, ColorDQ, permutation, resampling.
    let mut tabs: Vec<f32> = Vec::new();
    let mut prm = Params::default();
    let mut pre_off = [0_u32; 3];
    prm.off_exp = tabs.len() as u32;
    tabs.extend_from_slice(view.exp_lut);
    prm.off_edge = tabs.len() as u32;
    tabs.extend_from_slice(view.edge_lut);
    prm.off_dq = tabs.len() as u32;
    for table in view.color_dq {
        tabs.extend(table.iter().map(|&v| f32::from(v)));
    }
    prm.dq_mask = (view.color_dq[0].len() - 1) as u32;
    prm.off_perm = tabs.len() as u32;
    tabs.extend(view.permutation.iter().map(|&v| v as f32));
    prm.perm_len = view.permutation.len() as u32;
    if scaled {
        let (down, up) = view
            .resample
            .ok_or_else(|| gpu_error("resampling parameters missing"))?;
        let dt = resample_tables(down, fcols, frows);
        let ut = resample_tables(up, cols, rows);
        let mut push = |values: &[f32]| {
            let offset = tabs.len() as u32;
            tabs.extend_from_slice(values);
            offset
        };
        prm.off_dtab = push(&down.table);
        if prefilter {
            let whole = resample_tables(down, pre_size[0], pre_size[1]);
            pre_off = [push(&whole.hcol), push(&whole.hph), push(&whole.vrow)];
        }
        prm.off_dhcol = push(&dt.hcol);
        prm.off_dhph = push(&dt.hph);
        prm.off_dvrow = push(&dt.vrow);
        prm.off_utab = push(&up.table);
        prm.off_uhcol = push(&ut.hcol);
        prm.off_uhph = push(&ut.hph);
        prm.off_uvrow = push(&ut.vrow);
        (prm.d_taps, prm.d_phases, prm.d_tail, prm.d_half) =
            (down.kernel_x, down.kernel_y, down.tail, down.kernel_x / 2);
        (prm.u_taps, prm.u_phases, prm.u_tail, prm.u_half) =
            (up.kernel_x, up.kernel_y, up.tail, up.kernel_x / 2);
    }
    prm.cols = cols as u32;
    prm.rows = rows as u32;
    prm.fcols = fcols as u32;
    prm.frows = frows as u32;
    prm.core_w = layout.core[0] as u32;
    prm.core_h = layout.core[1] as u32;
    prm.halo_x = layout.halo[0] as u32;
    prm.halo_y = layout.halo[1] as u32;
    prm.grid_x = layout.grid[0] as u32;
    prm.grid_y = layout.grid[1] as u32;
    prm.img_w = size[0] as u32;
    prm.img_h = size[1] as u32;
    prm.img_stride = stride as u32;
    prm.bil_start = half_filter as u32;
    prm.kstep = 32;
    prm.kcols = fcols.div_ceil(32) as u32;
    prm.krows = frows.div_ceil(32) as u32;
    prm.subsample = u32::from(p.subsample);
    prm.filter_half = if p.filter_size >= 2 {
        half_filter as u32
    } else {
        0
    };
    prm.scaled = u32::from(scaled);
    prm.smooth_edge = u32::from(p.smooth_edge);
    prm.accumulate = u32::from(p.accumulate);
    prm.match_first0 = active[1] as u32;
    prm.match_last_cut = (size[1] - active[3]) as u32;
    // match_col_offsets with the banding slider in the middle position: window 27, phase 2.
    let match_filter = 27_usize;
    prm.db_radius = (match_filter / 2) as u32;
    prm.db_end = (cols + match_filter / 2).saturating_sub(match_filter | 1) as u32;
    prm.corr_radius = (match_filter.saturating_sub(1) / 2) as u32;
    prm.color_mode = u32::from(p.color_mixing > 0.0);
    prm.perm_half = p.perm_half;
    prm.blend_border = 4;
    prm.iso = p.iso;
    prm.lut_scale = view.lut_scale;
    prm.edge_bias = p.edge_bias;
    prm.edge_max = view.edge_max;
    prm.edge_factor = view.edge_factor;
    prm.db_weight = 2047.0 / (8.0 / p.column_noise_scale);
    [prm.w0, prm.w1, prm.w2] = view.channel_weights;
    prm.epsilon = p.epsilon;
    let nm = p.intensity_model.map(|v| v * p.aniso_noise_scale);
    [prm.nm0, prm.nm1, prm.nm2] = nm;
    prm.lambda = p.lambda;
    let k = (p.aniso_k as i32).wrapping_mul(p.aniso_k as i32);
    prm.kscale = 1.0 / k as f32;
    prm.mixing = p.mixing;
    prm.color_mixing = p.color_mixing;
    prm.strength = p.residual_strength;
    prm.sharp_limit = p.iso;
    prm.sharpen_amount = p.sharpen_amount;
    prm.alpha = p.alpha;
    prm.alpha_rest = (1.0 - f64::from(p.alpha)) as f32;
    prm.edge_border = 8;
    prm.lm_border = 8;
    prm.pre_w = pre_size[0] as u32;
    prm.pre_h = pre_size[1] as u32;
    prm.pre_core_x = pre_core[0] as u32;
    prm.pre_core_y = pre_core[1] as u32;
    prm.pre_halo_x = pre_halo[0] as u32;
    prm.pre_halo_y = pre_halo[1] as u32;
    prm.chroma_off = u32::from(p.chroma_off);
    if let Some(ca) = view.ca {
        prm.ca_on = 1;
        (prm.ca_v, prm.ca_flags, prm.ca_c) = ca.gpu_values();
    }
    for k in 0..3 {
        for c in 0..3 {
            prm.model[k * 3 + c] = p.color_model[k][c] as f32;
            prm.matrix[k * 3 + c] = view.matrix[k][c] as f32;
        }
    }
    for k in 0..2 {
        for c in 0..3 {
            prm.edge_model[k * 3 + c] = view.edge_model[k][c] as f32;
        }
    }

    let key = Key {
        image: stride * size[1],
        tiles,
        tile_pixels: cols * rows,
        batch,
        cols,
        rows,
        sharp: size[0] * size[1],
    };
    let t_setup = Instant::now();
    let tabs_len = tabs.len();
    ctx.buffers(key, tabs_len);
    if prefilter {
        ctx.pre_buffers(pre_size);
    }
    let ctx = &*ctx;
    let b = &ctx.buffers.as_ref().unwrap().1;
    let pre = (prefilter).then(|| &ctx.pre_buffers.as_ref().unwrap().1);
    let device = &ctx.device;
    let queue = &ctx.queue;
    queue.write_buffer(&b.tabs, 0, bytemuck::cast_slice(&tabs));
    let wait = |what: &str| -> Result<()> {
        let started = Instant::now();
        loop {
            control.check()?;
            ctx.check()?;
            match device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_millis(5)),
            }) {
                Ok(status) if status.is_queue_empty() => return Ok(()),
                Ok(_) | Err(wgpu::PollError::Timeout) => {}
                Err(error) => return Err(gpu_error(error)),
            }
            if started.elapsed() > Duration::from_secs(60) {
                return Err(gpu_error(format!("GPU timeout during {what}")));
            }
        }
    };
    // Mapping a buffer (blocking, cancellable).
    let map = |buffer: &wgpu::Buffer, bytes: u64, mode: wgpu::MapMode| -> Result<()> {
        let (send, recv) = mpsc::sync_channel(1);
        buffer.slice(..bytes).map_async(mode, move |result| {
            let _ = send.send(result);
        });
        wait("mapping")?;
        match recv.recv_timeout(Duration::from_secs(10)) {
            Ok(result) => result.map_err(gpu_error),
            Err(_) => Err(gpu_error("mapping failed")),
        }
    };
    let image_bytes = (stride * size[1] * 4) as u64;
    // Upload (≈ 193 MB), copied in parallel: directly into the mapped image buffer
    // or into the staging area of the queue.
    if ctx.mappable {
        map(&b.image, image_bytes, wgpu::MapMode::Write)?;
        {
            let mut view = b.image.slice(..image_bytes).get_mapped_range_mut();
            parallel_copy(&mut view, bytemuck::cast_slice(&data[..stride * size[1]]));
        }
        b.image.unmap();
    } else {
        let mut view = queue
            .write_buffer_with(&b.image, 0, wgpu::BufferSize::new(image_bytes).unwrap())
            .ok_or_else(|| gpu_error("staging buffer not available"))?;
        parallel_copy(&mut view, bytemuck::cast_slice(&data[..stride * size[1]]));
    }
    // Parameter blocks of all batches (separate offset per batch → a single submit).
    let n = batch;
    let batches: Vec<usize> = (0..tiles).step_by(n).collect();
    let mut block = vec![0_u8; (batches.len() + 1) * PARAM_STRIDE];
    for (i, &first) in batches.iter().enumerate() {
        prm.tile_base = first as u32;
        prm.batch = n.min(tiles - first) as u32;
        block[i * PARAM_STRIDE..][..std::mem::size_of::<Params>()]
            .copy_from_slice(bytemuck::bytes_of(&prm));
    }
    if prefilter {
        // Parameter block of the prefilter: the reduced image as "filter grid", the source
        // as full grid, one batch, borders of the whole-image variant.
        let mut whole = prm;
        whole.cols = (stride / 3) as u32;
        whole.rows = size[1] as u32;
        whole.fcols = pre_size[0] as u32;
        whole.frows = pre_size[1] as u32;
        whole.tile_base = 0;
        whole.batch = 1;
        whole.edge_border = 0;
        whole.lm_border = 4;
        [whole.off_dhcol, whole.off_dhph, whole.off_dvrow] = pre_off;
        block[batches.len() * PARAM_STRIDE..][..std::mem::size_of::<Params>()]
            .copy_from_slice(bytemuck::bytes_of(&whole));
    }
    queue.write_buffer(&b.params, 0, &block);
    let group0: Vec<wgpu::BindGroup> = (0..=batches.len())
        .map(|i| {
            device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("SPP group 0"),
                layout: &ctx.layout0,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                            buffer: &b.params,
                            offset: (i * PARAM_STRIDE) as u64,
                            size: wgpu::BufferSize::new(std::mem::size_of::<Params>() as u64),
                        }),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: b.tabs.as_entire_binding(),
                    },
                ],
            })
        })
        .collect();
    // Process tiles directly in the store if their batch ranges are suitably
    // aligned (otherwise work buffer + copy).
    let tile_bytes = (cols * rows * 3 * 4) as u64;
    let align = u64::from(device.limits().min_storage_buffer_offset_alignment);
    let in_store = (n as u64 * tile_bytes) % align == 0;
    let mut bind_cache: HashMap<(Vec<usize>, usize), wgpu::BindGroup> = HashMap::new();
    // `batch`: batch index; only relevant for the slot `work` (→ range of the tile store).
    let mut bind = |slots: &[&wgpu::Buffer], batch: usize| -> wgpu::BindGroup {
        let ids: Vec<usize> = slots.iter().map(|b| *b as *const _ as usize).collect();
        let uses_work = in_store && slots.iter().any(|s| std::ptr::eq(*s, &b.work));
        bind_cache
            .entry((ids, if uses_work { batch } else { usize::MAX }))
            .or_insert_with(|| {
                let first = batches[batch] as u64;
                let count = n.min(tiles - batches[batch]) as u64;
                let entries: Vec<_> = (0..6)
                    .map(|i| wgpu::BindGroupEntry {
                        binding: i as u32,
                        resource: match slots.get(i).copied() {
                            Some(s) if uses_work && std::ptr::eq(s, &b.work) => {
                                wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                                    buffer: &b.store,
                                    offset: first * tile_bytes,
                                    size: wgpu::BufferSize::new(count * tile_bytes),
                                })
                            }
                            Some(s) => s.as_entire_binding(),
                            None => ctx.dummies[i].as_entire_binding(),
                        },
                    })
                    .collect();
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: None,
                    layout: &ctx.layout1,
                    entries: &entries,
                })
            })
            .clone()
    };

    // Stage sequence of one batch (see `Pipeline::process_tile`).
    let full = groups2(cols, rows, n);
    let filt_grid = groups2(fcols, frows, n);
    let mut steps: Vec<(Step, &'static str)> = Vec::new();
    steps.push(step(
        "init_tiles",
        vec![&b.image, &b.work],
        full,
        "set up tiles",
    ));
    if let Some(pre) = pre {
        // Prefiltered: the tile takes its share of the whole-image result and upsamples it.
        steps.push(step(
            "copy_prefiltered",
            vec![&pre.filt, &b.filt],
            filt_grid,
            "Resampling",
        ));
        steps.push(step(
            "resample_up",
            vec![&b.filt, &b.smooth],
            full,
            "Resampling",
        ));
    } else {
        let (fsrc, fdst) = if scaled {
            steps.push(step(
                "resample_down",
                vec![&b.work, &b.smooth],
                filt_grid,
                "Resampling",
            ));
            (&b.smooth, &b.filt)
        } else {
            (&b.work, &b.smooth)
        };
        let kblocks = (prm.kcols * prm.krows) as usize;
        steps.push(step(
            "kmap_mean",
            vec![fsrc, &b.kbuf],
            groups1(kblocks, n),
            "KernelMap+Bilateral",
        ));
        // kmap_radius uses only b1 → assignment [placeholder, kbuf].
        steps.push(step(
            "kmap_radius",
            vec![&ctx.dummies[0], &b.kbuf],
            groups1(kblocks, n),
            "KernelMap+Bilateral",
        ));
        steps.push(step(
            "bilateral",
            vec![fsrc, fdst, &b.kbuf],
            filt_grid,
            "KernelMap+Bilateral",
        ));
        if !scaled {
            steps.push(step(
                "border_copy",
                vec![&b.work],
                full,
                "KernelMap+Bilateral",
            ));
        }
        steps.push(step(
            "edge_median",
            vec![fdst, &b.tmp],
            filt_grid,
            "EdgeMap+Remap",
        ));
        steps.push(step(
            "edge_energy",
            vec![&b.tmp, &b.aux],
            filt_grid,
            "EdgeMap+Remap",
        ));
        steps.push(step(
            "edge_final",
            vec![&b.aux, &b.edge],
            filt_grid,
            "EdgeMap+Remap",
        ));
        steps.push(step(
            "iir_rows",
            vec![fdst, &b.edge, &b.tmp, &b.nf],
            groups1(frows, n),
            "LocalMean (IIR)",
        ));
        steps.push(step(
            "iir_cols",
            vec![&b.tmp, &b.edge, fdst, &b.nf],
            groups1(fcols, n),
            "LocalMean (IIR)",
        ));
        if scaled {
            steps.push(step(
                "resample_up",
                vec![&b.filt, &b.smooth],
                full,
                "Resampling",
            ));
        }
    }
    // graineo step colour "Off": keep the unsmoothed tile B (after the banding correction).
    if prm.chroma_off != 0 {
        steps.push(step("copy_rgb", vec![&b.work, &b.raw], full, "colour off"));
    }
    steps.push(step(
        "luma",
        vec![&b.work, &b.mono],
        full,
        "Luma+Diffusion+OSF+Ratio",
    ));
    steps.push(step(
        "diffuse_nm",
        vec![&b.mono, &b.mono2],
        full,
        "Luma+Diffusion+OSF+Ratio",
    ));
    steps.push(step(
        "diffuse_k",
        vec![&b.mono2, &b.mono],
        full,
        "Luma+Diffusion+OSF+Ratio",
    ));
    steps.push(step(
        "osf_ratio",
        vec![&b.mono, &b.mono2, &b.smooth],
        full,
        "Luma+Diffusion+OSF+Ratio",
    ));
    let color = prm.color_mode != 0;
    if color {
        steps.push(step(
            "mix_color",
            vec![&b.mono2, &b.filt, &b.smooth, &b.work],
            full,
            "residual mix",
        ));
    } else {
        steps.push(step(
            "mix_mono",
            vec![&b.mono2, &b.mono, &b.smooth, &b.work],
            full,
            "residual mix",
        ));
    }
    steps.push(step("matrix_dq", vec![&b.smooth], full, "Matrix+ColorDQ"));
    // graineo: CA shift of the base image (tile-local as on the CPU); `tmp` is free here.
    if prm.ca_on != 0 {
        steps.push(step("ca_shift", vec![&b.smooth, &b.tmp], full, "CA"));
        steps.push(step("copy_rgb", vec![&b.tmp, &b.smooth], full, "CA"));
    }
    steps.push(step(
        "sharp_l",
        vec![&b.mono2, &b.aux, &b.smooth],
        full,
        "SharpL+apply",
    ));
    let scale_buffer = if color { &b.filt } else { &b.mono };
    steps.push(step(
        if color { "apply_color" } else { "apply_mono" },
        vec![&b.aux, &b.mono2, scale_buffer, &b.smooth, &b.work, &b.sharp],
        full,
        "SharpL+apply",
    ));
    // graineo step colour "Off": B through matrix, ColorDQ and CA, then the colour of B
    // with the luminance of the result (`Pipeline::restore_chroma`).
    if prm.chroma_off != 0 {
        steps.push(step("matrix_dq", vec![&b.raw], full, "colour off"));
        let shifted = if prm.ca_on != 0 {
            steps.push(step("ca_shift", vec![&b.raw, &b.tmp], full, "colour off"));
            &b.tmp
        } else {
            &b.raw
        };
        steps.push(step(
            "restore_chroma",
            vec![&b.work, shifted, &b.smooth],
            full,
            "colour off",
        ));
    }
    let bound: Vec<Vec<(wgpu::BindGroup, &wgpu::ComputePipeline)>> = (0..batches.len())
        .map(|k| {
            steps
                .iter()
                .map(|(step, _)| (bind(&step.slots, k), &ctx.pipelines[step.entry]))
                .collect()
        })
        .collect();
    let compose_group = bind(&[&b.image, &b.store, &b.sharp], 0);
    // Whole-image prefilter (one pass, before all tiles).
    let pre_steps: Vec<(wgpu::BindGroup, &wgpu::ComputePipeline, [u32; 3])> = match pre {
        None => Vec::new(),
        Some(pre) => {
            let [w, h] = pre_size;
            let grid = groups2(w, h, 1);
            let list: Vec<(&'static str, Vec<&wgpu::Buffer>, [u32; 3])> = vec![
                ("resample_down", vec![&b.image, &pre.down], grid),
                ("bilateral_image", vec![&pre.down, &pre.filt], grid),
                ("edge_median", vec![&pre.filt, &pre.tmp], grid),
                ("edge_energy", vec![&pre.tmp, &pre.aux], grid),
                ("edge_final", vec![&pre.aux, &pre.edge], grid),
                (
                    "iir_rows",
                    vec![&pre.filt, &pre.edge, &pre.tmp, &pre.nf],
                    groups1(h, 1),
                ),
                (
                    "iir_cols",
                    vec![&pre.tmp, &pre.edge, &pre.filt, &pre.nf],
                    groups1(w, 1),
                ),
            ];
            list.into_iter()
                .map(|(entry, slots, groups)| (bind(&slots, 0), &ctx.pipelines[entry], groups))
                .collect()
        }
    };
    if timing {
        eprintln!(
            "SPP GPU   buffers + upload (queued): {:.1} ms",
            t_setup.elapsed().as_secs_f64() * 1e3
        );
    }

    let mut stage_times: Vec<(&'static str, f64)> = Vec::new();
    let t_tiles = Instant::now();
    let mut encoder = device.create_command_encoder(&Default::default());
    if !pre_steps.is_empty() {
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_bind_group(0, &group0[batches.len()], &[]);
            for (group, pipeline, groups) in &pre_steps {
                pass.set_pipeline(pipeline);
                pass.set_bind_group(1, group, &[]);
                pass.dispatch_workgroups(groups[0], groups[1], groups[2]);
            }
        }
        if timing {
            queue.submit([std::mem::replace(
                &mut encoder,
                device.create_command_encoder(&Default::default()),
            )
            .finish()]);
            wait("prefilter")?;
            eprintln!(
                "SPP GPU   whole-image prefilter: {:.1} ms",
                t_tiles.elapsed().as_secs_f64() * 1e3
            );
        }
    }
    for (k, &first) in batches.iter().enumerate() {
        control.check()?;
        ctx.check()?;
        let count = n.min(tiles - first);
        let group0 = &group0[k];
        let bound = &bound[k];
        if profile {
            // Measuring mode: each stage as a separate compute pass with GPU timestamps at
            // start and end; wait for and evaluate batches individually.
            if !ctx.timestamps {
                return Err(gpu_error(
                    "GPU does not support timestamps (TIMESTAMP_QUERY)",
                ));
            }
            let count_q = (2 * steps.len()) as u32;
            let set = device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("SPP timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: count_q,
            });
            let resolve = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: u64::from(count_q) * 8,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            });
            let read = device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: u64::from(count_q) * 8,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            for (i, ((step, _), (group, pipeline))) in steps.iter().zip(bound).enumerate() {
                let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                    label: None,
                    timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                        query_set: &set,
                        beginning_of_pass_write_index: Some(2 * i as u32),
                        end_of_pass_write_index: Some(2 * i as u32 + 1),
                    }),
                });
                pass.set_pipeline(pipeline);
                pass.set_bind_group(0, group0, &[]);
                pass.set_bind_group(1, group, &[]);
                pass.dispatch_workgroups(step.groups[0], step.groups[1], count as u32);
            }
            encoder.resolve_query_set(&set, 0..count_q, &resolve, 0);
            encoder.copy_buffer_to_buffer(&resolve, 0, &read, 0, u64::from(count_q) * 8);
            if !in_store {
                encoder.copy_buffer_to_buffer(
                    &b.work,
                    0,
                    &b.store,
                    first as u64 * tile_bytes,
                    count as u64 * tile_bytes,
                );
            }
            queue.submit([std::mem::replace(
                &mut encoder,
                device.create_command_encoder(&Default::default()),
            )
            .finish()]);
            let slice = read.slice(..);
            slice.map_async(wgpu::MapMode::Read, |_| {});
            wait("timestamps")?;
            {
                let mapped = slice.get_mapped_range();
                let ticks: &[u64] = bytemuck::cast_slice(&mapped);
                let period = f64::from(queue.get_timestamp_period()) * 1e-9;
                for (i, (step, stage)) in steps.iter().enumerate() {
                    let dt = ticks[2 * i + 1].saturating_sub(ticks[2 * i]) as f64 * period;
                    let stage: &'static str = if std::env::var_os("X3F_SPP_GPU_KERNELS").is_some() {
                        step.entry
                    } else {
                        stage
                    };
                    match stage_times.iter_mut().find(|(name, _)| *name == stage) {
                        Some(entry) => entry.1 += dt,
                        None => stage_times.push((stage, dt)),
                    }
                }
            }
            read.unmap();
        } else {
            // All batches in one command buffer; the parameters are at a separate offset
            // per batch (group 0), so a single submit suffices.
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                pass.set_bind_group(0, group0, &[]);
                for ((step, _), (group, pipeline)) in steps.iter().zip(bound) {
                    pass.set_pipeline(pipeline);
                    pass.set_bind_group(1, group, &[]);
                    pass.dispatch_workgroups(step.groups[0], step.groups[1], count as u32);
                }
            }
            if !in_store {
                encoder.copy_buffer_to_buffer(
                    &b.work,
                    0,
                    &b.store,
                    first as u64 * tile_bytes,
                    count as u64 * tile_bytes,
                );
            }
        }
    }
    if timing {
        queue.submit([std::mem::replace(
            &mut encoder,
            device.create_command_encoder(&Default::default()),
        )
        .finish()]);
        wait("tiles")?;
        eprintln!(
            "SPP GPU   tiles ({tiles} in batches of {n}): {:.1} ms",
            t_tiles.elapsed().as_secs_f64() * 1e3
        );
        for (stage, dt) in &stage_times {
            eprintln!("SPP GPU     {stage:<26} {:>7.1} ms", dt * 1e3);
        }
    }
    let t_compose = Instant::now();
    {
        let mut pass = encoder.begin_compute_pass(&Default::default());
        pass.set_pipeline(&ctx.pipelines["compose"]);
        pass.set_bind_group(0, &group0[0], &[]);
        pass.set_bind_group(1, &compose_group, &[]);
        let g = groups2(size[0], size[1], 1);
        pass.dispatch_workgroups(g[0], g[1], 1);
    }
    if timing {
        queue.submit([std::mem::replace(
            &mut encoder,
            device.create_command_encoder(&Default::default()),
        )
        .finish()]);
        wait("blending")?;
        eprintln!(
            "SPP GPU   blending + sharpening: {:.1} ms",
            t_compose.elapsed().as_secs_f64() * 1e3
        );
    }
    let source = match &b.readback {
        Some(readback) => {
            encoder.copy_buffer_to_buffer(&b.image, 0, readback, 0, image_bytes);
            readback
        }
        None => &b.image,
    };
    queue.submit([encoder.finish()]);
    let t_read = Instant::now();
    let result: Result<()> = (|| {
        map(source, image_bytes, wgpu::MapMode::Read)?;
        ctx.check()?;
        let mapped = source.slice(..image_bytes).get_mapped_range();
        parallel_copy(
            bytemuck::cast_slice_mut(&mut data[..stride * size[1]]),
            &mapped,
        );
        Ok(())
    })();
    source.unmap();
    result?;
    // graineo: read back the detail image "L" (i32 per pixel, value already clamped to i16)
    // via the readback buffer, which is free again after the image.
    let t_sharp = Instant::now();
    let sharp_bytes = (size[0] * size[1] * 4) as u64;
    let own;
    let target = match &b.readback {
        Some(readback) => readback,
        None => {
            own = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("SPP sharpening image readback"),
                size: sharp_bytes,
                usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            &own
        }
    };
    let mut encoder = device.create_command_encoder(&Default::default());
    encoder.copy_buffer_to_buffer(&b.sharp, 0, target, 0, sharp_bytes);
    queue.submit([encoder.finish()]);
    let result = (|| {
        map(target, sharp_bytes, wgpu::MapMode::Read)?;
        ctx.check()?;
        let mapped = target.slice(..sharp_bytes).get_mapped_range();
        let words: &[i32] = bytemuck::cast_slice(&mapped);
        // no rayon while the GPU lock is held (see `parallel_copy`)
        Ok(words.iter().map(|&v| v as i16).collect::<Vec<i16>>())
    })();
    target.unmap();
    drop(steps);
    if timing {
        eprintln!(
            "SPP GPU   read back sharpening image: {:.1} ms",
            t_sharp.elapsed().as_secs_f64() * 1e3
        );
    }
    if timing {
        eprintln!(
            "SPP GPU   readback: {:.1} ms · total {:.1} ms",
            t_read.elapsed().as_secs_f64() * 1e3,
            t0.elapsed().as_secs_f64() * 1e3
        );
    }
    result
}

// ---------------------------------------------------------------------------
// graineo addition: patch fits of the Merrill row offset correction (`cr_offset`).

/// Parameter block of `gpu_offset.wgsl`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct OffsetParams {
    stride: u32,
    n: u32,
    terms: u32,
    area: u32,
    patches: u32,
    x_limit: f32,
    size_f: f32,
    pad: u32,
}
// SAFETY: only u32/f32 fields, `repr(C)`, no padding.
unsafe impl bytemuck::Zeroable for OffsetParams {}
unsafe impl bytemuck::Pod for OffsetParams {}

/// One patch size of the offset correction: size `n`, polynomial terms, pseudoinverse
/// (`terms × n²`), basis (`n² × terms`) and the patches `[x0, y0, px]` in accumulation order.
pub(crate) struct OffsetScale<'a> {
    pub n: u32,
    pub terms: usize,
    pub inverse: &'a [f32],
    pub basis: &'a [f32],
    pub patches: Vec<[u32; 3]>,
}

/// Patch fits of all sizes on the GPU. Result per size and patch (`1 + 4·n` floats): accepted
/// flag, then per patch row the residual mean (3 floats) and the number of valid columns.
pub(crate) fn offset_fits(
    data: &[f32],
    stride: usize,
    x_limit: u16,
    scales: &[OffsetScale<'_>],
    control: Control<'_>,
) -> Result<Vec<Vec<f32>>> {
    control.check()?;
    let mut guard = GPU.lock().unwrap_or_else(|e| e.into_inner());
    let ctx = guard
        .get_or_insert_with(Context::new)
        .as_mut()
        .map_err(|e| gpu_error(e.clone()))?;
    ctx.check()?;
    let device = &ctx.device;
    let queue = &ctx.queue;
    // Errors of this pass are caught here (error scope) and lead to the CPU path; they must not
    // reach the device-wide error handler, which would block the noise reduction afterwards.
    device.push_error_scope(wgpu::ErrorFilter::Validation);
    let result = offset_fits_scoped(ctx, data, stride, x_limit, scales, control);
    let scope = pollster::block_on(ctx.device.pop_error_scope());
    match (result, scope) {
        (Err(e), _) => Err(e),
        (Ok(_), Some(error)) => Err(gpu_error(error)),
        (Ok(v), None) => Ok(v),
    }
}

fn offset_fits_scoped(
    ctx: &Context,
    data: &[f32],
    stride: usize,
    x_limit: u16,
    scales: &[OffsetScale<'_>],
    control: Control<'_>,
) -> Result<Vec<Vec<f32>>> {
    let device = &ctx.device;
    let queue = &ctx.queue;
    let pipeline = {
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("SPP offset"),
            bind_group_layouts: &[&ctx.layout0, &ctx.layout1],
            push_constant_ranges: &[],
        });
        let module = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("SPP gpu_offset.wgsl"),
            source: wgpu::ShaderSource::Wgsl(include_str!("gpu_offset.wgsl").into()),
        });
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some("offset_patch"),
            layout: Some(&layout),
            module: &module,
            entry_point: Some("offset_patch"),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    use wgpu::util::DeviceExt;
    use wgpu::BufferUsages as U;
    let image = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("SPP offset image"),
        contents: bytemuck::cast_slice(data),
        usage: U::STORAGE,
    });
    let mut results = Vec::with_capacity(scales.len());
    for scale in scales {
        control.check()?;
        let n = scale.n as usize;
        let count = scale.patches.len();
        let per = 1 + 4 * n;
        if count == 0 {
            results.push(Vec::new());
            continue;
        }
        let prm = OffsetParams {
            stride: stride as u32,
            n: scale.n,
            terms: scale.terms as u32,
            area: (n * n) as u32,
            patches: count as u32,
            x_limit: f32::from(x_limit),
            size_f: scale.n as f32,
            pad: 0,
        };
        let params = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::bytes_of(&prm),
            usage: U::STORAGE,
        });
        let mut tabs = scale.inverse.to_vec();
        tabs.extend_from_slice(scale.basis);
        let tabs = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&tabs),
            usage: U::STORAGE,
        });
        let flat: Vec<u32> = scale.patches.iter().flatten().copied().collect();
        let patches = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: None,
            contents: bytemuck::cast_slice(&flat),
            usage: U::STORAGE,
        });
        let bytes = (count * per * 4) as u64;
        let out = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: U::STORAGE | U::COPY_SRC,
            mapped_at_creation: false,
        });
        let read = device.create_buffer(&wgpu::BufferDescriptor {
            label: None,
            size: bytes,
            usage: U::MAP_READ | U::COPY_DST,
            mapped_at_creation: false,
        });
        fn entry(i: u32, b: &wgpu::Buffer) -> wgpu::BindGroupEntry<'_> {
            wgpu::BindGroupEntry { binding: i, resource: b.as_entire_binding() }
        }
        let g0 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &ctx.layout0,
            entries: &[entry(0, &params), entry(1, &tabs)],
        });
        let slots = [&image, &patches, &out, &ctx.dummies[3], &ctx.dummies[4], &ctx.dummies[5]];
        let entries: Vec<_> = slots.iter().enumerate().map(|(i, b)| entry(i as u32, b)).collect();
        let g1 = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: None,
            layout: &ctx.layout1,
            entries: &entries,
        });
        let mut encoder = device.create_command_encoder(&Default::default());
        {
            let mut pass = encoder.begin_compute_pass(&Default::default());
            pass.set_pipeline(&pipeline);
            pass.set_bind_group(0, &g0, &[]);
            pass.set_bind_group(1, &g1, &[]);
            let gx = count.min(65535) as u32;
            pass.dispatch_workgroups(gx, count.div_ceil(65535) as u32, 1);
        }
        encoder.copy_buffer_to_buffer(&out, 0, &read, 0, bytes);
        queue.submit([encoder.finish()]);
        let (send, recv) = mpsc::sync_channel(1);
        read.slice(..).map_async(wgpu::MapMode::Read, move |r| {
            let _ = send.send(r);
        });
        let started = Instant::now();
        loop {
            control.check()?;
            ctx.check()?;
            match device.poll(wgpu::PollType::Wait {
                submission_index: None,
                timeout: Some(Duration::from_millis(5)),
            }) {
                Ok(status) if status.is_queue_empty() => break,
                Ok(_) | Err(wgpu::PollError::Timeout) => {}
                Err(error) => return Err(gpu_error(error)),
            }
            if started.elapsed() > Duration::from_secs(60) {
                return Err(gpu_error("GPU timeout during the offset correction"));
            }
        }
        recv.recv_timeout(Duration::from_secs(10))
            .map_err(|_| gpu_error("mapping failed"))?
            .map_err(gpu_error)?;
        let values: Vec<f32> = bytemuck::cast_slice(&read.slice(..).get_mapped_range()).to_vec();
        read.unmap();
        results.push(values);
    }
    Ok(results)
}

//! Photos with preserved gamut and HDR in the normal egui drawing.
//!
//! On macOS the window surface is `Rgba16Float` with the colour space *extended sRGB* (sRGB-encoded values,
//! below 0 and above 1 allowed; see `third_party/egui-wgpu/MODIFICATIONS.md`). egui draws the UI exactly as on an
//! 8-bit surface (same values, same blending in gamma space). Photos with colours beyond sRGB are uploaded as
//! float textures ([`PhotoTex`], `Rgba16Float`, extended sRGB-encoded) and drawn by egui like every other texture:
//! fitted, zoomed, panned, in the loupe, the navigator, the comparison, with clipping and tint, in the same frame
//! as the UI – there is no separate layer that could lag behind, stretch or cover menus.
//!
//! The colour proof (soft proof, gamut warning) is a uniform of egui's shader that applies to all photo textures
//! ([`frame`]); switching computes nothing. 8-bit photos (thumbnails, previews of film and monochrome) are sRGB,
//! they lie inside every proof space, so the proof leaves them unchanged.
//!
//! HDR photos (gain map, e.g. iPhone): developed image × gain map, values above 1 = brighter than SDR white. The
//! window asks for EDR (extended dynamic range) only while such a photo is on screen ([`frame`]).
//!
//! Without a float surface (Windows, test hook `GRAINEO_SURFACE=8`) nothing of this is active: photos are 8-bit
//! sRGB as before.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use eframe::egui;
use eframe::egui_wgpu::{self, wgpu};
use half::f16;

static RENDER_STATE: OnceLock<egui_wgpu::RenderState> = OnceLock::new();
/// Texture ids to free at the start of the next frame (the frame being painted may still refer to them)
static TO_FREE: Mutex<Vec<egui::TextureId>> = Mutex::new(Vec::new());
/// Bytes of all photo textures alive (GPU memory, for the log and the budget)
static GPU_BYTES: AtomicUsize = AtomicUsize::new(0);

/// Before the window is created: ask egui-wgpu for a float surface (macOS). Test hook `GRAINEO_SURFACE=8`
/// keeps the 8-bit surface (comparison measurements).
pub fn prefer_float_surface() {
    if cfg!(target_os = "macos") && crate::env::var("GRAINEO_SURFACE").as_deref() != Ok("8") {
        egui_wgpu::PREFER_FLOAT_SURFACE.store(true, Ordering::Relaxed);
    }
}

/// At start (App::new): remember the render state of the window.
pub fn init(cc: &eframe::CreationContext) {
    if let Some(rs) = cc.wgpu_render_state.clone() {
        let a = rs.adapter.get_info();
        crate::log(&format!("display: surface {:?}, adapter {} ({:?}, {:?}, vendor 0x{:04x}, driver {} {})", rs.target_format, a.name, a.backend, a.device_type, a.vendor, a.driver, a.driver_info));
        crate::gpu_guard::note_window_adapter(&a);
        // wgpu ends the program on an uncaught GPU error (panic in the main thread, the window simply disappears):
        // in the window it costs one picture instead – logged (the first 20, then every 1000th)
        static GPU_ERRORS: AtomicUsize = AtomicUsize::new(0);
        rs.device.on_uncaptured_error(std::sync::Arc::new(|e: wgpu::Error| {
            let n = GPU_ERRORS.fetch_add(1, Ordering::Relaxed) + 1;
            if n <= 20 || n % 1000 == 0 {
                crate::log(&format!("display: GPU error #{n} (caught, the app goes on): {e}"));
            }
        }));
        // a lost device (driver reset, graphics switch, sleep) cannot be recovered by eframe: end cleanly (crash_log)
        rs.device.set_device_lost_callback(|reason, message| {
            if reason != wgpu::DeviceLostReason::Destroyed {
                crate::crash_log::note_device_lost(format!("{reason:?}: {message}"));
            }
        });
        let _ = RENDER_STATE.set(rs);
    }
}

/// Does the window draw into a float surface (colours beyond sRGB and HDR possible)?
pub fn float_surface() -> bool {
    RENDER_STATE.get().is_some_and(|r| r.target_format == wgpu::TextureFormat::Rgba16Float)
}

static WIDE_ON: OnceLock<AtomicBool> = OnceLock::new();

fn wide_flag() -> &'static AtomicBool {
    WIDE_ON.get_or_init(|| {
        // Setting "wideGamutDisplay" (default: on); test hook GRAINEO_WIDE=0 switches it off
        let on = crate::env::var("GRAINEO_WIDE").as_deref() != Ok("0") && library::settings::value("wideGamutDisplay").as_bool().unwrap_or(true);
        AtomicBool::new(on)
    })
}

/// Setting "Wide color space in the display" (independent of the screen).
pub fn wide_on() -> bool {
    wide_flag().load(Ordering::Relaxed)
}

/// Toggle of the setting (also saved).
pub fn set_wide(on: bool) {
    wide_flag().store(on, Ordering::Relaxed);
    library::settings::set("wideGamutDisplay", serde_json::json!(on));
}

/// Are wide-gamut versions computed and shown (setting on and a float surface)?
pub fn wide_active() -> bool {
    wide_on() && float_surface()
}

/// Largest headroom of the main display now or possible (1 = SDR only).
pub fn screen_headroom() -> (f32, f32) {
    // Test hook: GRAINEO_HDR_HEADROOM=<factor> pretends the display has this headroom
    if let Some(h) = crate::env::var("GRAINEO_HDR_HEADROOM").ok().and_then(|v| v.parse::<f32>().ok()) {
        return (h.max(1.0), h.max(1.0));
    }
    #[cfg(target_os = "macos")]
    unsafe {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        let s: *mut AnyObject = msg_send![class!(NSScreen), mainScreen];
        if s.is_null() {
            return (1.0, 1.0);
        }
        let now: f64 = msg_send![s, maximumExtendedDynamicRangeColorComponentValue];
        let possible: f64 = msg_send![s, maximumPotentialExtendedDynamicRangeColorComponentValue];
        (now.max(1.0) as f32, possible.max(1.0) as f32)
    }
    #[cfg(not(target_os = "macos"))]
    (1.0, 1.0)
}

/// A photo as float texture (`Rgba16Float`, extended sRGB-encoded), registered with egui (draw it with its
/// [`Self::id`] like any texture). Freed when dropped.
pub struct PhotoTex {
    id: egui::TextureId,
    /// The same texture with bilinear magnification (registered only for `nearest` textures, see [`Self::id_smooth`])
    smooth: Option<egui::TextureId>,
    size: [usize; 2],
    bytes: usize,
    _texture: wgpu::Texture,
}

impl std::fmt::Debug for PhotoTex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PhotoTex({:?}, {}×{})", self.id, self.size[0], self.size[1])
    }
}

impl PhotoTex {
    /// Upload `rgba` (`w`×`h`, f16, extended sRGB-encoded) – from any thread. `nearest`: magnified pixels stay
    /// pixels (zoomed view); `hdr`: values above 1 are brightness. `None` without a float surface.
    pub fn upload(w: usize, h: usize, rgba: &[f16], nearest: bool, hdr: bool) -> Option<PhotoTex> {
        let rs = RENDER_STATE.get().filter(|_| float_surface())?;
        if w == 0 || h == 0 || rgba.len() < w * h * 4 {
            return None;
        }
        let max = rs.device.limits().max_texture_dimension_2d as usize;
        if w > max || h > max {
            return None;
        }
        let size = wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 };
        let texture = rs.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("photo"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba16Float,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        upload_pixels(rs, &texture, bytemuck::cast_slice(&rgba[..w * h * 4]), w * 8, w, h);
        let view = texture.create_view(&Default::default());
        let (id, smooth) = {
            let mut r = rs.renderer.write();
            let id = r.register_photo_texture(&rs.device, &view, nearest, hdr);
            // a second binding of the same texture with bilinear magnification (no extra GPU memory)
            (id, nearest.then(|| r.register_photo_texture(&rs.device, &view, false, hdr)))
        };
        let bytes = w * h * 8;
        GPU_BYTES.fetch_add(bytes, Ordering::Relaxed);
        Some(PhotoTex { id, smooth, size: [w, h], bytes, _texture: texture })
    }

    /// Upload `rgb` (`w`×`h`, 8 bit sRGB) as an ordinary 8-bit texture (drawn exactly like an egui texture), with
    /// a nearest and a bilinear binding (see [`Self::id_smooth`]) – from any thread. Works on every surface (also
    /// without float surface); `None` without a wgpu render state or for a texture too large for the device.
    pub fn upload_rgb8(w: usize, h: usize, rgb: &[u8]) -> Option<PhotoTex> {
        let rs = RENDER_STATE.get()?;
        if w == 0 || h == 0 || rgb.len() < w * h * 3 {
            return None;
        }
        let max = rs.device.limits().max_texture_dimension_2d as usize;
        if w > max || h > max {
            return None;
        }
        let rgba: Vec<u8> = rgb[..w * h * 3].chunks_exact(3).flat_map(|p| [p[0], p[1], p[2], 255]).collect();
        let size = wgpu::Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 };
        // same format as egui's own textures (sRGB-encoded values, blended like the rest of the UI)
        let texture = rs.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("photo-8bit"),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        upload_pixels(rs, &texture, &rgba, w * 4, w, h);
        let view = texture.create_view(&Default::default());
        let sampler = |mag: wgpu::FilterMode| wgpu::SamplerDescriptor { label: Some("photo-8bit"), mag_filter: mag, min_filter: wgpu::FilterMode::Linear, ..Default::default() };
        let (id, smooth) = {
            let mut r = rs.renderer.write();
            let id = r.register_native_texture_with_sampler_options(&rs.device, &view, sampler(wgpu::FilterMode::Nearest));
            (id, Some(r.register_native_texture_with_sampler_options(&rs.device, &view, sampler(wgpu::FilterMode::Linear))))
        };
        let bytes = w * h * 4;
        GPU_BYTES.fetch_add(bytes, Ordering::Relaxed);
        Some(PhotoTex { id, smooth, size: [w, h], bytes, _texture: texture })
    }

    pub fn id(&self) -> egui::TextureId {
        self.id
    }

    /// The texture with bilinear magnification (the same as [`Self::id`] if that one is bilinear already).
    pub fn id_smooth(&self) -> egui::TextureId {
        self.smooth.unwrap_or(self.id)
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// Rows per staging buffer of [`upload_pixels`] (bounded so that a huge photo needs no second copy of its size).
const UPLOAD_CHUNK_BYTES: usize = 16 << 20;

/// Copy `data` (`h` rows of `row_bytes`, tightly packed) into `texture` without holding the window's GPU queue
/// while the pixels are copied.
///
/// `Queue::write_texture` copies the whole picture into its staging memory while it holds the queue's lock of pending
/// writes, which every frame of the window needs for its own submit: for a large photo uploaded from a background job
/// (tens to hundreds of MB, slow under memory pressure) the window stood still until the copy was done – seconds up to
/// minutes (QA 10, H-1). Here the pixels go into buffers mapped at creation (no lock of the queue held while they are
/// copied), in bands of at most [`UPLOAD_CHUNK_BYTES`]; the queue is only taken for the short submit of the copy
/// commands.
fn upload_pixels(rs: &egui_wgpu::RenderState, texture: &wgpu::Texture, data: &[u8], row_bytes: usize, w: usize, h: usize) {
    let align = wgpu::COPY_BYTES_PER_ROW_ALIGNMENT as usize;
    let padded = row_bytes.div_ceil(align) * align;
    let rows_per_chunk = (UPLOAD_CHUNK_BYTES / padded).clamp(1, h.max(1));
    let mut encoder = rs.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("photo-upload") });
    let mut y = 0;
    while y < h {
        let rows = rows_per_chunk.min(h - y);
        let buffer = rs.device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("photo-upload"),
            size: (padded * rows) as u64,
            usage: wgpu::BufferUsages::COPY_SRC | wgpu::BufferUsages::MAP_WRITE,
            mapped_at_creation: true,
        });
        {
            let Ok(mut view) = buffer.slice(..).get_mapped_range_mut() else {
                // not mappable (should not happen): this band the ordinary way
                rs.queue.write_texture(
                    wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: y as u32, z: 0 }, aspect: wgpu::TextureAspect::All },
                    &data[y * row_bytes..(y + rows) * row_bytes],
                    wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(row_bytes as u32), rows_per_image: Some(rows as u32) },
                    wgpu::Extent3d { width: w as u32, height: rows as u32, depth_or_array_layers: 1 },
                );
                y += rows;
                continue;
            };
            for r in 0..rows {
                let src = &data[(y + r) * row_bytes..(y + r + 1) * row_bytes];
                view.slice(r * padded..r * padded + row_bytes).copy_from_slice(src);
            }
        }
        buffer.unmap();
        encoder.copy_buffer_to_texture(
            wgpu::TexelCopyBufferInfo { buffer: &buffer, layout: wgpu::TexelCopyBufferLayout { offset: 0, bytes_per_row: Some(padded as u32), rows_per_image: Some(rows as u32) } },
            wgpu::TexelCopyTextureInfo { texture, mip_level: 0, origin: wgpu::Origin3d { x: 0, y: y as u32, z: 0 }, aspect: wgpu::TextureAspect::All },
            wgpu::Extent3d { width: w as u32, height: rows as u32, depth_or_array_layers: 1 },
        );
        y += rows;
    }
    rs.queue.submit([encoder.finish()]);
}

impl Drop for PhotoTex {
    fn drop(&mut self) {
        GPU_BYTES.fetch_sub(self.bytes, Ordering::Relaxed);
        if let Ok(mut l) = TO_FREE.lock() {
            l.push(self.id);
            l.extend(self.smooth);
        }
    }
}

/// Bytes of all photo textures alive.
pub fn gpu_bytes() -> usize {
    GPU_BYTES.load(Ordering::Relaxed)
}

/// Sign-preserving sRGB transfer (the encoding of extended sRGB).
pub fn encode(v: f32) -> f32 {
    let a = v.abs();
    let e = if a <= 0.0031308 { a * 12.92 } else { 1.055 * a.powf(1.0 / 2.4) - 0.055 };
    e.copysign(v)
}

fn srgb_linear(v: f32) -> f32 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Extended sRGB-encoded RGB values (f32, as the wide output stages deliver them) → RGBA f16 (alpha 1).
pub fn rgba_from_encoded(rgb: &[f32]) -> Vec<f16> {
    use rayon::prelude::*;
    let mut rgba = vec![f16::ONE; rgb.len() / 3 * 4];
    rgba.par_chunks_mut(4).zip(rgb.par_chunks(3)).for_each(|(o, p)| {
        for c in 0..3 {
            o[c] = f16::from_f32(if p[c].is_finite() { p[c].clamp(-60000.0, 60000.0) } else { 0.0 });
        }
    });
    rgba
}

/// [`rgba_from_encoded`] with triangular dither of ±1 step of an 8-bit display (fixed pattern from the pixel
/// position): a merged photo (HDR) has smooth, almost noise-free gradients that would otherwise show contour steps
/// where the display quantises to 8 bits. The mean stays the same.
pub fn rgba_from_encoded_dithered(rgb: &[f32]) -> Vec<f16> {
    use rayon::prelude::*;
    let hash = |mut x: u32| {
        x ^= x >> 16;
        x = x.wrapping_mul(0x7feb_352d);
        x ^= x >> 15;
        x = x.wrapping_mul(0x846c_a68b);
        x ^ (x >> 16)
    };
    let mut rgba = vec![f16::ONE; rgb.len() / 3 * 4];
    rgba.par_chunks_mut(4).zip(rgb.par_chunks(3)).enumerate().for_each(|(i, (o, p))| {
        let a = hash(i as u32 * 2 + 1) as f32 / u32::MAX as f32;
        let b = hash(i as u32 * 2 + 2) as f32 / u32::MAX as f32;
        let d = (a - b) / 255.0;
        for c in 0..3 {
            o[c] = f16::from_f32(if p[c].is_finite() { (p[c] + d).clamp(-60000.0, 60000.0) } else { 0.0 });
        }
    });
    rgba
}

/// Encoded pixels for a photo texture: dithered for a merged photo, as they are for every other photo.
pub fn rgba_for(doc: Option<&rawdev::RawDoc>, rgb: &[f32]) -> Vec<f16> {
    if doc.is_some_and(|d| d.merged.is_some()) && crate::env::var_os("GRAINEO_HDR_NO_DITHER").is_none() {
        rgba_from_encoded_dithered(rgb)
    } else {
        rgba_from_encoded(rgb)
    }
}

/// Half resolution (2×2 mean) of an RGBA f16 image: the level for strongly reduced views.
pub fn half_rgba(w: usize, h: usize, rgba: &[f16]) -> (usize, usize, Vec<f16>) {
    use rayon::prelude::*;
    let (hw, hh) = (w.div_ceil(2).max(1), h.div_ceil(2).max(1));
    let mut out = vec![f16::ONE; hw * hh * 4];
    out.par_chunks_mut(hw * 4).enumerate().for_each(|(y, row)| {
        let (y0, y1) = ((y * 2).min(h - 1), (y * 2 + 1).min(h - 1));
        for x in 0..hw {
            let (x0, x1) = ((x * 2).min(w - 1), (x * 2 + 1).min(w - 1));
            for c in 0..3 {
                let v = |xx: usize, yy: usize| rgba[(yy * w + xx) * 4 + c].to_f32();
                row[x * 4 + c] = f16::from_f32((v(x0, y0) + v(x1, y0) + v(x0, y1) + v(x1, y1)) * 0.25);
            }
        }
    });
    (hw, hh, out)
}

/// HDR preview: developed image (8 bit sRGB, `w`×`h`) × gain map with weight `k` → RGBA f16, extended
/// sRGB-encoded (values above 1 = brighter than SDR white).
pub fn hdr_rgba(rgb: &[u8], w: usize, h: usize, map: &rawdev::doc::HdrMap, k: f32) -> Vec<f16> {
    use rayon::prelude::*;
    let lin: Vec<f32> = (0..256).map(|i| srgb_linear(i as f32 / 255.0)).collect();
    let mut rgba = vec![f16::ONE; w * h * 4];
    rgba.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        let ky = (y * map.h / h.max(1)).min(map.h - 1);
        for x in 0..w {
            let kx = (x * map.w / w.max(1)).min(map.w - 1);
            let g = (map.log2[ky * map.w + kx] * k).exp2();
            let i = (y * w + x) * 3;
            for c in 0..3 {
                row[x * 4 + c] = f16::from_f32(encode(lin[rgb[i + c] as usize] * g));
            }
        }
    });
    rgba
}

/// HDR crop: `region` of the sensor image at full resolution (8 bit sRGB) × gain map → RGBA f16, extended
/// sRGB-encoded. The map (at preview size) is laid bilinearly over the pixels of the crop; `full` = size of the
/// whole sensor image.
/// Returns the pixels and the largest gain (linear factor over SDR white) laid over the crop.
pub fn hdr_detail_rgba(rgb: &[u8], w: usize, h: usize, region: [usize; 4], map: &rawdev::doc::HdrMap, k: f32, full: [usize; 2]) -> (Vec<f16>, f32) {
    use rayon::prelude::*;
    let lin: Vec<f32> = (0..256).map(|i| srgb_linear(i as f32 / 255.0)).collect();
    let mut rgba = vec![f16::ONE; w * h * 4];
    let (sx, sy) = (region[2] as f32 / w.max(1) as f32, region[3] as f32 / h.max(1) as f32);
    let (kw, kh) = (map.w as f32 / full[0].max(1) as f32, map.h as f32 / full[1].max(1) as f32);
    let peak = rgba.par_chunks_mut(w * 4).enumerate().map(|(y, row)| {
        let mut peak = 1.0f32;
        let py = ((region[1] as f32 + (y as f32 + 0.5) * sy) * kh - 0.5).clamp(0.0, (map.h - 1) as f32);
        let (y0, ty) = (py.floor() as usize, py.fract());
        let y1 = (y0 + 1).min(map.h - 1);
        for x in 0..w {
            let px = ((region[0] as f32 + (x as f32 + 0.5) * sx) * kw - 0.5).clamp(0.0, (map.w - 1) as f32);
            let (x0, tx) = (px.floor() as usize, px.fract());
            let x1 = (x0 + 1).min(map.w - 1);
            let v = |xx: usize, yy: usize| map.log2[yy * map.w + xx];
            let a = v(x0, y0) + tx * (v(x1, y0) - v(x0, y0));
            let b = v(x0, y1) + tx * (v(x1, y1) - v(x0, y1));
            let g = ((a + ty * (b - a)) * k).exp2();
            peak = peak.max(g);
            let i = (y * w + x) * 3;
            for c in 0..3 {
                row[x * 4 + c] = f16::from_f32(encode(lin[rgb[i + c] as usize] * g));
            }
        }
        peak
    }).reduce(|| 1.0, f32::max);
    (rgba, peak)
}

/// Photo textures of one image: per tile its texture and its pixel rectangle (x, y, w, h) in the image.
pub type Tiles = Vec<(Arc<PhotoTex>, [usize; 4])>;

/// An image split into textures of at most `max_side` per edge (see [`Tiles`]). `None` without a float surface
/// or when an upload fails.
pub fn tiles(w: usize, h: usize, rgba: &[f16], max_side: usize, nearest: bool, hdr: bool) -> Option<Tiles> {
    let edge = max_side.max(256);
    let mut out = Vec::new();
    for ty in (0..h).step_by(edge) {
        for tx in (0..w).step_by(edge) {
            let (tw, th) = ((w - tx).min(edge), (h - ty).min(edge));
            let tex = if (tw, th) == (w, h) {
                PhotoTex::upload(w, h, rgba, nearest, hdr)?
            } else {
                let mut part = Vec::with_capacity(tw * th * 4);
                for y in ty..ty + th {
                    part.extend_from_slice(&rgba[(y * w + tx) * 4..(y * w + tx + tw) * 4]);
                }
                PhotoTex::upload(tw, th, &part, nearest, hdr)?
            };
            out.push((Arc::new(tex), [tx, ty, tw, th]));
        }
    }
    Some(out)
}

/// An 8-bit sRGB image (`w`×`h` RGB) split into textures of at most `max_side` per edge, uploaded with
/// [`PhotoTex::upload_rgb8`]. `None` without a wgpu render state or when an upload fails.
pub fn tiles_rgb8(w: usize, h: usize, rgb: &[u8], max_side: usize) -> Option<Tiles> {
    let edge = max_side.max(256);
    let mut out = Vec::new();
    for ty in (0..h).step_by(edge) {
        for tx in (0..w).step_by(edge) {
            let (tw, th) = ((w - tx).min(edge), (h - ty).min(edge));
            let tex = if (tw, th) == (w, h) {
                PhotoTex::upload_rgb8(w, h, rgb)?
            } else {
                let mut part = Vec::with_capacity(tw * th * 3);
                for y in ty..ty + th {
                    part.extend_from_slice(&rgb[(y * w + tx) * 3..(y * w + tx + tw) * 3]);
                }
                PhotoTex::upload_rgb8(tw, th, &part)?
            };
            out.push((Arc::new(tex), [tx, ty, tw, th]));
        }
    }
    Some(out)
}

/// Half resolution (2×2 mean) of an 8-bit RGB image: the level for reduced views of crops.
pub fn half_rgb8(w: usize, h: usize, rgb: &[u8]) -> (usize, usize, Vec<u8>) {
    use rayon::prelude::*;
    let (hw, hh) = (w.div_ceil(2).max(1), h.div_ceil(2).max(1));
    let mut out = vec![0u8; hw * hh * 3];
    out.par_chunks_mut(hw * 3).enumerate().for_each(|(y, row)| {
        let (y0, y1) = ((y * 2).min(h - 1), (y * 2 + 1).min(h - 1));
        for x in 0..hw {
            let (x0, x1) = ((x * 2).min(w - 1), (x * 2 + 1).min(w - 1));
            for c in 0..3 {
                let v = |xx: usize, yy: usize| rgb[(yy * w + xx) * 3 + c] as u32;
                row[x * 3 + c] = ((v(x0, y0) + v(x1, y0) + v(x0, y1) + v(x1, y1) + 2) / 4) as u8;
            }
        }
    });
    (hw, hh, out)
}

/// Largest edge of a texture of the device (for the tiles), 8192 without a render state.
pub fn max_texture_side() -> usize {
    RENDER_STATE.get().map_or(8192, |r| r.device.limits().max_texture_dimension_2d as usize)
}

/// Last proof state written to the renderer (`proof::generation`)
static PROOF_SENT: AtomicU64 = AtomicU64::new(0);
/// HDR versions are drawn in this frame (set by the app from [`State::hdr_shown`])
static HDR_FRAME: AtomicBool = AtomicBool::new(false);
/// an HDR texture was drawn in the last frame (the window keeps EDR)
static HDR_DRAWN: AtomicBool = AtomicBool::new(false);

/// Are HDR versions drawn in this frame (instead of the 8-bit ones)?
pub fn hdr_frame() -> bool {
    let on = HDR_FRAME.load(Ordering::Relaxed);
    if on {
        HDR_DRAWN.store(true, Ordering::Relaxed);
    }
    on
}

/// An HDR texture is being drawn (the window asks for EDR from the next frame on).
pub fn hdr_drawn() {
    HDR_DRAWN.store(true, Ordering::Relaxed);
}

/// Every frame (start of `App::ui`): free textures dropped in the last frame, hand the colour proof to egui's
/// shader on change and ask the window for EDR while an HDR photo is shown. `hdr`: HDR versions may be drawn in
/// this frame ([`State::hdr_shown`]).
pub fn frame(frame: &eframe::Frame, hdr: bool) {
    HDR_FRAME.store(hdr, Ordering::Relaxed);
    let hdr_shown = HDR_DRAWN.swap(false, Ordering::Relaxed);
    let Some(rs) = RENDER_STATE.get() else { return };
    let free: Vec<egui::TextureId> = TO_FREE.lock().map(|mut l| std::mem::take(&mut *l)).unwrap_or_default();
    let generation = crate::proof::generation();
    if !free.is_empty() || PROOF_SENT.load(Ordering::Relaxed) != generation {
        let mut r = rs.renderer.write();
        for id in &free {
            r.free_texture(id);
        }
        if PROOF_SENT.swap(generation, Ordering::Relaxed) != generation {
            r.set_photo_proof(proof_uniform());
        }
    }
    crate::display_color::set_edr(frame, float_surface() && hdr_shown);
}

/// The colour proof for egui's shader: matrices linear sRGB ↔ proof space, flags, warning colour (encoded).
pub fn proof_uniform() -> [f32; 32] {
    let mut u = crate::proof::uniform(graineo_color::Space::Srgb, false);
    // warning colour: linear sRGB → encoded (magenta stays 1, 0, 1)
    for c in 28..31 {
        u[c] = encode(u[c]);
    }
    u
}

/// Visible image rectangle of the last frame in pixels (x, y, w, h), for [`capture_note`]
static IMAGE_RECT: Mutex<Option<[u32; 4]>> = Mutex::new(None);

/// Remember where the photo is on screen (points → pixels), for the screenshot statistics.
pub fn note_image_rect(r: egui::Rect, ppp: f32) {
    if let Ok(mut g) = IMAGE_RECT.lock() {
        *g = Some([(r.min.x * ppp).max(0.0) as u32, (r.min.y * ppp).max(0.0) as u32, (r.width() * ppp) as u32, (r.height() * ppp) as u32]);
    }
}

/// After a screenshot (test hook): log what the float surface held in the image area (the PNG is 8 bit).
pub fn capture_note(path: &str) {
    let Some(rect) = IMAGE_RECT.lock().ok().and_then(|g| *g) else { return };
    if let Some((outside, bright, max)) = capture_stats(rect) {
        crate::log(&format!("testbild {path}: image area {}×{} px, outside sRGB {:.2} %, a component above 1 {:.2} %, largest value {max:.3}", rect[2], rect[3], outside * 100.0, bright * 100.0));
    }
}

/// Statistics of the last screenshot of the float surface inside `rect` (pixels): share of pixels outside sRGB
/// (a component below −1/255 or above 1 + 1/255), share above SDR white (HDR) and the largest value. For the test
/// hook `GRAINEO_SCREENSHOT` (the 8-bit PNG cannot show them).
pub fn capture_stats(rect: [u32; 4]) -> Option<(f64, f64, f32)> {
    let guard = egui_wgpu::capture::LAST_FLOAT_CAPTURE.lock().ok()?;
    let (w, h, v) = guard.as_ref()?;
    let [x0, y0, rw, rh] = rect;
    let (x1, y1) = ((x0 + rw).min(*w), (y0 + rh).min(*h));
    let (mut n, mut outside, mut bright, mut max) = (0usize, 0usize, 0usize, 0f32);
    for y in y0..y1 {
        for x in x0..x1 {
            let p = &v[((y * w + x) * 4) as usize..((y * w + x) * 4 + 3) as usize];
            n += 1;
            let lo = p.iter().any(|c| *c < -1.0 / 255.0);
            let hi = p.iter().any(|c| *c > 1.0 + 1.0 / 255.0);
            // above white in all three: brightness (HDR), otherwise a colour beyond sRGB
            let all_hi = p.iter().all(|c| *c > 1.0 + 1.0 / 255.0);
            if lo || (hi && !all_hi) {
                outside += 1;
            }
            if hi {
                bright += 1;
            }
            max = p.iter().fold(max, |m, c| m.max(*c));
        }
    }
    (n > 0).then(|| (outside as f64 / n as f64, bright as f64 / n as f64, max))
}

/// An HDR crop converted in the background (see [`State::new_hdr_detail`]).
pub struct HdrDetail {
    pub index: usize,
    pub settings: develop::Settings,
    pub region: [usize; 4],
    pub size: [usize; 2],
    pub tiles: Tiles,
    /// largest gain over SDR white in this crop (log and tests: > 1 means the crop glows in EDR)
    pub peak: f32,
}

enum Ready {
    Preview { generation: u64, index: usize, settings: develop::Settings, tex: PhotoTex },
    Detail { generation: u64, detail: HdrDetail },
}

/// The display versions of the photo on screen: its preview with preserved gamut or in HDR (the 8-bit preview
/// stays the basis and is shown until the matching version is there).
pub struct State {
    /// "Show HDR" toggle
    pub hdr_on: bool,
    /// The display supports EDR (None: not checked yet)
    pub hdr_possible: Option<bool>,
    /// wide or HDR version of the preview: photo index, settings of the render it belongs to, texture, HDR
    preview: Option<(usize, develop::Settings, Arc<PhotoTex>, bool)>,
    /// weight of the gain map for this display (from the preview)
    weight: f32,
    tx: std::sync::mpsc::Sender<Ready>,
    rx: std::sync::mpsc::Receiver<Ready>,
    /// counts every photo change: conversions of an earlier photo are dropped
    generation: u64,
}

impl Default for State {
    fn default() -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        State { hdr_on: crate::env::var("GRAINEO_HDR").as_deref() != Ok("0"), hdr_possible: None, preview: None, weight: 0.0, tx, rx, generation: 0 }
    }
}

impl State {
    /// Another photo (or none): the versions of the previous one must never be shown over it.
    pub fn clear(&mut self) {
        self.preview = None;
        self.generation += 1;
    }

    /// The preview with preserved gamut arrived (job `RenderWide`/`RenderWideSigma`).
    pub fn set_wide_preview(&mut self, index: usize, settings: develop::Settings, tex: Arc<PhotoTex>) {
        self.preview = Some((index, settings, tex, false));
    }

    /// New developed preview (8 bit sRGB) of a photo with gain map: the HDR version in the background.
    pub fn new_hdr_preview(&mut self, ctx: &egui::Context, index: usize, settings: develop::Settings, w: usize, h: usize, rgb: Vec<u8>, map: Arc<rawdev::doc::HdrMap>) {
        let (now, possible) = screen_headroom();
        self.hdr_possible = Some(possible > 1.05);
        let k = map.weight(now.max(possible));
        self.weight = k;
        if !float_surface() {
            return;
        }
        let (tx, generation, ctx) = (self.tx.clone(), self.generation, ctx.clone());
        std::thread::spawn(move || {
            let rgba = hdr_rgba(&rgb, w, h, &map, k);
            if let Some(tex) = PhotoTex::upload(w, h, &rgba, false, true) {
                let _ = tx.send(Ready::Preview { generation, index, settings, tex });
                ctx.request_repaint();
            }
        });
    }

    /// New crop at full resolution (8 bit sRGB) of a photo with gain map: its HDR version in the background
    /// (taken with [`Self::poll`]). `full`: size of the whole sensor image.
    #[allow(clippy::too_many_arguments)]
    pub fn new_hdr_detail(&mut self, ctx: &egui::Context, index: usize, settings: develop::Settings, region: [usize; 4], w: usize, h: usize, rgb: Vec<u8>, map: Arc<rawdev::doc::HdrMap>, full: [usize; 2]) {
        if !float_surface() {
            return;
        }
        let (tx, generation, ctx, k) = (self.tx.clone(), self.generation, ctx.clone(), self.weight);
        std::thread::spawn(move || {
            let (rgba, peak) = hdr_detail_rgba(&rgb, w, h, region, &map, k, full);
            if let Some(tiles) = tiles(w, h, &rgba, max_texture_side(), true, true) {
                let _ = tx.send(Ready::Detail { generation, detail: HdrDetail { index, settings, region, size: [w, h], tiles, peak } });
                ctx.request_repaint();
            }
        });
    }

    /// Every frame: take finished conversions; returns the HDR crops (the app keeps them next to the 8-bit crop).
    pub fn poll(&mut self) -> Vec<HdrDetail> {
        let mut out = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            match r {
                Ready::Preview { generation, index, settings, tex } if generation == self.generation => self.preview = Some((index, settings, Arc::new(tex), true)),
                Ready::Detail { generation, detail } if generation == self.generation => out.push(detail),
                _ => {}
            }
        }
        out
    }

    /// Is HDR shown for photos with a gain map (toggle on, display capable, float surface)?
    pub fn hdr_shown(&self) -> bool {
        self.hdr_on && self.hdr_possible != Some(false) && float_surface()
    }

    /// Texture to draw instead of the 8-bit preview of photo `index` whose last render had `rendered`
    /// settings: the wide or HDR version of exactly that render (`None`: the 8-bit preview).
    pub fn preview_id(&self, index: usize, rendered: Option<&develop::Settings>) -> Option<(egui::TextureId, bool)> {
        let (i, s, tex, hdr) = self.preview.as_ref()?;
        let on = if *hdr { self.hdr_shown() } else { wide_active() };
        (*i == index && rendered == Some(s) && on).then(|| (tex.id(), *hdr))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Map with a bright band in its right half (+2 stops), like glowing clouds.
    fn map() -> rawdev::doc::HdrMap {
        let (w, h) = (16, 8);
        let log2 = (0..w * h).map(|i| if i % w >= w / 2 { 2.0 } else { 0.0 }).collect();
        rawdev::doc::HdrMap { w, h, log2, headroom: 4.0, iso: true, base_headroom: 0.0, alt_headroom: 2.0 }
    }

    /// A zoomed crop (and the whole image) gets the gain of the map laid over its region: EDR > 1 where the map
    /// glows, exactly 1 where it does not. The map stays at preview size; the crop only cuts and scales it.
    #[test]
    fn zoomed_crop_keeps_gain() {
        let (m, full) = (map(), [1600, 800]);
        let rgb = vec![200u8; 100 * 50 * 3];
        let (_, peak) = hdr_detail_rgba(&rgb, 100, 50, [1200, 300, 100, 50], &m, 1.0, full);
        assert!((peak - 4.0).abs() < 1e-3, "peak {peak}");
        let (_, peak) = hdr_detail_rgba(&rgb, 100, 50, [100, 300, 100, 50], &m, 1.0, full);
        assert!((peak - 1.0).abs() < 1e-3, "peak {peak}");
        // whole image at full resolution (the zoomed view below 100 %): glows
        let rgb = vec![200u8; 1600 * 800 * 3];
        let (rgba, peak) = hdr_detail_rgba(&rgb, 1600, 800, [0, 0, 1600, 800], &m, 0.5, full);
        assert!((peak - 2.0).abs() < 1e-3, "peak {peak}");
        assert!(rgba[(400 * 1600 + 1500) * 4].to_f32() > 1.0);
    }
}

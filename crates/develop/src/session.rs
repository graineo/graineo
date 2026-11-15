//! An open image with cached processing stages.
//!
//! Only the raw data is kept (planar, u16, defective pixels already replaced,
//! ≈ 96 MB). The full floating-point image (≈ 177 MB) is only created on demand
//! (for export and the 1:1 view); the preview is computed directly from the raw data.

use std::path::{Path, PathBuf};

use x3f::{PropList, RawImage, X3fFile};

use crate::denoise::{self, DenoiseSliders};
use crate::{defects, filllight, linearize, linearize_scaled, render, Calibration, Linear, Rgb16, Settings, View};

pub struct Session {
    pub path: PathBuf,
    /// Shared so that preview rendering works without the session lock
    pub cal: std::sync::Arc<Calibration>,
    pub props: PropList,
    /// For Quattro the synthetic raw image from [`crate::quattro`] (shared with the cache)
    raw: std::sync::Arc<RawImage>,
    /// Full linear image without noise reduction (only for sensor views at 1:1)
    base: Option<Linear>,
    /// Full linear image according to the noise settings
    current: Option<(Option<(u8, u8)>, Linear)>,
    fill_map: Option<Vec<f32>>,
    /// SPP working image (counts before the tone curve) per colour mode and noise sliders, shared: the lock is held
    /// only to swap the `Arc`, rendering takes a snapshot and runs without any session lock
    /// ([`Session::spp_snapshot`], [`render_from_spp`])
    spp: std::sync::Mutex<Option<std::sync::Arc<SppBase>>>,
    /// [`spp_key_hash`] of the installed base (0 = none), readable without the lock ([`Session::has_spp_for`])
    spp_key: std::sync::atomic::AtomicU64,
    /// Damaged file: what was rescued and how the rest was filled (see [`crate::damaged`]); `None` when intact
    pub damage: Option<(x3f::Rescue, crate::damaged::Damage)>,
}

/// Sigma's exact base: preprocessing + noise reduction + colour matrix + ColorDQ. Shared between threads; its
/// Fill Light map and dehaze analysis are computed once on first use and never change.
pub struct SppBase {
    /// Colour mode and noise sliders the base was computed with
    pub key: crate::SppKey,
    pub counts: Linear,
    fill_map: std::sync::OnceLock<Vec<f32>>,
    haze: std::sync::OnceLock<std::sync::Arc<crate::dehaze::HazeField>>,
    /// Tone equalizer analysis of the whole base for the last mask setting (smoothing, edge sharpness)
    toneq: std::sync::Mutex<Option<std::sync::Arc<crate::tone_eq::Grid>>>,
}

impl SppBase {
    pub fn new(key: crate::SppKey, counts: Linear) -> Self {
        Self { key, counts, fill_map: std::sync::OnceLock::new(), haze: std::sync::OnceLock::new(), toneq: std::sync::Mutex::new(None) }
    }

    /// Tone equalizer mask of the whole base (computed once per mask setting; crops take their part of it). `None`
    /// when the settings do not need it.
    pub fn toneq(&self, cal: &Calibration, s: &Settings) -> Option<crate::tone_eq::Mask> {
        if !crate::tone_eq::needed(s) {
            return None;
        }
        let mut g = self.toneq.lock().unwrap_or_else(|e| e.into_inner());
        let grid = match g.as_ref().filter(|g| g.fits(&s.tone_eq)) {
            Some(grid) => grid.clone(),
            None => {
                let grid = std::sync::Arc::new(crate::tone_eq::Grid::analyze(&self.counts.data, self.counts.width, self.counts.height, cal.max_count(), &s.tone_eq));
                *g = Some(grid.clone());
                grid
            }
        };
        Some(crate::tone_eq::Mask::full(grid))
    }

    /// Fill Light map of the whole base (computed once).
    pub fn fill_map(&self) -> &[f32] {
        self.fill_map.get_or_init(|| filllight::log_gain_map_counts(&self.counts))
    }

    /// Dehaze analysis of the whole base (computed once; crops take their part of it).
    pub fn haze(&self) -> &std::sync::Arc<crate::dehaze::HazeField> {
        self.haze.get_or_init(|| std::sync::Arc::new(crate::dehaze::HazeField::analyze(&self.counts.data, self.counts.width, self.counts.height, 1.0)))
    }

    /// Colour matrix the base was created with.
    pub fn matrix(&self, cal: &Calibration) -> crate::matrix::Mat3 {
        cal.mode(self.key.mode.as_deref()).cam_to_srgb
    }

    /// Does this base fit the settings (same colour mode and noise sliders)?
    pub fn fits(&self, s: &Settings) -> bool {
        self.key == s.spp_key_class(self.key.prepared)
    }
}

/// FNV-1a over the text form of a base key (never 0), for the lock-free check [`Session::has_spp_for`].
pub fn spp_key_hash(k: &crate::SppKey) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in format!("{k:?}").bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h.max(1)
}

impl Session {
    pub fn open(path: impl AsRef<Path>) -> x3f::Result<Self> {
        Self::open_binned(path, crate::Binning::Auto)
    }

    /// Open for the settings of the photo: binning (Quattro at high ISO, see
    /// [`crate::quattro::binning_factor`]). A binned session has the small size throughout (raw data, calibration,
    /// [`Self::size`], Sigma's base); another binning needs another session ([`Self::fits_binning`]).
    pub fn open_for(path: impl AsRef<Path>, s: &Settings) -> x3f::Result<Self> {
        Self::open_binned(path, s.binning)
    }

    /// Does this session have the binning these settings call for? Otherwise reopen with [`Self::open_for`].
    pub fn fits_binning(&self, s: &Settings) -> bool {
        self.cal.binning == crate::quattro::binning_for(&self.cal, s)
    }

    fn open_binned(path: impl AsRef<Path>, binning: crate::Binning) -> x3f::Result<Self> {
        if crate::quattro::is_prepared(path.as_ref()) {
            // Quattro: decoding, preprocessing and upsampling via x3fuse-core. The own pipeline (first picture,
            // previews, sensor views) does not wait for Sigma's first noise reduction stage of the colour grid: it
            // opens with the layers before the stage; the Sigma base computes the stage in the background (on the same
            // x3fuse-core result) and replaces the picture when it is ready (`GRAINEO_QUATTRO_OPEN_STAGE1=1`: wait
            // for it as before)
            let q = if std::env::var("GRAINEO_QUATTRO_OPEN_STAGE1").is_ok_and(|x| x == "1") {
                crate::quattro::prepared(path.as_ref())
            } else {
                crate::quattro::prepared_quick(path.as_ref())
            }
            .map_err(x3f::Error::Unsupported)?;
            let n = crate::quattro::binning_factor(&q.cal, binning);
            let (cal, raw) = if n > 1 {
                let raw = crate::quattro::bin_raw(&q.raw, n);
                (std::sync::Arc::new(q.cal.binned(n, raw.columns, raw.rows)), std::sync::Arc::new(raw))
            } else {
                (q.cal.clone(), q.raw.clone())
            };
            return Ok(Self {
                path: path.as_ref().to_path_buf(),
                cal,
                props: q.props.clone(),
                raw,
                base: None,
                current: None,
                fill_map: None,
                spp: std::sync::Mutex::new(None),
                spp_key: std::sync::atomic::AtomicU64::new(0),
                damage: None,
            });
        }
        let (mut raw, cal, props, rescue) = {
            let file = X3fFile::open(path.as_ref())?;
            // damaged files (e.g. faulty memory card): rescue as much as possible instead of refusing
            let (raw, rescue) = file.decode_raw_rescue()?;
            let cal = Calibration::read(&file, &raw)?;
            (raw, cal, file.properties()?, rescue)
        };
        let damage = rescue.is_damaged().then(|| {
            let d = crate::damaged::fill(&mut raw, &rescue, &cal);
            (rescue, d)
        });
        defects::replace_bad_pixels_raw(&mut raw, &cal);
        Ok(Self { path: path.as_ref().to_path_buf(), cal: std::sync::Arc::new(cal), props, raw: std::sync::Arc::new(raw), base: None, current: None, fill_map: None, spp: std::sync::Mutex::new(None), spp_key: std::sync::atomic::AtomicU64::new(0), damage })
    }

    /// Discard large intermediate results (full resolution), keep raw data and calibration;
    /// for images that only remain in the cache.
    /// Raw data (defective pixels already replaced).
    pub fn raw(&self) -> &RawImage {
        &self.raw
    }

    /// Raw data shared (without copying), e.g. for the pipeline inspection without the session lock.
    pub fn raw_shared(&self) -> std::sync::Arc<RawImage> {
        self.raw.clone()
    }

    pub fn release_heavy(&mut self) {
        self.base = None;
        self.current = None;
        self.fill_map = None;
        *self.spp.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.spp_key.store(0, std::sync::atomic::Ordering::Release);
    }

    /// Size of the image area in pixels.
    pub fn size(&self) -> [usize; 2] {
        let [x0, y0, x1, y1] = self.cal.active;
        [x1 - x0 + 1, y1 - y0 + 1]
    }

    /// Damaged file: plane states per row of a preview made with `factor` and `height` rows (see [`Self::preview_linear`]).
    pub fn rescue_rows(&self, factor: usize, height: usize) -> Option<Vec<[u8; 3]>> {
        let (rescue, damage) = self.damage.as_ref()?;
        Some(damage.row_states(rescue, self.cal.active[1], factor, height))
    }

    /// Linear image at preview size (longer edge ≤ `max_edge`) and the downscale factor.
    pub fn preview_linear(&self, max_edge: usize) -> (Linear, usize) {
        let [w, h] = self.size();
        let factor = w.max(h).div_ceil(max_edge).max(1);
        (linearize_scaled(&self.raw, &self.cal, factor), factor)
    }

    /// Full linear image without noise reduction.
    pub fn base(&mut self) -> &Linear {
        self.base.get_or_insert_with(|| linearize(&self.raw, &self.cal))
    }

    /// Full linear image matching the noise settings (computed on demand, ≈1 s).
    pub fn linear(&mut self, s: &Settings) -> &Linear {
        let key = s.denoise_key();
        if self.current.as_ref().map(|c| c.0) != Some(key) {
            self.current = None;
            self.fill_map = None;
            let mut lin = linearize(&self.raw, &self.cal);
            if s.denoise {
                denoise::denoise(&mut lin, &self.cal, DenoiseSliders::new(s.nr_luma, s.nr_chroma));
            }
            self.current = Some((key, lin));
        }
        &self.current.as_ref().unwrap().1
    }

    /// Fill Light map of the current linear image.
    pub fn fill_map(&mut self, s: &Settings) -> &[f32] {
        self.linear(s);
        if self.fill_map.is_none() {
            let lin = &self.current.as_ref().unwrap().1;
            self.fill_map = Some(filllight::log_gain_map(lin, &self.cal));
        }
        self.fill_map.as_deref().unwrap()
    }

    /// Does this setting use the SPP base? (Sigma's noise reduction; the noise sliders
    /// act via the base as in SPP. Without noise reduction (which SPP does not offer) and in
    /// the sensor views, our own pipeline.)
    pub fn uses_spp(s: &Settings) -> bool {
        // The pipeline inspection shows the stages of the SPP base, so it needs it just the same
        (s.view == View::Developed || s.view.is_stage()) && s.denoise
    }

    /// Ensure the SPP base for the colour mode and noise sliders of the settings (≈ 3 s). The computation runs
    /// without the base lock; the finished base is then installed.
    pub fn ensure_spp(&self, s: &Settings) -> Result<std::sync::Arc<SppBase>, String> {
        let key = s.spp_key(&self.path);
        if let Some(b) = self.spp_snapshot().filter(|b| b.key == key) {
            return Ok(b);
        }
        // GPU if compiled in and available (otherwise CPU automatically)
        let counts = crate::diskcache::working_key(&self.path, &key, true, &std::sync::atomic::AtomicBool::new(false))?;
        let base = std::sync::Arc::new(SppBase::new(key, counts));
        self.install_spp_base(base.clone());
        Ok(base)
    }

    /// Insert a fully computed SPP base (computed outside any lock via
    /// [`crate::diskcache::working_key`]).
    pub fn install_spp(&self, key: crate::SppKey, counts: Linear) {
        self.install_spp_base(std::sync::Arc::new(SppBase::new(key, counts)));
    }

    /// Insert a shared base (lock held only for the swap; the key hash follows after the base is in place).
    pub fn install_spp_base(&self, base: std::sync::Arc<SppBase>) {
        // a base of another binning (computed for settings from before a switch) does not fit this session's size
        if self.cal.is_prepared() && [base.counts.width, base.counts.height] != self.size() {
            eprintln!("Foveon base {} × {} does not match the session {:?} (binning), discarded", base.counts.width, base.counts.height, self.size());
            return;
        }
        let hash = spp_key_hash(&base.key);
        *self.spp.lock().unwrap_or_else(|e| e.into_inner()) = Some(base);
        self.spp_key.store(hash, std::sync::atomic::Ordering::Release);
    }

    /// The installed base, shared (lock held only to clone the `Arc`); rendering from it needs no session lock.
    pub fn spp_snapshot(&self) -> Option<std::sync::Arc<SppBase>> {
        self.spp.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Is Sigma's base in place for these settings (full-resolution crops then come from it without
    /// recomputation)? Reads only the key hash, no lock.
    pub fn has_spp_for(&self, s: &Settings) -> bool {
        let h = self.spp_key.load(std::sync::atomic::Ordering::Acquire);
        h != 0 && h == spp_key_hash(&s.spp_key(&self.path))
    }

    pub fn spp_base(&self) -> Option<std::sync::Arc<SppBase>> {
        self.spp_snapshot()
    }

    /// Colour matrix the SPP base was created with.
    pub fn spp_matrix(&self) -> Option<crate::matrix::Mat3> {
        self.spp_snapshot().map(|b| b.matrix(&self.cal))
    }

    /// The installed base if the settings use it and it fits them.
    fn fitting_base(&self, s: &Settings) -> Option<std::sync::Arc<SppBase>> {
        if !Self::uses_spp(s) || s.view != View::Developed {
            return None;
        }
        self.spp_snapshot().filter(|b| b.fits(s))
    }

    fn render_spp(&self, s: &Settings, region: Option<[usize; 4]>) -> Option<Rgb16> {
        if !Self::uses_spp(s) || s.view != View::Developed {
            return None;
        }
        // Crops (interactive) never compute the SPP base themselves; that takes seconds. They only use it if it
        // already exists; the base comes from the background job. Full output (export) computes it.
        let base = if region.is_some() { self.fitting_base(s)? } else { self.ensure_spp(s).ok()? };
        Some(render_from_spp(&self.cal, &base, s, region))
    }

    /// Develop at full resolution, with Sigma's exact base if the settings allow it.
    pub fn render_full(&mut self, s: &Settings) -> Rgb16 {
        let s = &*s.effective();
        let mut img = match self.render_spp(s, None) {
            Some(img) => img,
            None => self.render_full_own_base(s, false),
        };
        let [w, h] = self.size();
        crate::look::apply(&mut img, s, crate::look::Geo { camera: self.cal.orientation, ..crate::look::Geo::full(w, h) });
        img
    }

    /// Full resolution with our own (fast) pipeline.
    pub fn render_full_own(&mut self, s: &Settings) -> Rgb16 {
        let s = &*s.effective();
        let mut img = self.render_full_own_base(s, false);
        let [w, h] = self.size();
        crate::look::apply(&mut img, s, crate::look::Geo { camera: self.cal.orientation, ..crate::look::Geo::full(w, h) });
        img
    }

    /// Like [`Self::render_full_own`], but with true 16-bit gradations (see [`crate::render_fine`]);
    /// for TIFF/PNG export. The film simulation keeps its own output.
    pub fn render_full_own_fine(&mut self, s: &Settings) -> Rgb16 {
        let s = &*s.effective();
        let mut img = self.render_full_own_base(s, true);
        let [w, h] = self.size();
        crate::look::apply(&mut img, s, crate::look::Geo { camera: self.cal.orientation, ..crate::look::Geo::full(w, h) });
        img
    }

    /// Wide-gamut export: the whole image as extended sRGB-encoded floats (colours outside sRGB
    /// kept as values below 0 or above 1) including the look stage, from Sigma's base when `spp`
    /// and the settings use it, else from the own pipeline with interpolated tone curve.
    /// `None` for the film simulation and Classic cameras (their output is defined in sRGB).
    pub fn render_full_wide(&mut self, s: &Settings, spp: bool) -> Result<Option<Linear>, String> {
        let s = &*s.effective();
        if s.view != View::Developed || s.film.is_active() || self.cal.is_classic() {
            return Ok(None);
        }
        let n = crate::profile::neutral(&self.cal, s);
        let mut img = if spp && Self::uses_spp(s) {
            let base = self.ensure_spp(s)?;
            let cal = self.cal.clone();
            let m = base.matrix(&cal);
            let fill = (s.fill_light != 0.0).then(|| base.fill_map());
            let haze = (s.dehaze != 0.0).then(|| base.haze());
            let toneq = base.toneq(&cal, s);
            crate::profile::mix_f32(s, n.as_ref(), |t| crate::render_counts_wide_with_haze(&base.counts, &cal, t, &m, fill, haze.map(|h| &**h), toneq.as_ref()))
        } else {
            crate::profile::mix_f32(s, n.as_ref(), |t| self.render_own_wide(t))
        };
        // whole image: its own size (a binned base is smaller than the image area)
        let (w, h) = (img.width, img.height);
        crate::look::wide::apply(&mut img.data, img.width, img.height, s, crate::look::Geo { camera: self.cal.orientation, ..crate::look::Geo::full(w, h) })?;
        Ok(Some(img))
    }

    /// Wide-gamut display: a crop at full resolution (x, y, w, h in image pixels) like [`Self::render_region`],
    /// as extended sRGB-encoded floats (see [`Self::render_full_wide`]); Sigma's base only if it already exists
    /// (as for crops). `None` where there is no wide output (film, monochrome, Classic, local adjustments: those
    /// need the whole image).
    pub fn render_region_wide(&mut self, s: &Settings, region: [usize; 4]) -> Result<Option<Linear>, String> {
        if crate::local::active(s) {
            return Ok(None);
        }
        let s = &*s.effective();
        if !wide_display_possible(&self.cal, s) {
            return Ok(None);
        }
        if Self::uses_spp(s) {
            if let Some(base) = self.fitting_base(s) {
                return Ok(wide_from_base(&self.cal, &base, s, Some(region)));
            }
        }
        let n = crate::profile::neutral(&self.cal, s);
        let mut img = crate::profile::mix_f32(s, n.as_ref(), |t| self.render_region_own_wide(t, region));
        let [w, h] = self.size();
        let geo = crate::look::Geo { full_w: w, full_h: h, off_x: region[0], off_y: region[1], scale: 1.0, camera: self.cal.orientation };
        crate::look::wide::apply(&mut img.data, img.width, img.height, s, geo)?;
        Ok(Some(img))
    }

    fn render_region_own_wide(&mut self, s: &Settings, region: [usize; 4]) -> Linear {
        if s.fill_light != 0.0 {
            self.fill_map(s);
        } else {
            self.linear(s);
        }
        attach_haze(&mut self.current.as_mut().unwrap().1, s);
        attach_toneq(&mut self.current.as_mut().unwrap().1, s);
        let lin = &self.current.as_ref().unwrap().1;
        let (crop, map) = crop_with_map(lin, self.fill_map.as_deref(), region);
        crate::render_wide(&crop, &self.cal, s, map.as_deref(), false)
    }

    fn render_own_wide(&mut self, s: &Settings) -> Linear {
        if s.fill_light != 0.0 {
            self.fill_map(s);
        } else {
            self.linear(s);
        }
        attach_haze(&mut self.current.as_mut().unwrap().1, s);
        attach_toneq(&mut self.current.as_mut().unwrap().1, s);
        let lin = &self.current.as_ref().unwrap().1;
        crate::render_wide(lin, &self.cal, s, self.fill_map.as_deref(), true)
    }

    /// Scene-linear image for floating-point exports: linear sRGB primaries, middle grey 0.184,
    /// unclipped (before Sigma's tone curve; the look stage and film simulation are display-referred
    /// and not part of it). From Sigma's base when `spp` and the settings use it.
    pub fn render_full_scene(&mut self, s: &Settings, spp: bool) -> Result<Linear, String> {
        let s = &*s.effective();
        if spp && Self::uses_spp(s) && s.view == View::Developed {
            let base = self.ensure_spp(s)?;
            let cal = self.cal.clone();
            let m = base.matrix(&cal);
            let fill = (s.fill_light != 0.0).then(|| base.fill_map());
            let haze = (s.dehaze != 0.0).then(|| base.haze());
            let toneq = base.toneq(&cal, s);
            return Ok(crate::render_counts_scene_with_haze(&base.counts, &cal, s, &m, fill, haze.map(|h| &**h), toneq.as_ref()));
        }
        if s.fill_light != 0.0 {
            self.fill_map(s);
        } else {
            self.linear(s);
        }
        attach_haze(&mut self.current.as_mut().unwrap().1, s);
        attach_toneq(&mut self.current.as_mut().unwrap().1, s);
        let lin = &self.current.as_ref().unwrap().1;
        Ok(crate::render_scene(lin, &self.cal, s, self.fill_map.as_deref()))
    }

    fn render_full_own_base(&mut self, s: &Settings, fine: bool) -> Rgb16 {
        let n = crate::profile::neutral(&self.cal, s);
        crate::profile::mix(s, n.as_ref(), |t| self.render_full_own_core(t, fine))
    }

    fn render_full_own_core(&mut self, s: &Settings, fine: bool) -> Rgb16 {
        if s.view != View::Developed {
            self.base();
            return render(self.base.as_ref().unwrap(), &self.cal, s, None);
        }
        if s.fill_light != 0.0 {
            self.fill_map(s);
        } else {
            self.linear(s);
        }
        attach_haze(&mut self.current.as_mut().unwrap().1, s);
        attach_toneq(&mut self.current.as_mut().unwrap().1, s);
        let lin = &self.current.as_ref().unwrap().1;
        if s.film.is_active() {
            let (cal, map) = (&self.cal, self.fill_map.as_deref());
            let film = film_region(cal, lin.width, lin.height, cal.orientation, s, None, |r| {
                let (crop, map) = crop_with_map(lin, map, r);
                crate::render_scene(&crop, cal, s, map.as_deref())
            });
            if let Some(img) = film {
                return img;
            }
        }
        if fine {
            return crate::render_fine(lin, &self.cal, s, self.fill_map.as_deref());
        }
        render(lin, &self.cal, s, self.fill_map.as_deref())
    }

    /// Grain loupe: upscale before the film computation; 100 % keeps the existing path.
    pub fn render_grain_region(&mut self, s: &Settings, region: [usize;4], factor: usize) -> Result<Rgb16,String> {
        if factor<=1 || !s.film.is_active() || crate::local::active(s) {return Ok(self.render_region(s,region));}
        let s=&*s.effective();let [fw,fh]=self.size();
        let [x,y,w,h]=region;let x=x.min(fw-1);let y=y.min(fh-1);let w=w.max(1).min(fw-x);let h=h.max(1).min(fh-y);
        let m=crate::film::margin(&s.film,fw,fh);let ex=x.saturating_sub(m);let ey=y.saturating_sub(m);let ew=(x+w+m).min(fw)-ex;let eh=(y+h+m).min(fh)-ey;
        let r=[ex,ey,ew,eh];
        if let Some(b) = self.fitting_base(s) {
            return grain_region_from_base(&self.cal, &b, s, region, factor);
        }
        let scene={
            if s.fill_light!=0.0{self.fill_map(s);}else{self.linear(s);}
            attach_haze(&mut self.current.as_mut().unwrap().1, s);
            attach_toneq(&mut self.current.as_mut().unwrap().1, s);
            let lin=&self.current.as_ref().unwrap().1;let (crop,map)=crop_with_map(lin,self.fill_map.as_deref(),r);
            crate::render_scene(&crop,&self.cal,s,map.as_deref())
        };
        let scene=crate::grain_detail::upscale(&scene,factor)?;
        let geo=crate::grain_detail::geometry([fw,fh],[ex,ey],self.cal.orientation,factor);
        let mut img=crate::film::develop(&scene,&crate::analog::film_settings(&self.cal,s),geo)?;
        if s.profile_amount<1.0 {
            let mut n=s.clone();n.film.active=false;n.profile_amount=1.0;
            let base=self.render_region_base(&n,r);
            let linear=Linear{ mono: false, haze: None, toneq: None,width:base.width,height:base.height,data:base.data.iter().map(|&v|v as f32).collect(),detail:None};
            let up=crate::grain_detail::upscale(&linear,factor)?;
            let base=Rgb16{width:up.width,height:up.height,data:up.data.iter().map(|&v|v.round() as u16).collect()};
            crate::profile::blend(&mut img,&base,s.profile_amount.clamp(0.0,1.0));
        }
        crate::look::apply(&mut img,s,geo);
        Ok(crate::film::crop_rgb16(&img,(x-ex)*factor,(y-ey)*factor,w*factor,h*factor))
    }

    /// Develop a crop at full resolution (x, y, width, height in image pixels).
    /// Fill Light uses the map of the whole image so that the crop matches the full view.
    pub fn render_region(&mut self, s: &Settings, region: [usize; 4]) -> Rgb16 {
        if crate::local::active(s) {
            // Local adjustments: only the crop plus the reach of their filters (retouching: whole image)
            if !crate::local::region_possible(s) {
                let full = self.render_full(s);
                return crate::local::crop(&full, region);
            }
            let size = self.size();
            let r = crate::local::expand(region, crate::local::margin(s, size), size);
            let img = self.render_region_plain(s, r);
            return crate::local::crop(&img, [region[0] - r[0], region[1] - r[1], region[2], region[3]]);
        }
        self.render_region_plain(s, region)
    }

    fn render_region_plain(&mut self, s: &Settings, region: [usize; 4]) -> Rgb16 {
        let s = &*s.effective();
        let [w, h] = self.size();
        let geo = crate::look::Geo { full_w: w, full_h: h, off_x: region[0], off_y: region[1], scale: 1.0, camera: self.cal.orientation };
        let mut img = self.render_region_base(s, region);
        crate::look::apply(&mut img, s, geo);
        img
    }

    fn render_region_base(&mut self, s: &Settings, region: [usize; 4]) -> Rgb16 {
        if let Some(img) = self.render_spp(s, Some(region)) {
            return img;
        }
        let n = crate::profile::neutral(&self.cal, s);
        crate::profile::mix(s, n.as_ref(), |t| self.render_region_own(t, region))
    }

    /// Crop with our own pipeline (without profile amount).
    fn render_region_own(&mut self, s: &Settings, region: [usize; 4]) -> Rgb16 {
        let sensor_view = s.view != View::Developed;
        if sensor_view {
            self.base();
        } else if s.fill_light != 0.0 {
            self.fill_map(s);
        } else {
            self.linear(s);
        }
        if !sensor_view {
            attach_haze(&mut self.current.as_mut().unwrap().1, s);
            attach_toneq(&mut self.current.as_mut().unwrap().1, s);
        }
        let lin = if sensor_view { self.base.as_ref().unwrap() } else { &self.current.as_ref().unwrap().1 };
        if !sensor_view && s.film.is_active() {
            let (cal, map) = (&self.cal, self.fill_map.as_deref());
            let film = film_region(cal, lin.width, lin.height, cal.orientation, s, Some(region), |r| {
                let (crop, map) = crop_with_map(lin, map, r);
                crate::render_scene(&crop, cal, s, map.as_deref())
            });
            if let Some(img) = film {
                return img;
            }
        }
        let [x, y, w, h] = region;
        let (x, y) = (x.min(lin.width - 1), y.min(lin.height - 1));
        let (w, h) = (w.min(lin.width - x).max(1), h.min(lin.height - y).max(1));
        let crop = lin.crop(x, y, w, h);
        let map: Option<Vec<f32>> = self.fill_map.as_ref().filter(|_| !sensor_view).map(|m| {
            (y..y + h).flat_map(|yy| m[yy * lin.width + x..yy * lin.width + x + w].iter().copied()).collect()
        });
        render(&crop, &self.cal, s, map.as_deref())
    }
}

/// Crop or whole image (`region = None`) from a shared Sigma base, without any session lock: Fill Light map and
/// dehaze analysis come from the base (computed once), the output stage is [`spp_output`]. Settings must be
/// effective (see [`Settings::effective`]) and use the base.
pub fn render_from_spp(cal: &Calibration, base: &SppBase, s: &Settings, region: Option<[usize; 4]>) -> Rgb16 {
    let m = base.matrix(cal);
    let fill = (s.fill_light != 0.0).then(|| base.fill_map());
    let haze = (s.dehaze != 0.0).then(|| base.haze());
    let toneq = base.toneq(cal, s);
    // Profile amount: the neutral reference (different colour mode) comes from the same
    // base, converted linearly as when switching colour modes (see [`crate::profile`]).
    let n = crate::profile::neutral(cal, s);
    crate::profile::mix(s, n.as_ref(), |t| spp_output(cal, &base.counts, fill, haze, toneq.as_ref(), &m, t, region))
}

/// Size of the image area of a calibration (as [`Session::size`]).
fn size_of(cal: &Calibration) -> [usize; 2] {
    let [x0, y0, x1, y1] = cal.active;
    [x1 - x0 + 1, y1 - y0 + 1]
}

/// Whole image at full resolution from a shared base (like [`Session::render_full`], but without the session and
/// without computing a missing base): `None` when the settings do not use the base or it does not fit them.
pub fn full_from_base(cal: &Calibration, base: &SppBase, s: &Settings) -> Option<Rgb16> {
    let s = &*s.effective();
    if !Session::uses_spp(s) || s.view != View::Developed || !base.fits(s) {
        return None;
    }
    let mut img = render_from_spp(cal, base, s, None);
    let [w, h] = size_of(cal);
    crate::look::apply(&mut img, s, crate::look::Geo { camera: cal.orientation, ..crate::look::Geo::full(w, h) });
    Some(img)
}

/// Crop at full resolution from a shared base (like [`Session::render_region`]); `None` as in [`full_from_base`].
pub fn region_from_base(cal: &Calibration, base: &SppBase, s: &Settings, region: [usize; 4]) -> Option<Rgb16> {
    if crate::local::active(s) {
        if !crate::local::region_possible(s) {
            let full = full_from_base(cal, base, s)?;
            return Some(crate::local::crop(&full, region));
        }
        // only the crop plus the reach of the local adjustments' filters
        let size = size_of(cal);
        let r = crate::local::expand(region, crate::local::margin(s, size), size);
        let img = region_plain_from_base(cal, base, s, r)?;
        return Some(crate::local::crop(&img, [region[0] - r[0], region[1] - r[1], region[2], region[3]]));
    }
    region_plain_from_base(cal, base, s, region)
}

fn region_plain_from_base(cal: &Calibration, base: &SppBase, s: &Settings, region: [usize; 4]) -> Option<Rgb16> {
    let s = &*s.effective();
    if !Session::uses_spp(s) || s.view != View::Developed || !base.fits(s) {
        return None;
    }
    let [w, h] = size_of(cal);
    let geo = crate::look::Geo { full_w: w, full_h: h, off_x: region[0], off_y: region[1], scale: 1.0, camera: cal.orientation };
    let mut img = render_from_spp(cal, base, s, Some(region));
    crate::look::apply(&mut img, s, geo);
    Some(img)
}

/// Is there a wide-gamut output for these (effective) settings? Not for film (its output is defined in sRGB),
/// monochrome, Classic cameras and the sensor/pipeline views.
pub fn wide_display_possible(cal: &Calibration, s: &Settings) -> bool {
    s.view == View::Developed && !s.film.is_active() && !s.monochrome && !cal.is_classic()
}

/// Wide-gamut display from a shared base, without any session lock: the whole image (`region = None`) or a crop
/// as extended sRGB-encoded floats including the look stage, as [`Session::render_full_wide`] computes it from
/// the base. `None` when the settings do not use the base, it does not fit them, there is no wide output (see
/// [`wide_display_possible`]) or a crop has local adjustments (they need the whole image).
pub fn wide_from_base(cal: &Calibration, base: &SppBase, s: &Settings, region: Option<[usize; 4]>) -> Option<Linear> {
    if region.is_some() && crate::local::active(s) {
        return None;
    }
    let s = &*s.effective();
    if !Session::uses_spp(s) || !base.fits(s) || !wide_display_possible(cal, s) {
        return None;
    }
    let m = base.matrix(cal);
    let fill = (s.fill_light != 0.0).then(|| base.fill_map());
    let haze = (s.dehaze != 0.0).then(|| base.haze());
    let toneq = base.toneq(cal, s);
    let n = crate::profile::neutral(cal, s);
    let [w, h] = size_of(cal);
    let mut img = crate::profile::mix_f32(s, n.as_ref(), |t| match region {
        None => crate::render_counts_wide_with_haze(&base.counts, cal, t, &m, fill, haze.map(|h| &**h), toneq.as_ref()),
        Some(r) => {
            let (counts, map, haze, toneq) = crop_with_map_haze(&base.counts, fill, haze, toneq.as_ref(), r);
            crate::render_counts_wide_with_haze(&counts, cal, t, &m, map.as_deref(), haze.as_deref(), toneq.as_ref())
        }
    });
    let (ox, oy) = region.map_or((0, 0), |r| (r[0], r[1]));
    let geo = crate::look::Geo { full_w: w, full_h: h, off_x: ox, off_y: oy, scale: 1.0, camera: cal.orientation };
    crate::look::wide::apply(&mut img.data, img.width, img.height, s, geo).ok()?;
    Some(img)
}

/// Grain loupe from a shared base (like [`Session::render_grain_region`]; 100 % keeps [`region_from_base`]).
/// `Err`: the base does not fit the settings (the session path is needed) or the film computation failed.
pub fn grain_region_from_base(cal: &Calibration, base: &SppBase, s: &Settings, region: [usize; 4], factor: usize) -> Result<Rgb16, String> {
    if factor <= 1 || !s.film.is_active() || crate::local::active(s) {
        return region_from_base(cal, base, s, region).ok_or_else(|| "Basis passt nicht zu den Einstellungen".to_string());
    }
    let s = &*s.effective();
    if !Session::uses_spp(s) || s.view != View::Developed || !base.fits(s) {
        return Err("Basis passt nicht zu den Einstellungen".into());
    }
    let [fw, fh] = size_of(cal);
    let [x, y, w, h] = region;
    let x = x.min(fw - 1);
    let y = y.min(fh - 1);
    let w = w.max(1).min(fw - x);
    let h = h.max(1).min(fh - y);
    let m = crate::film::margin(&s.film, fw, fh);
    let ex = x.saturating_sub(m);
    let ey = y.saturating_sub(m);
    let ew = (x + w + m).min(fw) - ex;
    let eh = (y + h + m).min(fh) - ey;
    let r = [ex, ey, ew, eh];
    let matrix = base.matrix(cal);
    let fill = (s.fill_light != 0.0).then(|| base.fill_map());
    let haze = (s.dehaze != 0.0).then(|| base.haze());
    let toneq = base.toneq(cal, s);
    let (counts, map, haze, toneq) = crop_with_map_haze(&base.counts, fill, haze, toneq.as_ref(), r);
    let scene = crate::render_counts_scene_with_haze(&counts, cal, s, &matrix, map.as_deref(), haze.as_deref(), toneq.as_ref());
    let scene = crate::grain_detail::upscale(&scene, factor)?;
    let geo = crate::grain_detail::geometry([fw, fh], [ex, ey], cal.orientation, factor);
    let mut img = crate::film::develop(&scene, &crate::analog::film_settings(cal, s), geo)?;
    if s.profile_amount < 1.0 {
        let mut n = s.clone();
        n.film.active = false;
        n.profile_amount = 1.0;
        let plain = render_from_spp(cal, base, &n, Some(r));
        let linear = Linear { mono: false, haze: None, toneq: None, width: plain.width, height: plain.height, data: plain.data.iter().map(|&v| v as f32).collect(), detail: None };
        let up = crate::grain_detail::upscale(&linear, factor)?;
        let plain = Rgb16 { width: up.width, height: up.height, data: up.data.iter().map(|&v| v.round() as u16).collect() };
        crate::profile::blend(&mut img, &plain, s.profile_amount.clamp(0.0, 1.0));
    }
    crate::look::apply(&mut img, s, geo);
    Ok(crate::film::crop_rgb16(&img, (x - ex) * factor, (y - ey) * factor, w * factor, h * factor))
}

/// Tone equalizer analysis of the whole image, computed once per mask setting and kept with the image (crops take their
/// part of it). The own pipeline's data are normalised to sensor saturation (white = 1).
fn attach_toneq(lin: &mut Linear, s: &Settings) {
    if crate::tone_eq::needed(s) && !lin.toneq.as_ref().is_some_and(|m| m.grid().fits(&s.tone_eq) && (m.width, m.height) == (lin.width, lin.height)) {
        let grid = crate::tone_eq::Grid::analyze(&lin.data, lin.width, lin.height, 1.0, &s.tone_eq);
        lin.toneq = Some(crate::tone_eq::Mask::full(std::sync::Arc::new(grid)));
    }
}

/// Dehaze analysis of the whole image, computed once and kept with the image (crops take their part of it).
fn attach_haze(lin: &mut Linear, s: &Settings) {
    if s.dehaze != 0.0 && lin.haze.is_none() {
        lin.haze = Some(std::sync::Arc::new(crate::dehaze::HazeField::analyze(&lin.data, lin.width, lin.height, 1.0)));
    }
}

/// Output stage on the SPP base `lin` (full image or crop): film or Sigma's tone curve. `haze`: the dehaze analysis
/// of the whole base (crops take their part of it).
#[allow(clippy::too_many_arguments)]
fn spp_output(cal: &Calibration, lin: &Linear, fill_map: Option<&[f32]>, haze: Option<&std::sync::Arc<crate::dehaze::HazeField>>, toneq: Option<&crate::tone_eq::Mask>, m: &crate::matrix::Mat3, s: &Settings, region: Option<[usize; 4]>) -> Rgb16 {
    if s.film.is_active() {
        // Film simulation instead of Sigma's tone curve; crops with margin (halation, DIR)
        let film = film_region(cal, lin.width, lin.height, cal.orientation, s, region, |r| {
            let (counts, map, haze, toneq) = crop_with_map_haze(lin, fill_map, haze, toneq, r);
            crate::render_counts_scene_with_haze(&counts, cal, s, m, map.as_deref(), haze.as_deref(), toneq.as_ref())
        });
        if let Some(img) = film {
            return img;
        }
    }
    match region {
        None => crate::render_counts_with_haze(lin, cal, s, m, fill_map, haze.map(|h| &**h), toneq),
        Some(r) => {
            let (counts, map, haze, toneq) = crop_with_map_haze(lin, fill_map, haze, toneq, r);
            crate::render_counts_with_haze(&counts, cal, s, m, map.as_deref(), haze.as_deref(), toneq.as_ref())
        }
    }
}

/// [`crop_with_map`] plus the matching part of the dehaze analysis (the whole image keeps it as is, a crop takes
/// its part, as [`Linear::crop`] does with an attached analysis).
#[allow(clippy::type_complexity)]
fn crop_with_map_haze<'a>(
    lin: &'a Linear,
    map: Option<&[f32]>,
    haze: Option<&std::sync::Arc<crate::dehaze::HazeField>>,
    toneq: Option<&crate::tone_eq::Mask>,
    region: [usize; 4],
) -> (std::borrow::Cow<'a, Linear>, Option<Vec<f32>>, Option<std::sync::Arc<crate::dehaze::HazeField>>, Option<crate::tone_eq::Mask>) {
    let [x, y, w, h] = region;
    let (x, y) = (x.min(lin.width - 1), y.min(lin.height - 1));
    let (w, h) = (w.min(lin.width - x).max(1), h.min(lin.height - y).max(1));
    let (crop, map) = crop_with_map(lin, map, region);
    let haze = haze.map(|f| if (x, y, w, h) == (0, 0, lin.width, lin.height) { f.clone() } else { std::sync::Arc::new(f.crop(x, y, w, h)) });
    let toneq = toneq.map(|m| m.crop(x, y, w, h));
    (crop, map, haze, toneq)
}

/// Crop `[x, y, w, h]` (clipped to the image) together with the matching part of the Fill Light map.
fn crop_with_map<'a>(lin: &'a Linear, map: Option<&[f32]>, region: [usize; 4]) -> (std::borrow::Cow<'a, Linear>, Option<Vec<f32>>) {
    let [x, y, w, h] = region;
    let (x, y) = (x.min(lin.width - 1), y.min(lin.height - 1));
    let (w, h) = (w.min(lin.width - x).max(1), h.min(lin.height - y).max(1));
    if (x, y, w, h) == (0, 0, lin.width, lin.height) {
        return (std::borrow::Cow::Borrowed(lin), map.map(<[f32]>::to_vec));
    }
    let map = map.map(|m| (y..y + h).flat_map(|yy| m[yy * lin.width + x..yy * lin.width + x + w].iter().copied()).collect());
    (std::borrow::Cow::Owned(lin.crop(x, y, w, h)), map)
}

/// Film simulation for the full image (`region = None`) or a crop at full
/// resolution: `scene` returns the scene-linear image of a rectangle; crops are computed with
/// a margin ([`crate::film::margin`]) and then cropped so that halation, DIR couplers
/// and grain sit as in the full image. `None` on errors (missing data) → Sigma's tone curve.
fn film_region(
    cal: &Calibration,
    full_w: usize,
    full_h: usize,
    camera: u8,
    s: &Settings,
    region: Option<[usize; 4]>,
    scene: impl FnOnce([usize; 4]) -> Linear,
) -> Option<Rgb16> {
    let [x, y, w, h] = region.unwrap_or([0, 0, full_w, full_h]);
    let (x, y) = (x.min(full_w - 1), y.min(full_h - 1));
    let (w, h) = (w.min(full_w - x).max(1), h.min(full_h - y).max(1));
    let m = if region.is_some() { crate::film::margin(&s.film, full_w, full_h) } else { 0 };
    let (ex, ey) = (x.saturating_sub(m), y.saturating_sub(m));
    let (ew, eh) = ((x + w + m).min(full_w) - ex, (y + h + m).min(full_h) - ey);
    let lin = scene([ex, ey, ew, eh]);
    let geo = crate::look::Geo { full_w, full_h, off_x: ex, off_y: ey, scale: 1.0, camera };
    let img = crate::film::develop(&lin, &crate::analog::film_settings(cal, s), geo).map_err(film_error).ok()?;
    Some(if (ex, ey, ew, eh) == (x, y, w, h) { img } else { crate::film::crop_rgb16(&img, x - ex, y - ey, w, h) })
}

fn film_error(e: String) {
    eprintln!("film simulation: {e}");
}

/// Preview data: downscaled linear image plus its own Fill Light map.
pub struct Preview {
    pub lin: Linear,
    pub fill_map: Vec<f32>,
    pub factor: usize,
    /// Set if `lin` contains SPP counts: base key (colour mode, noise sliders)
    /// and matrix of the computation
    pub spp: Option<(crate::SppKey, crate::matrix::Mat3)>,
    /// Damaged Sigma file: state of each plane per row of `lin` (see [`crate::damaged::Damage::row_states`]),
    /// for the "rescued areas" marking of the sensor views
    pub rescue_rows: Option<std::sync::Arc<Vec<[u8; 3]>>>,
}

/// Detail for the sharpness slider in a downscaled preview: SPP's way gets its own detail image; the Classic
/// output step only a marker with the scale `√(1/factor)` (empty high-pass) – the render builds the preview's
/// high-pass from the linear codes it computes anyway (`classic_new::detail::preview_high_pass`).
fn preview_sharp_detail(lin: &Linear, d: &crate::sharpness::SharpDetail, factor: usize) -> crate::sharpness::SharpDetail {
    if d.is_classic_high_pass() {
        crate::sharpness::SharpDetail { l: Vec::new(), base: crate::sharpness::CLASSIC_HIGH_PASS, scale: (1.0 / factor.max(1) as f64).sqrt() as f32 }
    } else {
        crate::sharpness::preview_detail(lin, d.base, factor)
    }
}

impl Preview {
    /// Develop preview: noise reduction directly at preview size
    /// (radii and noise model scaled by the factor) plus Fill Light map.
    pub fn developed(mut lin: Linear, factor: usize, cal: &Calibration, s: &Settings) -> Self {
        if s.denoise {
            denoise::denoise_scaled(&mut lin, cal, DenoiseSliders::new(s.nr_luma, s.nr_chroma), factor);
        }
        let fill_map = filllight::log_gain_map(&lin, cal);
        Self { lin, fill_map, factor, spp: None, rescue_rows: None }
    }

    /// Sensor preview without processing.
    pub fn plain(lin: Linear, factor: usize) -> Self {
        Self { lin, fill_map: Vec::new(), factor, spp: None, rescue_rows: None }
    }

    /// Preview from the SPP base (counts), downscaled.
    pub fn from_spp(base: &SppBase, matrix: crate::matrix::Mat3, max_edge: usize) -> Self {
        let factor = base.counts.width.max(base.counts.height).div_ceil(max_edge).max(1);
        let mut lin = base.counts.downsample(factor);
        // Sharpness slider in the preview: own detail image of the downscaled counts
        // (approximation, as SPP does for downscaled images; 1:1 is exact)
        if factor > 1 {
            if let Some(d) = base.counts.detail.as_ref() {
                lin.detail = Some(std::sync::Arc::new(preview_sharp_detail(&lin, d, factor)));
            }
        }
        let fill_map = filllight::log_gain_map_counts(&lin);
        Self { lin, fill_map, factor, spp: Some((base.key.clone(), matrix)), rescue_rows: None }
    }

    /// Smaller scale step of this preview (longer edge about `max_edge`, whole divisor of the size).
    /// `None`: the image is already small enough, use the original.
    pub fn scaled(&self, cal: &Calibration, max_edge: usize) -> Option<Preview> {
        let k = self.lin.width.max(self.lin.height).div_ceil(max_edge.max(1));
        if k <= 1 {
            return None;
        }
        let mut lin = self.lin.downsample(k);
        if let Some(d) = self.lin.detail.as_ref() {
            lin.detail = Some(std::sync::Arc::new(preview_sharp_detail(&lin, d, self.factor * k)));
        }
        let fill_map = if self.fill_map.is_empty() {
            Vec::new()
        } else if self.spp.is_some() {
            filllight::log_gain_map_counts(&lin)
        } else {
            filllight::log_gain_map(&lin, cal)
        };
        Some(Self { lin, fill_map, factor: self.factor * k, spp: self.spp.clone(), rescue_rows: None })
    }

    pub fn render(&self, cal: &Calibration, s: &Settings) -> Rgb16 {
        let s = &*s.effective();
        let mut img = self.render_base_effective(cal, s);
        if let (true, Some(rows)) = (s.view_rescue && s.view != View::Developed && !s.view.is_stage(), &self.rescue_rows) {
            crate::damaged::mark_rows(&mut img, rows, s.view);
        }
        let f = self.factor;
        let geo = crate::look::Geo { full_w: self.lin.width * f, full_h: self.lin.height * f, off_x: 0, off_y: 0, scale: f as f32, camera: cal.orientation };
        crate::look::apply(&mut img, s, geo);
        img
    }

    /// Like [`Self::render`], but as extended (unclipped) sRGB-encoded floats for a wide-gamut display:
    /// colours outside sRGB stay as values below 0 or above 1 (output stage as in the wide-gamut export,
    /// Sigma's tone table unchanged inside its range, then the look stage in floating point). `None` where
    /// the output is defined in sRGB or holds no colour beyond it: film simulation, Classic cameras,
    /// monochrome, sensor and pipeline views.
    pub fn render_wide(&self, cal: &Calibration, s: &Settings) -> Result<Option<Linear>, String> {
        let s = &*s.effective();
        if s.view != View::Developed || s.film.is_active() || s.monochrome || cal.is_classic() {
            return Ok(None);
        }
        let n = crate::profile::neutral(cal, s);
        let mut img = crate::profile::mix_f32(s, n.as_ref(), |t| self.render_wide_core(cal, t));
        crate::look::wide::apply(&mut img.data, img.width, img.height, s, self.geo(cal))?;
        Ok(Some(img))
    }

    fn render_wide_core(&self, cal: &Calibration, s: &Settings) -> Linear {
        // noise brake only at full resolution, as in [`Self::render_base_core`]
        let no_brake;
        let s = if self.factor > 1 && s.noise_brake != crate::NoiseBrake::Off {
            no_brake = Settings { noise_brake: crate::NoiseBrake::Off, ..s.clone() };
            &no_brake
        } else {
            s
        };
        let map = (!self.fill_map.is_empty()).then_some(self.fill_map.as_slice());
        match &self.spp {
            Some((_, matrix)) => crate::render_counts_wide_with_haze(&self.lin, cal, s, matrix, map, None, None),
            None => crate::render_wide(&self.lin, cal, s, map, false),
        }
    }

    /// Position of the preview in the full image (for grain, vignette).
    fn geo(&self, cal: &Calibration) -> crate::look::Geo {
        let f = self.factor;
        crate::look::Geo { full_w: self.lin.width * f, full_h: self.lin.height * f, off_x: 0, off_y: 0, scale: f as f32, camera: cal.orientation }
    }

    /// Scene-linear image (input of the film simulation) of this preview.
    fn scene(&self, cal: &Calibration, s: &Settings, map: Option<&[f32]>) -> Linear {
        match &self.spp {
            Some((_, matrix)) => crate::render_counts_scene(&self.lin, cal, s, matrix, map),
            None => crate::render_scene(&self.lin, cal, s, map),
        }
    }

    /// Small scene-linear image (longer edge ≤ `max_edge`) with its position in the full image, for
    /// [`crate::film::thumbnails`] (image grid of all films).
    pub fn film_scene(&self, cal: &Calibration, s: &Settings, max_edge: usize) -> (Linear, crate::look::Geo) {
        let s = &*s.effective();
        let k = self.lin.width.max(self.lin.height).div_ceil(max_edge.max(1)).max(1);
        let small = self.lin.downsample(k);
        let map: Option<Vec<f32>> = (!self.fill_map.is_empty() && k > 1).then(|| {
            (0..small.height)
                .flat_map(|y| (0..small.width).map(move |x| (y, x)))
                .map(|(y, x)| self.fill_map[(y * k) * self.lin.width + x * k])
                .collect()
        });
        let map = if k > 1 { map } else { (!self.fill_map.is_empty()).then(|| self.fill_map.clone()) };
        let scene = match &self.spp {
            Some((_, matrix)) => crate::render_counts_scene(&small, cal, s, matrix, map.as_deref()),
            None => crate::render_scene(&small, cal, s, map.as_deref()),
        };
        let f = self.factor * k;
        let geo = crate::look::Geo { full_w: small.width * f, full_h: small.height * f, off_x: 0, off_y: 0, scale: f as f32, camera: cal.orientation };
        (scene, geo)
    }

    /// Like [`Self::render`], but without the look stage (Sigma tone curve, finished without look).
    pub fn render_base(&self, cal: &Calibration, s: &Settings) -> Rgb16 {
        self.render_base_effective(cal, &s.effective())
    }

    fn render_base_effective(&self, cal: &Calibration, s: &Settings) -> Rgb16 {
        let n = crate::profile::neutral(cal, s);
        crate::profile::mix(s, n.as_ref(), |t| self.render_base_core(cal, t))
    }

    fn render_base_core(&self, cal: &Calibration, s: &Settings) -> Rgb16 {
        // The noise brake acts on fine colour noise that the downscaled preview averages out anyway: only at
        // full resolution (100 % view, export), so Fill Light stays fast in the fitted view.
        let no_brake;
        let s = if self.factor > 1 && s.noise_brake != crate::NoiseBrake::Off {
            no_brake = Settings { noise_brake: crate::NoiseBrake::Off, ..s.clone() };
            &no_brake
        } else {
            s
        };
        let map = (!self.fill_map.is_empty()).then_some(self.fill_map.as_slice());
        if s.film.is_active() && s.view == View::Developed {
            if let Some(img) = crate::film::develop(&self.scene(cal, s, map), &crate::analog::film_settings(cal, s), self.geo(cal)).map_err(film_error).ok() {
                return img;
            }
        }
        match &self.spp {
            // Different colour mode/white balance than computed: render_counts converts linearly.
            Some((_, matrix)) => crate::render_counts(&self.lin, cal, s, matrix, map),
            None => render(&self.lin, cal, s, map),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv(data: &[u16]) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for v in data {
            for b in v.to_le_bytes() {
                h ^= b as u64;
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
        }
        h
    }

    fn test_files() -> Vec<PathBuf> {
        let Some(dir) = std::env::var_os("GRAINEO_TEST_MERRILL_DIR").map(PathBuf::from) else { return Vec::new() };
        ["DP2M5418.X3F", "DP2M5412.X3F"].iter().map(|n| dir.join(n)).filter(|p| p.is_file()).collect()
    }

    /// Reference hashes of crops and whole images with Fill Light, Dehaze and film, from the Sigma base (run
    /// before and after a change of the session, the lines must not change). `cargo test --release -p
    /// develop reference_hashes -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn reference_hashes() {
        for path in test_files() {
            let mut session = Session::open(&path).unwrap();
            for film in [false, true] {
                let mut s = Settings::default();
                s.dehaze = 0.3;
                s.fill_light = 0.5;
                if film {
                    s.film.active = true;
                    s.film.stock = "kodak_trix_400".into();
                }
                let full = session.render_full(&s);
                let region = session.render_region(&s, [400, 300, 800, 600]);
                println!("ref {} film={film}: full {:016x} region {:016x}", path.file_name().unwrap().to_string_lossy(), fnv(&full.data), fnv(&region.data));
            }
        }
    }
}

/// The Sigma base is shared: rendering from it needs no session lock.
#[cfg(test)]
mod session_shared_tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    fn test_file() -> Option<PathBuf> {
        let dir = std::env::var_os("GRAINEO_TEST_MERRILL_DIR").map(PathBuf::from)?;
        Some(dir.join("DP2M5418.X3F")).filter(|p| p.is_file())
    }

    fn settings() -> Settings {
        let mut s = Settings::default();
        s.dehaze = 0.3;
        s.fill_light = 0.5;
        s
    }

    /// `has_spp_for` reads only the key hash: true after install, false for another key and after `release_heavy`.
    #[test]
    fn has_spp_for_without_lock() {
        let Some(path) = test_file() else { return };
        let mut session = Session::open(&path).unwrap();
        let s = Settings::default();
        assert!(!session.has_spp_for(&s));
        let counts = crate::diskcache::working_key(&path, &s.spp_key(&path), false, &std::sync::atomic::AtomicBool::new(false)).unwrap();
        session.install_spp(s.spp_key(&path), counts);
        assert!(session.has_spp_for(&s));
        let mut other = s.clone();
        other.nr_luma = 1.0;
        assert!(!session.has_spp_for(&other));
        session.release_heavy();
        assert!(!session.has_spp_for(&s));
        assert!(session.spp_snapshot().is_none());
    }

    /// Fill Light map and dehaze analysis of a base are computed once (same address on the second call).
    #[test]
    fn fill_map_and_haze_computed_once() {
        let counts = Linear { mono: false, haze: None, toneq: None, width: 8, height: 8, data: vec![0.5; 8 * 8 * 3], detail: None };
        let base = SppBase::new(Settings::default().spp_key_class(false), counts);
        assert_eq!(base.fill_map().as_ptr(), base.fill_map().as_ptr());
        assert!(Arc::ptr_eq(base.haze(), base.haze()));
    }

    /// Crop and whole image are rendered from the shared base on two threads at once, while the test holds the
    /// session lock the whole time (proof that the rendering does not need it); results equal the sequential ones.
    #[test]
    fn region_and_full_concurrently_equal_sequential() {
        let Some(path) = test_file() else { return };
        let session = Arc::new(Mutex::new(Session::open(&path).unwrap()));
        let s = settings();
        let (cal, base) = {
            let g = session.lock().unwrap();
            let base = g.ensure_spp(&s).unwrap();
            (g.cal.clone(), base)
        };
        let region = [400, 300, 800, 600];
        let seq_region = region_from_base(&cal, &base, &s, region).unwrap();
        let seq_full = full_from_base(&cal, &base, &s).unwrap();
        let held = session.lock().unwrap();
        let (c2, b2, s2) = (cal.clone(), base.clone(), s.clone());
        let a = std::thread::spawn(move || region_from_base(&c2, &b2, &s2, region).unwrap());
        let (c3, b3, s3) = (cal.clone(), base.clone(), s.clone());
        let b = std::thread::spawn(move || full_from_base(&c3, &b3, &s3).unwrap());
        let (par_region, par_full) = (a.join().unwrap(), b.join().unwrap());
        drop(held);
        assert_eq!(seq_region.data, par_region.data);
        assert_eq!(seq_full.data, par_full.data);
    }

    /// The wide-gamut preview (display) equals the sRGB preview wherever the colour lies inside sRGB, from the
    /// Sigma base and from the own pipeline; film and monochrome have no wide version.
    #[test]
    fn wide_preview_equals_srgb_inside_gamut() {
        let Some(path) = test_file() else { return };
        let session = Session::open(&path).unwrap();
        let s = settings();
        let base = session.ensure_spp(&s).unwrap();
        let cal = session.cal.clone();
        let spp = Preview::from_spp(&base, base.matrix(&cal), 1200);
        let (lin, factor) = session.preview_linear(1200);
        let own = Preview::developed(lin, factor, &cal, &s);
        for (name, p) in [("spp", &spp), ("own", &own)] {
            let sdr = p.render(&cal, &s);
            let wide = p.render_wide(&cal, &s).unwrap().expect("wide version");
            assert_eq!((wide.width, wide.height), (sdr.width, sdr.height));
            let (mut inside, mut off1, mut max) = (0usize, 0usize, 0i32);
            let mut outside = 0usize;
            for (a, b) in sdr.data.chunks_exact(3).zip(wide.data.chunks_exact(3)) {
                if !b.iter().all(|v| (0.002..=0.998).contains(v)) {
                    outside += b.iter().any(|v| *v < -0.002 || *v > 1.002) as usize;
                    continue;
                }
                inside += 1;
                let d = (0..3).map(|c| ((a[c] / 257) as i32 - (b[c] * 255.0).round() as i32).abs()).max().unwrap();
                off1 += (d > 1) as usize;
                max = max.max(d);
            }
            println!("{name}: {inside} inside, {:.3} % differ by more than 1/255, max {max}; outside sRGB {:.2} %", off1 as f64 * 100.0 / inside as f64, outside as f64 * 100.0 / (sdr.width * sdr.height) as f64);
            assert!(inside > 0 && off1 == 0 && max <= 1, "{name}: {off1} of {inside} pixels differ, max {max}");
        }
        let mut film = s.clone();
        film.film.active = true;
        film.film.stock = "kodak_trix_400".into();
        assert!(spp.render_wide(&cal, &film).unwrap().is_none());
        let mut mono = s.clone();
        mono.monochrome = true;
        assert!(spp.render_wide(&cal, &mono).unwrap().is_none());
    }
}

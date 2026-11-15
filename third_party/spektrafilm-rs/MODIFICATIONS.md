# Changes relative to spektrafilm-rs (GPL-3.0)

Source: https://github.com/turbasvin/spektrafilm-rs (Rust port of Andrea Volpato's
[spektrafilm](https://github.com/andreavolpato/spektrafilm)), commit see `UPSTREAM_COMMIT`
(`9dd59b0380194b93686aaa230a8bb9680aa270a4`, 2026-06-15). Licence unchanged GPL-3.0 (`LICENSE`),
origin and acknowledgements in the original `README.md`. According to their metadata the film profiles in
`data/profiles` are licensed under CC BY-SA 4.0 (© 2026 Andrea Volpato, attribution and link to
https://github.com/andreavolpato/spektrafilm required); curves, spectra and measurement data are unchanged. The only
addition is the field `info.card` (info card for the darkroom: German text by graineo, `origin: "spektrafilm"`); it is
not part of the original and, as part of the profile file, is likewise licensed under CC BY-SA 4.0. Embedded into
graineo (GPL-3.0-only) on 2026-10-27; used by `crates/develop/src/film.rs`.

## Taken over or left out

- Taken over: `crates/spektrafilm-{math,model,core,gpu}`, `crates/spektrafilm-shaders/wgsl`,
  `data/{profiles,luts,filters}` (≈ 24 MB), `LICENSE`, `README.md`.
- Left out: `spektrafilm-cli`, `spektrafilm-gui`, CUDA backend (`cuda_backend.rs`, shaders
  `cuda/`, feature `cuda-backend`, dependency `cudarc`), Metal shaders (`metal/`, unused),
  `examples/`, `scripts/`, `docs/`, `assets/`, test data, original `Cargo.lock`.
- `Cargo.toml` (workspace): only the four library crates; GUI/CLI dependencies
  (eframe, egui, clap, exr, tracing-subscriber, anyhow) removed.

## Code changes

1. **wgpu 24 → 30** (same as egui in the main project, no third wgpu version in the build):
   `spektrafilm-gpu/src/wgpu_backend.rs`: `InstanceDescriptor::new_without_display_handle()` +
   `Instance::new(desc)`; `RequestAdapterOptions::apply_limit_buckets`, `request_adapter`
   returns `Result`; `DeviceDescriptor` with `experimental_features`/`trace`, `request_device`
   without trace argument; `PipelineLayoutDescriptor::bind_group_layouts` as `&[Option<&_>]`,
   `immediate_size: 0` instead of `push_constant_ranges`; `device.poll(PollType::wait_indefinitely())`;
   `get_mapped_range{,_mut}()` returns `Result` (`expect`). Shaders unchanged except for item 3.
2. **CUDA removed**: `spektrafilm-gpu/src/lib.rs`: module `cuda_backend` and its selection in
   `select_backend` deleted (requesting `SPEKTRAFILM_BACKEND=cuda` only logs a warning).
3. **Grain fixed in place** (crop and full image show the same grain; GPU and CPU identical):
   - `spektrafilm-core/src/params.rs`: `GrainParams::origin: Option<[u32; 3]>` (x, y, full width;
     `#[serde(default)]`, `None` = original behaviour).
   - `spektrafilm-gpu/src/lib.rs`: `GrainGpuParams::origin`.
   - `spektrafilm-shaders/wgsl/grain.wgsl`: uniform extended with `origin_x/origin_y/full_width/width`,
     random stream from the position in the full image instead of the buffer index.
   - `spektrafilm-gpu/src/wgpu_backend.rs` (`build_grain_state`): fields filled in.
   - `spektrafilm-core/src/pipeline.rs`: `origin` from the parameters (otherwise `[0, 0, width]`).
   - `spektrafilm-model/src/grain.rs`: new function `apply_grain_hashed` (CPU port of the shader:
     PCG/SplitMix hash, normal approximation of Poisson and binomial).
   - `spektrafilm-core/src/stages/filming.rs`: uses `apply_grain_hashed` when `origin` is set
     (otherwise numpy MT19937 as before, for Python parity).
4. **Light sources "T" and "K75P"** (as in the original's `standard_illuminant`): `spektrafilm-math/src/spectral.rs`:
   `ILLUMINANT_INCANDESCENT_F64` ("T", colour-science `SDS_LIGHT_SOURCES['Incandescent']`, reference
   illuminant of Vision3 200T/500T) and `ILLUMINANT_K75P_F64` ("K75P", `['Kinoton 75P']`, viewing illuminant
   of the print films 2383/2393), 380–780 nm / 5 nm, mean 1; `illuminant_by_name{,_f64}` used by all
   `select_illuminant{,_f64}` (filming, pipeline incl. GPU viewing illuminant, scanning, colour reference).
   The port knew neither: "T" fell back to D55 (later graineo used Planck 3200 K, still a blue cast),
   "K75P" to D50.
5. **Warnings**: `#![allow(unused)]` at the top of the four `lib.rs` (unused helper functions of the
   original would otherwise fill every build of the main project with warnings).
6. **Validate shaders up front**: `spektrafilm-gpu/src/wgpu_backend.rs`: `WgpuBackend::new` compiles
   all shaders (`ALL_SHADERS`) inside a validation error scope. If one fails on the device
   (e.g. Windows/D3D12), `new` returns `None` and the CPU computes; previously the first
   compute job aborted with an uncaught wgpu error. A software renderer
   (`DeviceType::Cpu`, e.g. WARP in a Windows virtual machine) also counts as
   "no GPU": buffer mappings failed there, and the CPU computation is faster.

7. **TC LUT kept between calibrations**: `spektrafilm-core/src/pipeline.rs` (`TC_LUT_CACHE`, `tc_lut_key`):
   `Pipeline::new_with_spectral` keeps the computed TC LUT (spectra LUT × film sensitivity, window, input gamut
   compression) keyed by everything it is computed from. A new calibration with only other exposure or enlarger
   filtration (film sliders) no longer reads the spectra LUT and recomputes it. Pure cache: the LUT is
   bit-identical to a fresh computation.

8. **Sub-layer grain (port of upstream's experimental path) and grain size classes** (2026-11-13; since
   2026-11-14 graineo always uses one of the two – choice "even" = (A), "film" = (B) – and no longer the former
   single-size path for colour films; with neither switch set the port itself still computes as before):
   - Finding: upstream `model/grain.py::apply_grain` uses `apply_grain_to_density_layers` whenever
     `grain.sublayers_active` (upstream default `True`): every colour record is split into its sub-layers
     (`density_curves_layers` of the profile, fast/medium/slow), each with its own density share, crystal area
     (`particle_scale_layers`, default 2.0/1.0/0.5) and dye-cloud blur. The port read the parameters but only ran
     the single-size path (`n_sub_layers` copies with other seeds); so for colour films it differed from upstream
     Python, for B&W (profiles without sub-layer curves) not.
   - (A) Port of that path: `spektrafilm-model/src/grain.rs::apply_grain_to_density_layers` (exact, numpy
     MT19937 streams `seed = ch + 10·layer`, `interp_density_cmy_layers`, `add_micro_structure` with a position
     hash instead of numpy's generator – inactive above 0.6 µm pixels). Checked against upstream Python
     (`tests/grain_layers_parity.rs`, Portra 400 / Velvia 100, 64×48 px, same streams: mean |Δ| 2–3·10⁻⁵, max
     1.5–2.8·10⁻³ density). Grain fixed in place / GPU: `spektrafilm_gpu::GrainClasses::upstream_layers` (normal
     approximation per sub-layer, dye-cloud blur combined with the common blur; σ within 4–6 % of the exact
     path, test `hashed_sublayer_grain_has_upstream_statistics`). Switch: `GrainParams::use_sublayers` (new,
     default `false`) together with `sublayers_active`.
   - (B) graineo addition, `GrainParams::size_classes` (`GrainSizeClasses`, profile field
     `info.grain_size_classes`): crystal size classes also for B&W (single-hit model: speed ∝ area^κ, large
     crystals develop first; reversal films: they leave dye last), for colour films the sub-layer curves; per
     class its own blur (partly ∝ √area, `GRAIN_BLUR_FIXED_SHARE`), calibrated so that at D/Dmax = 0.5 strength
     and size equal the single-size model (keeps `grain_scale`). Profiles: `grain_size_classes` added to
     `fujifilm_acros_100_ii` (area spread 2), `kodak_trix_400` and `ilford_hp5_plus` (6), and estimated from the
     crystal type for `ilford_delta_100/400/3200`, `kodak_tmax_100/400/p3200` (tabular, 2), `ilford_fp4_plus`,
     `agfa_apx_100/400`, `kodak_doublex`, `ilford_ortho_plus`, `agfa_scala_200x` (5), `kodak_trix` (6); not part of
     upstream.
   - Shared: `spektrafilm-gpu/src/lib.rs` (`GrainClasses`, `GrainGpuParams::classes`, no longer `Copy`),
     shader `spektrafilm-shaders/wgsl/grain_classes.wgsl` (copy / class noise / add, table p → p_i),
     `wgpu_backend.rs` (`build_grain_classes_jobs`, `make_blur_job`), `spektrafilm-core/src/params.rs`,
     `profile.rs`, `stages/filming.rs` (`grain_classes`, `film_layer_count`), `pipeline.rs`.

9. **Size limit of the GPU backend** (2026-11-14): `spektrafilm-gpu/src/lib.rs` (`ComputeBackend::max_pixels`, default
   unlimited) and `wgpu_backend.rs` (`WgpuBackend::max_pixels`): the per-pixel shaders run one-dimensional dispatch
   grids with `@workgroup_size(1024)` and bind the whole image (3 × f32 per pixel) as one storage buffer, so the
   device limits cap the image size (65 535 × 1024 ≈ 67 MP on typical GPUs). Larger images (e.g. a 100-MP export)
   ended in an uncaught wgpu validation error ("dispatch group size … must be less or equal to 65535"); graineo now
   computes them on the CPU (`crates/develop/src/film.rs`). Test `graineo_limit_tests`.

Not changed: spectral computation, curves and measurement data of the profiles, calibration, parameter defaults.

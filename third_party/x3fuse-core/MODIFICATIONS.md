# Changes relative to x3fuse-core (Apache-2.0, section 4b)

The following was added for graineo (everything else unchanged; base version see UPSTREAM_COMMIT):

- `crates/x3f-sys/src/process/spp.rs`: new function `working`: SPP working image before the tone curve
  (same flow as `render` up to noise reduction).
- `crates/x3f-sys/src/process.rs`, `crates/x3f-sys/src/lib.rs`: public function `spp_working_image`.
- `crates/x3f-core/src/lib.rs`: method `Reader::spp_working_image`.
- `crates/x3f-sys/src/cr_offset.rs`: `run_patches_parallel`: patch adjustments in parallel, accumulation
  in the original order (bit-identical; CROC ≈ 1 s → 0.15 s).
- `crates/x3f-sys/src/float_front.rs`: despeckle detection in parallel per row, merging in
  raster order (bit-identical; ≈ 0.3 s → 0.05 s).
- `crates/x3f-sys/src/process/spp.rs`: optional timing via `X3F_SPP_TIMING`.

Bit-identity checked with `graineo-dev spphash` on the reference images.
- `crates/x3f-sys/src/float_front.rs`: highlight reconstruction: density map, donor statistics and
  PixelRepair in parallel (bit-identical; ≈ 0.36 s → 0.16 s).

### SPP noise reduction: profiling, CPU speed-up, GPU version (October 2026)

CPU path (default): result bit-identical, checked with `graineo-dev spphash` against the
reference values and `cargo test -p x3f-sys --lib` (245 tests):

- `crates/x3f-sys/src/spp_denoise/prof.rs` (new): per-stage timing of the tile stages when
  `X3F_SPP_TIMING` is set; measuring points in `pipeline.rs::process_tile`, wall-clock times and
  tile utilisation in `process/spp.rs::denoise_image`.
- `crates/x3f-sys/src/spp_denoise/residual.rs`: `sharpen` in parallel per row and directly
  in the image (≈ 105 → 10 ms).
- `crates/x3f-sys/src/spp_denoise/tiling.rs`: `TileLayout::source_of` (source pixel of every
  tile position as a pure function of the copy chain of `initialize`),
  `initialize_parallel` (≈ 64 → 30 ms) and `blend_pixel`/`blend_parallel` (blending per
  pixel; slower on the CPU than the serial `blend` and not used there, serves as the
  template for the GPU kernel). Tests `source_of_matches_initialize` and
  `blend_parallel_matches_blend` check bit-identity.
- `crates/x3f-sys/src/spp_denoise/pipeline.rs`: with `rcps_blending == 0`,
  `gen_residual_min_max_map` and `row_column_pattern_suppression` are skipped (the
  result is then exactly the unchanged residual; ≈ 12 % of the tile computation time).
  New read-only accessor `Pipeline::gpu_view` for the GPU version.
- `crates/x3f-sys/src/process/spp.rs`: diagnostic environment variables `X3F_SPP_PARAMS`
  (print noise parameters) and `X3F_SPP_TEST` (tests only: `rcps=…`, `rcpsthr=…`,
  `colormix=…`, `scale=…`, `nudge=…` override noise parameters for CPU and GPU alike).

GPU path (new Cargo feature `spp-gpu` in x3f-sys and x3f-core, uses the existing
wgpu 27 dependencies of the `gpu` feature):

- `crates/x3f-sys/src/spp_denoise/gpu.rs` and `gpu.wgsl` (new): `Pipeline::process_tile`,
  tile setup, blending and sharpening as a wgpu/WGSL compute pipeline (34 kernels).
  Not bit-identical (f32 instead of f64), PSNR against the CPU 112–123 dB on the test images.
  The prefilter path (`scale ≤ 0.54`, high ISO) is not implemented → falls back to the CPU.
- `crates/x3f-sys/src/process/spp.rs`: `denoise_dispatch` (GPU if requested and
  available, otherwise, or on any GPU error, CPU with the unchanged input image),
  `working(…, gpu)`; `render` uses the GPU with `X3F_SPP_GPU=1`.
- `crates/x3f-sys/src/process.rs`, `lib.rs`: `spp_working_image` selects the GPU with
  `X3F_SPP_GPU=1`; new `spp_working_image_with(…, gpu)`, `spp_last_used_gpu`,
  `spp_gpu_adapter`, `spp_gpu_release`.
- `crates/x3f-core/src/lib.rs`: `Reader::spp_working_image_gpu`, re-export of
  `spp_gpu_adapter`, `spp_gpu_release`, `spp_last_used_gpu`; `Cargo.toml`: feature `spp-gpu`.
- Changed files formatted with rustfmt.


## Table-driven TRUE decoder (2026-10-26)

- `crates/x3f-sys/src/entropy.rs`: `FastTrue` (lookup table built from the Huffman tree) and `FastBits` (bit reader with 64-bit lookahead) replace the bit-by-bit tree descent in `true_decode_one_color`, provided all codes are ≤ 16 bits long (otherwise the previous path).
- Result bit-identical (SPP base hashes unchanged: on the reference images; RAW decoding of a DP2M file 320 → 200 ms.

## Faster highlight reconstruction (2026-10-26)

- `crates/x3f-sys/src/float_front.rs`, `HighlightRestoration::restore`:
  - The dense map is now built from a centre map with a separable 5×5 dilation (horizontal, then vertical) instead of 25 re-evaluations per pixel.
  - The serial cleanup of the saturation marks skips empty areas in 8-byte steps; the raster order is preserved.
- Result bit-identical: reference images unchanged; SD1M sample unchanged.
- Run time of the step 157 → 65 ms.

## Handling of FPNSuppression (2026-10-26)

- `crates/x3f-sys/src/process/spp.rs`: the SPP pipeline rejected every file with `FPNSuppression ≠ 0`. Now only the values 3 and 7 are rejected; the other values are processed, so early DP1M/DP2M files (X3F 2.3, `FPNSuppression = 2`) open.

## XMP packet in the DNG (2026-10-26)

- `ProcessOptions::xmp_packet` (new, default `None`): if an XMP packet is passed, the DNG writer writes it as XMLPacket (tag 700) into IFD0 (`output/dng/mod.rs`). Lightroom/ACR thus picks up the development settings from graineo. Without the option the output is unchanged.

## SPP noise sliders luminance and colour (2026-10-26)

The middle position stays bit-identical; the reference images are unchanged.

- `crates/x3f-sys/src/spp_denoise/params.rs`:
  - New `NoiseSliders` (public as `x3f_sys::SppNoiseSliders`) and `NoiseParams::setup(…, NoiseSliders)`.
  - The luminance step selects the row for c0/c9/c11, plus the RCPS offset. The colour step selects the row for c2/c3/c7/c10/c12 as well as alpha/Locality/EdgeMapStrength/LocalMeanDecay.
  - Special rule for column 11 and a built-in step table in case the CAMF is missing.
  - `setup_neutral` calls `setup` with 0.5/0.5.
  - New test `slider_steps_follow_spp_rows`.
- `crates/x3f-sys/src/spp_denoise/pipeline.rs`:
  - `alpha_mix` (blend after the bilateral filter for `alpha ≠ 1`) in the tile path.
  - `valid_params` no longer requires `alpha == 1`, only a finite value. The test was adapted.
- `crates/x3f-sys/src/process/spp.rs`:
  - `noise_params(…, sliders)`, `working(…, sliders)`.
  - `alpha_mix` in the whole-image prefilter.
  - `X3F_SPP_TEST` additionally knows `alpha=` and `decay=`.
  - The test in `process/spp/prefilter_tests.rs` was adapted.
- `crates/x3f-sys/src/process.rs`, `lib.rs`: new `spp_working_image_sliders(…, gpu, sliders)`. `spp_working_image_with` calls it with the middle position.
- `crates/x3f-sys/src/spp_denoise/gpu.rs` and `gpu.wgsl`:
  - `Params.alpha` (instead of `pad1`) and `alpha_rest`; blending in the `bilateral` kernel.
  - `supports()` rejects `alpha ≠ 1` and `rcps_blending ≠ 0` for now; the CPU computes then.
- `crates/x3f-core/src/lib.rs`: `Reader::spp_working_image_sliders`, re-export `SppNoiseSliders`.

## Detail image for the sharpness slider (2026-10-26)

- `crates/x3f-sys/src/process/spp.rs`: `denoise_image` returns the detail image "L" (i16,
  whole image) instead of discarding it after sharpening; `denoise_dispatch` passes it
  through, `render` discards it. `working` returns the new struct `SppWorking` (counts,
  width, height, L cropped to the active area, `sharpen_base` = NoiseParams
  `sharpen_amount`). `X3F_SPP_TEST` additionally knows `sharpen=…` (sharpening amount, tests only).
- `crates/x3f-sys/src/spp_denoise/gpu.rs`: `denoise_image` also reads back the
  sharpening image buffer after the image (via the readback buffer, ≈ 3 ms) and returns it as i16.
- `crates/x3f-sys/src/process.rs`, `lib.rs`: new function `spp_working_image_detail`,
  re-export `SppWorking`; `spp_working_image_sliders` unchanged in its interface.
- `crates/x3f-core/src/lib.rs`: `Reader::spp_working_image_detail`, re-export `SppWorking`.
- Counts unchanged, bit-identical on the reference images.

### Correction of chromatic aberration as in SPP (October 2026)

- `crates/x3f-sys/src/spp_denoise/ca.rs` (new): `CaCorrection` (public as `SppCaCorrection`),
  `CaTransform::prepare` (preparation: range, safety brake) and `CaTransform::apply`
  (radial shift of red and blue), with tests.
- `crates/x3f-sys/src/spp_denoise/pipeline.rs`: `Pipeline::set_ca`/`has_ca`; `process_tile` has the
  new argument `origin` (tile position in the image). With a correction set, the shift replaces the
  copy after `apply_matrix_color_dq` (matching SPP's output); sharpening image and residual use the
  shifted image. Without a correction unchanged (bit-identical, `graineo-dev spphash`).
- `crates/x3f-sys/src/process/spp.rs`: `working(…, ca)`; tile origin passed to `process_tile`; with
  a correction, noise reduction runs on the CPU (the GPU path does not support it).
- `crates/x3f-sys/src/process.rs`, `lib.rs`: `spp_working_image_ca`; `spp_working_image_detail`
  calls it with `None`. `crates/x3f-core/src/lib.rs`: `Reader::spp_working_image_ca`, re-export
  `SppCaCorrection`.

### Noise slider step "Off" below SPP's minimum (October 2026)

- `crates/x3f-sys/src/spp_denoise/params.rs`: `NoiseSliders::OFF` (−0.25); every slider value < 0
  computes like step 0, additionally luminance "Off" → `mixing = 0` (output residual only from the
  unsmoothed image), colour "Off" → new field `NoiseParams::chroma_off`. Values ≥ 0 unchanged.
- `crates/x3f-sys/src/spp_denoise/pipeline.rs`: `Pipeline::restore_chroma`: with `chroma_off`
  the colour component from the unsmoothed tile (matrix, ColorDQ, CA like the
  base image) is inserted after the residual; the channel sum stays. Otherwise no additional step (bit-identical).
- `crates/x3f-sys/src/spp_denoise/gpu.rs`: `supports()` rejects `chroma_off` (CPU).
- `crates/x3f-sys/src/process/spp/image_tests.rs`: initialiser extended with `chroma_off`.

### Chroma BM3D on the SPP working image (prototype, October 2026)

- `crates/x3f-sys/src/denoise.rs`: `chroma_bm3d_rgb` and `ChromaBm3dInfo` (BM3D only on U/V of a
  linear RGB image, noise profile over brightness, GPU with CPU fallback); export in
  `x3f-sys/src/lib.rs`, re-export including `Control` in `x3f-core/src/lib.rs`.
- `crates/x3f-sys/src/denoise/bm3d.rs`: `stage_with` (block search on a separate plane) and
  `denoise_plane_matched`; `stage` calls `stage_with` with unchanged values (bit-identical).
- Only used via graineo-dev, not in the default path.


### Quattro: black reference as in SPP, separate luminance noise strength (October 2026)

- `crates/x3f-sys/src/process.rs`: new function `quattro_spp_black`. For dp Quattro and
  sd Quattro (not sd Quattro H), `preprocess_data_impl` and
  `color_mode_calibration` subtract the CAMF value `RawOffset` as the
  black level of all three layers instead of the DarkShield mean; the DarkShield areas are no
  longer measured for this. Background: at high ISO the
  shielded areas read up to ≈ 6 (dp, top layer) or ≈ 13–22 (sd Quattro) counts above
  `RawOffset`; subtracting the shield mean shifted the layers against each other (yellow-green
  or violet cast, crushed shadows). `X3F_QUATTRO_BLACK=shield` restores the old
  behaviour. Merrill, Classic and sd Quattro H unchanged.
  Exception sd Quattro above ISO 3200 (`quattro_black_levels`): there the image black rises
  (ISO 6400 ≈ +16/+8/+4 above `RawOffset`); black = `RawOffset + a·(DarkShield − RawOffset)`
  with `a = 0.85 · clamp(log2(CaptureISO/SensorISO) − 3, 0, 1)` (up to ISO 3200 a = 0,
  ISO 6400 a = 0.85). `X3F_QUATTRO_SDQ_MIX` replaces 0.85 for measurements.
- `crates/x3f-sys/src/denoise.rs`: `DenoiseOptions::luma_strength` (default 1.0 = previous
  behaviour): additional factor only on the luminance σ of the NLM/BM3D channels
  (`channel_strengths`), so that the Quattro preprocessing can denoise luminance less strongly than
  colour. `validate` checks 0.0–3.0.

Merrill bit-identity checked with `graineo-dev spphash` on the reference images.

### Merrill: multiscale chroma filter removed (November 2026)

- The experimental multiscale chroma filter before the SPP noise reduction (graineo setting „Rauschen: Erweitert“,
  `spp_denoise/multiscale.rs`, derived from an external noise method) has been removed again: the module, its call in
  `process/spp.rs` (`working`), the environment variables `X3F_SPP_MULTISCALE`, `X3F_SPP_MS_KEEP`, `X3F_SPP_MS_AXIS`
  and the field `NoiseSliders::multiscale`. Goal: SPP quality only.
- The remaining path is the one that was unchanged before: on the reference images.

### Merrill: very high estimated ISO no longer rejected (November 2026)

- `crates/x3f-sys/src/spp_denoise/params.rs` (`setup`): when the estimated ISO lies beyond the last row of
  `AdditionalISONoiseSettings`, the native reads past the table (undefined). Previously the file was rejected
  ("unsupported SPP noise calibration") and graineo fell back to its simpler own noise reduction. Now the last row
  is used. Seen on a DP3 Merrill at ISO 6400. Files within the table are bit-identical (spphash unchanged).

### Merrill: X3F version 3.1 (saved again by SPP) accepted (November 2026)

- `crates/x3f-sys/src/process.rs` (`merrill_count_scale`, Merrill process selection): version `0x0003_0001` is treated
  like `0x0003_0000`. SPP writes 3.1 when it saves adjustments and an extra preview into a Merrill file; the sensor
  data are unchanged. Checked: a 3.1 original gives the same `spphash` as a copy with only the version set to 3.0
  . Tests updated; reference files bit-identical.

- 2026-11-07 (graineo): `spp_denoise/gpu.rs`: no rayon parallel loops while the global GPU lock is held (`parallel_copy` and the readback conversion run on the calling thread). A parallel loop inside the lock let the waiting thread steal another job that wanted the same lock, which deadlocked when several developments ran at once. Results are bit-identical.

### Merrill: per-step alternative computations (November 2026)

New `SppVariants` (`crates/x3f-sys/src/spp_variants.rs`, re-exported in x3f-core): one switch per step of the
Merrill working image, chosen once per development. `SppVariants::NONE` (default) leaves every path unchanged;
on the reference images.

- `process.rs`: new `spp_working_image_variants(…, ca, variants)`; `spp_working_image_ca` calls it with `NONE`.
  New `merrill_prepare_float_variants`; `merrill_prepare_float` calls it with `NONE`.
- `process/spp.rs` (`working`): new argument `variants`; `pattern_off` sets `rcps_blending = 0` (the existing exact
  "off" path); the variants are handed to the pipeline (`Pipeline::set_variants`).
- `float_front.rs`: `ListedRepair::per_channel` and `Despeckle::per_channel` (`defects_per_channel`): the pixel is
  replaced as a whole, every channel only from the same channel of its good neighbours (average of the opposite
  pair with the smallest difference, otherwise the trimmed mean).
- `float_front/propagate.rs` (new, `highlights_own`): `HighlightRestoration::restore_own`, highlight
  reconstruction by colour propagation (idea after RawTherapee's "Color propagation", GPL-3.0; own code): clipped
  middle/top samples from the bottom layer times the layer ratio averaged over growing windows of the unclipped
  surroundings; then desaturation towards the bottom layer by brightness and a feathered clip mask.
- `spp_denoise/pipeline.rs`: `columns_off` skips the column offset matching; `residual_power` selects
  `*_residual_mixing_with` / `generate_sharp_l_with` in `residual.rs` with the log-domain scaling
  `spp_variants::power_tone` (`r^λ`, for negative amounts `1 + λ·ln r`) instead of the previous mapping.
  The previous public functions keep their signatures and call the new ones with `false`.
- `spp_denoise/gpu.rs`, `gpu.wgsl`: the same switches on the GPU (`Params.power` instead of `pad0`; column
  kernels skipped with `columns_off`), so the alternatives keep the GPU path where the default has it.

- 2026-11-09 (graineo): `spp_denoise/gpu.rs`, `gpu.wgsl`: the whole-image prefilter used at internal scale `<= 0.54` (high ISO) now also runs on the GPU: reduced image from the source (`resample_down`), new kernel `bilateral_image`, `edge_*` and `iir_*` with a parameter block for the reduced size (new fields `edge_border`, `lm_border`, borders 0 and 4 instead of 8), then new kernel `copy_prefiltered` extracts each tile like `copy_image_to_tile`, and `resample_up` continues as in the ordinary path. `supports` no longer excludes this scale. The CPU path is unchanged and remains the fallback; `alpha != 1`, RCPS and "colour off" still fall back to the CPU. Work buffers of the prefilter are separate and only exist while needed.
- 2026-11-09 (graineo): `spp_denoise/gpu.rs`: the CPU lock for RCPS is removed: RCPS runs on the GPU (not bit-exact: a mask bit flipped by f32 rounding shifts the scrambling up to the end of the tile; in the finished image only different noise, ΔE00 mean ≤ 0.012 over ISO 800–6400).

- 2026-11-09 (graineo): `process/spp.rs`: new `prepared` / `SppPrepared` (exported via `process.rs`
  `spp_prepared_image`, x3f-core `Reader::spp_prepared_image`): the steps of `working` up to and including the
  preprocessing (`merrill_prepare_float_variants`), cropped to the active area, without noise reduction, colour
  matrix or ColorDQ, plus the ISO factor and the estimated ISO of the noise parameters. Input of graineo's
  black-and-white path. Existing paths unchanged.

## Merrill Monochrome and FPN Suppression 1 from upstream PR #34 (2026-11-09)

Taken over from the open upstream pull request sagwaco/x3fuse-core#34 (branch `spp-monochrome-fpn`,
author Simone / GitHub `simopa`, commit `3f8331373d98c30a3f0b0f7653704e0abaaa1eed`, Apache-2.0),
adapted to the graineo changes above:

- New files as in the PR: `crates/x3f-sys/src/merrill_mono.rs`, `crates/x3f-sys/src/spp_denoise/mono.rs`,
  `spp_denoise/mono/{bcs,hist_bilateral,hn,sharpen_bnw}.rs`, test fixtures
  `tests/fixtures/{merrill-mono-output,spp-mono-*}-native.txt`; changes in `merrill.rs`, `process.rs`
  (`merrill_gain_parts`, `make_merrill_mono_render_luts`), `process/spp.rs` (`render_monochrome`,
  `header_monochrome`, `mono_noise_scaling`, `denoise_mode_supported`), `spp_denoise/residual.rs`
  (Despeckle3 threshold `(64 / (4096 / s))^3`), docs. The native oracles under `scripts/spp-native/`
  were not taken over.
- Adapted by hand (rejected hunks): `spp_denoise.rs` (module list), `process/spp.rs` `render`
  (monochrome dispatch; FPN gate via `denoise_mode_supported` in `render` and `working`, same
  behaviour as the earlier graineo change: only FPNSuppression 3 and 7 are rejected).
- graineo addition: `process/spp.rs` `mono_working`, `process.rs` `spp_mono_working_image`,
  `x3f-core` `Reader::spp_mono_working_image`: the monochrome working image (Stage2 + base
  sharpening, before the tone curve) of the active area with a selectable B/W mixer, the
  preprocessing variants (`SppVariants`) and the noise sliders (noise scaling × ratio of the colour
  path's noise scaling to its neutral value; neutral sliders = exactly the PR's render).
- Colour path unchanged on the reference images.

## Comment clean-up (November 2026)

- `float_front.rs`: the dense-map comment of the saturation map describes the step functionally only.
- Comments only; no code change.

## Comment clean-up: functional descriptions instead of native names (November 2026)

- `crates/x3f-sys/src/merrill_mono.rs`, `crates/x3f-sys/src/merrill.rs`: comments that named classes and
  functions of the reference software's native library and their code addresses now describe the step
  functionally (setup, tone-LUT generation, Stage4, 48-bit output row, crop/rotate rounding, gamma and
  tanh LUTs). Comments only; no code change, Merrill hashes unchanged.
- Second pass (same section): code addresses, struct offsets, native function/class names and
  references to archived harness files were replaced by functional descriptions in
  `crates/x3f-sys/src/`: `cr_offset.rs`, `float_front.rs`, `float_front/repair.rs`, `process.rs`,
  `process/spp.rs`, `process/spp/image_tests.rs`, `process/spp/prefilter_tests.rs`, `spatial_gain.rs`,
  `spp_denoise.rs`, `spp_denoise/{params,pipeline,residual,tiling,pixmap,mono}.rs`,
  `spp_denoise/mono/{bcs,hist_bilateral,hn,sharpen_bnw}.rs`, `color_modes.rs`, `merrill.rs`,
  `merrill_mono.rs`, `merge_frame.rs`. Only constants and file-format values (float bit patterns,
  hashes, RNG seeds, header constants) remain as numeric literals. Comments only; no code change.

## Quattro black level as in the reference processing (November 2026)

- `crates/x3f-sys/src/process.rs` `quattro_spp_black`: the sd Quattro H also subtracts the constant CAMF
  `RawOffset` (before: DarkShield mean); `quattro_black_levels`: the sd Quattro blend towards the DarkShield
  mean at ISO 6400 is switched off (`SDQ_SHIELD_MIX` 0.85 → 0; `X3F_QUATTRO_SDQ_MIX` still overrides it,
  `X3F_QUATTRO_BLACK=shield` still selects the original measurement). Measured against the reference
  exports: sd Quattro ISO 6400 ΔE00 9.1 → 5.5, sd Quattro H ISO 6400 8.1 → 6.3, ISO 100 unchanged (±0.03).

## Quattro top layer via `get_image` (November 2026)

- `crates/x3f-core/src/image.rs` `Reader::get_image_with_control`: `ColorEncoding::Qtop` passes a null
  levels pointer like `ColorEncoding::Unprocessed` (the native side returns the unprocessed top layer and
  rejects a levels pointer for both encodings, so `get_image`/`dump_tiff` with `Qtop` always failed).
  Used by graineo's "sensor layers" export. No effect on any other encoding.

## Neutral name for the pixel-repair pass (November 2026)

- The pass called "RepairPix" is a function name of the reference software. It is now called
  "PixelRepair" everywhere: type `repair_pix_t` → `pixel_repair_t`, functions
  `repair_pix_init_defaults` / `repair_pix_apply_pixel` → `pixel_repair_init_defaults` /
  `pixel_repair_apply_pixel`, switches `X3F_REPAIR_PIX*` → `X3F_PIXEL_REPAIR*` (`_SAT`, `_THRESH`,
  `_DIVISOR`, `_ANCHOR_LO`, `_TRACE`, `_TRACE_R`, `_TRACE_C`), plus comments and docs
  (`crates/x3f-sys/src/{highlight,highlight_recovery,process,lib}.rs`, `crates/x3f-core`,
  `crates/x3f-cli`, `docs/`, `AGENTS.md`).
- The old `X3F_REPAIR_PIX*` environment variables are still read as an alias
  (`env_var_alias` in `highlight.rs` and `process.rs`); the new name takes precedence.
  No change to the computation: the Merrill hashes are unchanged.

## Floor for the intermediate bias (November 2026)

- `crates/x3f-sys/src/denoise.rs` `DenoiseOptions::min_intermediate_bias` (default 0 = upstream behaviour,
  validated 0…4096) and `crates/x3f-sys/src/process.rs` `preprocess_data_impl`: the intermediate bias (black of
  the integer intermediate buffers) is at least this value. graineo sets 2048 when it replaces the colour
  denoising of the Quattro preparation by its own noise reduction on the colour grid: without denoising, the
  noise of dark high-ISO regions can reach below the bias and be clipped at zero, which brightens the darks
  (measured with the high-ISO trial switch: sd Quattro ISO 6400 ΔL* +0.96 → +0.68 against the reference). No effect
  when the field is 0 (Merrill hashes unchanged).

## Sharpening image above ISO 1600 (November 2026)

- `crates/x3f-sys/src/spp_denoise/residual.rs` `generate_sharp_l_with` and `spp_denoise/gpu.wgsl`
  `sharp_l`: the weight of the residual in the sharpening image now uses `1 − clamp(ISO − 100, 0, 1500)/1500`
  so that the output matches the reference processing. Before, values above 1500 selected a
  factor of −100, which turned the residual deviation into strong pixel noise in the sharpening
  image; every Merrill colour file above ISO 1600 was grainy. Measured against SPP 6.8.2 exports
  (DP3 Merrill): ISO 3200 ΔE00 6.14 → 0.01, ISO 6400 6.05 → 0.02. ISO ≤ 1600 is unchanged (the
  clamp only differs above 1600): Merrill hashes unchanged.
- `crates/x3f-sys/src/process/spp.rs`: with `X3F_SPP_PARAMS` the ISO estimate inputs are printed as well;
  `X3F_SPP_TEST` additionally knows `aniso`, `mixing`, `strength`, `ns` and `intens` (noise parameters,
  tests only). No effect without these variables.

## Black and white without column matching (November 2026)

- `crates/x3f-sys/src/spp_variants.rs`: new flag `mono_columns_off`; `spp_denoise/mono.rs`
  (`MonoDenoiseArgs::skip_columns`) then skips the column matching of the grey plane, all other steps of the
  grey chain stay. `process/spp.rs` `mono_working` passes the flag; `render_monochrome` is unchanged.
  Default `false`: Merrill and black-and-white hashes unchanged.

## Steps removed in favour of an optional processing extension (November 2026)

The following steps were removed from this copy. Each call site now asks the optional processing extension of
the graineo tree (`crates/extension`, a C interface; module `src/ext.rs`) and otherwise runs the own way (or
leaves the step out):

- defective pixels: the estimate of a defective channel from the other channels of the pixel
  (`float_front.rs` `ListedRepair`) and the block-matching repair of the defect clusters (`float_front/repair.rs`,
  now `repair_bad_pixel_clusters_ext`). Own way: per-channel replacement and `fill_bad_pixel_clusters` (each
  channel only from the same channel of the good neighbours, filled from the outside in);
- highlights: the restoration from a donor ratio table (`float_front.rs` `HighlightRestoration::restore`, now
  `restore_ext`). Own way: `restore_own` (`float_front/propagate.rs`);
- noise reduction tiles: `match_col_offsets`/`directional_bilateral_rows`, `gen_residual_min_max_map`/
  `row_column_pattern_suppression` (`spp_denoise/residual.rs`) and the column matching of the grey plane
  (`spp_denoise/mono/bcs.rs`, file removed). Without the extension these steps are off;
- GPU: the kernels `dbilateral`, `col_means`, `col_apply`, `mm_h`, `mm_v`, `rc_count`, `rc_prefix`, `rc_rows`,
  `rc_cols`, `rc_blend` were removed from `spp_denoise/gpu.wgsl`. An extension may supply WGSL for these slots;
  `spp_denoise/gpu.rs` checks it (naga) and appends it to the module, otherwise the passes are not dispatched;
- the tests that compared these steps with native results and their fixtures are no longer part of this copy.
- `Cargo.toml`: the former feature for these steps is gone; `crates/x3f-sys` depends on `crates/extension` (path).
- 2026-11 (graineo): `float_front.rs` `BadPixelRecord` is `#[repr(C)]` (passed to the extension as three u16).

## GPU noise reduction for all slider steps, GPU offset correction (November 2026)

- 2026-11-12 (graineo): the GPU noise reduction (`spp_denoise/gpu.rs`, `gpu.wgsl`) now covers all
  parameter combinations; `supports()` only requires a finite `alpha`, and `process/spp.rs` `working`
  no longer forces the CPU when a CA correction is set.
  - Colour slider step 0/1 (`alpha ≠ 1`): the blend is removed from `dbilateral` (the column matching
    filters without it on the CPU, cause of the earlier deviation) and added to `bilateral_image` (whole-image
    prefilter, including its border).
  - CA correction: new kernel `ca_shift` (tile-local radial shift as `CaTransform::apply`, f32; values from
    the new `CaTransform::gpu_values`), after `matrix_dq`; `GpuView` carries the transform.
  - Colour slider "Off" (`chroma_off`): new kernels `copy_rgb` (keeps tile B after the column matching in the
    new buffer `raw`) and `restore_chroma` (as `Pipeline::restore_chroma`), B through `matrix_dq` and
    `ca_shift`.
  - The CPU path is unchanged (bit-exact; Merrill hashes unchanged).
- 2026-11-12 (graineo): `cr_offset.rs` (row offset correction of the Merrill preparation): on the GPU path of
  the SPP working image (`process/spp.rs` `working` with `gpu`, via the new thread-local switch
  `cr_offset::with_gpu`) the patch fits of all three sizes run on the GPU (new `spp_denoise/gpu_offset.wgsl`,
  `gpu::offset_fits`, one workgroup per patch, f32 reductions); accumulation and final application stay on the
  CPU in the documented order. Not bit-exact; a GPU error falls back to the unchanged CPU code. The CPU path
  (`gpu = false`, `prepared`, `mono_working`) is unchanged; Merrill hashes unchanged.
- 2026-11-13 (graineo): `float_front.rs`, own replacement of defective pixels (`defects_per_channel`, and always
  without the optional processing extension): listed records (`ListedRepair::replace_one`) replace only the
  channels marked in the record, each with the median of the same channel over the permitted neighbours; the
  other channels of the pixel stay unchanged (before: the whole pixel from the opposite pair); when the intact
  channels differ by more than 15 % from their neighbour median and the marked channels lie within their
  neighbours' range, the pixel is kept (the neighbour median would shift its colour). Despeckle hits
  (`Despeckle::replace_own`, new): only the marked channels, between the neighbour median and the neighbour
  extreme of the same channel (plus a quarter of the excess beyond the extreme), weighted by whether the unmarked
  layers of the pixel show a peak of the same sign in units of their detection threshold. The value never comes
  from another channel. Effect: fine textures (fabric, hair) keep their pixel-exact lines; measured against the
  reference computation, ΔE00 in textured areas clearly smaller at all ISO. The reference path and the Merrill
  hashes are unchanged.
- 2026-11-13 (graineo): `spp_variants.rs` (`power_tone`) and `spp_denoise/gpu.wgsl`: the log-domain scaling of the
  detail ratio is now `r^amount` for every amount (the first-order branch for negative amounts is removed; the
  amounts used are clamped to be non-negative, so it was never reached). Without the optional processing extension
  `residual_power` is always set (own way); the reference form stays selectable only with the extension. Merrill
  hashes of the reference path are unchanged.
- 2026-11-13 (graineo): the reference form of the detail scaling (ratio of the residual, weight of the sharpening
  image) is no longer part of this tree. `spp_denoise/residual.rs` (`color_residual_mixing_with`,
  `mono_residual_mixing_with`, `generate_sharp_l_with`) leaves the values unscaled when `residual_power` is not
  set and scales the finished map in one call, `ext::detail_scale` (new): the optional processing extension's
  function (`ExtensionV1::detail_scale`), without extension the own scaling `r^amount`. On the GPU
  (`spp_denoise/gpu.wgsl`, `gpu.rs` `shader_source`) `tone` and `sharp_l` call the helper functions
  `detail_ratio` / `detail_weight` (helper functions of the extension interface), taken from the extension's tile source
  when it defines them (signature checked with naga), otherwise the host's own scaling. Results of the reference
  path with the extension are bit-identical to before; the own path is unchanged.

## Optional processing extension removed: one way of computing (November 2026)

- 2026-11-13 (graineo): the interface to the optional processing extension is gone (graineo has only its own way of
  computing). Every step now runs the way it ran without an extension before; results are bit-identical to the
  previous build without an extension (Merrill, Quattro and the older Foveon cameras, checked with
  `scripts/check.sh` of the graineo tree).
  - `crates/x3f-sys/Cargo.toml`: no dependency on the extension crate; `src/ext.rs` removed.
  - `SppVariants` (`src/spp_variants.rs`) removed, together with `spp_working_image_variants` (x3f-sys, x3f-core) and
    `merrill_prepare_float_variants` (its body is now `merrill_prepare_float`). `spp_prepared_image` and
    `spp_mono_working_image` lose their `variants` argument. Each step keeps the own way: `ListedRepair` and
    `Despeckle` without `per_channel` (always the per-channel replacement), `repair_bad_pixel_clusters_ext` removed
    (always `fill_bad_pixel_clusters`), `HighlightRestoration::restore_ext` removed (always `restore_own`),
    `MonoDenoiseArgs::skip_columns` removed (no column matching of the grey plane).
  - `spp_denoise/pipeline.rs`: the call sites of the column offset matching and of the pattern suppression of the
    residual are removed (both were off without an extension); `Pipeline::set_variants`/`variants` and the order
    table for the extension (`permutation_u32`) are gone. `process/spp.rs` no longer sets `rcps_blending = 0` (the
    value is not read any more).
  - `spp_denoise/residual.rs`: `color_residual_mixing`, `mono_residual_mixing` and `generate_sharp_l` always scale in
    the log domain (`power_tone`, moved here from `spp_variants.rs`); the `*_with` variants are removed. Without an
    extension both forms gave the same values before.
  - `spp_denoise/gpu.rs`, `gpu.wgsl`: the module is `gpu.wgsl` alone (no appended extension source, no slot or
    signature checks); the entry points of the slots (`dbilateral`, `col_means`, `col_apply`, `mm_h`, `mm_v`,
    `rc_count`, `rc_prefix`, `rc_rows`, `rc_cols`, `rc_blend`) and `copy_mono` are no longer created or dispatched,
    the buffers `means` and `rcbuf` are gone; `tone` and `sharp_l` call `power_tone` directly. The fields of the
    parameter block that only those steps read (`rcps_*`, `perm_*`, `match_*`, `db_*`, `corr_radius`, `power`) stay
    in the block so that its layout is unchanged.

## Alternative Merrill parts behind a test switch (November 2026)

- 2026-11-13 (graineo): new module `src/merrill_new.rs` (re-exported by x3f-core as `merrill_new`): the environment
  variable `GRAINEO_MERRILL_NEW` (comma list: `outliers`, `highlights`, `ca`, `noise`, … or `all`) switches single
  parts to an alternative form for comparison. Without the variable every result is bit-identical to before.
  - `float_front.rs`, new `float_front/speckle_own.rs`: own outlier detection (`outliers`): per layer a noise model
    `σ² = a + b·v` fitted to the image itself (robust spread per level bin, after Foi et al. 2008), a sample is an
    outlier when it lies outside its eight neighbours and its distance to their median exceeds a multiple of
    `max(σ(median), neighbour spread)`. The replacement (`replace_own`) is unchanged; its evidence threshold uses the
    own model when the switch is on. `Despeckle` gains the fields `adjust` and `own`.
  - `float_front/propagate.rs`: step 1 (cleaning of the saturation map) moved into `clean_saturation`, shared with the
    new reconstruction; no change in results.
  - New `float_front/opposed.rs`: second own highlight reconstruction (`highlights`), "inpaint opposed" after darktable
    (GPL-3.0) plus a continuous saturation limit by the closeness of the layers to their clip level; `process.rs`
    chooses it with the switch.
  - `spp_denoise/ca.rs`: own radial form of the CA shift (`ca`); `spp_denoise/gpu.rs` leaves it to the CPU.
  - `spp_denoise/params.rs`: noise slider steps (`noise`): no built-in step table, detail strength per step as a
    power law of the luminance factor, no RCPS offset.
- 2026-11-15 (graineo): all names of this switch are English now (no behaviour change): module `merrill_neu` → `merrill_new`, `aktiv` → `active`,
  `aktive` → `active_parts`, `RANDTEILE`/`ALLE` → `EDGE_PARTS`/`ALL`, variable `GRAINEO_MERRILL_NEU` → `GRAINEO_MERRILL_NEW`; part names `schaerfe` →
  `sharpness`, `farbsaum` → `fringe`, `rausch` → `noise`, `ausreisser` → `outliers`, `lichter` → `highlights`, `randteile` → `edge_parts`, `alle` → `all`.
  The disk-cache key line `merrill_neu=…` is now `merrill_new=…` (only present while the test switch is on).


## Merrill highlight desaturation: brightness measure (November 2026)

- 2026-11-15 (graineo): `float_front/propagate.rs`, step 4 of the own highlight reconstruction (`desaturate`): the
  brightness that drives the desaturation weight is no longer the largest layer alone. New function
  `highlight_brightness`: `0.8 · largest + 0.2 · smallest layer` (grey part plus 80 % of the colour part), so that
  bright, strongly coloured pixels (red petals, coloured lights) keep their colour and only pixels that are bright as a
  whole turn neutral. Where a layer of the pixel itself is clipped (clip bits after cleaning the saturation map), the
  largest layer is used as before, because the colour there is only estimated. `desaturate` now receives the clip
  bits. Smoothstep width, the ¾ clip mask, the target (bottom layer) and the filling step are unchanged. Merrill only;
  results change only in bright coloured highlights.

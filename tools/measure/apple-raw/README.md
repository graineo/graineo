# tools/measure/apple-raw – system RAW engine (Core Image) vs. graineo

Measurement tools for the question whether the macOS system RAW engine (`CIRAWFilter`, Core Image) gives better
image quality or speed than graineo's own pipeline for non-Sigma RAW files. macOS only. Not part of the app.
License: GPL-3.0-only. No test images in the repository; all paths are arguments.

**When needed:** before deciding on or changing an optional system-engine path in the app; after a macOS update
(new `decoderVersion`, e.g. version 9 from macOS 27 on); for a new camera family; as a benchmark for our own
demosaicing (X-Trans, Bayer) and noise reduction.

| File | Purpose | Call (short form) |
|---|---|---|
| `apple_raw.swift` | develop RAWs with `CIRAWFilter`: `info` (supported decoder versions, defaults), `preview` (embedded preview via ImageIO, colour reference), `develop` (modes `neutral` = all optional steps off, linear Rec. 2020 half-float TIFF; `default` = engine defaults, sRGB 16-bit TIFF; all decoder versions; pixel hash for reproducibility) | `swiftc -O -o apple-raw apple_raw.swift; ./apple-raw develop FILE OUT --mode neutral --version all --repeat 2` |
| `apple_raw_bench.swift` | performance (every command takes `--version V` to pin the decoder version, e.g. 8 or 9): first image (preview / scaled render), full resolution GPU/CPU cold/warm, slider re-render with one long-lived filter, export JPEG, parallel decoding; peak RSS, physical footprint, CPU load | `swiftc -O -o apple-raw-bench apple_raw_bench.swift; ./apple-raw-bench slider FILE --px 1600` |
| `run_bench.sh` | all benchmark scenarios, one process each, N runs, interleaved with `graineo-dev rawapp --full` under the same load | `GRAINEO_DEV=… ./run_bench.sh ./apple-raw-bench OUT 3 FILE…` |
| `bench_eval.py` | summary table of `run_bench.sh` results: per file median (min–max) for both pipelines, peak footprint, CPU load | `python -I bench_eval.py OUT/bench.tsv` |
| `compare.py` | `colour`: CIEDE2000 against the embedded preview; `detail`: registration + tone matching, then detail (Laplacian energy), high-frequency chroma (false colour/maze/moiré indicator), noise in flat dark tiles, side-by-side crop sheet | `python -I compare.py detail graineo=A.png apple=B.tif --sheet S.png` |
| `ci-wgpu-probe/` | feasibility probe: Core Image renders a RAW straight into an `MTLTexture` on the wgpu device's `MTLDevice`, wrapped as `wgpu::Texture` without CPU copy; read back and compared bit by bit with a CPU-side render | `cargo build --release` (own workspace), `ci-wgpu-probe FILE --runs 3` |

graineo side: `graineo-dev raw FILE out.png --no-noise --sharpness 0 --no-lens --no-preview` (demosaic + colour only)
and `graineo-dev raw FILE out.png` (standard development); always with its own `GRAINEO_DATA_DIR` and `GRAINEO_NO_XMP=1`.

Notes:
- `compare.py detail` compares structure only: each image is registered (integer shift) and tone-matched to the
  first one, so tone curve and colour rendering do not count. Lens correction must be off on both sides.
- In `CIRAWFilter` the noise reduction, sharpening and moiré amounts are per-camera defaults; `neutral` sets them
  to 0. `decoderVersion` changes demosaicing and noise reduction only (Apple, developer forums).
- Core Image renders into a Metal texture with the origin at the bottom (rows flipped compared to a bitmap).
- Python: `numpy scipy pillow tifffile imagecodecs` in a venv; start with `python -I`.

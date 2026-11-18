# tools/measure – permanent measurement tools

> **PERMANENT – DO NOT DELETE.** These scripts are measurement procedures that are needed again and again (comparison before
> releases, after changes to film, colour, grain). Intermediate results, reports and test images belong in a
> git-ignored scratch folder that may be cleaned up; reusable tools belong here.
> License: GPL-3.0-only like the project. Do not check in test images; always pass paths as parameters or environment variables.

| Folder | Tool | Purpose | When needed again | Call (short form) |
|---|---|---|---|---|
| `color/` | `deltae.py`, `demath.py` | ΔE00 (CIEDE2000) of two 16-bit TIFFs or folders, whole and as 8×8 block means | for every colour/tone change, as before/after comparison | `python deltae.py A.tif B.tif` |
| `grain/` | `grain_measure.py` | grain strength (RMS), grain size (autocorrelation) per brightness zone in flat areas | after changes to the film grain (spektrafilm-rs, grain classes); measure again before release | `python grain_measure.py out.json image.png …` |
| `grain/` | `wedge.py`, `wedge_measure.py`, `compute.sh` | generate a step wedge (-6…+3 EV), measure strength/size per patch, render per film and mode | as above; reproducible measurement instead of a photo | see `grain/README.md` |
| `color-measurement/` | `color_measurement.py`, `charts.py` | colour fidelity before releases, one call → all tables: Sigma against manufacturer software (SPP TIFF), ColorChecker against target values (white balance 20–22, exposure patch 22), non-Sigma against camera JPEG (neutral and with camera look) | before every release, after changes to colour, tone curve, looks | `python -I color_measurement.py config.json` |
| `camera-color/` | `evaluate.py` (with `graineo-dev camera-color`) | distance of the standard development to the camera JPEG / embedded preview as ΔE00 distributions (median, 90th percentile) over many RAW+JPEG pairs; saturation sweep and hue-table fit (`--fit-look`) on training days, decision on held-out days | after changes to the standard development of non-Sigma cameras (tone curve, `camera_saturation`, `camera_look`) | `graineo-dev camera-color --list pairs.tsv --sat 0.8,0.9,1.0 > r.tsv; python3 -I evaluate.py r.tsv` |
| `color-chart/` | `measure.py`, `develop.py`, `sigma.py`, `test_images.py`, `crop.py`, `dngtags.py` | colour fidelity per camera: ColorChecker-24 shot, graineo and camera JPEG against target values (Lab D50) | after changes to colour matrices, DNG profiles, camera looks; for new cameras | see `color-chart/README.md` |
| `spektrafilm-parity/` | `build.sh`, `run.sh`, `py_run.py`, `compare.py`, `test_image.py`, `image.py`, `profcmp.py` | stage-by-stage comparison of spektrafilm-rs (f64 CPU, f32 GPU) against the Python original | after every change to `third_party/spektrafilm-rs`, when updating the film data | see `spektrafilm-parity/README.md` |
| `timing/` | `timing.sh`, `stamp.py`, `evaluate.py` | performance figures of the project website: open to first image, full quality, recompute after slider, export; per camera cold + N runs, median | before every release, after changes to compute paths, pools, GPU | `GRAINEO_APP=… GRAINEO_DEV=… ./timing.sh cases.tsv OUT 5` |
| `startpage/` | `run.sh`, `evaluate.py` (+ `graineo-dev bench-data`, app test aids `GRAINEO_GRID_BENCH`, `GRAINEO_IMPORT_BENCH`) | start page facts in one unattended run: grid with 65 000 synthetic photos (thumbnails present while scrolling and after jumps, frame times), import (window thumbnails, photos/s, frame times), one photo large (first image, full quality, slider, zoom/pan without recomputing); one result file with computer and load | before publishing figures, after changes to grid, preview store, import, render scheduling | `GRAINEO_APP=… GRAINEO_DEV=… ./run.sh WORK RAWS` |
| `apple-raw/` | `apple_raw.swift`, `apple_raw_bench.swift`, `run_bench.sh`, `bench_eval.py`, `compare.py`, `ci-wgpu-probe/` | macOS system RAW engine (Core Image `CIRAWFilter`) against graineo: colour, detail, false colour, noise, reproducibility per decoder version, speed and memory; probe for sharing a Core Image render with wgpu via Metal | before deciding on an optional system-engine path, after macOS updates (new decoder version), as benchmark for our own demosaicing | see `apple-raw/README.md` |
| `auto/` | `ci_auto.swift`, `metrics.py` | automatic tone ("Auto", ⌘U) against the adjustment proposed by Core Image's public `autoAdjustmentFilters`: proposed parameters, rendered result, L*/clip/near-black/contrast/chroma metrics over an exposure series | when tuning or changing the automatic tone | see `auto/README.md` |
| `qos/` | `bgprobe.py`, `qos_probe.py` | CPU time per macOS QoS class (background/utility): does the background starve? | when working on background threads, pools, fan/deadlock questions | `python3 -I bgprobe.py`, `python3 qos_probe.py FILE` |

## Common rules

- Tested 14.11.2026 (venv, Python 3.14): deltae, grain_measure, wedge, wedge_measure, color-chart/*, spektrafilm-parity/*, qos/* run and reproduce the values from the earlier reports.
- Test program: `GRAINEO_DEV=/path/to/graineo-dev` (or on the `PATH`). Always with its own `GRAINEO_DATA_DIR`, never the real library.
- Python dependencies (once into a venv): `numpy scipy pillow tifffile colour-science matplotlib`. Start scripts with `python -I` when they read third-party data.
- Adding a new tool: header comment (purpose, usage, source of the method), no fixed paths, a line in the table above.

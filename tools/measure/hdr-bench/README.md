# hdr-bench – test bench for merging raw exposure brackets

`hdrbench.py` compares HDR merge methods on the same brackets with the same metrics and the same
fixed tone mapping. It is a measurement tool only; third-party programs are run as external
references, none of their code is used.

## Setup

```sh
python3 -m venv venv
venv/bin/pip install numpy scipy opencv-python-headless tifffile imagecodecs rawpy exifread OpenEXR pillow
export HDRBENCH_ROOT=/path/to/bench        # all data and results live here, never in the repo
```

Optional `$HDRBENCH_ROOT/bench.json`:

```json
{
 "dirs":  {"series": "series", "results": "results", "sheets": "sheets", "tables": "tables"},
 "tools": {"hdrmerge": "/path/hdrmerge", "align_image_stack": "...", "enfuse": "...",
           "hugin_hdrmerge": "...", "luminance": "/path/luminance-hdr-cli", "prototype": "/path/merge-binary"},
 "methods": {
   "my-merge": {"cmd": "/path/my-merge {frames} --ref {ref} --out {out}", "output": "out.tif",
                "kind": "linear", "note": "what it does"}
 }
}
```

External methods get `{frames}` (raw files, sorted dark → bright), `{ref}` (index of the reference
frame), `{out}`, `{dev}` (developed 16-bit linear TIFFs), `{series_json}`, `{tmp}`. Accepted outputs:
float TIFF, EXR, Radiance HDR, float DNG (CFA or LinearRaw) or `.f32` + `.f32.json` (`{"w","h"}`, RGB
float32 little endian). Linear results should be camera RGB × reference white balance; set
`"wb_from_dev": true` if the output is camera RGB without white balance. A global scale is fitted by
the scorer, so the absolute level does not matter. The geometry may differ by a crop (found by
template matching), but not by a warp of the reference frame.

## Workflow

```sh
H="venv/bin/python -I hdrbench.py"
$H add-series NAME raws/*.CR2 --license "CC BY 4.0" --source "..." --tags "tripod,motion"
$H develop                      # common developer → series/NAME/dev/NN.tif (linear, reference WB)
$H truth NAME truth.tif         # low-noise radiance from a static tripod series (2× downscaled)
$H synth SYN --truth truth.tif --evs=-2,-1,0,1,2 --shift 12 --rot 0.3 --objects 2 --noise-a 1.2e-4 --noise-b 6e-9
$H develop SYN
$H run all                      # or: run opencv-debevec,hdrmerge NAME
$H score                        # → tables/scores.csv + .json
$H sheet                        # → sheets/NAME.jpg
$H ingest lightroom NAME merged.dng --seconds 12   # results produced by hand
```

`run` starts every method in a child process under `/usr/bin/time -l` (wall time and peak memory of the
whole method, including reading its inputs). `HDRBENCH_THREADS` (default 4) sets OpenCV, OpenMP and
Rayon threads.

## Common developer

rawpy/LibRaw mosaic (black per channel, white level), OpenCV edge-aware demosaic, as-shot white
balance of the reference frame for all frames, no colour matrix, no curve. Stored as 16-bit linear
TIFF scaled by `dev.scale` (= max WB multiplier). Programs that need developed images (OpenCV,
enfuse, Hugin, Luminance HDR) get these frames (8-bit or 16-bit sRGB-encoded where they require it),
float DNG outputs (HDRMerge) are demosaiced the same way. Programs that read raws themselves use their
own demosaic: compare sharpness between such programs with care. Mosaics that are not 2x2 (X-Trans) fall back
to LibRaw's own demosaic (linear camera RGB, reference WB divided by its maximum).

Results of external methods written as `.f32` are converted from the `wb` in their `.f32.json` to the developer's
white balance. Results in raw sensor geometry may differ from the developer's geometry by a sub-pixel amount, so the
integer registration can flip between neighbouring offsets; `HDRBENCH_FORCE_OFFSET="series:x,y;..."` fixes it.

## Metrics (per method × series)

Linear results are scaled by the median luma ratio to the target in mid-tones (`scale_ev`). Target =
truth for synthetic series, the developed reference frame for real ones. 3 % border excluded.

| column | definition |
|---|---|
| `log_rmse_ev`, `psnr_log_db` | RMS error of log2 luma against the truth (pixels above 2^-12 of the reference clip); PSNR with the truth's 0.1–99.9 % range in EV as peak (synthetic) |
| `ms_ssim_log` | MS-SSIM (5 scales) of log2 luma normalised to the truth's range (synthetic) |
| `snr_db_ev-8/-6`, `snr_by_ev` | SNR per 1-EV luma bin relative to the reference clip: synthetic = mean / σ(result − truth) in flat 16×16 blocks (median block), real = mean / σ of the luma high-pass in flat blocks (10th percentile, **relative only**: blur lowers σ) |
| `dr_ev` | log2(high / low): low = first bin with SNR ≥ 1, high = synthetic: highest 0.5-EV bin reproduced within 0.1 EV (median); real: 99.99th percentile of the result |
| `align_med_px`, `align_p95_px` | phase-correlation shift of 128-px tiles of the result against the target (textured tiles) |
| `ghost_err_ev`, `ghost_area_pct` | synthetic: mean \|log2 error\| inside the known motion mask, share above 0.3 EV |
| `ghost_proxy_ev`, `ghost_proxy_area_pct` | real: same against the reference frame inside a motion mask estimated from frame disagreement after ECC alignment (median) |
| `dE00_mid`, `_p95` | CIEDE2000 on 4×4 bins in mid-tones (target luma 0.02–0.5, reference unclipped), camera RGB treated as linear sRGB |
| `edge_grad_ratio` | mean log-luma gradient of the result / of the target on the 3 % strongest target edges in mid-tones (1 = as sharp, < 1 blurred, > 1 ringing or noise) |
| `seam_err` | mean \|∇log2 result − ∇log2 C\| (2×2 bins) in the band where the reference frame clips (0.75–0.99 of white): C = truth (synthetic) or the next darker frame (real) |
| `wall_s`, `rss_mb` | wall time and peak RSS of the whole method run |

Display-referred results (exposure fusion) get only the alignment metrics and the contact sheet.

## Contact sheets

One row per method: thumbnail with crop frames, then 256 px crops at 100 %: shadow (+3 EV), highlight
(scaled to its own level), motion, edge. All linear results use the same operator: reference clip = 1,
per-channel extended Reinhard (white 16), sRGB encoding. Crop positions are chosen once per series and
stored in `series.json` (`crops`).

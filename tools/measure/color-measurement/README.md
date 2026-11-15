# color-measurement – colour fidelity of graineo, one call → all tables

Permanent – do not delete. License GPL-3.0-only. No images in the repo; all paths are in a configuration
outside the repo.

```sh
python -m venv ~/venv-measure && ~/venv-measure/bin/pip install numpy tifffile pillow colour-science matplotlib
~/venv-measure/bin/python -I color_measurement.py config.json              # all three measurements, results cached
~/venv-measure/bin/python -I color_measurement.py config.json --only spp   # one measurement only (spp, color_chart, camera_jpeg)
~/venv-measure/bin/python -I charts.py OUTPUT_FOLDER                       # PNG charts
```

Result in the output folder: `tables.md` (all tables), `result.json` (all values per file/patch),
`spp/<name>.json`, `color_chart/<camera>.json`.

## Configuration (`config.json`, relative paths are relative to this file)

```json
{
  "graineo_dev": "/path/to/graineo-dev",
  "data_dir": "/tmp/graineo-measure-data",
  "output": "result",
  "parallel": 2,
  "spp":         {"pairs": "spp-pairs.tsv"},
  "color_chart": {"list": "color-chart.tsv", "corners_graineo": "corners_graineo.json", "corners_jpeg": "corners_jpeg.json"},
  "camera_jpeg": {"pairs": "jpeg-pairs.tsv"}
}
```

`data_dir` is set as `GRAINEO_DATA_DIR` (never the real library). A missing section is skipped.

## Lists (TSV with header line, `#` = comment)

| File | Columns |
|---|---|
| `spp-pairs.tsv` | `name camera group x3f reference own source options` – `reference` = 16-bit TIFF of the manufacturer software (sRGB, adjustment "as shot", original size), `group` = Merrill/Quattro/Classic, `own` = yes/no, `options` e.g. `--mono` |
| `color-chart.tsv` | `key camera kind file jpeg own source` – `kind` = sigma (X3F) or raw, `jpeg` = camera JPEG of the same shot or `-` |
| `corners_*.json` | per key 8 numbers: centres of patches 1, 6, 19, 24 as fractions (x, y) of the graineo image or the JPEG; read off with `../color-chart/crop.py`, check with `../color-chart/test_images.py` |
| `jpeg-pairs.tsv` | `camera raw reference style` – `reference` = camera JPEG of the same shot or `embedded` (preview in the RAW), `style` for evaluation only (e.g. from `graineo-dev style`) |

## Method

1. **Sigma against manufacturer software:** `graineo-dev develop X3F OUT.tif --spp --baseline` (like the app's export without
   own settings). Alignment (rotation, integer scale, offset ± 12 px, 16 px border). ΔE00 (CIEDE2000)
   per pixel → mean and p95 per file; additionally ΔE00 of the 8×8 block means (without noise and sharpening). Per camera
   mean over the files, additionally split by ISO ≤ 400 and > 400 (ISO from the X3F properties,
   `graineo-dev info`).
2. **ColorChecker against target values:** X-Rite ColorChecker Classic "after November 2014", Lab D50 (colour-science).
   White balance on patches 20–22 (like the eyedropper), exposure such that patch 22 reaches its target L*. Non-Sigma:
   `graineo-dev color-chart-raw` (export path, look neutral, white balance via temperature/tint); Sigma:
   `develop --spp --baseline --wb-mult=… --ev=…`, Newton steps until a*/b* of patches 20–22 < 0.25 and L*(22) ± 0.15.
   Per patch the mean of the inner 45 %. Metrics: ΔE00 mean, max, skin (1–2), saturated (13–18),
   chromatic (1–18), grey |ΔL*| (19–24), ΔC* chromatic, ΔE00 without lightness component (L* set to target). The camera JPEG is afterwards
   normalised in the same way (channel factors in linear light, factor on patch 22) – for orientation only.
3. **Non-Sigma against camera JPEG:** `graineo-dev rawapp RAW … --compare REF` (preview size, size matching),
   first match the exposure to the midtones of the reference, then measure; once `--ohne-look`, once
   `--kamera-look` (look of the shot's picture style, if graineo has one).

Limits: ΔE measures colour and lightness, not noise or sharpness. The camera JPEG is not a target value but the
manufacturer's intent (contrast, saturation, picture style). One ColorChecker shot per camera is a sample
(light, lens, age of the chart).

Sources: CIEDE2000 after Sharma, Wu, Dalal (2005); ColorChecker target values X-Rite (2014), colour-science dataset.

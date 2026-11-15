# color-chart – colour fidelity per camera

Permanent – do not delete. One subfolder per camera with a shot of a ColorChecker-24 chart (RAW and camera JPEG) in the working folder `COLOR_CHART_DIR` (default: current folder; no images in the repo). Target values: ColorChecker "After November 2014" (colour-science), Lab D50.

Dependencies: `numpy pillow colour-science`; test program `GRAINEO_DEV` (subcommand `farbtafel-raw`).

```sh
export COLOR_CHART_DIR=~/measurements/color-chart GRAINEO_DEV=…/graineo-dev GRAINEO_DATA_DIR=/tmp/graineo-measure-data
python crop.py IMAGE OUT.jpg x0 y0 x1 y1             # read off the corners of the 24 patches (grid in fractions)
python test_images.py corners_jpeg.json jpeg         # check the measurement boxes
python develop.py corners_graineo.json [--raw]       # graineo RAW development per camera
python sigma.py corners_graineo.json CAMERA          # X3F: eyedropper and exposure, iterative
python measure.py corners_jpeg.json corners_graineo.json result.json   # ΔE00/ΔC* per patch and camera
python dngtags.py FILE.dng                           # read the colour tags of a DNG
```

When: after changes to the colour matrix, white balance, DNG profile evaluation, camera looks, saturation correction; for every new camera.

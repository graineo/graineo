# spektrafilm-parity – Rust against the Python original

Permanent – do not delete. Checks `third_party/spektrafilm-rs` stage by stage ("taps": exposure, density, dyes, paper …) against the Python original of spektrafilm (GPL-3.0), in f64 (CPU) and f32 (GPU).

Dependencies: Python with `numpy colour-science matplotlib` plus the dependencies of spektrafilm; Rust toolchain; the Python original under `$WORKDIR/src/spektrafilm/src` (clone it yourself, do not add it to the repo).

```sh
WT=<working copy> ./build.sh                     # build the example program examples/paritaet (f64 and f32)
python test_image.py $WORKDIR/test_image         # synthetic test image: grey wedge, ColorChecker, hue ring
WT=… WORKDIR=… PY=…/python ./run.sh halation kodak_portra_400 kodak_endura_premier '{"film_render":{"halation":{"active":true}}}'
python profcmp.py FOLDER_A FOLDER_B              # compare same-named film profiles (JSON) of two folders field by field
python image.py out.png 384 80 py=py_NAME.rgb_out.f64 rs=rs_NAME.rgb_out.f64   # stack the stages as an image
```

When: after every change to spektrafilm-rs (halation, grain, paper, GPU path), after updating the film data, before releases. Expectation: deviations within the f32 rounding range, or exact in f64.

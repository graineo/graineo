# grain – grain measurement

Permanent – do not delete. Method: high-pass (image minus Gaussian σ=4) in flat blocks; strength = standard deviation of the high-pass (in code values 0…255), size = half-width of the radial autocorrelation and ρ(1).

Dependencies: `numpy scipy pillow`.

```sh
python wedge.py wedge.png gradient.png      # step wedge (4×4 patches) and gradient
GRAINEO_DEV=… GRAINEO_FILM_DATA=… WORKDIR=./sim ./compute.sh   # renders per film and mode
python wedge_measure.py sim/kodak_portra_400_new_wedge.png out.json
python grain_measure.py --zones 10 --block 32 out.json photo1.tif photo2.tif
```

When: after changes to the film grain and before every release (check grain size/strength against the data-sheet expectation and against spektrafilm Python).

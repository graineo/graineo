# startpage – benchmarks behind the facts on the project website

One unattended run (10–20 minutes, plus the test data once) that measures what the start page claims:

1. **Many photos:** the library grid with 65 000 photos – while scrolling at fixed speeds and after jumps, how many
   of the visible tiles already show their thumbnail (frame by frame), how long until every visible tile has one,
   and the frame times.
2. **Import:** the import window's thumbnails (first one, all visible), photos per second, frame times of the app
   during the import, and the grid with the new photos afterwards.
3. **One photo large:** per camera family the time to the first image, to full quality and after a slider change
   (`../timing`), and zoom/pan after the whole image is in place: render jobs sent (expected 0) and frame p95.

Result: `<work>/result/result.txt` (readable) and `result.json` (everything, with the computer and its load before
every part). A run is marked **UNDER LOAD** when the 1-minute load average was above 3 or the processor more than
25 % busy before a part.

## Usage

```sh
GRAINEO_APP=/path/release/graineo GRAINEO_DEV=/path/release/graineo-dev \
  ./run.sh <work folder> <folder with test RAWs>
```

The test RAWs (for example the CC BY 4.0 set at https://github.com/graineo/testdata, folder `raw`) are only read.
Build both programs with the normal release profile. Settings (environment): `PHOTOS=65000`, `JPEGS=2000`,
`RAWS=200`, `GRID_RUNS=3`, `GRID="speeds=5,10,20,40,80,160;secs=6;jumps=24"`, `TIMING_N=2`,
`PARTS="grid import large"`.

## Test data

- `graineo-dev bench-data library <folder> --sources <RAWs> [--photos N]`: synthetic library: day folders over
  fourteen years, ten cameras with their formats, sizes and aspect ratios (a fifth portrait), ratings, a few virtual
  copies. The photo files do not exist; every photo has its small preview (512 px, as the app stores it) cut from the
  sources with its own crop, mirror and colour gain.
- `graineo-dev bench-data import-source <folder> --sources <RAWs> [--jpegs N] [--raws N]`: JPEGs of 4–12 MP and
  RAW copies, each made unique by a few bytes at the end (still readable; APFS clones take almost no space).
- `graineo-dev bench-data system [--busy-secs 2] [--path <folder>]`: computer and load as JSON.

## Test aids in the app (only with `GRAINEO_DATA_DIR`)

- `GRAINEO_GRID_BENCH="out=<file.json>;start=6;speeds=…;secs=6;jumps=24;seed=1;timeout=10;dwell=0.4"`
  (`crates/app/src/grid_bench.rs`): the grid program; writes `<file>.json` and `<file>.frames.tsv` and closes the app.
- `GRAINEO_IMPORT_BENCH="out=<file.json>;timeout=600;after=120"` (`crates/app/src/import_bench.rs`) together with
  `GRAINEO_LIBRARY_TEST="import=<folder>"`, `GRAINEO_IMPORT_MODE=copy|inplace`, `GRAINEO_IMPORT_YES=<s>`,
  `GRAINEO_IMPORT_SHOW=0`.

## Limits

- The scroll position is set directly (like the scroll bar); trackpad inertia is not part of the measurement.
- Thumbnails come from the system's file cache after the warm-up start; a first start after a reboot is slower.
- Copying in the import reads clones of a few source files (file cache); a memory card is slower.
- "In place" shows the folder list instead of thumbnails: the window figures exist only for copying.
- The window must be visible and the screen on; covered windows do not draw.

License: GPL-3.0-only.

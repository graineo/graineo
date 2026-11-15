# timing – timing measurements for the project website

Measures the four timings of the project website per test photo (sub-page "Under the hood", section "How fast it
feels"):

| Quantity | Source | Meaning |
|---|---|---|
| Open to first image | app log `time: 0 shown after X ms` | from opening a never-seen photo until the developed image is shown |
| Full quality done | Sigma: `time: 0 exact after X ms`; other RAWs: end of the "Volle Auflösung voraus" job (line stamp) | Sigma: exact Sigma base; others: full resolution ready for 100 % |
| Recompute after slider | `time: edit exposure rendered after X ms` (test helper `GRAINEO_SET_EV=0.5` 3 s after opening) | changing exposure until the new preview |
| Export full resolution (JPEG) | wall time of `graineo-dev develop` (Sigma) or `graineo-dev color-export` (others) | same library calls as the app's export; including program and graphics card start |

Per case one cold run (first start, reported separately) and N warm runs (median, min, max). Every run
starts with a fresh `GRAINEO_DATA_DIR` (no disk cache, no previews), the app runs in normal operation
(graphics card, window at default size).

## Usage

```sh
GRAINEO_APP=/path/release/graineo GRAINEO_DEV=/path/release/graineo-dev \
  ./timing.sh cases.tsv /tmp/output 5
```

`cases.tsv`: one line per case, `name<TAB>path` (example `cases.example.tsv`). Names starting with `large` get
more time per run (`GRAINEO_TIMING_QUIT_LARGE`, default 24 s; otherwise `GRAINEO_TIMING_QUIT`, 14 s).
The files are copied into the output folder; the app only reads the copy.

Result: `output/result.md` (table), `output/timing.tsv` (all runs), `output/load.tsv` (system load,
CPU share of a running graineo app, three biggest processes before each block), `output/logs/`.
Afterwards delete `output/photos` and `output/export` (copies).

## Rules

- Never the real library: the script always sets its own `GRAINEO_DATA_DIR`, `GRAINEO_NO_XMP=1`,
  `GRAINEO_LIBRARY_NO_SYNC=1`, `GRAINEO_NO_PHOTOKIT=1`.
- Keep the machine quiet (no builds, no other measurements); include `load.tsv` in the report.
- The screen must be on: covered or sleeping windows do not draw, and timings will be missing.
- The app opens its own window per run (no mouse or keyboard control).

Tools: `timing.sh` (procedure), `stamp.py` (line stamp), `evaluate.py` (table). License GPL-3.0-only.

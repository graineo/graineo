#!/usr/bin/env python3
"""Evaluation of the timing run (tools/measure/timing/timing.sh).

Usage: python3 -I evaluate.py <output-folder> <cases.tsv>
Reads <output-folder>/logs/<case>.app.<i>.log and <case>.export.<i>.log (i = 0 cold, 1…N warm) and writes
<output-folder>/timing.tsv (all runs) and <output-folder>/result.md (table: median, min–max, cold).

Measured quantities (all in ms):
  open    – `time: 0 shown after X ms`: from opening until the first developed image is shown
  full    – Sigma: `time: 0 exact after X ms` (full Sigma quality); other RAWs: end of the job
            "Volle Auflösung voraus" (app log text), counted from the same opening time (line stamp from stamp.py)
  slider  – `time: edit exposure rendered after X ms`: exposure +0.5 until the recomputed image
  export  – wall time of graineo-dev develop / color-export (same library calls as the app's export,
            including program start and graphics card start)
License: GPL-3.0-only.
"""
import re
import statistics
import sys
from pathlib import Path

out_dir = Path(sys.argv[1])
cases = [l.split("\t")[0] for l in Path(sys.argv[2]).read_text().splitlines() if l.strip() and not l.startswith("#")]
logs = out_dir / "logs"


def app_values(p: Path):
    w = {}
    t0 = None
    preview = None
    for line in p.read_text(errors="replace").splitlines():
        ts, _, rest = line.partition("\t")
        try:
            ts = float(ts)
        except ValueError:
            continue
        if m := re.search(r"time: 0 shown after (\d+) ms", rest):
            w["open"] = float(m[1])
            t0 = ts - float(m[1])
        elif m := re.search(r"time: 0 exact after (\d+) ms", rest):
            w.setdefault("full", float(m[1]))
        elif "job Volle Auflösung voraus" in rest and t0 is not None and "full" not in w:
            w["full"] = ts - t0
        elif m := re.search(r"time: edit exposure rendered after (\d+) ms", rest):
            w.setdefault("slider", float(m[1]))
        elif m := re.search(r"render 0 applied \((\d+)×(\d+)\)", rest):
            preview = preview or f"{m[1]}×{m[2]}"
    return w, preview


def export_value(p: Path):
    for line in p.read_text(errors="replace").splitlines():
        if line.startswith("EXPORT\t"):
            parts = line.split("\t")
            size = parts[2].split() if len(parts) > 2 else []
            return float(parts[1]), ("×".join(size) if len(size) == 2 else "?")
    return None, "?"


def fmt(ms):
    if ms is None:
        return "–"
    return f"{ms / 1000:.2f} s" if ms >= 1000 else f"{ms:.0f} ms"


rows = ["case\trun\topen\tfull\tslider\texport\tpreview\toutput"]
table = []
for case in cases:
    runs = {}
    preview = output = "?"
    for p in sorted(logs.glob(f"{case}.app.*.log")):
        i = int(p.name.split(".")[-2])
        w, v = app_values(p)
        preview = v or preview
        runs.setdefault(i, {}).update(w)
    for p in sorted(logs.glob(f"{case}.export.*.log")):
        i = int(p.name.split(".")[-2])
        e, g = export_value(p)
        if e is not None:
            runs.setdefault(i, {})["export"] = e
            output = g
    for i in sorted(runs):
        w = runs[i]
        rows.append("\t".join([case, "cold" if i == 0 else str(i)] + [f"{w[k]:.0f}" if k in w else "" for k in ("open", "full", "slider", "export")] + [preview, output]))
    cols = []
    for k in ("open", "full", "slider", "export"):
        warm = [runs[i][k] for i in runs if i > 0 and k in runs[i]]
        cold = runs.get(0, {}).get(k)
        if warm:
            cols.append(f"{fmt(statistics.median(warm))} ({fmt(min(warm))}–{fmt(max(warm))}, n={len(warm)}; cold {fmt(cold)})")
        else:
            cols.append(f"– (cold {fmt(cold)})")
    table.append(f"| {case} | {preview} | {output} | " + " | ".join(cols) + " |")

(out_dir / "timing.tsv").write_text("\n".join(rows) + "\n")
header = "| Case | Preview | Export | Open to first image | Full quality done | Recompute after slider | Export full resolution (JPEG) |\n|---|---|---|---|---|---|---|\n"
(out_dir / "result.md").write_text(header + "\n".join(table) + "\n\nValues: median of the warm runs (min–max, count; cold = first run).\n")
print(header + "\n".join(table))

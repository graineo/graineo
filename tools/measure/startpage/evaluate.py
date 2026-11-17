#!/usr/bin/env python3
"""Evaluation of the start page benchmarks (tools/measure/startpage/run.sh).

Usage: python3 -I evaluate.py <result folder>
Reads the files run.sh wrote (system-*.json, grid-<r>.json, import-<kind>.json, timing/timing.tsv, zoom-<case>.log)
and writes <result folder>/result.json (everything) and result.txt (readable summary).

A run counts as "under load" when the 1-minute load average before a part was above LOAD_LIMIT or the processor
was more than BUSY_LIMIT busy in the 2 s before it; the summary then says so in its first line.
License: GPL-3.0-only.
"""
import json
import re
import statistics
import sys
from pathlib import Path

LOAD_LIMIT = 3.0
BUSY_LIMIT = 0.25

out = Path(sys.argv[1])


def load(p):
    try:
        return json.loads(Path(p).read_text())
    except (OSError, ValueError):
        return None


def spread(values):
    v = [x for x in values if x is not None]
    if not v:
        return None
    return {"n": len(v), "median": round(statistics.median(v), 1), "min": round(min(v), 1), "max": round(max(v), 1),
            "spread_pct": round((max(v) - min(v)) / statistics.median(v) * 100, 1) if statistics.median(v) else None}


def fmt_ms(x):
    if x is None:
        return "–"
    return f"{x / 1000:.2f} s" if x >= 1000 else f"{x:.0f} ms"


def fmt_spread(s, unit="ms"):
    if not s:
        return "–"
    f = fmt_ms if unit == "ms" else (lambda x: f"{x:.1f} {unit}")
    if s["n"] == 1:
        return f(s["median"])
    return f"{f(s['median'])} (runs {f(s['min'])}–{f(s['max'])}, n={s['n']})"


def system_note(name):
    s = load(out / f"{name}.json")
    if not s:
        return None, {}
    la = (s.get("load_average") or [None])[0]
    busy = s.get("busy_share")
    top = (out / f"{name}.top").read_text().strip().replace("\n", "; ") if (out / f"{name}.top").exists() else ""
    loaded = (la is not None and la > LOAD_LIMIT) or (busy is not None and busy > BUSY_LIMIT)
    return loaded, {"load_1min": la, "busy_share": busy, "top": top}


result = {"tool": "startpage-bench", "version": 1}
lines = []
under_load = []

before = load(out / "system-before.json") or {}
after = load(out / "system-after.json") or {}
result["system"] = {"before": before, "after": after}

# ── grid
grid_runs = []
for p in sorted(out.glob("grid-*.json"), key=lambda p: p.name):
    m = re.fullmatch(r"grid-(\d+)\.json", p.name)
    r = load(p) if m else None
    if not r:
        continue
    n = m[1]
    loaded, load_info = system_note(f"grid-{n}-system")
    if loaded:
        under_load.append(f"grid run {n}")
    r["load_before"] = load_info
    # start of the app: first tiles and all visible thumbnails after opening the library (app log)
    log = (out / f"grid-{n}.log").read_text(errors="replace") if (out / f"grid-{n}.log").exists() else ""
    m1 = re.search(r"grid: first (\d+) tiles after (\d+) ms", log)
    m2 = re.search(r"grid: all (\d+) visible previews after (\d+) ms", log)
    r["start_first_tiles_ms"] = float(m1[2]) if m1 else None
    r["start_all_visible_ms"] = float(m2[2]) if m2 else None
    grid_runs.append(r)
if grid_runs:
    g = {"runs": grid_runs, "grid": grid_runs[0].get("grid")}
    g["start_first_tiles_ms"] = spread([r.get("start_first_tiles_ms") for r in grid_runs])
    g["start_all_visible_ms"] = spread([r.get("start_all_visible_ms") for r in grid_runs])
    js = lambda k: [r["jump_summary"]["ms_to_complete"][k] for r in grid_runs if r["jump_summary"]["n"]]
    g["jump_complete_ms"] = {k: spread(js(k)) for k in ("median", "p95", "max")}
    g["jump_timeouts"] = [r["jump_summary"]["timeouts"] for r in grid_runs]
    g["jump_first_frame_placeholder_pct"] = spread([r["jump_summary"]["first_frame_placeholder_pct"]["median"] for r in grid_runs])
    speeds = []
    for i, s0 in enumerate(grid_runs[0]["scroll"]):
        per = [r["scroll"][i] for r in grid_runs if i < len(r["scroll"])]
        speeds.append({
            "rows_per_s": s0["rows_per_s"],
            "placeholder_mean_pct": spread([s["placeholders"]["mean_pct"] for s in per]),
            "placeholder_worst_frame_pct": spread([s["placeholders"]["worst_frame_pct"] for s in per]),
            "frames_with_placeholder_pct": spread([s["placeholders"]["frames_with_placeholder_pct"] for s in per]),
            "tiles_appeared_empty_pct": spread([s["tiles_appeared_empty_pct"] for s in per]),
            "fill_ms_median": spread([s["fill_ms"]["median"] for s in per if s["fill_ms"]["n"]]),
            "fill_ms_max": spread([s["fill_ms"]["max"] for s in per if s["fill_ms"]["n"]]),
            "frame_ms_mean": spread([s["frames_stats"]["interval_ms"]["mean"] for s in per]),
            "frame_ms_p95": spread([s["frames_stats"]["interval_ms"]["p95"] for s in per]),
            "frame_ms_p99": spread([s["frames_stats"]["interval_ms"]["p99"] for s in per]),
            "frames_over_16_7_pct": spread([s["frames_stats"]["interval_ms"]["over_16_7_pct"] for s in per]),
            "frames_over_33_pct": spread([s["frames_stats"]["interval_ms"]["over_33_pct"] for s in per]),
            "fps": spread([s["frames_stats"]["interval_ms"]["fps"] for s in per]),
        })
    g["scroll"] = speeds
    g["all_frames_p95"] = spread([r["all_frames"]["interval_ms"]["p95"] for r in grid_runs])
    g["all_frames_p99"] = spread([r["all_frames"]["interval_ms"]["p99"] for r in grid_runs])
    g["all_placeholder_mean_pct"] = spread([r["all_placeholders"]["mean_pct"] for r in grid_runs])
    result["grid"] = g
    lines.append(f"LIBRARY GRID ({g['grid']})")
    lines.append(f"  runs: {len(grid_runs)}; after opening the library: first tiles {fmt_spread(g['start_first_tiles_ms'])}, every visible thumbnail {fmt_spread(g['start_all_visible_ms'])}")
    lines.append(f"  jumps (end, middle, random; cold): until every visible tile has its thumbnail: median {fmt_spread(g['jump_complete_ms']['median'])}, "
                 f"p95 {fmt_spread(g['jump_complete_ms']['p95'])}, max {fmt_spread(g['jump_complete_ms']['max'])}; timeouts per run {g['jump_timeouts']}; "
                 f"placeholders in the first frame after a jump (median) {fmt_spread(g['jump_first_frame_placeholder_pct'], '%')}")
    lines.append("  steady scrolling (per speed; median of the runs):")
    lines.append("    rows/s | placeholders mean | worst frame | frames with placeholder | tiles in empty | frame p95 | frame p99 | > 16.7 ms | > 33 ms | fps")
    med = lambda s: "–" if not s else f"{s['median']:.1f}"
    for s in speeds:
        lines.append(f"    {s['rows_per_s']:>6.0f} | {med(s['placeholder_mean_pct']):>6} % | {med(s['placeholder_worst_frame_pct']):>6} % | {med(s['frames_with_placeholder_pct']):>6} % | "
                     f"{med(s['tiles_appeared_empty_pct']):>6} % | {med(s['frame_ms_p95']):>5} ms | {med(s['frame_ms_p99']):>5} ms | {med(s['frames_over_16_7_pct']):>5} % | {med(s['frames_over_33_pct']):>5} % | {med(s['fps'])}")
    lines.append(f"  all frames: p95 {fmt_spread(g['all_frames_p95'])}, p99 {fmt_spread(g['all_frames_p99'])}; placeholders mean {fmt_spread(g['all_placeholder_mean_pct'], '%')}")
    lines.append("")

# ── import
imports = {}
for kind in ("jpeg", "raw"):
    r = load(out / f"import-{kind}.json")
    if not r:
        continue
    loaded, load_info = system_note(f"import-{kind}-system")
    if loaded:
        under_load.append(f"import {kind}")
    r["load_before"] = load_info
    b = out / f"import-{kind}.bytes"
    # bytes per second only when copying (in place reads nothing during the run)
    if b.exists() and r["run"].get("seconds") and r.get("mode") != "inplace":
        r["run"]["source_mb"] = round(int(b.read_text().strip()) / 1e6)
        r["run"]["mb_per_s"] = round(int(b.read_text().strip()) / 1e6 / r["run"]["seconds"], 1)
    imports[kind] = r
if imports:
    result["import"] = imports
    lines.append("IMPORT")
    for kind, r in imports.items():
        w, run, f = r["window"], r["run"], r["frames"]
        thumbs = (f"first thumbnail after {fmt_ms(w['first_thumbnail_ms'])}, all {w['visible_cells']} visible cells after {fmt_ms(w['all_visible_ms'])}"
                  if w.get("first_thumbnail_ms") is not None else "no thumbnails in the window (in place shows the folder list)")
        lines.append(f"  {kind} ({r['mode']}, {r.get('photos_listed')} photos): list after {fmt_ms(w['list_ms'])}, read completely after {fmt_ms(w.get('read_complete_ms'))}, "
                     f"{thumbs} (from opening the source)")
        lines.append(f"    run: {run['photos']} photos in {run['seconds']} s = {run['photos_per_s']} photos/s ({run.get('mb_per_s', '–')} MB/s), errors {run['errors']}; "
                     f"{'TIMED OUT (grid not complete 120 s after the run), ' if r.get('timed_out') else ''}grid afterwards: first tile after {fmt_ms(r['grid_after'].get('first_tile_ms_after_run'))}, all {r['grid_after'].get('visible_tiles') or ''} visible after {fmt_ms(r['grid_after']['all_visible_ms_after_run'])}")
        lines.append(f"    frames during the run: p95 {fmt_ms(f['during_run']['p95'])}, p99 {fmt_ms(f['during_run']['p99'])}, max {fmt_ms(f['during_run']['max'])}, "
                     f"> 16.7 ms {f['during_run']['over_16_7_pct']} %, > 33 ms {f['during_run']['over_33_pct']} % (n={f['during_run']['n']})")
    lines.append("")

# ── single photo
timing = out / "timing" / "timing.tsv"
large = {}
if timing.exists():
    rows = [l.split("\t") for l in timing.read_text().splitlines()[1:] if l.strip()]
    for case in dict.fromkeys(r[0] for r in rows):
        rr = [r for r in rows if r[0] == case]
        val = lambda r, i: float(r[i]) if len(r) > i and r[i] else None
        entry = {}
        for i, k in ((2, "open_ms"), (3, "full_ms"), (4, "slider_ms")):
            warm = [val(r, i) for r in rr if r[1] != "cold"]
            cold = [val(r, i) for r in rr if r[1] == "cold"]
            entry[k] = {"warm": spread(warm), "cold": cold[0] if cold else None}
        entry["preview"] = rr[0][6] if len(rr[0]) > 6 else None
        large[case] = entry
for p in sorted(out.glob("zoom-*.log")):
    case = p.stem[len("zoom-"):]
    m = re.findall(r"stress: zoom jobs region=(\d+) full=(\d+) frame_p95=([\d.]+|n/a)", p.read_text(errors="replace"))
    ex = out / f"zoom-{case}.exit"
    z = {"exit": int(ex.read_text().strip()) if ex.exists() else None}
    if m:
        reg, full, p95 = m[-1]
        z.update({"render_region_jobs": int(reg), "render_full_jobs": int(full), "frame_p95_ms": None if p95 == "n/a" else float(p95)})
    large.setdefault(case, {})["zoom_pan"] = z
if large:
    loaded, load_info = system_note("large-system")
    if loaded:
        under_load.append("single photo")
    result["single_photo"] = {"cases": large, "load_before": load_info}
    lines.append("SINGLE PHOTO (open → first image → full quality; slider; zoom/pan after the whole image is in place)")
    for case, e in large.items():
        g = lambda k: e.get(k, {}) or {}
        w = lambda k: fmt_spread(g(k).get("warm"))
        z = e.get("zoom_pan", {})
        zs = f"render jobs after the whole image: region {z.get('render_region_jobs', '–')}, full {z.get('render_full_jobs', '–')}, frame p95 {fmt_ms(z.get('frame_p95_ms'))}" if z else "zoom/pan: –"
        lines.append(f"  {case}: first image {w('open_ms')} (cold {fmt_ms(g('open_ms').get('cold'))}); full quality {w('full_ms')} (cold {fmt_ms(g('full_ms').get('cold'))}); "
                     f"slider {w('slider_ms')}; {zs}")
    lines.append("")

loaded, _ = system_note("system-before")
if loaded:
    under_load.insert(0, "start")
result["under_load"] = under_load
head = []
cpu = before.get("cpu", "?")
mem = before.get("memory_total")
head.append(f"Computer: {before.get('model') or '?'} · {cpu} · {before.get('physical_cores') or '?'} cores ({before.get('logical_cores')} logical) · "
            f"{round(mem / 2**30) if mem else '?'} GB · {before.get('os', '?')} · graphics {', '.join(before.get('graphics') or []) or '?'} · data on {before.get('drive') or '?'} · power {before.get('power')}, thermal {before.get('thermal')}")
la_b, la_a = (before.get("load_average") or [None])[0], (after.get("load_average") or [None])[0]
head.append(f"Load: before {la_b} (processor busy {before.get('busy_share')}), after {la_a} (busy {after.get('busy_share')})")
if under_load:
    head.insert(0, f"UNDER LOAD – sample only, not for publication ({', '.join(under_load)}; limits: load > {LOAD_LIMIT} or busy > {BUSY_LIMIT:.0%})")
text = "\n".join(head) + "\n\n" + "\n".join(lines)
(out / "result.json").write_text(json.dumps(result, indent=2, ensure_ascii=False))
(out / "result.txt").write_text(text)
print(text)

#!/usr/bin/env python3
"""Evaluates runs of perf-baseline-run.sh: p50/p90/max per metric as JSON.

Usage: scripts/perf-evaluate.py <output_folder> [output.json]
Metrics from trace.log: shown after, exact after, edit exposure/noise rendered, edit noise exact,
slow frames (frame: … ms since the previous frame); peak memory from time.log (/usr/bin/time -l).
"""
import json
import re
import statistics
import sys
from pathlib import Path


def pct(v, p):
    v = sorted(v)
    if not v:
        return None
    return v[min(len(v) - 1, int(round(p / 100 * (len(v) - 1))))]


def summary(v):
    if not v:
        return {"n": 0}
    return {"n": len(v), "p50": round(statistics.median(v), 1), "p90": round(pct(v, 90), 1), "max": round(max(v), 1)}


def evaluate(run: Path):
    trace = (run / "trace.log")
    text = trace.read_text(errors="replace") if trace.exists() else ""
    def find(pattern):
        return [float(m.group(1)) for m in re.finditer(pattern, text)]
    shown = find(r"time: \d+ shown after (\d+) ms")
    shown_exact = find(r"time: \d+ shown after (\d+) ms, already exact")
    exact = find(r"time: \d+ exact after (\d+) ms")
    slider = find(r"time: edit exposure rendered after (\d+) ms")
    noise = find(r"time: edit noise rendered after (\d+) ms")
    noise_exact = find(r"time: edit noise exact after (\d+) ms")
    frames = find(r"frame: (\d+) ms since the previous frame")
    last_frames = re.findall(r"(\d+) of (\d+) frames slow, longest (\d+) ms", text)
    zoom = re.findall(r"stress: zoom jobs region=(\d+) full=(\d+) frame_p95=([\d.]+|n/a)", text)
    # Wait times at the gates (trace.log and stderr.log; the film gate writes only to stderr)
    stderr = (run / "stderr.log")
    err = stderr.read_text(errors="replace") if stderr.exists() else ""
    gates = {}
    for m in re.finditer(r"gate (\w+): waited (\d+) ms \(lane ([\w-]+?)(?:-\d+)?\)", text + "\n" + err):
        gates.setdefault(m.group(1), {}).setdefault(m.group(3), []).append(float(m.group(2)))
    gate_summary = {g: {lane: {**summary(v), "p95": round(pct(v, 95), 1)} for lane, v in lanes.items()} for g, lanes in gates.items()}
    jobs = {}
    for m in re.finditer(r"\bjob (\w+)", text):
        jobs[m.group(1)] = jobs.get(m.group(1), 0) + 1
    hover = re.findall(r"stress: hover sent=(\d+) rendered=(\d+) superseded=(\d+)", text + err)
    # Duplicate computations, outdated jobs, whole images, prefetchers
    planning = re.findall(r"stress: planning full_duplicates=(\d+) edit_dropped=(\d+)", text + err)
    count = lambda pattern: len(re.findall(pattern, text))
    timing = (run / "time.log")
    zt = timing.read_text(errors="replace") if timing.exists() else ""
    m = re.search(r"(\d+)\s+maximum resident set size", zt)
    pf = re.search(r"(\d+)\s+peak memory footprint", zt)
    real = re.search(r"([\d.]+)\s+real", zt)
    code = (run / "exitcode.txt")
    return {
        "exit_code": int(code.read_text().strip()) if code.exists() else None,
        "shown_after_ms": summary(shown),
        "shown_after_ms_already_exact": summary(shown_exact),
        "exact_after_ms": summary(exact),
        "slider_exposure_to_image_ms": summary(slider),
        "noise_stage_to_image_ms": summary(noise),
        "noise_stage_to_exact_ms": summary(noise_exact),
        "slow_frames_ms": summary(frames),
        "frames_slow_of_total": {"slow": int(last_frames[-1][0]), "total": int(last_frames[-1][1]), "longest_ms": int(last_frames[-1][2])} if last_frames else None,
        "zoom_guard": {"region": int(zoom[-1][0]), "full": int(zoom[-1][1]), "frame_p95_ms": None if zoom[-1][2] == "n/a" else float(zoom[-1][2])} if zoom else None,
        "wait_times_per_gate_ms": gate_summary,
        "job_lines": jobs,
        "planning": {
            "whole_image_duplicate": int(planning[-1][0]) if planning else None,
            "outdated_dropped": int(planning[-1][1]) if planning else None,
            "whole_image_requested": count(r"whole image requested"),
            "whole_image_outdated": count(r"whole image dropped \(outdated\)"),
            "whole_image_failed": count(r"whole image not computed"),
            "crop_old_state_dropped": count(r"crop of old settings dropped"),
            "prefetched": count(r"opened \d+ prefetch=true"),
            "photo_opened": count(r"opened \d+ prefetch=false"),
        },
        "hover_storm": {"sent": int(hover[-1][0]), "rendered": int(hover[-1][1]), "superseded": int(hover[-1][2])} if hover else None,
        "peak_memory_mb": round(int(m.group(1)) / 1e6, 0) if m else None,
        "peak_memory_footprint_mb": round(int(pf.group(1)) / 1e6, 0) if pf else None,
        "duration_s": float(real.group(1)) if real else None,
    }


def main():
    out = Path(sys.argv[1])
    result = {}
    runs = ["cold", "warm", "zoom", "a", "b", "c"]
    runs += sorted(d.name for d in out.iterdir() if d.is_dir() and d.name not in runs and (d / "trace.log").exists())
    for run in runs:
        if (out / run).is_dir():
            result[run] = evaluate(out / run)
    load_file = out / "load.txt"
    if load_file.exists():
        result["load"] = load_file.read_text().strip().splitlines()
    text = json.dumps(result, indent=2, ensure_ascii=False)
    target = Path(sys.argv[2]) if len(sys.argv) > 2 else out / "evaluation.json"
    target.write_text(text)
    print(text)


if __name__ == "__main__":
    main()

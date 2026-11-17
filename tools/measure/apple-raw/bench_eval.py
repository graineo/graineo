# SPDX-License-Identifier: GPL-3.0-only
"""bench_eval.py - summarise bench.tsv from run_bench.sh: per file median (min-max) over the runs.

Usage: python -I bench_eval.py OUTDIR/bench.tsv
Columns (system engine): first image (scaled render, first in a fresh process), full resolution GPU first in
process / warm, "CPU" context first / warm, slider re-render medians at 1600 px (exposure, temperature,
luminance NR, sharpness), export JPEG first / warm, peak physical footprint over all its processes.
graineo (graineo-dev rawapp --full): open + preview stage (first image), slider median, full resolution, peak
memory footprint (/usr/bin/time -l).
"""
import re
import statistics
import sys
from collections import defaultdict


def fmt(v):
    if not v:
        return "-"
    m = statistics.median(v)
    unit = lambda x: f"{x / 1000:.2f} s" if x >= 1000 else f"{x:.0f} ms"
    return f"{unit(m)} ({unit(min(v))}–{unit(max(v))})" if len(v) > 1 else unit(m)


def kv(fields):
    return {k: v for k, _, v in (f.partition("=") for f in fields)}


def dur_ms(s):
    m = re.match(r"([\d.]+)(ms|s|µs)", s)
    if not m:
        return None
    v = float(m.group(1))
    return v * 1000 if m.group(2) == "s" else v / 1000 if m.group(2) == "µs" else v


def main(path):
    d = defaultdict(lambda: defaultdict(list))
    cur = None
    scen = None
    for line in open(path, encoding="utf-8"):
        f = line.rstrip("\n").split("\t")
        if f[0] == "BENCH":
            scen, cur = f[1], f[2]
            a = kv(f[3:])
            r = a.get("run")
            if scen == "first" and "scaled_total_ms" in a:
                d[cur]["first"].append(float(a["scaled_total_ms"]))
            elif scen == "full":
                key = ("gpu" if a["device"] == "gpu" else "cpu") + ("_cold" if r == "0" else "_warm")
                d[cur][key].append(float(a["total_ms"]))
            elif scen == "slider" and "slider" in a:
                d[cur]["slider_" + a["slider"]].append(float(a["median_ms"]))
            elif scen == "export":
                d[cur]["export_cold" if r == "0" else "export_warm"].append(float(a["total_ms"]))
        elif f[0] == "PROC" and cur:
            a = kv(f[1:])
            d[cur]["footprint"].append(float(a["footprint_peak_mb"]))
            d[cur]["load_" + scen].append(float(a["cpu_load"]))
        elif f[0] == "GRAINEO":
            name, text = f[1], f[2]
            if "open" in text:
                o = re.search(r"open (\S+)", text)
                p = re.search(r"preview (\S+)", text)
                s = re.search(r"sliders ([\d.]+)–([\d.]+) ms", text)
                full = re.search(r"full resolution (\S+)", text)
                if o and p:
                    d[name]["g_first"].append(dur_ms(o.group(1)) + dur_ms(p.group(1)))
                if s:
                    d[name]["g_slider"].append(float(s.group(2)))
                if full:
                    d[name]["g_full"].append(dur_ms(full.group(1)))
            elif "peak memory footprint" in text:
                d[name]["g_footprint"].append(int(text.split()[0]) / 1048576)
    cols = [("first", "Apple erstes Bild 2048 px"), ("gpu_cold", "Apple voll, 1. im Prozess"), ("gpu_warm", "Apple voll, warm"),
            ("cpu_cold", "Apple „CPU“ 1."), ("cpu_warm", "Apple „CPU“ warm"), ("slider_exposure", "Regler Belichtung"),
            ("slider_temperature", "Regler WB"), ("slider_luminanceNR", "Regler RM"), ("slider_sharpness", "Regler Schärfe"),
            ("export_cold", "Apple Export 1."), ("export_warm", "Apple Export warm"), ("g_first", "graineo erstes Bild"),
            ("g_slider", "graineo Regler"), ("g_full", "graineo volle Aufl."), ]
    print("| Datei | " + " | ".join(c[1] for c in cols) + " | Apple Spitze Footprint | graineo Spitze Footprint |")
    print("|" + "---|" * (len(cols) + 3))
    for name, v in d.items():
        fp = f"{max(v['footprint']):.0f} MB" if v["footprint"] else "-"
        gfp = f"{statistics.median(v['g_footprint']) / 1024:.1f} GB" if v["g_footprint"] else "-"
        print(f"| {name} | " + " | ".join(fmt(v[c[0]]) for c in cols) + f" | {fp} | {gfp} |")
    print()
    print("CPU load (cpu time / wall time) per scenario, median:")
    for name, v in d.items():
        print(name, " ".join(f"{k[5:]}={statistics.median(x):.2f}" for k, x in v.items() if k.startswith("load_")))


if __name__ == "__main__":
    main(sys.argv[1])

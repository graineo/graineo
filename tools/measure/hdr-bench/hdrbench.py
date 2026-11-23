#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
# Copyright (C) 2026 graineo contributors
"""hdrbench: reproducible test bench for merging raw exposure brackets into HDR.

All data lives below a bench root (``--root`` or ``$HDRBENCH_ROOT``):

    <root>/bench.json            optional: directory names, tool paths, external methods
    <root>/series/<id>/series.json + dev/NN.tif (+ truth.tif, ghost.png for synthetic series)
    <root>/results/<method>/<id>.tif  (+ .json: runtime, memory, kind)
    <root>/sheets/<id>.jpg       labelled 100 % crops (fixed tone mapping)
    <root>/tables/scores.csv|md  metrics

Commands (``hdrbench.py <cmd> -h`` for options):
    add-series   register a real bracket of raw files
    develop      common development of every frame (linear, reference white balance) → dev/
    truth        build a low-noise linear "radiance truth" from a static tripod series
    synth        simulate raw brackets (noise, clipping, quantisation, motion, vignette) from a truth
    run          run a method on series → results/
    ingest       import an externally produced result (float DNG, EXR, TIFF, f32 dump)
    score        metrics for every result → tables/
    sheet        contact sheets → sheets/
    list         show series and methods

Units: every linear result is linear camera RGB multiplied by the reference frame's white balance
(green = 1) and, ideally, scaled so that 1.0 is the clip level of the reference frame. The scorer fits
one global scale factor anyway, so results in other units (e.g. relative to the darkest frame) are fine.
"Display" results (exposure fusion) are scored only with geometry-based metrics.

Third-party programs are run only as external references; no code from them is used here.
"""

import argparse
import hashlib
import json
import math
import os
import re
import shlex
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

import numpy as np

os.environ.setdefault("OPENCV_IO_ENABLE_OPENEXR", "1")
import cv2  # noqa: E402
import tifffile  # noqa: E402

THREADS = int(os.environ.get("HDRBENCH_THREADS", "4"))
cv2.setNumThreads(THREADS)

RAW_EXT = {".cr2", ".cr3", ".nef", ".dng", ".arw", ".raf", ".pef", ".orf", ".rw2", ".srw", ".x3f"}

# --------------------------------------------------------------------------------------------
# layout


class Bench:
    def __init__(self, root):
        self.root = Path(root).resolve()
        cfg = {}
        p = self.root / "bench.json"
        if p.exists():
            cfg = json.loads(p.read_text())
        self.cfg = cfg
        d = {"series": "series", "results": "results", "sheets": "sheets", "tables": "tables"}
        d.update(cfg.get("dirs", {}))
        self.series_dir = self.root / d["series"]
        self.results_dir = self.root / d["results"]
        self.sheets_dir = self.root / d["sheets"]
        self.tables_dir = self.root / d["tables"]
        self.tools = cfg.get("tools", {})
        self.external = cfg.get("methods", {})

    def tool(self, name):
        env = os.environ.get("HDRBENCH_TOOL_" + name.upper().replace("-", "_"))
        return env or self.tools.get(name)

    def series_ids(self):
        if not self.series_dir.exists():
            return []
        return sorted(p.parent.name for p in self.series_dir.glob("*/series.json"))

    def load(self, sid):
        return Series(self, sid)


class Series:
    def __init__(self, bench, sid):
        self.bench = bench
        self.id = sid
        self.dir = bench.series_dir / sid
        self.meta = json.loads((self.dir / "series.json").read_text())

    def save(self):
        (self.dir / "series.json").write_text(json.dumps(self.meta, indent=1, ensure_ascii=False) + "\n")

    def frame_paths(self):
        out = []
        for f in self.meta["frames"]:
            p = Path(f["file"])
            out.append(p if p.is_absolute() else (self.dir / p).resolve())
        return out

    @property
    def ref(self):
        return self.meta["reference"]

    def k(self):
        """exposure of each frame relative to the reference (linear factor)"""
        return [2.0 ** f["ev_rel"] for f in self.meta["frames"]]

    def dev(self, i):
        """developed frame i as float32 HxWx3 in reference-WB units (clip of frame i ≈ wb[c])"""
        d = self.meta["dev"]
        a = tifffile.imread(self.dir / d["files"][i]).astype(np.float32)
        return a * (d["scale"] / 65535.0)

    def truth(self):
        t = self.meta.get("synthetic", {}).get("truth")
        return None if t is None else tifffile.imread(self.dir / t).astype(np.float32)

    def ghost_mask(self):
        g = self.meta.get("synthetic", {}).get("ghost_mask")
        if not g:
            return None
        return cv2.imread(str(self.dir / g), cv2.IMREAD_GRAYSCALE) > 127


# --------------------------------------------------------------------------------------------
# raw reading and the common developer


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()


def exif_of(path):
    import exifread

    with open(path, "rb") as f:
        t = exifread.process_file(f, details=False)

    def num(k):
        v = t.get(k)
        if not v:
            return None
        v = v.values[0] if hasattr(v, "values") and isinstance(v.values, list) else v.values
        try:
            return float(v.num) / float(v.den) if hasattr(v, "num") else float(v)
        except (ZeroDivisionError, ValueError, TypeError):
            return None

    t_exp = num("EXIF ExposureTime") or num("Image ExposureTime")
    fn = num("EXIF FNumber") or num("Image FNumber")
    iso = num("EXIF ISOSpeedRatings") or num("Image ISOSpeedRatings")
    model = str(t.get("Image Model", "")).strip()
    make = str(t.get("Image Make", "")).strip()
    when = str(t.get("EXIF DateTimeOriginal", t.get("Image DateTime", "")))
    return {"exposure_time": t_exp, "fnumber": fn if fn and fn > 0 else None, "iso": iso, "make": make, "model": model, "datetime": when}


def read_mosaic(path):
    """normalised mosaic (0 = black, 1 = white level), 2x2 colour letters, as-shot WB (G = 1)"""
    import rawpy

    with rawpy.imread(str(path)) as r:
        m = r.raw_image_visible.astype(np.float32)
        cols = r.raw_colors_visible.copy()
        desc = r.color_desc.decode()
        black = np.array(r.black_level_per_channel, np.float32)
        white = float(r.camera_white_level_per_channel[0] if r.camera_white_level_per_channel else r.white_level)
        wb = np.array(r.camera_whitebalance[:3], np.float64)
        if r.num_colors != 3 or desc[:3] != "RGB":
            raise SystemExit(f"{path}: only RGB colour filter arrays are supported (got {desc})")
        if r.raw_pattern is None or r.raw_pattern.shape != (2, 2):
            raise SystemExit(f"{path}: only 2x2 Bayer patterns are supported by the common developer")
    m = (m - black[cols]) / (white - black[cols])
    letters = "".join(desc[cols[y, x]] for y in (0, 1) for x in (0, 1))
    wb = wb / wb[1] if wb[1] > 0 else np.ones(3)
    return m, letters, wb


_CV_CODES = {}


def _bayer_code(letters):
    """find the OpenCV edge-aware Bayer code that yields RGB for this 2x2 pattern (determined empirically)"""
    if letters in _CV_CODES:
        return _CV_CODES[letters]
    truth = np.array([10000, 30000, 50000], np.float64)
    m = np.zeros((16, 16), np.uint16)
    idx = {"R": 0, "G": 1, "B": 2}
    for y in range(16):
        for x in range(16):
            m[y, x] = truth[idx[letters[(y % 2) * 2 + (x % 2)]]]
    for name in ("BayerBG2RGB_EA", "BayerGB2RGB_EA", "BayerRG2RGB_EA", "BayerGR2RGB_EA"):
        code = getattr(cv2, "COLOR_" + name)
        out = cv2.cvtColor(m, code)[8, 8].astype(np.float64)
        if np.allclose(out, truth, rtol=0.02):
            _CV_CODES[letters] = code
            return code
    raise SystemExit(f"no OpenCV demosaic code for pattern {letters}")


PEDESTAL = 0.02


def demosaic(m, letters, scale=1.0):
    """edge-aware OpenCV demosaic of a normalised mosaic; values may exceed 1 (use scale)."""
    code = _bayer_code(letters)
    q = np.clip((m / scale + PEDESTAL) / (1.0 + PEDESTAL), 0.0, 1.0)
    q = np.round(q * 65535.0).astype(np.uint16)
    rgb = cv2.cvtColor(q, code).astype(np.float32) / 65535.0
    return (rgb * (1.0 + PEDESTAL) - PEDESTAL) * scale


def luma(img):
    return img[..., 0] * 0.2126 + img[..., 1] * 0.7152 + img[..., 2] * 0.0722


def write_float_tiff(path, img):
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    tifffile.imwrite(path, np.ascontiguousarray(img.astype(np.float32)), photometric="rgb", compression="zlib", predictor=True)


def read_any(path):
    """read a result image: .tif/.tiff (float or int), .exr/.hdr (OpenCV), .f32 (+ .f32.json), .dng (float CFA or LinearRaw)"""
    p = Path(path)
    ext = p.suffix.lower()
    if ext in (".tif", ".tiff"):
        a = tifffile.imread(p)
        if a.dtype == np.uint16:
            return a[..., :3].astype(np.float32) / 65535.0
        if a.dtype == np.uint8:
            return a[..., :3].astype(np.float32) / 255.0
        return a[..., :3].astype(np.float32)
    if ext == ".exr":
        import OpenEXR

        with OpenEXR.File(str(p)) as f:
            ch = f.channels()
            if "RGB" in ch:
                a = ch["RGB"].pixels
            elif "RGBA" in ch:
                a = ch["RGBA"].pixels[..., :3]
            else:
                a = np.dstack([ch[c].pixels for c in ("R", "G", "B")])
        return np.asarray(a, np.float32)
    if ext == ".hdr":
        a = cv2.imread(str(p), cv2.IMREAD_UNCHANGED | cv2.IMREAD_ANYDEPTH)
        if a is None:
            raise SystemExit(f"cannot read {p}")
        return a[..., 2::-1].astype(np.float32)
    if ext == ".f32":
        h = json.loads(Path(str(p) + ".json").read_text())
        return np.fromfile(p, np.float32).reshape(h["h"], h["w"], 3)
    if ext == ".dng":
        return read_float_dng(p)
    raise SystemExit(f"unsupported result format {p}")


def read_float_dng(p, wb=None):
    """develop a float DNG (HDR merge output) with the common developer: CFA → demosaic, LinearRaw → as is"""
    with tifffile.TiffFile(p) as tf:
        cand = [pg for pg in tf.pages] + list(_all_subifds(tf))
        raw = None
        for pg in cand:
            pi = pg.tags.get("PhotometricInterpretation")
            if pi is not None and int(pi.value) in (32803, 34892):
                if raw is None or pg.shape[0] * pg.shape[1] > raw.shape[0] * raw.shape[1]:
                    raw = pg
        if raw is None:
            raise SystemExit(f"{p}: no raw IFD")
        data = raw.asarray().astype(np.float32)
        tags = {t.name: t.value for t in raw.tags.values()}
        ifd0 = {t.name: t.value for t in tf.pages[0].tags.values()}
    black = np.mean(np.atleast_1d(tags.get("BlackLevel", 0))).astype(np.float32)
    white = float(np.atleast_1d(tags.get("WhiteLevel", 1.0))[0])
    if "ActiveArea" in tags:  # CFA pattern and DefaultCrop are relative to the active area (DNG spec)
        t_, l_, b_, r_ = [int(v) for v in np.atleast_1d(tags["ActiveArea"])[:4]]
        data = data[t_:b_, l_:r_]
    data = (data - black) / (white - black)
    neutral = ifd0.get("AsShotNeutral") or tags.get("AsShotNeutral")
    if neutral is not None:
        neutral = np.array(_rat_list(neutral, 3), np.float64)
    pi = int(tags["PhotometricInterpretation"])
    crop = None
    if "DefaultCropOrigin" in tags and "DefaultCropSize" in tags:
        o = [int(round(v)) for v in _rat_list(tags["DefaultCropOrigin"], 2)]
        sz = [int(round(v)) for v in _rat_list(tags["DefaultCropSize"], 2)]
        crop = (o[0], o[1], sz[0], sz[1])
    if pi == 32803:
        pat = tags.get("CFAPattern", b"\x00\x01\x01\x02")
        pat = list(pat) if isinstance(pat, (bytes, bytearray)) else list(np.atleast_1d(pat))
        letters = "".join("RGB"[int(v)] for v in pat[:4])
        top = max(float(np.percentile(data, 99.999)), 1.0) * 1.05
        rgb = demosaic(data, letters, scale=top)
    else:
        rgb = data[..., :3]
    if wb is None and neutral is not None:
        wb = (1.0 / neutral) / (1.0 / neutral[1])
    if wb is not None:
        rgb = rgb * np.asarray(wb, np.float32)[None, None, :]
    if crop is not None:
        x, y, w, h = crop
        rgb = rgb[y : y + h, x : x + w]
    return rgb.astype(np.float32)


def _all_subifds(tf):
    out = []
    for pg in tf.pages:
        try:
            subs = pg.pages
        except AttributeError:
            subs = None
        if subs:
            out.extend(list(subs))
    return out


def _rationals(v):
    """tifffile returns RATIONAL arrays flattened (num, den, num, den, ...) and SHORT/LONG as ints"""
    v = list(np.atleast_1d(v)) if not isinstance(v, (tuple, list)) else list(v)
    return v


def _rat_list(v, n):
    v = _rationals(v)
    if len(v) == 2 * n:
        return [float(v[2 * i]) / float(v[2 * i + 1]) for i in range(n)]
    return [float(x) for x in v[:n]]


# --------------------------------------------------------------------------------------------
# commands: add-series, develop


def cmd_add_series(b, a):
    files = [Path(f).resolve() for f in a.files]
    files = [f for f in files if f.suffix.lower() in RAW_EXT]
    if len(files) < 2:
        raise SystemExit("need at least two raw files")
    frames = []
    for f in files:
        e = exif_of(f)
        exp = (e["exposure_time"] or 1.0) * (e["iso"] or 100.0) / 100.0 / ((e["fnumber"] or 1.0) ** 2)
        frames.append({"file": str(f), "sha256": sha256(f), **e, "exposure": exp})
    frames.sort(key=lambda f: f["exposure"])
    ref = a.reference if a.reference is not None else len(frames) // 2
    for f in frames:
        f["ev_rel"] = math.log2(f["exposure"] / frames[ref]["exposure"])
    d = b.series_dir / a.id
    d.mkdir(parents=True, exist_ok=True)
    meta = {
        "id": a.id,
        "title": a.title or a.id,
        "source": a.source,
        "url": a.url,
        "license": a.license,
        "internal_only": bool(a.internal_only),
        "camera": f"{frames[0]['make']} {frames[0]['model']}".strip(),
        "tags": [t for t in (a.tags or "").split(",") if t],
        "notes": a.notes or "",
        "reference": ref,
        "frames": frames,
    }
    (d / "series.json").write_text(json.dumps(meta, indent=1, ensure_ascii=False) + "\n")
    print(f"{a.id}: {len(frames)} frames, reference {ref} ({Path(frames[ref]['file']).name}), EV " + ", ".join(f"{f['ev_rel']:+.2f}" for f in frames))


def as_shot_wb(path):
    import rawpy

    with rawpy.imread(str(path)) as r:
        wb = np.array(r.camera_whitebalance[:3], np.float64)
    return wb / wb[1] if wb[1] > 0 else np.ones(3)


def develop_libraw(path, wb):
    """fallback developer for mosaics the common developer cannot handle (X-Trans): LibRaw demosaic,
    camera RGB, linear, given WB divided by its maximum (= the common developer's 16-bit scaling)"""
    import rawpy

    with rawpy.imread(str(path)) as r:
        mul = (wb / max(wb)).tolist()
        return r.postprocess(gamma=(1, 1), no_auto_bright=True, output_bps=16, use_camera_wb=False, user_wb=[mul[0], mul[1], mul[2], mul[1]], output_color=rawpy.ColorSpace.raw, user_flip=0, highlight_mode=rawpy.HighlightMode.Ignore)


def cmd_develop(b, a):
    for sid in a.series or b.series_ids():
        s = b.load(sid)
        if s.meta.get("dev") and not a.force:
            print(f"{sid}: developed already")
            continue
        paths = s.frame_paths()
        wb = as_shot_wb(paths[s.ref])
        scale = float(max(wb)) * (1.0 + 1e-6)
        files = []
        (s.dir / "dev").mkdir(exist_ok=True)
        for i, p in enumerate(paths):
            try:
                m, letters, _ = read_mosaic(p)
                rgb = demosaic(m, letters) * wb[None, None, :].astype(np.float32)
                q = np.round(np.clip(rgb / scale, 0, 1) * 65535).astype(np.uint16)
            except SystemExit:
                # non-2x2 mosaics (X-Trans): LibRaw's own demosaic, linear, reference WB scaled by 1/max(WB)
                q, letters = develop_libraw(p, wb), "libraw"
            name = f"dev/{i:02d}.tif"
            tifffile.imwrite(s.dir / name, q, photometric="rgb", compression="zlib", predictor=True)
            files.append(name)
            print(f"{sid}: {p.name} → {name} {q.shape[1]}x{q.shape[0]} pattern {letters}")
        s.meta["dev"] = {"files": files, "wb": [float(v) for v in wb], "scale": scale, "developer": "rawpy/LibRaw mosaic, OpenCV edge-aware demosaic, reference as-shot WB, linear"}
        s.meta["size"] = [int(q.shape[1]), int(q.shape[0])]
        s.save()
        auto_crops(s)


# --------------------------------------------------------------------------------------------
# truth and synthetic brackets


def noise_merge(frames, ks, clips, a_noise=1e-4, b_noise=1e-7):
    """simple noise-optimal merge of perfectly registered developed frames (used only to build a truth)"""
    num = np.zeros_like(frames[0], np.float64)
    den = np.zeros(frames[0].shape[:2], np.float64)
    order = np.argsort(ks)[::-1]
    pilot = None
    for i in order:
        f, k = frames[i], ks[i]
        mx = (f / clips[None, None, :]).max(axis=2)
        mx = cv2.dilate(mx.astype(np.float32), np.ones((5, 5), np.uint8))
        sat = np.clip((0.92 - mx) / 0.17, 0, 1)
        g = luma(f) / k
        if pilot is None:
            pilot = g.copy()
        else:
            pilot = np.where(den > 0, num[..., 1] / np.maximum(den, 1e-30), g)
        s = np.maximum(k * pilot, 0)
        w = sat * k * k / (a_noise * s + b_noise)
        if i == order[-1]:
            w = np.where(den + w == 0, 1e-30, w)
        num += w[..., None] * f / k
        den += w
    return (num / np.maximum(den, 1e-300)[..., None]).astype(np.float32)


def cmd_truth(b, a):
    s = b.load(a.series)
    n = len(s.meta["frames"])
    ks = np.array(s.k())
    wb = np.array(s.meta["dev"]["wb"], np.float32)
    frames = [s.dev(i) for i in range(n)]
    clips = wb
    t = noise_merge(frames, ks, clips)
    del frames
    f = a.downscale
    h, w = (t.shape[0] // f) * f, (t.shape[1] // f) * f
    t = t[:h, :w].reshape(h // f, f, w // f, f, 3).mean(axis=(1, 3))
    if a.crop:
        x, y, cw, ch = [int(v) for v in a.crop.split(",")]
        t = t[y : y + ch, x : x + cw]
    t = np.maximum(t, 0)
    out = Path(a.out)
    write_float_tiff(out, t)
    Y = luma(t)
    print(f"truth {out}: {t.shape[1]}x{t.shape[0]}, luma p0.1 {np.percentile(Y, 0.1):.2e} p50 {np.median(Y):.3e} p99.9 {np.percentile(Y, 99.9):.3f} max {Y.max():.2f} → DR(p99.9/p0.1) {math.log2(np.percentile(Y, 99.9) / max(np.percentile(Y, 0.1), 1e-9)):.1f} EV")


def similarity_maps(w, h, tx, ty, rot_deg, scale):
    """sampling maps: frame pixel (x, y) shows the scene at the returned map coordinates"""
    cx, cy = w / 2.0, h / 2.0
    th = math.radians(rot_deg)
    c, s_ = math.cos(th) * scale, math.sin(th) * scale
    ys, xs = np.mgrid[0:h, 0:w].astype(np.float32)
    u, v = xs - cx, ys - cy
    mx = cx + c * u - s_ * v + tx
    my = cy + s_ * u + c * v + ty
    return mx.astype(np.float32), my.astype(np.float32)


def write_cfa_dng(path, mosaic_dn, black, white, wb, exposure_time, iso, fnumber, model="Synthetic Bayer"):
    """minimal uncompressed 16-bit CFA DNG (RGGB) that LibRaw and rawler can read"""
    h, w = mosaic_dn.shape
    neutral = [(int(round(1e6 / wb[0])), 1000000), (1000000, 1000000), (int(round(1e6 / wb[2])), 1000000)]
    eye = [(1, 1), (0, 1), (0, 1), (0, 1), (1, 1), (0, 1), (0, 1), (0, 1), (1, 1)]
    srgb_to_xyz_inv = [(32404542, 10000000), (-15371385, 10000000), (-4985314, 10000000), (-9692660, 10000000), (18760108, 10000000), (415560, 10000000), (556434, 10000000), (-2040259, 10000000), (10572252, 10000000)]
    et = (int(round(exposure_time * 1e6)), 1000000) if exposure_time < 1 else (int(round(exposure_time * 1000)), 1000)
    extratags = [
        (254, "I", 1, 0, True),
        (271, "s", 0, "graineo-bench", True),
        (272, "s", 0, model, True),
        (274, "H", 1, 1, True),
        (33421, "H", 2, (2, 2), True),
        (33422, "B", 4, (0, 1, 1, 2), True),
        (50706, "B", 4, (1, 4, 0, 0), True),
        (50707, "B", 4, (1, 1, 0, 0), True),
        (50708, "s", 0, model, True),
        (50710, "B", 3, (0, 1, 2), True),
        (50711, "H", 1, 1, True),
        (50713, "H", 2, (2, 2), True),
        (50714, "H", 4, (black,) * 4, True),
        (50717, "H", 1, white, True),
        (50721, "2i", 9, [v for p in srgb_to_xyz_inv for v in p], True),
        (50778, "H", 1, 21, True),
        (50728, "2I", 3, [v for p in neutral for v in p], True),
        (33434, "2I", 1, et, True),
        (33437, "2I", 1, (int(round(fnumber * 100)), 100), True),
        (34855, "H", 1, int(iso), True),
    ]
    del eye
    tifffile.imwrite(path, mosaic_dn.astype(np.uint16), photometric=32803, planarconfig=None, extratags=extratags, metadata=None, compression=None)


def cmd_synth(b, a):
    """simulate a raw bracket from a truth image (camera RGB, reference WB applied, 1.0 = reference clip)"""
    rng = np.random.default_rng(a.seed)
    truth_src = tifffile.imread(a.truth).astype(np.float32)
    wb = np.array([float(v) for v in a.wb.split(",")], np.float32)
    h, w = truth_src.shape[:2]
    h, w = h - h % 2, w - w % 2
    truth = truth_src[:h, :w]
    if a.contrast != 1.0:  # expand the scene's dynamic range around its median luma (keeps chromaticity)
        Y = np.maximum(luma(truth), 1e-9)
        med = float(np.median(Y))
        truth = truth * ((Y / med) ** (a.contrast - 1.0))[..., None]
    truth = (truth * np.float32(a.gain)).astype(np.float32)
    evs = [float(v) for v in a.evs.split(",")]
    n = len(evs)
    ref = evs.index(0.0) if 0.0 in evs else n // 2
    motions = []
    for i in range(n):
        if i == ref or a.shift == 0:
            motions.append((0.0, 0.0, 0.0, 1.0))
        else:
            r = rng.uniform(-1, 1, 4)
            motions.append((r[0] * a.shift, r[1] * a.shift, r[2] * a.rot, 1.0 + r[3] * a.scale))
    # moving objects: a textured patch cut from the truth, pasted at a different place in every frame
    objects = []
    ghost = np.zeros((h, w), np.uint8)
    if a.objects:
        for _ in range(a.objects):
            ow, oh = int(w * 0.12), int(h * 0.12)
            sx, sy = int(rng.uniform(0.05, 0.8) * w), int(rng.uniform(0.05, 0.8) * h)
            patch = truth[sy : sy + oh, sx : sx + ow].copy() * np.float32(rng.choice([0.25, 4.0]))
            yy, xx = np.mgrid[0:oh, 0:ow]
            alpha = np.clip(np.minimum.reduce([xx, yy, ow - 1 - xx, oh - 1 - yy]) / 8.0, 0, 1).astype(np.float32)
            x0, y0 = rng.uniform(0.1, 0.7) * w, rng.uniform(0.1, 0.7) * h
            dx, dy = rng.uniform(-1, 1, 2) * w * 0.06
            objects.append((patch, alpha, x0, y0, dx, dy))

    def place(img, i, mask=None):
        for patch, alpha, x0, y0, dx, dy in objects:
            px, py = int(x0 + dx * (i - ref)), int(y0 + dy * (i - ref))
            oh, ow = alpha.shape
            img[py : py + oh, px : px + ow] = img[py : py + oh, px : px + ow] * (1 - alpha[..., None]) + patch * alpha[..., None]
            if mask is not None:
                mask[py : py + oh, px : px + ow] = np.maximum(mask[py : py + oh, px : px + ow], (alpha > 0).astype(np.uint8) * 255)

    scene_ref = truth.copy()
    place(scene_ref, ref)
    yy, xx = np.mgrid[0:h, 0:w].astype(np.float32)
    r2 = ((xx - w / 2) ** 2 + (yy - h / 2) ** 2) / ((w / 2) ** 2 + (h / 2) ** 2)
    vign = (1.0 - a.vignette * r2).astype(np.float32)
    truth_out = scene_ref * vign[..., None]
    sid = a.id
    d = b.series_dir / sid
    (d / "raw").mkdir(parents=True, exist_ok=True)
    bits = a.bits
    white = 2**bits - 1
    black = 2 ** (bits - 6) if bits >= 12 else 16
    frames = []
    for i, ev in enumerate(evs):
        scene = truth.copy()
        place(scene, i, ghost if i != ref else None)
        tx, ty, rot, sc = motions[i]
        if (tx, ty, rot, sc) != (0.0, 0.0, 0.0, 1.0):
            mx, my = similarity_maps(w, h, tx, ty, rot, sc)
            scene = cv2.remap(scene, mx, my, cv2.INTER_LANCZOS4, borderMode=cv2.BORDER_REFLECT)
        k = 2.0**ev
        cam = scene * vign[..., None] * np.float32(k) / wb[None, None, :]
        mos = np.empty((h, w), np.float32)
        mos[0::2, 0::2] = cam[0::2, 0::2, 0]
        mos[0::2, 1::2] = cam[0::2, 1::2, 1]
        mos[1::2, 0::2] = cam[1::2, 0::2, 1]
        mos[1::2, 1::2] = cam[1::2, 1::2, 2]
        mos = np.maximum(mos, 0)
        var = a.noise_a * mos + a.noise_b
        mos = mos + rng.standard_normal(mos.shape, dtype=np.float32) * np.sqrt(var).astype(np.float32)
        dn = np.clip(np.round(black + mos * (white - black)), 0, white).astype(np.uint16)
        t_exp = a.base_time * k
        name = f"raw/F{i:02d}.dng"
        write_cfa_dng(d / name, dn, black, white, wb, t_exp, a.iso, 8.0)
        frames.append({"file": name, "sha256": sha256(d / name), "exposure_time": t_exp, "fnumber": 8.0, "iso": a.iso, "make": "graineo-bench", "model": "Synthetic Bayer", "exposure": t_exp * a.iso / 100 / 64, "ev_rel": ev, "motion": {"tx": tx, "ty": ty, "rot_deg": rot, "scale": sc}})
        print(f"{sid}: frame {i} EV {ev:+.1f} motion ({tx:+.2f},{ty:+.2f}) px rot {rot:+.3f}° scale {sc:.5f}, clipped {(dn >= white).mean() * 100:.1f} %")
    write_float_tiff(d / "truth.tif", truth_out)
    syn = {"truth": "truth.tif", "source_truth": str(a.truth), "gain": a.gain, "contrast": a.contrast, "noise_a": a.noise_a, "noise_b": a.noise_b, "bits": bits, "shift_px": a.shift, "rot_deg": a.rot, "scale": a.scale, "vignette": a.vignette, "objects": a.objects, "seed": a.seed}
    if a.objects:
        ghost = cv2.dilate(ghost, np.ones((9, 9), np.uint8))
        cv2.imwrite(str(d / "ghost.png"), ghost)
        syn["ghost_mask"] = "ghost.png"
    meta = {
        "id": sid,
        "title": a.title or sid,
        "source": f"synthetic from {Path(a.truth).name}",
        "url": "",
        "license": a.license,
        "internal_only": bool(a.internal_only),
        "camera": "Synthetic Bayer (RGGB)",
        "tags": [t for t in (a.tags or "").split(",") if t],
        "notes": a.notes or "",
        "reference": ref,
        "frames": frames,
        "synthetic": syn,
    }
    (d / "series.json").write_text(json.dumps(meta, indent=1, ensure_ascii=False) + "\n")


# --------------------------------------------------------------------------------------------
# methods


def timed(cmd, env=None, cwd=None, log=None):
    """run cmd under /usr/bin/time -l; return wall seconds and peak RSS (MB)"""
    full = ["/usr/bin/time", "-l"] + [str(c) for c in cmd]
    e = dict(os.environ)
    e.update({"OMP_NUM_THREADS": str(THREADS), "RAYON_NUM_THREADS": str(THREADS)})
    if env:
        e.update(env)
    t0 = time.time()
    p = subprocess.run(full, env=e, cwd=cwd, capture_output=True, text=True)
    wall = time.time() - t0
    m = re.search(r"(\d+)\s+maximum resident set size", p.stderr)
    rss = int(m.group(1)) / 1e6 if m else float("nan")
    if log is not None:
        log.write(f"$ {shlex.join(str(c) for c in cmd)}\n{p.stdout[-4000:]}\n{p.stderr[-4000:]}\n")
    if log is not None:
        log.flush()
    if p.returncode != 0:
        err = "\n".join(l for l in p.stderr.splitlines() if not re.match(r"^\s+\d+\s+[a-z]", l) and "real" not in l)
        raise RuntimeError(f"{Path(str(cmd[0])).name} failed ({p.returncode}): {err[-1500:]}")
    return wall, rss, p.stdout


def srgb_encode(x):
    x = np.clip(x, 0, 1)
    return np.where(x <= 0.0031308, 12.92 * x, 1.055 * np.power(x, 1 / 2.4) - 0.055)


def srgb_decode(y):
    return np.where(y <= 0.04045, y / 12.92, np.power((y + 0.055) / 1.055, 2.4))


def frames_8bit(s):
    """developed frames as 8-bit sRGB-encoded BGR images (input for the OpenCV HDR module)"""
    sc = s.meta["dev"]["scale"]
    out = []
    for i in range(len(s.meta["frames"])):
        f = s.dev(i) / sc
        out.append(np.round(srgb_encode(f) * 255).astype(np.uint8)[..., ::-1].copy())
    return out


def linear_response():
    z = np.arange(256) / 255.0
    r = srgb_decode(z)
    r[0] = r[1] * 0.5
    return np.repeat(r.astype(np.float32)[:, None, None], 3, axis=2)


def method_ref(s, tmp):
    return s.dev(s.ref), "linear", {}


def method_naive_mean(s, tmp):
    ks = s.k()
    wb = np.array(s.meta["dev"]["wb"], np.float32)
    num = None
    cnt = None
    darkest = int(np.argmin(ks))
    for i, k in enumerate(ks):
        f = s.dev(i)
        ok = ((f / wb[None, None, :]).max(axis=2) < 0.95).astype(np.float32)
        if i == darkest:
            ok = np.ones_like(ok) * 1e-6 + ok
        num = f * ok[..., None] / k if num is None else num + f * ok[..., None] / k
        cnt = ok if cnt is None else cnt + ok
    return num / np.maximum(cnt, 1e-9)[..., None], "linear", {"note": "exposure-normalised mean of unclipped frames, EXIF factors, no alignment"}


def method_naive_brightest(s, tmp):
    ks = s.k()
    wb = np.array(s.meta["dev"]["wb"], np.float32)
    out = None
    for i in np.argsort(ks):  # dark → bright, brighter unclipped frames overwrite
        f = s.dev(i)
        ok = (f / wb[None, None, :]).max(axis=2) < 0.95
        out = f / ks[i] if out is None else np.where(ok[..., None], f / ks[i], out)
    return out, "linear", {"note": "per pixel the brightest unclipped frame, EXIF factors, no alignment"}


def _cv_align(imgs):
    al = cv2.createAlignMTB()
    out = [im.copy() for im in imgs]
    al.process(imgs, out)
    return out


def _times(s):
    return np.array(s.k(), np.float32)


def method_opencv_debevec(s, tmp):
    imgs = _cv_align(frames_8bit(s))
    hdr = cv2.createMergeDebevec().process(imgs, _times(s), linear_response())
    return hdr[..., ::-1] * np.float32(s.meta["dev"]["scale"]), "linear", {"note": "AlignMTB + MergeDebevec, known (linearising) response"}


def method_opencv_debevec_cal(s, tmp):
    imgs = _cv_align(frames_8bit(s))
    t = _times(s)
    resp = cv2.createCalibrateDebevec().process(imgs, t)
    hdr = cv2.createMergeDebevec().process(imgs, t, resp)
    return hdr[..., ::-1], "linear", {"note": "AlignMTB + CalibrateDebevec + MergeDebevec (response estimated from the images)"}


def method_opencv_robertson(s, tmp):
    imgs = _cv_align(frames_8bit(s))
    hdr = cv2.createMergeRobertson().process(imgs, _times(s), linear_response())
    return hdr[..., ::-1] * np.float32(s.meta["dev"]["scale"]), "linear", {"note": "AlignMTB + MergeRobertson, known (linearising) response"}


def method_opencv_mertens(s, tmp):
    imgs = _cv_align(frames_8bit(s))
    fus = cv2.createMergeMertens().process(imgs)
    return np.clip(fus[..., ::-1], 0, None), "display", {"note": "AlignMTB + MergeMertens exposure fusion (display-referred)"}


def _write_gamma16(s, d):
    sc = s.meta["dev"]["scale"]
    paths = []
    for i in range(len(s.meta["frames"])):
        p = Path(d) / f"in{i:02d}.tif"
        g = np.round(srgb_encode(s.dev(i) / sc) * 65535).astype(np.uint16)
        tifffile.imwrite(p, g, photometric="rgb")
        paths.append(p)
    return paths


def method_enfuse(s, tmp):
    ais, enf = s.bench.tool("align_image_stack"), s.bench.tool("enfuse")
    if not ais or not enf:
        raise RuntimeError("tools align_image_stack/enfuse not configured")
    paths = _write_gamma16(s, tmp)
    log = open(Path(tmp) / "log.txt", "w")
    w1, r1, _ = timed([ais, "--use-given-order", "-a", Path(tmp) / "al_"] + paths, log=log)
    al = sorted(Path(tmp).glob("al_*.tif"))
    w2, r2, _ = timed([enf, "-o", Path(tmp) / "fused.tif", "--depth=16", "--compression=none"] + al, log=log)
    out = tifffile.imread(Path(tmp) / "fused.tif")[..., :3].astype(np.float32) / 65535.0
    return out, "display", {"note": "align_image_stack (control points) + enfuse (exposure fusion), 16-bit sRGB-encoded input", "tool_seconds": w1 + w2, "tool_rss_mb": max(r1, r2)}


def method_hugin_hdr(s, tmp):
    """align_image_stack alignment + hugin_hdrmerge Khan deghosting on exposure-normalised linear float frames"""
    ais, hm = s.bench.tool("align_image_stack"), s.bench.tool("hugin_hdrmerge")
    if not ais or not hm:
        raise RuntimeError("tools align_image_stack/hugin_hdrmerge not configured")
    ks = s.k()
    wb = np.array(s.meta["dev"]["wb"], np.float32)
    paths = []
    for i in range(len(ks)):
        f = s.dev(i)
        alpha = ((f / wb[None, None, :]).max(axis=2) < 0.95).astype(np.float32)
        if i == int(np.argmin(ks)):
            alpha[:] = 1
        rgba = np.dstack([f / ks[i], alpha])
        p = Path(tmp) / f"lin{i:02d}.tif"
        tifffile.imwrite(p, rgba.astype(np.float32), photometric="rgb", extrasamples=[2])
        paths.append(p)
    log = open(Path(tmp) / "log.txt", "w")
    w1, r1, _ = timed([ais, "-l", "--use-given-order", "-a", Path(tmp) / "al_"] + paths, log=log)
    al = sorted(Path(tmp).glob("al_*.tif"))
    w2, r2, _ = timed([hm, "-m", "khan", "-o", Path(tmp) / "merged.exr"] + al, log=log)
    out = read_any(Path(tmp) / "merged.exr")
    return out, "linear", {"note": "align_image_stack -l + hugin_hdrmerge -m khan on exposure-normalised linear frames (clipped pixels transparent)", "tool_seconds": w1 + w2, "tool_rss_mb": max(r1, r2)}


def method_hdrmerge(s, tmp):
    hm = s.bench.tool("hdrmerge")
    if not hm:
        raise RuntimeError("tool hdrmerge not configured")
    out = Path(tmp) / "merged.dng"
    log = open(Path(tmp) / "log.txt", "w")
    w, r, _ = timed([hm, "-o", out, "-b", "32", "--no-crop", "-p", "none"] + [str(p) for p in s.frame_paths()], log=log)
    img = read_float_dng(out, wb=np.array(s.meta["dev"]["wb"]))
    return img, "linear", {"note": "HDRMerge 0.5.0 CLI (translation alignment, own masks), float CFA DNG developed with the common developer", "tool_seconds": w, "tool_rss_mb": r}


def method_prototype(s, tmp):
    exe = s.bench.tool("prototype")
    if not exe:
        raise RuntimeError("tool prototype not configured")
    out = Path(tmp) / "merged.f32"
    log = open(Path(tmp) / "log.txt", "w")
    w, r, so = timed([exe] + [str(p) for p in s.frame_paths()], env={"OUT_F32": str(out), "OUT": str(tmp)}, log=log)
    img = read_any(out)
    hdr = json.loads(Path(str(out) + ".json").read_text())
    wb_p = np.array(hdr["wb"], np.float32)
    wb_d = np.array(s.meta["dev"]["wb"], np.float32)
    img = img * (wb_d / wb_p)[None, None, :]
    m = re.search(r"total wall ([\d.]+) s", so)
    return img, "linear", {"note": "graineo prototype v0 (rawler decode, RCD, similarity alignment, noise-optimal weights, no ghost handling)", "tool_seconds": w, "tool_rss_mb": r, "core_seconds": float(m.group(1)) if m else None, "reference_frame_by_tool": hdr.get("ref")}


def method_luminance(s, tmp):
    exe = s.bench.tool("luminance")
    if not exe:
        raise RuntimeError("tool luminance not configured")
    sc = s.meta["dev"]["scale"]
    paths = []
    for i in range(len(s.meta["frames"])):
        p = Path(tmp) / f"in{i:02d}.tif"
        tifffile.imwrite(p, np.round(srgb_encode(s.dev(i) / sc) * 255).astype(np.uint8), photometric="rgb")
        paths.append(p)
    ev = ",".join(f"{f['ev_rel']:.4f}" for f in s.meta["frames"])
    out = Path(tmp) / "merged.exr"
    log = open(Path(tmp) / "log.txt", "w")
    w, r, _ = timed([exe, "-e", ev, "--hdrWeight", "triangular", "--hdrResponseCurve", "srgb", "--hdrModel", "debevec", "-a", "MTB", "-s", out] + paths, log=log)
    return read_any(out), "linear", {"note": "luminance-hdr-cli: MTB + Debevec model, sRGB response, triangular weights, 8-bit sRGB input", "tool_seconds": w, "tool_rss_mb": r}


METHODS = {
    "ref": method_ref,
    "naive-mean": method_naive_mean,
    "naive-brightest": method_naive_brightest,
    "opencv-debevec": method_opencv_debevec,
    "opencv-debevec-cal": method_opencv_debevec_cal,
    "opencv-robertson": method_opencv_robertson,
    "opencv-mertens": method_opencv_mertens,
    "enfuse": method_enfuse,
    "hugin-khan": method_hugin_hdr,
    "hdrmerge": method_hdrmerge,
    "luminance": method_luminance,
    "prototype-v0": method_prototype,
}


def method_external(s, tmp, spec):
    """external method from bench.json: {"cmd": "... {frames} ... {out}", "output": "out.tif", "kind": "linear"}"""
    out = Path(tmp) / spec.get("output", "out.tif")
    fmt = {"frames": " ".join(shlex.quote(str(p)) for p in s.frame_paths()), "out": shlex.quote(str(out)), "ref": str(s.ref), "series_json": shlex.quote(str(s.dir / "series.json")), "tmp": shlex.quote(str(tmp)), "dev": " ".join(shlex.quote(str(s.dir / f)) for f in s.meta["dev"]["files"])}
    cmd = shlex.split(spec["cmd"].format(**fmt))
    log = open(Path(tmp) / "log.txt", "w")
    w, r, so = timed(cmd, env=spec.get("env"), log=log)
    img = read_any(out)
    side = Path(str(out) + ".json")
    if out.suffix == ".f32" and side.exists() and spec.get("kind", "linear") == "linear":
        # result in camera RGB × the tool's reference white balance: convert to the developer's balance
        hdr = json.loads(side.read_text())
        if "wb" in hdr:
            img = img * (np.array(s.meta["dev"]["wb"], np.float32) / np.array(hdr["wb"], np.float32))[None, None, :]
    if spec.get("wb_from_dev", False):
        img = img * np.array(s.meta["dev"]["wb"], np.float32)[None, None, :]
    if spec.get("encode") == "srgb":  # display result written as linear values: store it sRGB-encoded like other fusion tools
        img = srgb_encode(img).astype(np.float32)
    return img, spec.get("kind", "linear"), {"note": spec.get("note", ""), "tool_seconds": w, "tool_rss_mb": r}


def run_one(b, method, sid, child=False):
    s = b.load(sid)
    if "dev" not in s.meta:
        raise SystemExit(f"{sid}: run 'develop' first")
    outdir = b.results_dir / method
    outdir.mkdir(parents=True, exist_ok=True)
    res = outdir / f"{sid}.tif"
    with tempfile.TemporaryDirectory(prefix="hdrbench-") as tmp:
        t0 = time.time()
        try:
            if method in METHODS:
                img, kind, extra = METHODS[method](s, tmp)
            elif method in b.external:
                img, kind, extra = method_external(s, tmp, b.external[method])
            else:
                raise SystemExit(f"unknown method {method}")
        finally:
            log = Path(tmp) / "log.txt"
            if log.exists():
                shutil.copy(log, outdir / f"{sid}.log")
        wall = time.time() - t0
    img = np.nan_to_num(img.astype(np.float32), nan=0.0, posinf=0.0, neginf=0.0)
    write_float_tiff(res, img)
    info = {"method": method, "series": sid, "kind": kind, "seconds_in_process": round(wall, 3), "size": [int(img.shape[1]), int(img.shape[0])], **extra}
    (outdir / f"{sid}.json").write_text(json.dumps(info, indent=1) + "\n")
    return info


def cmd_run(b, a):
    methods = a.method.split(",") if a.method != "all" else list(METHODS) + list(b.external)
    sids = a.series or b.series_ids()
    for m in methods:
        for sid in sids:
            if not a.force and (b.results_dir / m / f"{sid}.tif").exists():
                print(f"{m} {sid}: exists")
                continue
            # each run in a child process to measure wall time and peak memory of the whole method
            cmd = [sys.executable, "-I", os.path.abspath(__file__), "--root", str(b.root), "_run1", m, sid]
            try:
                wall, rss, so = timed(cmd)
            except RuntimeError as e:
                print(f"{m} {sid}: FAILED {str(e)[-600:]}")
                err = b.results_dir / m / f"{sid}.error.txt"
                err.parent.mkdir(parents=True, exist_ok=True)
                err.write_text(str(e))
                continue
            j = b.results_dir / m / f"{sid}.json"
            info = json.loads(j.read_text())
            info["wall_seconds"] = round(wall, 2)
            info["peak_rss_mb"] = round(rss, 0)
            j.write_text(json.dumps(info, indent=1) + "\n")
            print(f"{m} {sid}: {wall:.1f} s, {rss:.0f} MB")


def cmd_run1(b, a):
    run_one(b, a.method, a.sid, child=True)


def cmd_ingest(b, a):
    s = b.load(a.series)
    img = read_any(a.file) if Path(a.file).suffix.lower() != ".dng" else read_float_dng(a.file)
    outdir = b.results_dir / a.method
    write_float_tiff(outdir / f"{a.series}.tif", img)
    info = {"method": a.method, "series": a.series, "kind": a.kind, "size": [int(img.shape[1]), int(img.shape[0])], "note": a.note or f"ingested from {Path(a.file).name}", "wall_seconds": a.seconds}
    (outdir / f"{a.series}.json").write_text(json.dumps(info, indent=1) + "\n")
    print(f"ingested {a.file} as {a.method}/{a.series}")


# --------------------------------------------------------------------------------------------
# geometry: bring a result into the target geometry


def _prep_reg(img, f):
    y = np.log2(np.maximum(luma(img), 1e-6)).astype(np.float32)
    y = cv2.resize(y, (img.shape[1] // f, img.shape[0] // f), interpolation=cv2.INTER_AREA)
    y = y - cv2.GaussianBlur(y, (0, 0), 8)
    return y


def register(result, target):
    """integer translation that maps result onto target (result may be larger/smaller, e.g. other crop)"""
    f = 4
    rt, tt = _prep_reg(result, f), _prep_reg(target, f)
    th, tw = tt.shape
    tpl = tt[th // 4 : th * 3 // 4, tw // 4 : tw * 3 // 4]
    pad = max(0, tpl.shape[0] - rt.shape[0] + 8, tpl.shape[1] - rt.shape[1] + 8)
    if pad:
        rt = np.pad(rt, pad)
    mm = cv2.matchTemplate(rt, tpl, cv2.TM_CCOEFF_NORMED)
    _, score, _, loc = cv2.minMaxLoc(mm)
    ox = (loc[0] - pad - tw // 4) * f
    oy = (loc[1] - pad - th // 4) * f
    # refine at full resolution on a central crop
    best = (ox, oy)
    c = 512
    cy, cx = target.shape[0] // 2, target.shape[1] // 2
    T = np.log2(np.maximum(luma(target[cy - c : cy + c, cx - c : cx + c]), 1e-6)).astype(np.float32)
    ry, rx = cy + oy, cx + ox
    if 0 <= ry - c - 8 and ry + c + 8 <= result.shape[0] and 0 <= rx - c - 8 and rx + c + 8 <= result.shape[1]:
        R = np.log2(np.maximum(luma(result[ry - c - 8 : ry + c + 8, rx - c - 8 : rx + c + 8]), 1e-6)).astype(np.float32)
        mm2 = cv2.matchTemplate(R, T, cv2.TM_CCOEFF_NORMED)
        _, _, _, l2 = cv2.minMaxLoc(mm2)
        best = (ox + l2[0] - 8, oy + l2[1] - 8)
    return best, float(score)


def into_target(result, target, force=None):
    (ox, oy), score = register(result, target) if force is None else (force, 1.0)
    H, W = target.shape[:2]
    out = np.full((H, W, 3), np.nan, np.float32)
    ys0, xs0 = max(0, oy), max(0, ox)
    ys1, xs1 = min(result.shape[0], oy + H), min(result.shape[1], ox + W)
    out[ys0 - oy : ys1 - oy, xs0 - ox : xs1 - ox] = result[ys0:ys1, xs0:xs1]
    return out, (ox, oy), score


# --------------------------------------------------------------------------------------------
# metrics


def lab_from_lin(rgb, white):
    """linear (approx. sRGB primaries) → CIELAB D65, white = linear value mapped to L = 100"""
    M = np.array([[0.4124, 0.3576, 0.1805], [0.2126, 0.7152, 0.0722], [0.0193, 0.1192, 0.9505]], np.float32)
    xyz = (rgb / white) @ M.T
    wn = np.array([0.95047, 1.0, 1.08883], np.float32)
    t = xyz / wn
    d = 6 / 29
    ft = np.where(t > d**3, np.cbrt(np.maximum(t, 0)), t / (3 * d * d) + 4 / 29)
    L = 116 * ft[..., 1] - 16
    A = 500 * (ft[..., 0] - ft[..., 1])
    B = 200 * (ft[..., 1] - ft[..., 2])
    return L, A, B


def delta_e00(l1, l2):
    L1, a1, b1 = l1
    L2, a2, b2 = l2
    C1, C2 = np.hypot(a1, b1), np.hypot(a2, b2)
    Cm = (C1 + C2) / 2
    G = 0.5 * (1 - np.sqrt(Cm**7 / (Cm**7 + 25.0**7)))
    a1p, a2p = (1 + G) * a1, (1 + G) * a2
    C1p, C2p = np.hypot(a1p, b1), np.hypot(a2p, b2)
    h1p = np.degrees(np.arctan2(b1, a1p)) % 360
    h2p = np.degrees(np.arctan2(b2, a2p)) % 360
    dLp = L2 - L1
    dCp = C2p - C1p
    dh = h2p - h1p
    dh = np.where(dh > 180, dh - 360, np.where(dh < -180, dh + 360, dh))
    dh = np.where(C1p * C2p == 0, 0, dh)
    dHp = 2 * np.sqrt(C1p * C2p) * np.sin(np.radians(dh / 2))
    Lm = (L1 + L2) / 2
    Cmp = (C1p + C2p) / 2
    hs = h1p + h2p
    hm = np.where(C1p * C2p == 0, hs, np.where(np.abs(h1p - h2p) > 180, np.where(hs < 360, (hs + 360) / 2, (hs - 360) / 2), hs / 2))
    T = 1 - 0.17 * np.cos(np.radians(hm - 30)) + 0.24 * np.cos(np.radians(2 * hm)) + 0.32 * np.cos(np.radians(3 * hm + 6)) - 0.20 * np.cos(np.radians(4 * hm - 63))
    dth = 30 * np.exp(-(((hm - 275) / 25) ** 2))
    Rc = 2 * np.sqrt(Cmp**7 / (Cmp**7 + 25.0**7))
    Sl = 1 + 0.015 * (Lm - 50) ** 2 / np.sqrt(20 + (Lm - 50) ** 2)
    Sc = 1 + 0.045 * Cmp
    Sh = 1 + 0.015 * Cmp * T
    Rt = -np.sin(np.radians(2 * dth)) * Rc
    return np.sqrt((dLp / Sl) ** 2 + (dCp / Sc) ** 2 + (dHp / Sh) ** 2 + Rt * (dCp / Sc) * (dHp / Sh))


def ssim_map(x, y):
    C1, C2 = 0.01**2, 0.03**2
    g = lambda z: cv2.GaussianBlur(z, (11, 11), 1.5)  # noqa: E731
    mx, my = g(x), g(y)
    sxx, syy, sxy = g(x * x) - mx * mx, g(y * y) - my * my, g(x * y) - mx * my
    cs = (2 * sxy + C2) / (sxx + syy + C2)
    l = (2 * mx * my + C1) / (mx * mx + my * my + C1)
    return l, cs


def ms_ssim(x, y, valid):
    """MS-SSIM (Wang 2003, 5 scales) of two images in [0, 1], invalid pixels replaced by the target"""
    x = np.where(valid, x, y).astype(np.float32)
    y = y.astype(np.float32)
    w = [0.0448, 0.2856, 0.3001, 0.2363, 0.1333]
    vals = []
    for i in range(5):
        l, cs = ssim_map(x, y)
        m = cv2.resize(valid.astype(np.uint8), (x.shape[1], x.shape[0]), interpolation=cv2.INTER_NEAREST) > 0
        if i < 4:
            vals.append(max(float(cs[m].mean()), 1e-6))
            x = cv2.resize(x, (x.shape[1] // 2, x.shape[0] // 2), interpolation=cv2.INTER_AREA)
            y = cv2.resize(y, (y.shape[1] // 2, y.shape[0] // 2), interpolation=cv2.INTER_AREA)
            valid = cv2.resize(valid.astype(np.uint8), (x.shape[1], x.shape[0]), interpolation=cv2.INTER_NEAREST) > 0
        else:
            vals.append(max(float((l * cs)[m].mean()), 1e-6))
    return float(np.prod([v**wi for v, wi in zip(vals, w)]))


def tile_shifts(R, T, valid, tile=128):
    """per-tile translation of R against T (phase correlation on log-luma high-pass), textured tiles only"""
    lr = np.log2(np.maximum(luma(np.nan_to_num(R)), 1e-6)).astype(np.float32)
    lt = np.log2(np.maximum(luma(T), 1e-6)).astype(np.float32)
    lr = lr - cv2.GaussianBlur(lr, (0, 0), 3)
    lt = lt - cv2.GaussianBlur(lt, (0, 0), 3)
    H, W = lt.shape
    win = cv2.createHanningWindow((tile, tile), cv2.CV_32F)
    out = []
    for y in range(tile, H - 2 * tile, tile):
        for x in range(tile, W - 2 * tile, tile):
            if not valid[y : y + tile, x : x + tile].all():
                continue
            a, b_ = lt[y : y + tile, x : x + tile], lr[y : y + tile, x : x + tile]
            if a.std() < 0.08:
                continue
            (dx, dy), resp = cv2.phaseCorrelate(a, b_, win)
            if resp > 0.2:
                out.append(math.hypot(dx, dy))
    return np.array(out)


def flat_block_noise(img, valid, block=16):
    """per block: mean luma and σ of the luma high-pass (pixel − mean of 4 neighbours) / sqrt(1.25)"""
    Y = luma(img).astype(np.float32)
    hp = Y - 0.25 * (np.roll(Y, 1, 0) + np.roll(Y, -1, 0) + np.roll(Y, 1, 1) + np.roll(Y, -1, 1))
    H, W = (Y.shape[0] // block) * block, (Y.shape[1] // block) * block
    rs = lambda z: z[:H, :W].reshape(H // block, block, W // block, block)  # noqa: E731
    v = rs(valid.astype(np.float32)).min(axis=(1, 3)) > 0
    mean = rs(Y).mean(axis=(1, 3))
    sd = rs(hp).std(axis=(1, 3)) / math.sqrt(1.25)
    return mean[v], sd[v], v


def bins_ev(lo=-14, hi=1, step=1.0):
    e = np.arange(lo, hi + 1e-9, step)
    return list(zip(e[:-1], e[1:]))


def halo_overshoot(Y, valid, far=14, near=9, n_max=20000, seed=1):
    """Halo measure for a display-referred luma image `Y` (0..1, encoded): at strong edges (top 0.5 % of the gradient,
    plateau step >= 0.12) the profile along the gradient axis from -near .. +near may not leave the band between the
    two far levels (`far` px either side). Overshoot = excursion outside that band / step height. Returns the mean and
    the 95th percentile over the edge pixels (natural texture contributes: compare with the same measure of the
    tone-mapped truth, the caller reports both)."""
    H, W = Y.shape
    Y = cv2.GaussianBlur(Y, (0, 0), 1.5)  # profiles are read on a lightly smoothed image: noise must not count as a halo
    gy, gx = np.gradient(Y)
    g = np.hypot(gx, gy)
    ok = valid.copy()
    ok[:far + 1] = ok[-far - 1:] = False
    ok[:, :far + 1] = ok[:, -far - 1:] = False
    thr = np.percentile(g[ok], 99.5)
    ys, xs = np.nonzero(ok & (g >= thr))
    if len(ys) > n_max:
        pick = np.random.default_rng(seed).choice(len(ys), n_max, replace=False)
        ys, xs = ys[pick], xs[pick]
    horiz = np.abs(gx[ys, xs]) >= np.abs(gy[ys, xs])
    dx = np.where(horiz, 1, 0)
    dy = np.where(horiz, 0, 1)
    sgn = np.where(horiz, np.sign(gx[ys, xs]), np.sign(gy[ys, xs])).astype(int)
    dx, dy = dx * sgn, dy * sgn  # direction of increasing brightness
    prof = lambda k: Y[ys + dy * k, xs + dx * k]
    a, b = prof(-far), prof(far)
    lo, hi = np.minimum(a, b), np.maximum(a, b)
    step = hi - lo
    inner = np.stack([prof(k) for k in range(-near, near + 1)])
    over = np.maximum(lo - inner.min(axis=0), inner.max(axis=0) - hi)
    sel = step >= 0.12
    if sel.sum() < 50:
        return None, None, int(sel.sum())
    o = np.maximum(over[sel], 0) / step[sel]
    return float(o.mean()), float(np.percentile(o, 95)), int(sel.sum())


def score_pair(s, method, R, kind, info):
    """all metrics of one result"""
    syn = "synthetic" in s.meta
    wb = np.array(s.meta["dev"]["wb"], np.float32)
    ks = s.k()
    D = s.dev(s.ref)
    T = s.truth() if syn else None
    target = T if syn else D
    fo = os.environ.get("HDRBENCH_FORCE_OFFSET", "")  # "series:ox,oy;..." – fixed integer offset for raw-geometry results
    force = None
    for item in fo.split(";"):
        if item.startswith(s.id + ":"):
            force = tuple(int(v) for v in item.split(":")[1].split(","))
    Ra, off, regscore = into_target(R, target, force)
    H, W = target.shape[:2]
    m = int(0.03 * min(H, W))
    valid = np.isfinite(Ra).all(axis=2)
    valid[:m] = valid[-m:] = False
    valid[:, :m] = valid[:, -m:] = False
    Ra = np.where(valid[..., None], Ra, 0).astype(np.float32)
    valid &= luma(Ra) > -1  # keep
    row = {"series": s.id, "method": method, "kind": kind, "offset": f"{off[0]},{off[1]}", "valid_pct": round(100 * valid.mean(), 1)}
    row["wall_s"] = info.get("wall_seconds")
    row["rss_mb"] = info.get("peak_rss_mb")
    Yt = luma(target)
    D_ok = ((D / wb[None, None, :]).max(axis=2) < 0.9)
    # ghost/motion mask
    gm = s.ghost_mask() if syn else motion_proxy(s)
    if gm is not None:
        gm = gm[:H, :W] & valid
    # alignment (works for both kinds)
    sh = tile_shifts(Ra, target if kind == "linear" else tonemap(target, 0, ref_level(s, target)), valid)
    if kind == "display":
        sh = tile_shifts(Ra, tonemap(target, 0, ref_level(s, target)), valid)
    row["align_med_px"] = round(float(np.median(sh)), 3) if len(sh) else None
    row["align_p95_px"] = round(float(np.percentile(sh, 95)), 3) if len(sh) else None
    if kind == "display":
        tm = tonemap(target, 0, ref_level(s, target))
        hm, hp, hn = halo_overshoot(luma(Ra), valid)
        tm_m, tm_p, _ = halo_overshoot(luma(tm), valid)
        row["halo_mean"] = None if hm is None else round(hm, 4)
        row["halo_p95"] = None if hp is None else round(hp, 4)
        row["halo_ref_mean"] = None if tm_m is None else round(tm_m, 4)
        row["halo_ref_p95"] = None if tm_p is None else round(tm_p, 4)
        row["halo_edges"] = hn
    if kind != "linear":
        return row
    # global scale: median ratio in mid-tones where the target is reliable
    Yr = luma(Ra)
    mid = valid & (Yt > 0.02) & (Yt < 0.5) & D_ok & (Yr > 0)
    if gm is not None:
        mid &= ~gm
    sc = float(np.median(Yt[mid] / Yr[mid])) if mid.sum() > 1000 else 1.0
    Ra = Ra * sc
    Yr = Yr * sc
    row["scale_ev"] = round(math.log2(sc), 3)
    eps = 2.0**-16
    # colour in mid-tones (4x4 binned to suppress noise)
    ref_white = float(np.median(Yt[mid])) * 5 if mid.any() else 1.0

    def bin4(z):
        h4, w4 = (z.shape[0] // 4) * 4, (z.shape[1] // 4) * 4
        return z[:h4, :w4].reshape(h4 // 4, 4, w4 // 4, 4, *z.shape[2:]).mean(axis=(1, 3))

    mb = bin4(mid.astype(np.float32)) > 0.99
    de = delta_e00(lab_from_lin(bin4(Ra)[mb], ref_white), lab_from_lin(bin4(target)[mb], ref_white))
    row["dE00_mid"] = round(float(np.mean(de)), 2) if de.size else None
    row["dE00_mid_p95"] = round(float(np.percentile(de, 95)), 2) if de.size else None
    # colour in highlights above the reference clip: angle between RGB vectors (degrees, 4x4 bins) against
    # the truth (synthetic) or the darkest frame scaled to reference units (real), where that is unclipped
    if syn:
        H_t = target
    else:
        dk = int(np.argmin(ks))
        H_t = s.dev(dk) / ks[dk]
    hl = valid & ~D_ok & (luma(H_t) > 1.0)
    if not syn:
        hl &= (s.dev(dk) / wb[None, None, :]).max(axis=2) < 0.9
    if gm is not None:
        hl &= ~gm
    hb_ = bin4(hl.astype(np.float32)) > 0.99
    if hb_.sum() > 50:
        a_, b2 = bin4(Ra)[hb_], bin4(H_t)[hb_]
        cosv = np.sum(a_ * b2, axis=1) / np.maximum(np.linalg.norm(a_, axis=1) * np.linalg.norm(b2, axis=1), 1e-12)
        ang = np.degrees(np.arccos(np.clip(cosv, -1, 1)))
        row["hl_color_deg"] = round(float(np.mean(ang)), 2)
        row["hl_level_ev"] = round(float(np.median(np.log2(np.maximum(luma(a_[None])[0], 1e-9) / np.maximum(luma(b2[None])[0], 1e-9)))), 3)
    # sharpness: gradient magnitude on strong target edges in mid-tones (log domain), result/target
    lt = np.log2(np.maximum(Yt, eps)).astype(np.float32)
    lr = np.log2(np.maximum(Yr, eps)).astype(np.float32)
    gt = np.hypot(cv2.Sobel(lt, cv2.CV_32F, 1, 0), cv2.Sobel(lt, cv2.CV_32F, 0, 1))
    gr = np.hypot(cv2.Sobel(lr, cv2.CV_32F, 1, 0), cv2.Sobel(lr, cv2.CV_32F, 0, 1))
    em = valid & (Yt > 0.01) & (Yt < 0.6) & D_ok
    if gm is not None:
        em &= ~cv2.dilate(gm.astype(np.uint8), np.ones((15, 15), np.uint8)).astype(bool)
    if em.sum() > 1000:
        thr = np.percentile(gt[em], 97)
        e2 = em & (gt >= thr)
        row["edge_grad_ratio"] = round(float(gr[e2].mean() / max(gt[e2].mean(), 1e-9)), 3)
    # seams/halos: gradient error in bands where the bracket switches frames, against a clean target there
    row["seam_err"] = seam_metric(s, Ra, target, valid, gm, syn)
    # noise / SNR per brightness bin (flat blocks)
    if syn:
        err = Ra - target
        fb_mean, fb_sd, _ = flat_block_noise_err(target, err, valid & ~(gm if gm is not None else np.zeros_like(valid)))
    else:
        fb_mean, fb_sd, _ = flat_block_noise(Ra, valid & ~(gm if gm is not None else np.zeros_like(valid)))
        rm, rsd, _ = flat_block_noise(D, valid & ~(gm if gm is not None else np.zeros_like(valid)))
    snr = {}
    for lo, hi in bins_ev(-14, 0, 1):
        sel = (fb_mean >= 2.0**lo) & (fb_mean < 2.0**hi)
        if sel.sum() >= 20:
            sd = np.percentile(fb_sd[sel], 10 if not syn else 50)
            snr[lo] = float(np.mean(fb_mean[sel]) / max(sd, 1e-12))
    row["snr_by_ev"] = {int(k): round(20 * math.log10(v), 1) for k, v in snr.items()}
    for lo_ev in (-10, -8, -6, -4):
        row[f"snr_db_ev{lo_ev}"] = round(20 * math.log10(snr[lo_ev]), 1) if lo_ev in snr else None
    # dynamic range: from lowest level with SNR >= 1 (interpolated) to the highest correctly reproduced level
    lo_level = None
    for lo in sorted(snr):
        if snr[lo] >= 1.0:
            lo_level = 2.0 ** (lo + 0.5)
            break
    if syn:
        ratio = np.abs(np.log2(np.maximum(Yr, eps) / np.maximum(Yt, eps)))
        hi_level = None
        for lo, hi in bins_ev(-6, 8, 0.5)[::-1]:
            sel = valid & (Yt >= 2.0**lo) & (Yt < 2.0**hi)
            if gm is not None:
                sel &= ~gm
            if sel.sum() > 200 and np.median(ratio[sel]) < 0.1:
                hi_level = 2.0**hi
                break
    else:
        hi_level = float(np.percentile(Yr[valid], 99.99))
    if lo_level and hi_level:
        row["dr_ev"] = round(math.log2(hi_level / lo_level), 1)
    # truth-based quality
    if syn:
        good = valid & (Yt > 2.0**-12)
        e = (lr - lt)[good]
        rmse = float(np.sqrt(np.mean(e**2)))
        drt = float(np.log2(np.percentile(Yt[good], 99.9) / np.percentile(Yt[good], 0.1)))
        row["log_rmse_ev"] = round(rmse, 4)
        row["psnr_log_db"] = round(20 * math.log10(drt / max(rmse, 1e-9)), 2)
        lo_, hi_ = float(np.percentile(lt[good], 0.1)), float(np.percentile(lt[good], 99.9))
        nx = np.clip((lr - lo_) / (hi_ - lo_), 0, 1)
        ny = np.clip((lt - lo_) / (hi_ - lo_), 0, 1)
        row["ms_ssim_log"] = round(ms_ssim(nx, ny, good), 4)
        if gm is not None and gm.sum() > 100:
            ge = np.abs(lr - lt)[gm & (Yt > 2.0**-12)]
            row["ghost_err_ev"] = round(float(ge.mean()), 3)
            row["ghost_area_pct"] = round(float(100 * (ge > 0.3).mean()), 1)
    else:
        if gm is not None and gm.sum() > 100:
            sel = gm & D_ok & (luma(D) > 0.005)
            if sel.sum() > 100:
                ge = np.abs(lr - np.log2(np.maximum(luma(D), eps)))[sel]
                row["ghost_proxy_ev"] = round(float(np.median(ge)), 3)
                row["ghost_proxy_area_pct"] = round(float(100 * (ge > 0.3).mean()), 1)
    return row


def flat_block_noise_err(truth, err, valid, block=16):
    Yt = luma(truth).astype(np.float32)
    E = luma(err).astype(np.float32)
    gt = cv2.Laplacian(np.log2(np.maximum(cv2.GaussianBlur(Yt, (0, 0), 1), 2.0**-16)), cv2.CV_32F)
    H, W = (Yt.shape[0] // block) * block, (Yt.shape[1] // block) * block
    rs = lambda z: z[:H, :W].reshape(H // block, block, W // block, block)  # noqa: E731
    v = rs(valid.astype(np.float32)).min(axis=(1, 3)) > 0
    flat = np.abs(rs(gt)).mean(axis=(1, 3)) < 0.05
    v &= flat
    return rs(Yt).mean(axis=(1, 3))[v], rs(E).std(axis=(1, 3))[v], v


def seam_metric(s, R, target, valid, gm, syn):
    """mean |∇log2 R − ∇log2 C| (EV/px, on 2x2 bins) in bands where frames switch, C = clean comparison:
    synthetic: the truth; real: the next darker frame (unclipped there), scaled to reference units"""
    ks = np.array(s.k())
    wb = np.array(s.meta["dev"]["wb"], np.float32)
    D = s.dev(s.ref)
    order = np.argsort(ks)
    ref_pos = list(order).index(s.ref)
    if ref_pos == 0:
        return None
    dark = order[ref_pos - 1]
    sat = (D / wb[None, None, :]).max(axis=2)
    band = (sat > 0.75) & (sat < 0.99)
    band = cv2.dilate(band.astype(np.uint8), np.ones((5, 5), np.uint8)).astype(bool) & valid
    if gm is not None:
        band &= ~gm
    if band.sum() < 500:
        return None
    C = target if syn else s.dev(dark) / ks[dark]

    def g2(img):
        Y = luma(img)
        h2, w2 = (Y.shape[0] // 2) * 2, (Y.shape[1] // 2) * 2
        Y = Y[:h2, :w2].reshape(h2 // 2, 2, w2 // 2, 2).mean(axis=(1, 3))
        L = np.log2(np.maximum(Y, 2.0**-16)).astype(np.float32)
        return cv2.Sobel(L, cv2.CV_32F, 1, 0) / 8, cv2.Sobel(L, cv2.CV_32F, 0, 1) / 8

    rx, ry = g2(R)
    cx, cy = g2(C)
    b2 = band[: rx.shape[0] * 2 : 2, : rx.shape[1] * 2 : 2][: rx.shape[0], : rx.shape[1]]
    return round(float(np.mean(np.hypot(rx - cx, ry - cy)[b2])), 4)


_MOTION = {}


def motion_proxy(s):
    """real series: pixels where exposure-normalised frames disagree with the reference after a global
    alignment (ECC, Euclidean) at 1/4 resolution, in the range valid in both frames"""
    if s.id in _MOTION:
        return _MOTION[s.id]
    ks = s.k()
    wb = np.array(s.meta["dev"]["wb"], np.float32)
    D = s.dev(s.ref)
    H, W = D.shape[:2]
    f = 4
    small = lambda im: cv2.resize(im, (W // f, H // f), interpolation=cv2.INTER_AREA)  # noqa: E731
    Dr = small(D)
    lr = np.log2(np.maximum(luma(Dr), 1e-6)).astype(np.float32)
    okr = (small(D) / wb).max(axis=2) < 0.9
    acc = np.zeros(lr.shape, np.float32)
    for i in range(len(ks)):
        if i == s.ref:
            continue
        Fi = small(s.dev(i))
        li = np.log2(np.maximum(luma(Fi) / ks[i], 1e-6)).astype(np.float32)
        oki = (Fi / wb).max(axis=2) < 0.9
        floor = max(2.0**-9 / min(ks[i], 1.0), 2.0**-9)
        both = okr & oki & (luma(Dr) > floor) & (luma(Fi) / ks[i] > floor)
        warp = np.eye(2, 3, dtype=np.float32)
        try:
            a = np.clip(lr, np.log2(floor), 0)
            b_ = np.clip(li, np.log2(floor), 0)
            _, warp = cv2.findTransformECC(a, b_, warp, cv2.MOTION_EUCLIDEAN, (cv2.TERM_CRITERIA_EPS | cv2.TERM_CRITERIA_COUNT, 60, 1e-5), both.astype(np.uint8), 5)
        except cv2.error:
            pass
        li_w = cv2.warpAffine(li, warp, (lr.shape[1], lr.shape[0]), flags=cv2.INTER_LINEAR | cv2.WARP_INVERSE_MAP)
        bw = cv2.warpAffine(both.astype(np.uint8), warp, (lr.shape[1], lr.shape[0]), flags=cv2.INTER_NEAREST | cv2.WARP_INVERSE_MAP) > 0
        diff = np.abs(cv2.GaussianBlur(li_w, (5, 5), 1) - cv2.GaussianBlur(lr, (5, 5), 1))
        acc = np.maximum(acc, np.where(bw & both, diff, 0))
    m = acc > 0.5
    m = cv2.morphologyEx(m.astype(np.uint8), cv2.MORPH_OPEN, np.ones((3, 3), np.uint8))
    m = cv2.dilate(m, np.ones((5, 5), np.uint8))
    full = cv2.resize(m, (W, H), interpolation=cv2.INTER_NEAREST) > 0
    if full.mean() > 0.6:
        full = None  # registration failed, mask meaningless
    _MOTION[s.id] = full
    return full


def ref_level(s, target):
    """mid-grey anchor: median luma of the target in well exposed areas"""
    Y = luma(target)
    sel = (Y > 0.01) & (Y < 0.6)
    return float(np.median(Y[sel])) if sel.any() else 0.18


def tonemap(img, ev, anchor=None):
    """fixed operator for all linear results (reference clip = 1.0): ×2^ev, per-channel extended Reinhard (white 16), sRGB.
    `anchor` is ignored (kept for callers); results are compared after the global scale fit."""
    x = np.maximum(img * (2.0**ev), 0)
    y = x * (1 + x / 256.0) / (1 + x)
    return srgb_encode(y).astype(np.float32)


def cmd_score(b, a):
    rows = []
    sids = a.series or b.series_ids()
    methods = sorted(p.name for p in b.results_dir.iterdir() if p.is_dir()) if b.results_dir.exists() else []
    if a.method:
        methods = [m for m in methods if m in a.method.split(",")]
    for sid in sids:
        s = b.load(sid)
        for m in methods:
            p = b.results_dir / m / f"{sid}.tif"
            if not p.exists():
                continue
            info = json.loads((b.results_dir / m / f"{sid}.json").read_text())
            R = tifffile.imread(p).astype(np.float32)
            try:
                row = score_pair(s, m, R, info["kind"], info)
            except Exception as e:  # keep scoring the rest
                row = {"series": sid, "method": m, "error": str(e)[:200]}
            rows.append(row)
            print(json.dumps(row))
    b.tables_dir.mkdir(parents=True, exist_ok=True)
    out = b.tables_dir / (a.out or "scores")
    keys = []
    for r in rows:
        for k in r:
            if k not in keys and k != "snr_by_ev":
                keys.append(k)
    import csv

    with open(str(out) + ".csv", "w", newline="") as f:
        w = csv.DictWriter(f, fieldnames=keys + ["snr_by_ev"])
        w.writeheader()
        for r in rows:
            w.writerow({**r, "snr_by_ev": json.dumps(r.get("snr_by_ev", {}))})
    (Path(str(out) + ".json")).write_text(json.dumps(rows, indent=1) + "\n")
    print(f"→ {out}.csv / .json ({len(rows)} rows)")


# --------------------------------------------------------------------------------------------
# contact sheets


def auto_crops(s, size=256):
    """pick crop positions once per series (stored in series.json): shadow, highlight, motion, edge"""
    D = s.dev(s.ref)
    wb = np.array(s.meta["dev"]["wb"], np.float32)
    T = s.truth()
    tgt = T if T is not None else D
    H, W = D.shape[:2]
    Y = luma(tgt)
    sat = (D / wb).max(axis=2)
    gm = s.ghost_mask() if T is not None else motion_proxy(s)
    ks = s.k()
    Dd = s.dev(int(np.argmin(ks))) / min(ks)
    ld = np.log2(np.maximum(luma(Dd), 2.0**-16)).astype(np.float32)
    gdark = np.hypot(cv2.Sobel(ld, cv2.CV_32F, 1, 0), cv2.Sobel(ld, cv2.CV_32F, 0, 1))
    Ydark = luma(Dd)
    del Dd
    lt = np.log2(np.maximum(Y, 2.0**-16))
    grad = np.hypot(cv2.Sobel(lt.astype(np.float32), cv2.CV_32F, 1, 0), cv2.Sobel(lt.astype(np.float32), cv2.CV_32F, 0, 1))
    best = {}
    step = size // 2
    m = int(0.05 * min(H, W))
    for y in range(m, H - size - m, step):
        for x in range(m, W - size - m, step):
            ty = Y[y : y + size, x : x + size]
            ts = sat[y : y + size, x : x + size]
            tg = grad[y : y + size, x : x + size]
            med = float(np.median(ty))
            tex = float(np.median(tg))
            cand = {
                "shadow": (-math.log2(max(med, 1e-9)) + 4 * min(tex, 0.5)) if med < 0.02 else None,
                "highlight": float(np.median(gdark[y : y + size, x : x + size][ts > 0.99])) if (ts > 0.99).mean() > 0.5 else None,
                "edge": float(np.percentile(tg, 90)) if 0.02 < med < 0.4 and (ts < 0.95).all() else None,
                "motion": float(gm[y : y + size, x : x + size].mean()) if gm is not None else None,
            }
            for k, v in cand.items():
                if v is not None and (k not in best or v > best[k][0]):
                    best[k] = (v, x, y)
    s.meta["crops"] = {k: [v[1], v[2]] for k, v in best.items()}
    if "highlight" in best:
        x, y = best["highlight"][1], best["highlight"][2]
        s.meta["highlight_ev"] = -float(np.ceil(max(0.0, math.log2(max(float(np.percentile(Ydark[y : y + size, x : x + size], 95)), 1e-6)))))
    s.meta["crop_size"] = size
    s.save()


def put_label(img8, text, y=4):
    cv2.putText(img8, text, (5, y + 12), cv2.FONT_HERSHEY_SIMPLEX, 0.42, (0, 0, 0), 3, cv2.LINE_AA)
    cv2.putText(img8, text, (5, y + 12), cv2.FONT_HERSHEY_SIMPLEX, 0.42, (255, 255, 0), 1, cv2.LINE_AA)


def cmd_sheet(b, a):
    sids = a.series or b.series_ids()
    for sid in sids:
        s = b.load(sid)
        if "crops" not in s.meta:
            auto_crops(s)
        size = s.meta.get("crop_size", 256)
        T = s.truth()
        D = s.dev(s.ref)
        target = T if T is not None else D
        anchor = ref_level(s, target)
        crops = [(k, s.meta["crops"][k]) for k in ("shadow", "highlight", "motion", "edge") if k in s.meta["crops"]]
        ev_of = {"shadow": 3.0, "highlight": s.meta.get("highlight_ev", -1.0), "motion": 0.0, "edge": 0.0}
        rows = []
        entries = [("truth" if T is not None else "ref (dev)", target, "linear", None)]
        methods = sorted(p.name for p in b.results_dir.iterdir() if p.is_dir() and (p / f"{sid}.tif").exists())
        if a.method:
            methods = [m for m in methods if m in a.method.split(",")]
        for mth in methods:
            if mth == "ref" and T is None:
                continue  # identical to the target row
            info = json.loads((b.results_dir / mth / f"{sid}.json").read_text())
            R = tifffile.imread(b.results_dir / mth / f"{sid}.tif").astype(np.float32)
            Ra, off, _ = into_target(R, target)
            Ra = np.nan_to_num(Ra)
            if info["kind"] == "linear":
                Y, Yt = luma(Ra), luma(target)
                sel = (Yt > 0.02) & (Yt < 0.5) & (Y > 0)
                sc = float(np.median(Yt[sel] / Y[sel])) if sel.sum() > 1000 else 1.0
                Ra = Ra * sc
            entries.append((mth, Ra, info["kind"], info.get("wall_seconds")))
        thumb_h = size
        for name, img, kind, secs in entries:
            tiles = []
            th = cv2.resize(img, (int(img.shape[1] * thumb_h / img.shape[0]), thumb_h), interpolation=cv2.INTER_AREA)
            th8 = tonemap(th, 0, anchor) if kind == "linear" else np.clip(th, 0, 1)
            th8 = np.round(th8 * 255).astype(np.uint8)[..., ::-1].copy()
            for k, (x, y) in crops:
                cv2.rectangle(th8, (int(x * thumb_h / img.shape[0]), int(y * thumb_h / img.shape[0])), (int((x + size) * thumb_h / img.shape[0]), int((y + size) * thumb_h / img.shape[0])), (0, 200, 255), 1)
            put_label(th8, f"{name}" + (f"  {secs:.1f}s" if secs else ""))
            tiles.append(th8)
            for k, (x, y) in crops:
                c = img[y : y + size, x : x + size]
                c8 = tonemap(c, ev_of[k], anchor) if kind == "linear" else np.clip(c * (2.0 ** (ev_of[k] / 2.2) if k == "shadow" else 1.0), 0, 1)
                c8 = np.round(c8 * 255).astype(np.uint8)[..., ::-1].copy()
                put_label(c8, f"{k} {ev_of[k]:+.0f}EV" if kind == "linear" else f"{k} (display)")
                tiles.append(c8)
            rows.append(np.hstack([np.pad(t, ((2, 2), (2, 2), (0, 0))) for t in tiles]))
        wmax = max(r.shape[1] for r in rows)
        rows = [np.pad(r, ((0, 0), (0, wmax - r.shape[1]), (0, 0))) for r in rows]
        sheet = np.vstack(rows)
        title = np.zeros((28, wmax, 3), np.uint8)
        put_label(title, f"{sid}: {s.meta.get('title', '')} | 100% crops, fixed tone map (ref clip = 1, extended Reinhard w=16, sRGB); shadow +3 EV, highlight crop scaled to its own level", 6)
        b.sheets_dir.mkdir(parents=True, exist_ok=True)
        out = b.sheets_dir / f"{sid}.jpg"
        cv2.imwrite(str(out), np.vstack([title, sheet]), [cv2.IMWRITE_JPEG_QUALITY, 92])
        print(f"→ {out}")


SYN_COLS = [("psnr_log_db", "PSNR-log dB", "+"), ("ms_ssim_log", "MS-SSIM-log", "+"), ("snr_db_ev-8", "SNR@-8EV dB", "+"), ("snr_db_ev-6", "SNR@-6EV dB", "+"), ("snr_db_ev-4", "SNR@-4EV dB", "+"), ("dr_ev", "DR EV", "+"), ("align_p95_px", "Ausr. p95 px", "-"), ("ghost_err_ev", "Geist EV", "-"), ("dE00_mid", "ΔE00", "-"), ("hl_color_deg", "Lichter-Farbe °", "-"), ("hl_level_ev", "Lichter-Pegel EV", "0"), ("edge_grad_ratio", "Kante", "1"), ("seam_err", "Naht", "-"), ("wall_s", "Zeit s", "-"), ("rss_mb", "RAM MB", "-")]
REAL_COLS = [("snr_db_ev-8", "SNR@-8EV dB*", "+"), ("snr_db_ev-6", "SNR@-6EV dB*", "+"), ("dr_ev", "DR EV*", "+"), ("align_p95_px", "Ausr. p95 px", "-"), ("ghost_proxy_ev", "Geist-Proxy EV", "-"), ("dE00_mid", "ΔE00", "-"), ("hl_color_deg", "Lichter-Farbe °", "-"), ("hl_level_ev", "Lichter-Pegel EV", "0"), ("edge_grad_ratio", "Kante", "1"), ("seam_err", "Naht", "-"), ("wall_s", "Zeit s", "-"), ("rss_mb", "RAM MB", "-")]


def cmd_report(b, a):
    """markdown tables per series (method × metric) and a mean-rank summary from a scores .json"""
    rows = json.loads((b.tables_dir / ((a.scores or "scores") + ".json")).read_text())
    rows = [r for r in rows if "error" not in r]
    sids = sorted({r["series"] for r in rows}, key=lambda x: (not x.startswith("syn"), x))
    out = []
    ranks = {}
    for sid in sids:
        rs = sorted([r for r in rows if r["series"] == sid], key=lambda r: r["method"])
        cols = SYN_COLS if sid.startswith("syn") else REAL_COLS
        out.append(f"### {sid}\n")
        out.append("| Verfahren | " + " | ".join(c[1] for c in cols) + " |")
        out.append("|---|" + "---|" * len(cols))
        for r in rs:
            vals = []
            for k, _, _ in cols:
                v = r.get(k)
                vals.append("–" if v is None else (f"{v:.4f}" if k == "ms_ssim_log" else f"{v:g}" if isinstance(v, (int, float)) else str(v)))
            out.append(f"| {r['method']}{' (display)' if r.get('kind') == 'display' else ''} | " + " | ".join(vals) + " |")
        out.append("")
        for k, _, d in cols:
            if k in ("wall_s", "rss_mb"):
                continue
            cand = [(r[k], r["method"]) for r in rs if r.get(k) is not None and r.get("kind") == "linear" and r["method"] != "ref"]
            if len(cand) < 2:
                continue
            key = (lambda v: -v) if d == "+" else (lambda v: abs(v - 1)) if d == "1" else (lambda v: abs(v)) if d == "0" else (lambda v: v)
            for i, (_, m) in enumerate(sorted(cand, key=lambda t: key(t[0]))):
                ranks.setdefault(m, []).append(i + 1)
    summ = ["| Verfahren | mittlerer Rang (linear, ohne ref) | Anzahl Wertungen |", "|---|---|---|"]
    for m, rk in sorted(ranks.items(), key=lambda t: np.mean(t[1])):
        summ.append(f"| {m} | {np.mean(rk):.2f} | {len(rk)} |")
    text = "## Rangübersicht\n\n" + "\n".join(summ) + "\n\n## Je Reihe\n\n" + "\n".join(out)
    p = b.tables_dir / ((a.scores or "scores") + ".md")
    p.write_text(text + "\n")
    print(f"→ {p}")


def cmd_list(b, a):
    for sid in b.series_ids():
        s = b.load(sid)
        m = s.meta
        print(f"{sid:28} {len(m['frames'])} frames  ref {m['reference']}  {m.get('camera', '')}  {m.get('license', '')}  {'synthetic' if 'synthetic' in m else ''}")
    print("methods:", ", ".join(list(METHODS) + list(b.external)))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--root", default=os.environ.get("HDRBENCH_ROOT", "hdr-bench"))
    sp = ap.add_subparsers(dest="cmd", required=True)
    p = sp.add_parser("add-series")
    p.add_argument("id")
    p.add_argument("files", nargs="+")
    p.add_argument("--reference", type=int)
    for k in ("title", "source", "url", "license", "tags", "notes"):
        p.add_argument("--" + k, default="")
    p.add_argument("--internal-only", action="store_true")
    p = sp.add_parser("develop")
    p.add_argument("series", nargs="*")
    p.add_argument("--force", action="store_true")
    p = sp.add_parser("truth")
    p.add_argument("series")
    p.add_argument("out")
    p.add_argument("--downscale", type=int, default=2)
    p.add_argument("--crop", default="")
    p = sp.add_parser("synth")
    p.add_argument("id")
    p.add_argument("--truth", required=True)
    p.add_argument("--wb", default="2.0,1.0,1.5", help="white balance multipliers of the simulated camera")
    p.add_argument("--gain", type=float, default=1.0, help="scale the truth before simulation")
    p.add_argument("--contrast", type=float, default=1.0, help="luma exponent around the median (2 = double the scene DR in EV)")
    p.add_argument("--evs", default="-2,0,2")
    p.add_argument("--noise-a", type=float, default=1.2e-4)
    p.add_argument("--noise-b", type=float, default=7e-9)
    p.add_argument("--bits", type=int, default=14)
    p.add_argument("--iso", type=float, default=100)
    p.add_argument("--base-time", type=float, default=1 / 60)
    p.add_argument("--shift", type=float, default=0.0, help="max translation in px (uniform ±)")
    p.add_argument("--rot", type=float, default=0.0, help="max rotation in degrees")
    p.add_argument("--scale", type=float, default=0.0, help="max relative scale change")
    p.add_argument("--vignette", type=float, default=0.0, help="light fall-off at the corner (0.3 = −30 %%)")
    p.add_argument("--objects", type=int, default=0, help="number of moving objects")
    p.add_argument("--seed", type=int, default=1)
    for k in ("title", "license", "tags", "notes"):
        p.add_argument("--" + k, default="")
    p.add_argument("--internal-only", action="store_true")
    p = sp.add_parser("run")
    p.add_argument("method", help="method name, comma list or 'all'")
    p.add_argument("series", nargs="*")
    p.add_argument("--force", action="store_true")
    p = sp.add_parser("_run1")
    p.add_argument("method")
    p.add_argument("sid")
    p = sp.add_parser("ingest")
    p.add_argument("method")
    p.add_argument("series")
    p.add_argument("file")
    p.add_argument("--kind", default="linear", choices=["linear", "display"])
    p.add_argument("--note", default="")
    p.add_argument("--seconds", type=float)
    p = sp.add_parser("score")
    p.add_argument("series", nargs="*")
    p.add_argument("--method", default="")
    p.add_argument("--out", default="")
    p = sp.add_parser("sheet")
    p.add_argument("series", nargs="*")
    p.add_argument("--method", default="")
    p = sp.add_parser("report")
    p.add_argument("--scores", default="")
    sp.add_parser("list")
    a = ap.parse_args()
    b = Bench(a.root)
    {"add-series": cmd_add_series, "develop": cmd_develop, "truth": cmd_truth, "synth": cmd_synth, "run": cmd_run, "_run1": cmd_run1, "ingest": cmd_ingest, "score": cmd_score, "sheet": cmd_sheet, "report": cmd_report, "list": cmd_list}[a.cmd](b, a)


if __name__ == "__main__":
    main()

#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Colour measurement of graineo: one call → all tables.

Three measurements, each can be switched off (section missing in the configuration = skipped):

  spp          Sigma X3F against the exports of the manufacturer software (16-bit TIFF, sRGB, "as shot").
               graineo develops every file with `graineo-dev develop X3F OUT.tif --spp --baseline` (like the app
               when exporting without own settings). Pixel-by-pixel comparison (CIEDE2000), after alignment
               (rotation, integer scale, offset ± 12 px, 16 px border removed). Per file: ΔE00 mean, p95,
               ΔE00 of the 8×8 block means (without noise/sharpening); per camera mean over the files.
  color_chart  ColorChecker-24 shot against target values (X-Rite "after November 2014", Lab D50, via
               colour-science). White balance on the neutral patches 20–22 (like the eyedropper), exposure such
               that patch 22 reaches its target L*. Non-Sigma: `graineo-dev color-chart-raw` (export path, look
               neutral); Sigma: `develop --spp --baseline --wb-mult --ev` (iterative). Optionally the camera JPEG
               of the same shot, normalised afterwards in the same way (channel factors in linear light on 20–22,
               factor on patch 22). Metrics per camera: ΔE00 mean, max (patch), skin 1–2, saturated 13–18,
               chromatic 1–18, grey |ΔL*| 19–24, ΔC* chromatic, ΔE00 without lightness component.
  camera_jpeg  Non-Sigma RAW against the camera JPEG of the same shot (or the embedded preview):
               `graineo-dev rawapp RAW … --compare REF` (preview size, exposure first matched to the midtones
               of the reference), once with look "neutral" and once with the camera look of the picture style.

Usage:
    python -I color_measurement.py CONFIG.json [--only spp,color_chart,camera_jpeg] [--redo]
Result in the output folder of the configuration: tables.md (all tables), result.json (all values),
spp/<name>.json, color_chart/<camera>.json, camera_jpeg.tsv. Developed images are cached
(`--redo` recomputes everything). Charts: charts.py. Example configuration and list formats: README.md.

Dependencies: numpy, tifffile, pillow, colour-science (target values, Lab). Test program: graineo-dev.
Environment of the runs: GRAINEO_DATA_DIR (required, own test folder), no sync, no sidecar files.
"""
import argparse
import concurrent.futures as cf
import json
import os
import struct
import subprocess
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'color'))
from demath import de2000  # noqa: E402  (CIEDE2000, Sharma et al. 2005)

# ---------------------------------------------------------------- shared colour math (sRGB D65)
M = np.array([[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]])
WP65 = np.array([0.95047, 1.0, 1.08883])


def decode(v):
    v = np.asarray(v, dtype=np.float64)
    s, x = np.sign(v), np.abs(v)
    return s * np.where(x <= 0.04045, x / 12.92, ((x + 0.055) / 1.055) ** 2.4)


def encode(lin):
    return np.where(lin <= 0.0031308, lin * 12.92, 1.055 * np.power(np.maximum(lin, 0), 1 / 2.4) - 0.055)


def lab_d65(lin):
    """linear sRGB → Lab (D65 white), like earlier comparisons against SPP (image against image, same white reference)."""
    xyz = lin @ M.T / WP65
    d = 6 / 29
    f = np.where(xyz > d ** 3, np.cbrt(np.maximum(xyz, 0)), xyz / (3 * d * d) + 4 / 29)
    return 116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])


# ---------------------------------------------------------------- configuration, runs
class Environment:
    def __init__(self, cfg_path):
        self.cfg_path = Path(cfg_path).resolve()
        self.cfg = json.loads(self.cfg_path.read_text())
        self.out = self.path(self.cfg['output'])
        self.out.mkdir(parents=True, exist_ok=True)
        self.bin = str(self.path(self.cfg['graineo_dev']))
        data_dir = self.path(self.cfg['data_dir'])
        data_dir.mkdir(parents=True, exist_ok=True)
        self.env = dict(os.environ, GRAINEO_DATA_DIR=str(data_dir), GRAINEO_LIBRARY_NO_SYNC='1', GRAINEO_LIBRARY_SIDECARS='0',
                        GRAINEO_NO_PHOTOKIT='1', GRAINEO_NO_XMP='1')
        self.parallel = int(self.cfg.get('parallel', 2))

    def path(self, p):
        p = Path(os.path.expanduser(str(p)))
        return p if p.is_absolute() else (self.cfg_path.parent / p).resolve()

    def run(self, args):
        p = subprocess.run([self.bin] + [str(a) for a in args], capture_output=True, text=True, env=self.env)
        if p.returncode:
            raise RuntimeError(f'{" ".join(map(str, args[:3]))}: {p.stderr[-1200:]}')
        return p.stdout


def tsv(p):
    lines = [z.rstrip('\n').split('\t') for z in open(p, encoding='utf-8') if z.strip() and not z.startswith('#')]
    head, rest = lines[0], lines[1:]
    return [dict(zip(head, z)) for z in rest]


# ---------------------------------------------------------------- 1. Sigma against manufacturer software
def load_tiff(path):
    import tifffile
    t = tifffile.TiffFile(path)
    p = t.pages[0]
    a = p.asarray()
    o = int(p.tags['Orientation'].value) if 'Orientation' in p.tags else 1
    if a.ndim == 2:
        a = np.stack([a] * 3, -1)
    a = a[..., :3]
    if a.dtype == np.uint8:
        a = a.astype(np.uint16) * 257
    a = {3: lambda x: x[::-1, ::-1], 6: lambda x: np.rot90(x, -1), 8: lambda x: np.rot90(x, 1),
         2: lambda x: x[:, ::-1], 4: lambda x: x[::-1]}.get(o, lambda x: x)(a)
    return np.ascontiguousarray(a)


def align(s, a):
    """Brings a (graineo) onto s (reference): rotation, integer scale, offset ± 12 px, 16 px border removed."""
    notes = []
    if s.shape[:2] != a.shape[:2]:
        k = min(range(4), key=lambda k: abs(np.rot90(a, k).shape[0] - s.shape[0]) + abs(np.rot90(a, k).shape[1] - s.shape[1]))
        if k:
            a = np.rot90(a, k)
            notes.append(f'rotated {k * 90}°')
    for large, small, who in ((a, s, 'graineo'), (s, a, 'reference')):
        f = round(large.shape[1] / small.shape[1])
        if f >= 2 and abs(large.shape[1] / f - small.shape[1]) < 4 and abs(large.shape[0] / f - small.shape[0]) < 4:
            hh, ww = large.shape[0] // f * f, large.shape[1] // f * f
            red = np.clip(np.round(large[:hh, :ww].reshape(hh // f, f, ww // f, f, 3).astype(np.float64).mean((1, 3))), 0, 65535).astype(np.uint16)
            notes.append(f'{who} downscaled 1:{f}')
            if who == 'graineo':
                a = red
            else:
                s = red
            break
    h, w = min(s.shape[0], a.shape[0]), min(s.shape[1], a.shape[1])
    if (h, w) != s.shape[:2] or (h, w) != a.shape[:2]:
        notes.append(f'size ref {s.shape[1]}×{s.shape[0]}, graineo {a.shape[1]}×{a.shape[0]}')
    lum = lambda x: (x.astype(np.float32) / 65535) @ np.array([0.2126, 0.7152, 0.0722], np.float32)
    ls, la = lum(s), lum(a)
    cys, cxs = (s.shape[0] - h) // 2, (s.shape[1] - w) // 2
    cya, cxa = (a.shape[0] - h) // 2, (a.shape[1] - w) // 2
    sh, sw = min(800, h - 40), min(1200, w - 40)
    ys, xs = h // 2 - sh // 2, w // 2 - sw // 2
    ref = ls[cys + ys:cys + ys + sh, cxs + xs:cxs + xs + sw]
    ref = ref - ref.mean()
    best = (None, 0, 0)
    for dy in range(-12, 13):
        for dx in range(-12, 13):
            y, x = cya + ys + dy, cxa + xs + dx
            if y < 0 or x < 0 or y + sh > a.shape[0] or x + sw > a.shape[1]:
                continue
            c = la[y:y + sh, x:x + sw]
            e = float(np.mean((c - c.mean() - ref) ** 2))
            if best[0] is None or e < best[0]:
                best = (e, dy, dx)
    _, dy, dx = best
    if dy or dx:
        notes.append(f'offset {dx:+d}/{dy:+d} px')
    m = 16
    y0s, x0s, y0a, x0a = cys + m, cxs + m, cya + m + dy, cxa + m + dx
    hh = min(h - 2 * m, a.shape[0] - y0a, s.shape[0] - y0s)
    ww = min(w - 2 * m, a.shape[1] - x0a, s.shape[1] - x0s)
    return s[y0s:y0s + hh, x0s:x0s + ww], a[y0a:y0a + hh, x0a:x0a + ww], notes


def block8(x, f=8):
    lin = decode(x.astype(np.float32) / 65535.0)
    hh, ww = lin.shape[0] // f * f, lin.shape[1] // f * f
    return lin[:hh, :ww].reshape(hh // f, f, ww // f, f, 3).mean((1, 3))


def compare_images(ref, own):
    """ΔE00 per pixel (in stripes) and of the 8×8 block means; returns: metrics and 8×8 ΔE map."""
    s, a, notes = align(ref, own)
    hist = np.zeros(4001, np.int64)
    total = dl = dc = 0.0
    n = s.shape[0] * s.shape[1]
    for y0 in range(0, s.shape[0], 256):
        L1, A1, B1 = lab_d65(decode(s[y0:y0 + 256].astype(np.float32) / 65535))
        L2, A2, B2 = lab_d65(decode(a[y0:y0 + 256].astype(np.float32) / 65535))
        de = de2000(L1, A1, B1, L2, A2, B2)
        hist += np.bincount(np.clip((de * 100).astype(np.int64), 0, 4000).ravel(), minlength=4001)
        total += float(de.sum())
        dl += float((L2 - L1).sum())
        dc += float((np.hypot(A2, B2) - np.hypot(A1, B1)).sum())
    c = np.cumsum(hist)
    p95 = float(np.searchsorted(c, 0.95 * c[-1]) / 100)
    bs, ba = block8(s), block8(a)
    deb = de2000(*lab_d65(bs), *lab_d65(ba))
    return dict(de_mean=total / n, de_p95=p95, de8_mean=float(deb.mean()), de8_p95=float(np.percentile(deb, 95)),
                dL=dl / n, dC=dc / n, share_over_2=float(1 - c[200] / c[-1]), pixels=n, notes=notes), deb


def thumbnail(lin, width=480):
    from PIL import Image
    im = Image.fromarray((np.clip(encode(lin), 0, 1) * 255 + 0.5).astype(np.uint8))
    return im.resize((width, round(im.height * width / im.width)), Image.LANCZOS)


def spp_one(u, z, redo):
    out = u.out / 'spp'
    (out / 'images').mkdir(parents=True, exist_ok=True)
    js = out / f"{z['name']}.json"
    if js.exists() and not redo:
        return json.loads(js.read_text())
    tif = out / f"{z['name']}.tif"
    opts = [o for o in z.get('options', '').split() if o]
    u.run(['develop', u.path(z['x3f']), tif, '--spp', '--baseline', *opts])
    ref, own = load_tiff(u.path(z['reference'])), load_tiff(tif)
    r, dmap = compare_images(ref, own)
    r.update(name=z['name'], camera=z['camera'], group=z['group'], own=z.get('own', '') == 'yes', source=z.get('source', ''),
             options=' '.join(opts))
    if r['own']:  # thumbnails and ΔE map only for own shots (for charts)
        np.save(out / 'images' / f"{z['name']}-de8.npy", dmap.astype(np.float16))
        thumbnail(block8(ref)).save(out / 'images' / f"{z['name']}-ref.jpg", quality=88)
        thumbnail(block8(own)).save(out / 'images' / f"{z['name']}-graineo.jpg", quality=88)
    if not u.cfg['spp'].get('keep_tiff'):
        tif.unlink()
    js.write_text(json.dumps(r, indent=1, ensure_ascii=False))
    return r


def spp_measure(u, redo):
    rows = tsv(u.path(u.cfg['spp']['pairs']))
    res = []
    with cf.ThreadPoolExecutor(u.parallel) as ex:
        futs = {ex.submit(spp_one, u, z, redo): z for z in rows}
        for f in cf.as_completed(futs):
            z = futs[f]
            try:
                r = f.result()
                res.append(r)
                print(f"spp {z['name']:50s} ΔE00 {r['de_mean']:5.2f} p95 {r['de_p95']:5.2f} 8×8 {r['de8_mean']:5.2f} {r['notes']}", flush=True)
            except Exception as e:  # noqa: BLE001
                print(f"spp {z['name']}: ERROR {e}", flush=True)
                res.append(dict(name=z['name'], camera=z['camera'], group=z['group'], error=str(e)))
    x3f = {z['name']: z['x3f'] for z in rows}
    for r in res:  # ISO from the X3F properties (for the table by ISO)
        if 'error' not in r and 'iso' not in r:
            r['iso'] = x3f_iso(u, x3f[r['name']])
            (u.out / 'spp' / f"{r['name']}.json").write_text(json.dumps(r, indent=1, ensure_ascii=False))
    return sorted(res, key=lambda r: r['name'])


def x3f_iso(u, x3f):
    for z in u.run(['info', u.path(x3f)]).splitlines():
        t = z.split()
        if len(t) == 2 and t[0] == 'ISO':
            try:
                return int(float(t[1]))
            except ValueError:
                return None
    return None


# ---------------------------------------------------------------- 2. ColorChecker against target values
def cc_reference():
    import colour
    cc = colour.CCS_COLOURCHECKERS['ColorChecker24 - After November 2014']
    ref = np.array([colour.XYZ_to_Lab(colour.xyY_to_XYZ(v), cc.illuminant) for v in cc.data.values()])
    return list(cc.data.keys()), ref, cc.illuminant


def lin_to_lab_d50(rgb):
    import colour
    d50 = colour.CCS_ILLUMINANTS['CIE 1931 2 Degree Standard Observer']['D50']
    d65 = colour.CCS_ILLUMINANTS['CIE 1931 2 Degree Standard Observer']['D65']
    brad = colour.adaptation.matrix_chromatic_adaptation_VonKries(colour.xy_to_XYZ(d65), colour.xy_to_XYZ(d50), 'Bradford')
    return colour.XYZ_to_Lab(np.asarray(rgb) @ M.T @ brad.T, d50)


def boxes(corners, share=0.45):
    """24 measurement boxes (fractions 0…1) from the centres of patches 1, 6, 19, 24; per patch the inner `share`."""
    p1, p6, p19, p24 = [np.array(corners[i:i + 2]) for i in (0, 2, 4, 6)]
    px = np.linalg.norm(p6 - p1) / 5 * share / 2
    py = np.linalg.norm(p19 - p1) / 3 * share / 2
    out = []
    for r in range(4):
        for k in range(6):
            u, v = k / 5, r / 3
            m = (1 - u) * (1 - v) * p1 + u * (1 - v) * p6 + (1 - u) * v * p19 + u * v * p24
            out.append([m[0] - px, m[1] - py, m[0] + px, m[1] + py])
    return out


def patch_means(lin, corners):
    h, w, _ = lin.shape
    return np.array([lin[int(y0 * h):int(np.ceil(y1 * h)), int(x0 * w):int(np.ceil(x1 * w))].reshape(-1, 3).mean(0) for x0, y0, x1, y1 in boxes(corners)])


def load_f32(p):
    b = open(p, 'rb').read()
    w, h = struct.unpack('<II', b[4:12])
    return np.frombuffer(b[12:], '<f4').reshape(h, w, 3).astype(np.float64)


def metrics(lab, ref):
    import colour
    de = colour.delta_E(lab, ref, method='CIE 2000')
    lab_c = lab.copy()
    lab_c[:, 0] = ref[:, 0]
    de_color = colour.delta_E(lab_c, ref, method='CIE 2000')
    C, Cr = np.hypot(lab[:, 1], lab[:, 2]), np.hypot(ref[:, 1], ref[:, 2])
    dL = lab[:, 0] - ref[:, 0]
    r = lambda x: round(float(x), 2)
    return dict(de=de.round(2).tolist(), lab=lab.round(2).tolist(), dL=dL.round(2).tolist(), dC=(C - Cr).round(2).tolist(),
                mean=r(de.mean()), max=r(de.max()), max_patch=int(de.argmax()) + 1, skin=r(de[0:2].mean()),
                saturated=r(de[12:18].mean()), chromatic=r(de[:18].mean()), grey_dL=r(np.abs(dL[18:]).mean()),
                dC_chromatic=r((C - Cr)[:18].mean()), without_dL=r(de_color.mean()))


def normalised(lin, corners, ref):
    """Camera JPEG normalised afterwards like graineo: channel factors neutral on 20–22, patch 22 to target L*."""
    import colour
    pm = patch_means(lin, corners)
    g = pm[19:22]
    k = np.mean(g[:, 1:2] / g, axis=0)  # per channel to green
    pm = pm * k
    y22 = pm[21] @ M[1]
    yt = colour.Lab_to_XYZ(ref[21], colour.CCS_ILLUMINANTS['CIE 1931 2 Degree Standard Observer']['D50'])[1]
    return pm * yt / y22


def color_chart_raw(u, k, z, corners, ref):
    out = u.out / 'color_chart' / k
    out.mkdir(parents=True, exist_ok=True)
    b = boxes(corners)
    r = lambda i: ','.join(f'{v:.5f}' for v in b[i])
    stdout = u.run(['color-chart-raw', u.path(z['file']), out / 'graineo', '--patch22', ';'.join(r(i) for i in (19, 20, 21)),
                     '--patch20', r(21), '--l20', f'{ref[21][0]:.2f}'])
    meta = json.loads([l for l in stdout.splitlines() if l.startswith('COLOR_CHART')][0].split('\t', 1)[1])
    lin = decode(load_f32(out / 'graineo-export.f32'))
    return patch_means(lin, corners), dict(white_balance=meta['white_balance'], exposure_ev=meta['exposure_ev'])


def color_chart_sigma(u, k, z, corners, ref):
    import tifffile
    out = u.out / 'color_chart' / k
    out.mkdir(parents=True, exist_ok=True)
    tif = out / 'graineo.tif'
    log = []

    def run(x, ev):
        wb = [float(np.exp(x[0])), 1.0, float(np.exp(x[1]))]
        u.run(['develop', u.path(z['file']), tif, '--spp', '--baseline', f'--wb-mult={wb[0]:.6f},1,{wb[2]:.6f}', f'--ev={ev:.4f}'])
        pm = patch_means(decode(tifffile.imread(tif).astype(np.float64) / 65535), corners)
        lab = lin_to_lab_d50(pm)
        log.append(dict(wb=wb, ev=round(ev, 4), l22=lab[21].round(2).tolist(), ab_20_22=lab[19:22, 1:].mean(0).round(2).tolist()))
        return pm, lab

    x, ev = np.zeros(2), 0.0
    for _ in range(6):
        pm, lab = run(x, ev)
        f0 = lab[19:22, 1:].mean(0)
        if abs(lab[21][0] - ref[21][0]) < 0.15 and np.all(np.abs(f0) < 0.25):
            break
        J = np.zeros((2, 2))
        for i in range(2):
            dx = np.zeros(2)
            dx[i] = 0.02
            J[:, i] = (run(x + dx, ev)[1][19:22, 1:].mean(0) - f0) / 0.02
        x = x - np.linalg.solve(J, f0)
        Y = lambda L: ((L + 16) / 116) ** 3
        ev += float(np.log2(Y(ref[21][0]) / Y(lab[21][0])))
    tif.unlink()
    return pm, dict(iterations=log, wb_mult=log[-1]['wb'], exposure_ev=log[-1]['ev'])


def color_chart_measure(u):
    from PIL import Image
    c = u.cfg['color_chart']
    names, ref, _ = cc_reference()
    eg, ej = json.loads(u.path(c['corners_graineo']).read_text()), json.loads(u.path(c['corners_jpeg']).read_text())
    res = {}
    for z in tsv(u.path(c['list'])):
        k = z['key']
        try:
            fn = color_chart_sigma if z['kind'] == 'sigma' else color_chart_raw
            pm, meta = fn(u, k, z, eg[k], ref)
            e = dict(camera=z['camera'], kind=z['kind'], source=z.get('source', ''), own=z.get('own', '') == 'yes', graineo=metrics(lin_to_lab_d50(pm), ref), meta=meta)
            if z.get('jpeg', '-') not in ('', '-') and k in ej:
                lin = decode(np.asarray(Image.open(u.path(z['jpeg'])).convert('RGB'), dtype=np.float64) / 255)
                e['jpeg'] = metrics(lin_to_lab_d50(normalised(lin, ej[k], ref)), ref)
            res[k] = e
            g = e['graineo']
            print(f"color_chart {k:20s} ΔE00 {g['mean']:5.2f} max {g['max']:5.2f} ({g['max_patch']}) skin {g['skin']:5.2f} sat. {g['saturated']:5.2f} ΔC {g['dC_chromatic']:+5.1f}"
                  + (f" | JPEG {e['jpeg']['mean']:5.2f}" if 'jpeg' in e else ''), flush=True)
        except Exception as ex:  # noqa: BLE001
            print(f'color_chart {k}: ERROR {ex}', flush=True)
            res[k] = dict(camera=z['camera'], error=str(ex))
        (u.out / 'color_chart' / f'{k}.json').parent.mkdir(parents=True, exist_ok=True)
        (u.out / 'color_chart' / f'{k}.json').write_text(json.dumps(res[k], indent=1, ensure_ascii=False))
    return dict(reference=dict(name='ColorChecker24 - After November 2014 (Lab D50)', patches=names, lab=ref.round(2).tolist()), cameras=res)


# ---------------------------------------------------------------- 3. Non-Sigma against camera JPEG
def rawapp(u, raw, ref, ev, look, tag):
    out = u.out / 'camera_jpeg' / 'tmp'
    out.mkdir(parents=True, exist_ok=True)
    js = out / f'{tag}.json'
    js.write_text(json.dumps({'exposure': ev}))
    args = ['rawapp', raw, out / f'{tag}.jpg', '--settings', js, '--compare', ref, '--camera-look' if look else '--no-look']
    so = u.run(args)
    t = [l for l in so.splitlines() if l.startswith('LRCOMPARE')][0].split('\t')
    name = ([l.split('\t')[1] for l in so.splitlines() if l.startswith('LOOK\t')] or ['neutral'])[0]
    (out / f'{tag}.jpg').unlink(missing_ok=True)
    js.unlink(missing_ok=True)
    return dict(de=float(t[3]), ev_mid=float(t[5]), dL=float(t[6]), dC=float(t[7]), look=name)


def jpeg_one(u, z):
    raw, ref = str(u.path(z['raw'])), z['reference'] if z['reference'] == 'embedded' else str(u.path(z['reference']))
    tag = f"{os.getpid()}-{Path(raw).stem}-{abs(hash(raw)) % 10**6}"
    r = dict(camera=z['camera'], file=Path(raw).name, style=z.get('style', ''), reference='embedded' if ref == 'embedded' else 'JPEG')
    for look in (False, True):
        a = rawapp(u, raw, ref, 0.0, look, tag)
        ev = max(-3.0, min(3.0, -a['ev_mid']))
        b = rawapp(u, raw, ref, ev, look, tag)
        r['look' if look else 'neutral'] = dict(de=b['de'], dL=b['dL'], dC=b['dC'], ev=round(ev, 3), look=b['look'])
    return r


def camera_jpeg_measure(u):
    rows = tsv(u.path(u.cfg['camera_jpeg']['pairs']))
    res = []
    with cf.ThreadPoolExecutor(u.parallel) as ex:
        for z, f in [(z, ex.submit(jpeg_one, u, z)) for z in rows]:
            try:
                r = f.result()
                res.append(r)
                print(f"jpeg {r['camera']:14s} {r['file']:16s} {r['style']:16s} neutral {r['neutral']['de']:5.2f} (ΔC {r['neutral']['dC']:+5.1f}) | Look {r['look']['look']:22s} {r['look']['de']:5.2f}", flush=True)
            except Exception as e:  # noqa: BLE001
                print(f"jpeg {z['raw']}: ERROR {e}", flush=True)
    return res


# ---------------------------------------------------------------- tables
def f2(v):
    return '–' if v is None else f'{v:.2f}'.replace('.', ',')


def fs(v, n=2):
    return f'{v:+.{n}f}'.replace('.', ',')


def table_spp(res):
    ok = [r for r in res if 'error' not in r]
    lines = ['| Group | Camera | Files | of which own | ΔE00 mean | ΔE00 p95 | ΔE00 8×8 | worst file (ΔE00) |', '|---|---|---|---|---|---|---|---|']
    cams = {}
    for r in ok:
        cams.setdefault((r['group'], r['camera']), []).append(r)
    order = {'Merrill': 0, 'Quattro': 1, 'Classic': 2}
    for (g, k), v in sorted(cams.items(), key=lambda x: (order.get(x[0][0], 9), x[0][1])):
        m = lambda key: float(np.mean([r[key] for r in v]))
        w = max(v, key=lambda r: r['de_mean'])
        lines.append(f"| {g} | {k} | {len(v)} | {sum(r['own'] for r in v)} | {f2(m('de_mean'))} | {f2(m('de_p95'))} | {f2(m('de8_mean'))} | {w['name']} ({f2(w['de_mean'])}) |")
    # by ISO: base ISO range (≤ 400) against higher ISO, per camera
    lines += ['', '| Group | Camera | ISO | Files | ΔE00 mean | ΔE00 p95 | ΔE00 8×8 |', '|---|---|---|---|---|---|---|']
    for (g, k), v in sorted(cams.items(), key=lambda x: (order.get(x[0][0], 9), x[0][1])):
        for title, part in (('≤ 400', [r for r in v if (r.get('iso') or 0) <= 400]), ('> 400', [r for r in v if (r.get('iso') or 0) > 400])):
            if part:
                m = lambda key: float(np.mean([r[key] for r in part]))
                isos = sorted({r.get('iso') for r in part if r.get('iso')})
                span = f" ({isos[0]}–{isos[-1]})" if len(isos) > 1 else (f" ({isos[0]})" if isos else '')
                lines.append(f"| {g} | {k} | {title}{span} | {len(part)} | {f2(m('de_mean'))} | {f2(m('de_p95'))} | {f2(m('de8_mean'))} |")
    all_rows = ['', '| File | Camera | ISO | own | ΔE00 mean | p95 | 8×8 | ΔL* | ΔC* | Note |', '|---|---|---|---|---|---|---|---|---|---|']
    for r in res:
        if 'error' in r:
            all_rows.append(f"| {r['name']} | {r['camera']} | | | Error | | | | | {r['error'][:80]} |")
        else:
            all_rows.append(f"| {r['name']} | {r['camera']} | {r.get('iso') or ''} | {'yes' if r['own'] else ''} | {f2(r['de_mean'])} | {f2(r['de_p95'])} | {f2(r['de8_mean'])} | {fs(r['dL'])} | {fs(r['dC'])} | {'; '.join(r['notes'] + ([r['options']] if r['options'] else []))} |")
    return '\n'.join(lines), '\n'.join(all_rows)


def table_color_chart(res):
    z = ['| Camera | Path | Source | ΔE00 mean | Max (patch) | Skin 1–2 | saturated 13–18 | chromatic 1–18 | Grey \\|ΔL*\\| | ΔC* chromatic | without ΔL | Camera JPEG (normalised) |',
         '|---|---|---|---|---|---|---|---|---|---|---|---|']
    for k, e in res['cameras'].items():
        if 'error' in e:
            z.append(f"| {e['camera']} | | | Error: {e['error'][:60]} | | | | | | | | |")
            continue
        g = e['graineo']
        j = e.get('jpeg')
        z.append(f"| {e['camera']} | {'Sigma' if e['kind'] == 'sigma' else 'RAW'} | {'own' if e['own'] else 'third-party'} | {f2(g['mean'])} | {f2(g['max'])} ({g['max_patch']}) | {f2(g['skin'])} | {f2(g['saturated'])} | {f2(g['chromatic'])} | {f2(g['grey_dL'])} | {fs(g['dC_chromatic'], 1)} | {f2(g['without_dL'])} | {f2(j['mean']) if j else '–'} |")
    return '\n'.join(z)


def table_jpeg(res):
    z = ['| Camera | Pairs | Reference | neutral ΔE00 | neutral ΔC* | with camera look ΔE00 | look ΔC* | Pairs with look |', '|---|---|---|---|---|---|---|---|']
    cams = {}
    for r in res:
        cams.setdefault(r['camera'], []).append(r)
    for k, v in sorted(cams.items()):
        m = lambda a, b: float(np.mean([r[a][b] for r in v]))
        with_look = [r for r in v if r['look']['look'] != 'neutral']
        refs = sorted({r['reference'] for r in v})
        z.append(f"| {k} | {len(v)} | {', '.join(refs)} | {f2(m('neutral', 'de'))} | {fs(m('neutral', 'dC'), 1)} | {f2(m('look', 'de'))} | {fs(m('look', 'dC'), 1)} | {len(with_look)} |")
    styles = ['', '| Camera | Picture style | Pairs | neutral ΔE00 | with look ΔE00 | Look |', '|---|---|---|---|---|---|']
    ks = {}
    for r in res:
        ks.setdefault((r['camera'], r['style']), []).append(r)
    for (k, s), v in sorted(ks.items()):
        styles.append(f"| {k} | {s} | {len(v)} | {f2(float(np.mean([r['neutral']['de'] for r in v])))} | {f2(float(np.mean([r['look']['de'] for r in v])))} | {', '.join(sorted({r['look']['look'] for r in v}))} |")
    return '\n'.join(z), '\n'.join(styles)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument('config')
    ap.add_argument('--only', default='spp,color_chart,camera_jpeg')
    ap.add_argument('--redo', action='store_true', help='discard cached results')
    a = ap.parse_args()
    u = Environment(a.config)
    only = set(a.only.split(','))
    prev = u.out / 'result.json'
    result = json.loads(prev.read_text()) if prev.exists() else {}
    if 'spp' in only and 'spp' in u.cfg:
        result['spp'] = spp_measure(u, a.redo)
    if 'color_chart' in only and 'color_chart' in u.cfg:
        result['color_chart'] = color_chart_measure(u)
    if 'camera_jpeg' in only and 'camera_jpeg' in u.cfg:
        result['camera_jpeg'] = camera_jpeg_measure(u)
    result['graineo_dev'] = u.bin
    prev.write_text(json.dumps(result, indent=1, ensure_ascii=False))
    md = ['# Colour measurement – tables', '', '(generated by tools/measure/color-measurement/color_measurement.py)', '']
    if 'spp' in result:
        k, d = table_spp(result['spp'])
        md += ['## 1. Sigma against manufacturer software (SPP export, 16-bit TIFF sRGB)', '', k, '', '### per file', d, '']
    if 'color_chart' in result:
        md += ['## 2. ColorChecker 24 against target values (Lab D50, after Nov. 2014)', '', 'White balance on patches 20–22, exposure on patch 22 (target L*).', '',
               table_color_chart(result['color_chart']), '']
    if 'camera_jpeg' in result:
        k, s = table_jpeg(result['camera_jpeg'])
        md += ['## 3. Non-Sigma against camera JPEG', '', 'Exposure per pair matched to the midtones of the reference.', '', k, s, '']
    (u.out / 'tables.md').write_text('\n'.join(md))
    print(f"done: {u.out / 'tables.md'}")


if __name__ == '__main__':
    main()

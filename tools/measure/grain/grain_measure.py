# SPDX-License-Identifier: GPL-3.0-only
"""Grain strength and grain size per brightness zone in uniform areas.

Usage: grain_measure.py [--zones 10] [--block 32] [--max 3000] OUTPUT.json IMAGE...
Per image: greyscale (gamma-encoded 0..1), high-pass H = image - Gauss(σ=4).
BLOCK×BLOCK blocks; "flat" = low structure (std of the low-pass within the block).
Per block: strength = Std(H) (in code values 0..255), size = half-width of the
radial autocorrelation of H (pixels) and ρ(1) (correlation at 1 px distance).
Per zone (block mean): median over the flattest blocks.
"""
import sys, json, argparse
import numpy as np
from PIL import Image
from scipy.ndimage import gaussian_filter

Image.MAX_IMAGE_PIXELS = None

def load(p, max_edge):
    im = Image.open(p)
    if im.mode in ('I;16', 'I;16B', 'I'):
        a = np.asarray(im, dtype=np.float64) / 65535.0
    else:
        im = im.convert('RGB') if im.mode not in ('L', 'RGB') else im
        a = np.asarray(im, dtype=np.float64)
        if a.ndim == 3:
            a = a.mean(axis=2)
        a = a / (65535.0 if a.max() > 255 else 255.0)
    return a

def acf_measure(h):
    h = h - h.mean()
    n = h.shape[0]
    F = np.fft.fft2(h, s=(2 * n, 2 * n))
    ac = np.real(np.fft.ifft2(F * np.conj(F)))
    ac = np.fft.fftshift(ac)
    c = n
    ac = ac / ac[c, c]
    # radial mean up to 8 px
    ys, xs = np.mgrid[-8:9, -8:9]
    r = np.sqrt(xs ** 2 + ys ** 2)
    sub = ac[c - 8:c + 9, c - 8:c + 9]
    prof = []
    for k in range(0, 9):
        m = (r >= k - 0.5) & (r < k + 0.5)
        prof.append(sub[m].mean())
    prof = np.array(prof)
    rho1 = 0.5 * (ac[c, c + 1] + ac[c + 1, c])
    # half-width radius (linearly interpolated)
    half = 8.0
    for k in range(1, 9):
        if prof[k] < 0.5:
            half = (k - 1) + (prof[k - 1] - 0.5) / (prof[k - 1] - prof[k])
            break
    return rho1, half

def measure(a, block, zones, hp_sigma=4.0):
    tp = gaussian_filter(a, hp_sigma)
    h = a - tp
    H, W = a.shape
    rows = []
    for y in range(0, H - block, block):
        for x in range(0, W - block, block):
            t = tp[y:y + block, x:x + block]
            m = t.mean()
            if m < 0.01 or m > 0.99:
                continue
            structure = t.std()
            hb = h[y:y + block, x:x + block]
            rows.append((m, structure, hb.std(), y, x))
    return rows, h

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument('--zones', type=int, default=10)
    ap.add_argument('--block', type=int, default=32)
    ap.add_argument('--fraction', type=float, default=0.25, help='fraction of the flattest blocks per zone')
    ap.add_argument('--hp', type=float, default=4.0)
    ap.add_argument('out')
    ap.add_argument('images', nargs='+')
    o = ap.parse_args()
    zones = [[] for _ in range(o.zones)]
    for p in o.images:
        try:
            a = load(p, 0)
        except Exception as e:
            print('skipped', p, e, file=sys.stderr)
            continue
        rows, h = measure(a, o.block, o.zones, o.hp)
        for m, s, sd, y, x in rows:
            z = min(int(m * o.zones), o.zones - 1)
            zones[z].append((s, sd, p, y, x))
        print('measured', p, len(rows), file=sys.stderr)
    res = []
    cache = {}
    for zi, lst in enumerate(zones):
        if len(lst) < 5:
            res.append({'zone': zi, 'n': len(lst)})
            continue
        lst.sort(key=lambda r: r[0])
        k = max(5, int(len(lst) * o.fraction))
        sel = lst[:k]
        sds = np.array([r[1] for r in sel]) * 255.0
        rhos, halves = [], []
        for s, sd, p, y, x in sorted(sel[:200], key=lambda r: r[2]):
            if p not in cache:
                cache.clear()
                a = load(p, 0)
                cache[p] = a - gaussian_filter(a, o.hp)
            hb = cache[p][y:y + o.block, x:x + o.block]
            r1, hw = acf_measure(hb)
            rhos.append(r1); halves.append(hw)
        res.append({'zone': zi, 'n': len(lst), 'n_flat': k,
                    'strength': float(np.median(sds)), 'strength_q25': float(np.percentile(sds, 25)), 'strength_q75': float(np.percentile(sds, 75)),
                    'rho1': float(np.median(rhos)), 'halfwidth_px': float(np.median(halves)),
                    'structure_max': float(sel[-1][0] * 255)})
    json.dump({'images': o.images, 'zones': res, 'block': o.block, 'hp': o.hp}, open(o.out, 'w'), indent=1, ensure_ascii=False)
    for e in res:
        print(json.dumps(e, ensure_ascii=False))

if __name__ == '__main__':
    main()

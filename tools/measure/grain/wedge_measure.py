# SPDX-License-Identifier: GPL-3.0-only
"""Per patch of the step wedge (4×4, 600 px): mean (code 0..255), grain strength Std(high-pass σ4) and
grain size (half-width radius of the autocorrelation, ρ1). Usage: wedge_measure.py image.png [out.json]"""
import sys, json, numpy as np
from PIL import Image
from scipy.ndimage import gaussian_filter
sys.path.insert(0, __file__.rsplit('/', 1)[0])
from grain_measure import acf_measure
a = np.asarray(Image.open(sys.argv[1]).convert('RGB'), dtype=np.float64).mean(axis=2) / 255.0
h = a - gaussian_filter(a, 4.0)
evs = [float(x) for x in open(sys.argv[1].rsplit('/', 1)[0] + '/wedge.png.ev.txt' if '/' in sys.argv[1] else 'wedge.png.ev.txt').read().split()]
out = []
for i, ev in enumerate(evs):
    y, x = divmod(i, 4)
    blk = h[y*600+60:(y+1)*600-60, x*600+60:(x+1)*600-60]
    m = a[y*600+60:(y+1)*600-60, x*600+60:(x+1)*600-60].mean()
    rs, hs = [], []
    for by in range(0, 480 - 32, 48):
        for bx in range(0, 480 - 32, 48):
            r1, hw = acf_measure(blk[by:by+32, bx:bx+32]); rs.append(r1); hs.append(hw)
    out.append({'ev': ev, 'mean': m * 255, 'strength': blk.std() * 255, 'rho1': float(np.median(rs)), 'halfwidth_px': float(np.median(hs))})
for o in out:
    print(f"{o['ev']:6.2f} {o['mean']:7.1f} {o['strength']:6.2f} {o['rho1']:6.3f} {o['halfwidth_px']:5.2f}")
if len(sys.argv) > 2:
    json.dump(out, open(sys.argv[2], 'w'), indent=1)

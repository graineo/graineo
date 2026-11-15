# SPDX-License-Identifier: GPL-3.0-only
"""Sigma X3F with graineo-dev develop (default values), eyedropper substitute via wb_mult and exposure via exposure,
iterative on patch 22 (neutral) and patch 20 (L* 81.29). Writes graineo-foveon.f32 (sRGB encoded, 16 bit → f32)."""
import sys, json, glob, subprocess, os, struct
import numpy as np
from PIL import Image
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from measure import F, patch_means, decode, lin_srgb_to_lab

BIN = os.environ.get('GRAINEO_DEV', 'graineo-dev')  # test program (path or in PATH)
S = os.path.join(F, '_work')  # intermediate files (sigma-*.json/tif, data folder)
os.makedirs(S, exist_ok=True)
env = dict(os.environ, GRAINEO_DATA_DIR=os.environ.get('GRAINEO_DATA_DIR', f'{S}/data'), GRAINEO_LIBRARY_NO_SYNC='1', GRAINEO_LIBRARY_SIDECARS='0', GRAINEO_NO_PHOTOKIT='1')
corners = json.load(open(sys.argv[1]))
for k in sys.argv[2:]:
    x3f = glob.glob(f'{F}/{k}/*.X3F')[0]
    log = []
    def run(wb, ev):
        js = f'{S}/sigma-{k}.json'
        json.dump({'wb_mult': [float(v) for v in wb], 'exposure': float(ev)}, open(js, 'w'))
        out = f'{S}/sigma-{k}.tif'
        p = subprocess.run([BIN, 'develop', x3f, out, f'--settings={js}'], capture_output=True, text=True, env=env)
        if p.returncode:
            raise SystemExit(p.stderr[-1500:])
        import tifffile
        a = tifffile.imread(out).astype(np.float64)
        pm = patch_means(decode(a / 65535.0), corners[k])
        l20 = lin_srgb_to_lab(pm[19]); l22 = lin_srgb_to_lab(pm[21])
        log.append(([round(float(v), 4) for v in wb], round(float(ev), 3), l20.round(2).tolist(), l22.round(2).tolist()))
        print(k, log[-1], flush=True)
        return a, l20, l22
    x = np.zeros(2); ev = 0.0
    wbx = lambda x: np.array([np.exp(x[0]), 1.0, np.exp(x[1])])
    for it in range(5):
        a, l20, l22 = run(wbx(x), ev)
        f0 = l22[1:3]
        if abs(l20[0] - 81.29) < 0.15 and np.all(np.abs(f0) < 0.25):
            break
        J = np.zeros((2, 2))
        for i in range(2):
            dx = np.zeros(2); dx[i] = 0.01
            _, _, l = run(wbx(x + dx), ev)
            J[:, i] = (l[1:3] - f0) / 0.01
        x = x - np.linalg.solve(J, f0)
        ev += np.log2(((81.29 + 16) / 116) ** 3 / ((l20[0] + 16) / 116) ** 3) * 0.9
    wb = wbx(x)
    h, w, _ = a.shape
    with open(f'{F}/{k}/graineo-foveon.f32', 'wb') as f:
        f.write(b'GRF1' + struct.pack('<II', w, h)); f.write((a / 65535.0).astype('<f4').tobytes())
    json.dump({'camera': k, 'iterations': log, 'wb_mult': wb.tolist(), 'exposure_ev': ev}, open(f'{F}/{k}/graineo.json', 'w'), indent=1)

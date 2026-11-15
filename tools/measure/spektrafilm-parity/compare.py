# SPDX-License-Identifier: GPL-3.0-only
# Compare two runs tap by tap. Usage: compare.py prefixA prefixB W H [label]
import sys, os, numpy as np, colour
a, b, W, H = sys.argv[1], sys.argv[2], int(sys.argv[3]), int(sys.argv[4])
P = int(sys.argv[5]) if len(sys.argv) > 5 else 16
def load(p):
    return np.fromfile(p, '<f8').reshape(H, W, 3) if os.path.exists(p) else None
def patches(img):
    r = img.reshape(H//P, P, W//P, P, 3)[:, P//4:3*P//4, :, P//4:3*P//4, :]
    return r.mean(axis=(1, 3))
res = {}
for t in ['log_e_film', 'cmy_film', 'log_e_print', 'cmy_print', 'rgb_out']:
    A, B = load(f'{a}.{t}.f64'), load(f'{b}.{t}.f64')
    if A is None or B is None: continue
    d = np.abs(patches(A) - patches(B))
    line = f'{t:12s} patch mean |Δ| max {d.max():.4g} mean {d.mean():.4g}'
    if t == 'rgb_out':
        def lab(x):
            x = np.clip(x, 0, None)
            lin = colour.cctf_decoding(x, 'sRGB')
            return colour.XYZ_to_Lab(colour.RGB_to_XYZ(lin, 'sRGB'), colour.CCS_ILLUMINANTS['CIE 1931 2 Degree Standard Observer']['D65'])
        de = colour.delta_E(lab(patches(A)), lab(patches(B)), method='CIE 2000')
        rows = ['Grey wedge', 'ColorChecker', 'Hue ring 90%', 'Hue ring 50%', 'Highlights']
        line += f'\n   ΔE00 max {de.max():.2f} mean {de.mean():.2f}; max per row: ' + ', '.join(f'{rows[i]} {de[i].max():.2f}' for i in range(de.shape[0]))
        wi = np.unravel_index(de.argmax(), de.shape)
        line += f'\n   worst patch row {wi[0]} column {wi[1]}: A {np.round(patches(A)[wi],4)} B {np.round(patches(B)[wi],4)}'
    print(line)

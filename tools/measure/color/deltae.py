"""ΔE00 of two images or two folders (16-bit TIFF, sRGB), whole and as 8×8 block means.

Usage:   python deltae.py A.tif B.tif
         python deltae.py FOLDER_A FOLDER_B     (files of the same name, *.tif)
Per pair: ΔE00 mean (full resolution, 16 px border cropped) and ΔE00 of the 8×8 block means
(suppresses noise and grain, shows colour and tone differences). Same size is required.
Method: sRGB → XYZ (D65) → Lab, ΔE CIEDE2000 (Sharma et al. 2005). Computation core: demath.py.
Dependencies: numpy, tifffile.
"""
# SPDX-License-Identifier: GPL-3.0-only
import sys
from pathlib import Path
import numpy as np
import tifffile

sys.path.insert(0, str(Path(__file__).resolve().parent))
from demath import to_lab, de2000, block


def pair(pa, pb):
    a = tifffile.imread(pa)[..., :3]
    b = tifffile.imread(pb)[..., :3]
    if a.shape != b.shape:
        return None
    a, b = a[16:-16, 16:-16], b[16:-16, 16:-16]
    full = de2000(*to_lab(a)[:3], *to_lab(b)[:3])
    bl = de2000(*to_lab(block(a))[:3], *to_lab(block(b))[:3])
    return float(full.mean()), float(bl.mean())


def main():
    if len(sys.argv) != 3:
        sys.exit(__doc__)
    a, b = Path(sys.argv[1]), Path(sys.argv[2])
    if a.is_dir():
        pairs = [(f, b / f.name) for f in sorted(a.glob('*.tif')) if (b / f.name).exists()]
    else:
        pairs = [(a, b)]
    res = []
    for pa, pb in pairs:
        r = pair(pa, pb)
        if r is None:
            print(f'{pa.stem:32s} sizes differ')
            continue
        res.append(r)
        print(f'{pa.stem:32s} {r[0]:6.2f} {r[1]:6.2f}')
    if res:
        print('MEAN full / 8x8:', round(np.mean([r[0] for r in res]), 3), round(np.mean([r[1] for r in res]), 3))


if __name__ == '__main__':
    main()

# SPDX-License-Identifier: GPL-3.0-only
"""Image metrics for evaluating automatic tone adjustments (sRGB images: JPEG, PNG, 8/16-bit TIFF).

Usage: python -I metrics.py IMAGE…   (one JSON line per image)

Metrics: mean and median L*, 1 % / 99 % percentiles of encoded luma, clip share in % (any channel >= 0.995),
near-black share in % (luma < 0.02), global contrast (standard deviation of L*), mean chroma C*ab and mean
a*/b* (colour cast). Images are subsampled to about 800 px on the long edge.
"""
import json
import sys

import numpy as np
import tifffile
from PIL import Image


def load(path):
    if path.lower().endswith(('.tif', '.tiff')):
        a = tifffile.imread(path).astype(np.float64)
        return a[..., :3] / (65535.0 if a.max() > 255 else 255.0)
    return np.asarray(Image.open(path).convert('RGB'), dtype=np.float64) / 255.0


def to_lin(v):
    return np.where(v <= 0.04045, v / 12.92, ((v + 0.055) / 1.055) ** 2.4)


def lab(rgb):
    lin = to_lin(np.clip(rgb, 0, 1))
    m = np.array([[0.4124, 0.3576, 0.1805], [0.2126, 0.7152, 0.0722], [0.0193, 0.1192, 0.9505]])
    t = (lin @ m.T) / np.array([0.95047, 1.0, 1.08883])
    f = np.where(t > 216 / 24389, np.cbrt(t), (24389 / 27 * t + 16) / 116)
    return 116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])


def metrics(path, max_edge=800):
    rgb = load(path)
    s = max(1, max(rgb.shape[:2]) // max_edge)
    rgb = rgb[::s, ::s]
    L, a, b = lab(rgb)
    luma = rgb @ np.array([0.2126, 0.7152, 0.0722])
    return {
        'L_mean': float(L.mean()),
        'L_med': float(np.median(L)),
        'y01': float(np.percentile(luma, 1)),
        'y99': float(np.percentile(luma, 99)),
        'L_std': float(L.std()),
        'clip': float((rgb.max(-1) >= 0.995).mean() * 100),
        'black': float((luma < 0.02).mean() * 100),
        'C_mean': float(np.hypot(a, b).mean()),
        'a_mean': float(a.mean()),
        'b_mean': float(b.mean()),
    }


if __name__ == '__main__':
    for p in sys.argv[1:]:
        print(json.dumps({'file': p, **metrics(p)}))

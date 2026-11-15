# SPDX-License-Identifier: GPL-3.0-only
"""Crop of an image with a coordinate grid (in fractions) for reading off the colour chart corners.
python crop.py IMAGE OUT x0 y0 x1 y1 [corners x1,y1,x6,y6,x19,y19,x24,y24]
Image: JPEG or GRF1 f32 (extended sRGB)."""
import sys, struct
import numpy as np
from PIL import Image, ImageDraw


def load(p):
    if p.endswith('.f32'):
        b = open(p, 'rb').read()
        w, h = struct.unpack('<II', b[4:12])
        a = np.frombuffer(b[12:], '<f4').reshape(h, w, 3)
        if p.endswith('-lin.f32') or p.endswith('-std.f32'):
            a = np.sign(a) * np.where(np.abs(a) <= 0.0031308, np.abs(a) * 12.92, 1.055 * np.abs(a) ** (1 / 2.4) - 0.055)
        return Image.fromarray((np.clip(a, 0, 1) * 255 + 0.5).astype(np.uint8))
    return Image.open(p).convert('RGB')


def patch_boxes(c, box=0.45):
    """c: centres of patches 1, 6, 19, 24 (x,y fractions) → 24 rectangles (centre, half width in fractions)"""
    p1, p6, p19, p24 = [np.array(c[i:i + 2]) for i in (0, 2, 4, 6)]
    out = []
    for r in range(4):
        for k in range(6):
            u, v = k / 5, r / 3
            m = (1 - u) * (1 - v) * p1 + u * (1 - v) * p6 + (1 - u) * v * p19 + u * v * p24
            out.append(m)
    pitch_x = np.linalg.norm(p6 - p1) / 5
    pitch_y = np.linalg.norm(p19 - p1) / 3
    return out, pitch_x * box / 2, pitch_y * box / 2


if __name__ == '__main__':
    img = load(sys.argv[1])
    W, H = img.size
    x0, y0, x1, y1 = map(float, sys.argv[3:7])
    cr = img.crop((int(x0 * W), int(y0 * H), int(x1 * W), int(y1 * H)))
    s = 1000 / cr.size[0]
    cr = cr.resize((1000, int(cr.size[1] * s)))
    d = ImageDraw.Draw(cr)
    tx = lambda x: (x - x0) * W * s
    ty = lambda y: (y - y0) * H * s
    if len(sys.argv) > 7:
        c = [float(v) for v in sys.argv[7].split(',')]
        ms, hx, hy = patch_boxes(c)
        for i, m in enumerate(ms):
            d.rectangle([tx(m[0] - hx), ty(m[1] - hy), tx(m[0] + hx), ty(m[1] + hy)], outline=(255, 255, 255))
            d.text((tx(m[0] - hx) + 2, ty(m[1] - hy) + 1), str(i + 1), fill=(255, 255, 255))
    else:
        step = 0.01 if x1 - x0 > 0.15 else 0.005
        for g in np.arange(np.ceil(x0 / step) * step, x1, step):
            d.line([tx(g), 0, tx(g), cr.size[1]], fill=(255, 0, 0) if round(g / step) % 5 == 0 else (90, 90, 90))
            if round(g / step) % 5 == 0:
                d.text((tx(g) + 2, 2), f'{g:.3f}', fill=(255, 255, 0))
        for g in np.arange(np.ceil(y0 / step) * step, y1, step):
            d.line([0, ty(g), cr.size[0], ty(g)], fill=(255, 0, 0) if round(g / step) % 5 == 0 else (90, 90, 90))
            if round(g / step) % 5 == 0:
                d.text((2, ty(g) + 2), f'{g:.3f}', fill=(255, 255, 0))
    cr.save(sys.argv[2], quality=90)

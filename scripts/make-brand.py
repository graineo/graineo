#!/usr/bin/env python3
"""Generates the graineo brand as vector graphics (SVG) and all raster images from them.

Usage (once, when the brand changes):
    python3 -m venv /tmp/brand && /tmp/brand/bin/pip install fonttools
    /tmp/brand/bin/python scripts/make-brand.py
Also needs rsvg-convert, iconutil (macOS) and magick (ImageMagick) in the PATH.

Brand values (set on 2026-11-12):
- App icon (changed on 2026-11-13): background #111214, aperture with size 0.87, no ring (0), gap 0.005,
  opening 0.22, seams 0.033, rotation 48°, saturation 1.3. The wordmark uses the same aperture.
- Aperture: six equal blades between a circle and a hexagonal opening; colours (before saturation)
  blue #3E6FB0 at the top, green #6E9E6A in the middle, red #C8553D at the bottom.
- Wordmark: "graineo" in Syne Medium (OFL); the last "o" is the regular Syne "o", with the aperture inside.

Result: crates/app/assets/brand/*.svg (sources), crates/app/assets/{AppIcon.icns,graineo.ico,icon-1024.png,
logo-64.png,wordmark.png} and crates/app/assets/brand/favicon-*.png.
"""
import math
import os
import shutil
import subprocess
import sys
import tempfile

from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.pens.boundsPen import BoundsPen
from fontTools.pens.recordingPen import RecordingPen
from fontTools.ttLib import TTFont

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "crates/app/assets")
BRAND = os.path.join(ASSETS, "brand")
FONT = os.path.join(ASSETS, "fonts/Syne-Medium.ttf")

BG = "#111214"
OFF = "#F2EFE6"  # text and ring on a dark background
INK = "#16171A"  # text on a light background
COLORS = ["#3E6FB0", "#6E9E6A", "#C8553D"]  # blue at the top, green in the middle, red at the bottom
ICON = dict(size=0.87, ring=0.0, opacity=1.0, gap=0.005, open=0.22, seam=0.033, rotation=48.0, sat=1.3)
WORD = dict(gap=0.005, open=0.22, seam=0.033, rotation=48.0, sat=1.3)


def sat(hex_, s):
    n = int(hex_[1:], 16)
    r, g, b = n >> 16 & 255, n >> 8 & 255, n & 255
    l = (r + g + b) / 3
    r, g, b = (max(0, min(255, round(l + (c - l) * s))) for c in (r, g, b))
    return "#%02X%02X%02X" % (r, g, b)


def f(v):
    return f"{v:.2f}".rstrip("0").rstrip(".")


def blades(cx, cy, R, r, rot_deg):
    """Six blades between a circle (radius R) and a hexagonal opening (circumradius r), sorted from top to bottom.
    Returns (path, colour index): the upper two blue, the middle two green, the lower two red."""
    V = [(cx + r * math.cos(math.radians(rot_deg + k * 60)), cy + r * math.sin(math.radians(rot_deg + k * 60))) for k in range(6)]
    P = []
    for k in range(6):
        A, B = V[k], V[(k + 1) % 6]
        d = (B[0] - A[0], B[1] - A[1])
        fx, fy = B[0] - cx, B[1] - cy
        a2 = d[0] ** 2 + d[1] ** 2
        b2 = 2 * (fx * d[0] + fy * d[1])
        c2 = fx * fx + fy * fy - R * R
        t = (-b2 + math.sqrt(b2 * b2 - 4 * a2 * c2)) / (2 * a2)
        P.append((B[0] + t * d[0], B[1] + t * d[1]))
    out = []
    for k in range(6):
        A, B, p, q = V[k], V[(k + 1) % 6], P[k], P[(k + 5) % 6]
        d = math.atan2(q[1] - cy, q[0] - cx) - math.atan2(p[1] - cy, p[0] - cx)
        while d > math.pi:
            d -= 2 * math.pi
        while d <= -math.pi:
            d += 2 * math.pi
        cyc = (A[1] + B[1] + p[1] + q[1]) / 4
        path = (f"M{f(A[0])},{f(A[1])} L{f(B[0])},{f(B[1])} L{f(p[0])},{f(p[1])} "
                f"A{f(R)} {f(R)} 0 0 {1 if d > 0 else 0} {f(q[0])},{f(q[1])} Z")
        out.append((cyc, path))
    out.sort(key=lambda x: x[0])
    return [(p, i // 2) for i, (_, p) in enumerate(out)]


def aperture_solid(cx, cy, R, p, bg, mono=None):
    """Aperture on a solid background: seams as strokes in the background colour (as in the workshop)."""
    seam = p["seam_abs"]
    s = [f'<circle cx="{f(cx)}" cy="{f(cy)}" r="{f(R)}" fill="{bg}"/>']
    for path, ci in blades(cx, cy, R, R * p["open"], p["rotation"]):
        col = mono or sat(COLORS[ci], p["sat"])
        s.append(f'<path d="{path}" fill="{col}" stroke="{bg}" stroke-width="{f(seam)}" stroke-linejoin="round"/>')
    return "".join(s)


def aperture_cut(cx, cy, R, p, mask_id, mono=None):
    """Aperture for any background: opening and seams are transparent (mask)."""
    seam = p["seam_abs"]
    bl = blades(cx, cy, R, R * p["open"], p["rotation"])
    e = R + 2 * seam  # set the mask area explicitly: the default (-10 %…120 % of the area) cuts off negative coordinates
    m = [f'<mask id="{mask_id}" maskUnits="userSpaceOnUse" x="{f(cx - e)}" y="{f(cy - e)}" width="{f(2 * e)}" height="{f(2 * e)}">']
    m.append(f'<circle cx="{f(cx)}" cy="{f(cy)}" r="{f(R + seam)}" fill="#fff"/>')
    for path, _ in bl:
        m.append(f'<path d="{path}" fill="none" stroke="#000" stroke-width="{f(seam)}" stroke-linejoin="round"/>')
    m.append("</mask>")
    g = [f'<g mask="url(#{mask_id})">']
    for path, ci in bl:
        col = mono or sat(COLORS[ci], p["sat"])
        g.append(f'<path d="{path}" fill="{col}"/>')
    g.append("</g>")
    return "".join(m), "".join(g)


def icon_svg(pad=100, radius=185, shade=True):
    """App icon 1024 × 1024. pad=100: macOS grid (icon with margin); pad=0: borderless (Windows, favicon)."""
    s = 1024
    body = s - 2 * pad
    o = body * ICON["size"]
    c = s / 2
    t = ICON["ring"] * o
    R = max(1, o / 2 - t - ICON["gap"] * o)
    p = dict(ICON, seam_abs=ICON["seam"] * o)
    rx = radius * body / 824
    out = [f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {s} {s}" width="{s}" height="{s}">']
    out.append('<defs><linearGradient id="sh" x1="0" y1="0" x2="0" y2="1">'
               '<stop offset="0" stop-color="#fff" stop-opacity=".06"/><stop offset="1" stop-color="#000" stop-opacity=".12"/>'
               '</linearGradient></defs>')
    out.append(f'<rect x="{pad}" y="{pad}" width="{body}" height="{body}" rx="{f(rx)}" fill="{BG}"/>')
    if shade:
        out.append(f'<rect x="{pad}" y="{pad}" width="{body}" height="{body}" rx="{f(rx)}" fill="url(#sh)"/>')
    if t > 0:
        out.append(f'<circle cx="{c}" cy="{c}" r="{f(o / 2 - t / 2)}" fill="none" stroke="{OFF}" stroke-opacity="{ICON["opacity"]}" stroke-width="{f(t)}"/>')
    out.append(aperture_solid(c, c, R, p, BG))
    out.append("</svg>")
    return "".join(out)


def mark_svg():
    """Only the "o" with the aperture (ring as in the icon), transparent background: logo at the top left."""
    s = 256
    c = s / 2
    o = s
    t = 0.06 * o if ICON["ring"] > 0 else 0.0  # ring slightly bolder than in the icon so it stays visible at 18 pt
    R = o / 2 - t - ICON["gap"] * o
    p = dict(ICON, seam_abs=0.034 * o)
    mask, g = aperture_cut(c, c, R, p, "m")
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {s} {s}" width="{s}" height="{s}"><defs>{mask}</defs>'
            + (f'<circle cx="{c}" cy="{c}" r="{f(o / 2 - t / 2)}" fill="none" stroke="{OFF}" stroke-opacity="0.85" stroke-width="{f(t)}"/>' if t > 0 else '')
            + f'{g}</svg>')


def glyph_path(gs, name, dx):
    pen = SVGPathPen(gs)
    # font coordinates (y up) → SVG (y down): y' = -y, baseline at y = 0
    gs[name].draw(TransformPen(pen, (1, 0, 0, -1, dx, 0)))
    return pen.getCommands()


def counter_of_o(gs):
    """Inner space (counter) of the "o": centre and half width/height in font units (y up)."""
    rec = RecordingPen()
    gs["o"].draw(rec)
    contours, cur = [], []
    for op, a in rec.value:
        if op == "moveTo":
            cur = [a[0]]
        elif op in ("lineTo", "qCurveTo", "curveTo"):
            cur += [q for q in a if q]
        elif op in ("closePath", "endPath"):
            contours.append(cur)
    boxes = []
    for c in contours:
        xs, ys = [q[0] for q in c], [q[1] for q in c]
        boxes.append((min(xs), min(ys), max(xs), max(ys)))
    boxes.sort(key=lambda b: (b[2] - b[0]) * (b[3] - b[1]))
    x0, y0, x1, y1 = boxes[0]  # smallest contour = counter
    ox0, oy0, ox1, oy1 = boxes[-1]
    return ((x0 + x1) / 2, (y0 + y1) / 2, (x1 - x0) / 2, (y1 - y0) / 2, oy1 - oy0)


def wordmark_svg(ink, mono=None, mask_id="wm"):
    font = TTFont(FONT)
    gs = font.getGlyphSet()
    cmap = font.getBestCmap()
    hmtx = font["hmtx"]
    x = 0
    paths = []
    o_x = 0
    for ch in "graineo":
        g = cmap[ord(ch)]
        paths.append(glyph_path(gs, g, x))
        o_x = x
        x += hmtx[g][0]
    cx, cy, hw, hh, oh = counter_of_o(gs)
    R = min(hw, hh) - WORD["gap"] * oh
    p = dict(WORD, seam_abs=WORD["seam"] * oh)
    mask, g = aperture_cut(o_x + cx, -cy, R, p, mask_id, mono)
    # box: from the ascender (dot of the "i") to the descender of the "g", with some margin
    b = BoundsPen(gs)
    for ch in "graineo":
        gs[cmap[ord(ch)]].draw(b)
    bx0, by0, bx1, by1 = b.bounds
    m = 20
    vx, vy, vw, vh = bx0 - m, -by1 - m, (x - bx0) + 2 * m, (by1 - by0) + 2 * m
    return (f'<svg xmlns="http://www.w3.org/2000/svg" viewBox="{f(vx)} {f(vy)} {f(vw)} {f(vh)}" '
            f'width="{f(vw)}" height="{f(vh)}"><defs>{mask}</defs>'
            f'<path d="{" ".join(paths)}" fill="{ink}"/>{g}</svg>'), (vx, vy, vw, vh)


def write(path, text):
    with open(path, "w") as fh:
        fh.write(text + "\n")


def rsvg(svg, png, w, h=None):
    args = ["rsvg-convert", "-w", str(w)] + (["-h", str(h)] if h else []) + ["-o", png, svg]
    subprocess.run(args, check=True)


def main():
    for tool in ("rsvg-convert", "magick"):
        if not shutil.which(tool):
            sys.exit(f"{tool} is missing")
    os.makedirs(BRAND, exist_ok=True)
    write(os.path.join(BRAND, "icon.svg"), icon_svg())
    write(os.path.join(BRAND, "icon-square.svg"), icon_svg(pad=0, radius=150, shade=False))
    write(os.path.join(BRAND, "mark.svg"), mark_svg())
    dark, box = wordmark_svg(OFF, mask_id="wd")
    write(os.path.join(BRAND, "wordmark-dark.svg"), dark)
    write(os.path.join(BRAND, "wordmark-light.svg"), wordmark_svg(INK, mask_id="wl")[0])
    write(os.path.join(BRAND, "wordmark-mono.svg"), wordmark_svg("#000", mono="#000", mask_id="wm")[0])

    tmp = tempfile.mkdtemp()
    # macOS: iconset of all sizes → AppIcon.icns
    iconset = os.path.join(tmp, "AppIcon.iconset")
    os.makedirs(iconset)
    for size in (16, 32, 128, 256, 512):
        rsvg(os.path.join(BRAND, "icon.svg"), os.path.join(iconset, f"icon_{size}x{size}.png"), size)
        rsvg(os.path.join(BRAND, "icon.svg"), os.path.join(iconset, f"icon_{size}x{size}@2x.png"), size * 2)
    if shutil.which("iconutil"):
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", os.path.join(ASSETS, "AppIcon.icns")], check=True)
    rsvg(os.path.join(BRAND, "icon.svg"), os.path.join(ASSETS, "icon-1024.png"), 1024)
    # Windows: borderless icon, several sizes in one .ico
    sizes = (16, 24, 32, 48, 64, 128, 256)
    pngs = []
    for size in sizes:
        q = os.path.join(tmp, f"sq{size}.png")
        rsvg(os.path.join(BRAND, "icon-square.svg"), q, size)
        pngs.append(q)
    subprocess.run(["magick"] + pngs + [os.path.join(ASSETS, "graineo.ico")], check=True)
    for size in (16, 32, 180):
        rsvg(os.path.join(BRAND, "icon-square.svg"), os.path.join(BRAND, f"favicon-{size}.png"), size)
    # logo at the top left and wordmark (start window)
    rsvg(os.path.join(BRAND, "mark.svg"), os.path.join(ASSETS, "logo-64.png"), 64)
    rsvg(os.path.join(BRAND, "wordmark-dark.svg"), os.path.join(ASSETS, "wordmark.png"), 960)
    shutil.rmtree(tmp)
    vx, vy, vw, vh = box
    # position of the baseline in the wordmark (for brand.rs: to align the subtitle)
    print(f"Wordmark: box {vw:.0f} × {vh:.0f} units, baseline at {-vy / vh:.4f} of the height, left {-vx / vw:.4f}")


if __name__ == "__main__":
    main()

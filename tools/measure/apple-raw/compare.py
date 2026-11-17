# SPDX-License-Identifier: GPL-3.0-only
"""compare.py - compare developments of the same RAW file from different pipelines.

Measurement tool (not part of the app). Typical inputs: the macOS system RAW engine via `apple-raw develop`
(this folder) and graineo via `graineo-dev raw`/`graineo-dev rawapp`, plus the embedded camera preview
(`apple-raw preview`) as reference for colour and tone.

Usage:
  python -I compare.py colour  PREVIEW.png NAME=IMAGE [NAME=IMAGE …]
      Mean/median/p90 CIEDE2000 of every image against the embedded preview (images are resampled to the
      preview size, 4 % border ignored, small translation search). Display-referred sRGB images expected.

  python -I compare.py detail  REF_NAME=IMAGE NAME=IMAGE [NAME=IMAGE …] [--sheet OUT.png] [--crop 320]
      Structure metrics at full resolution. Every image is first registered to the first one (integer
      translation, phase correlation) and tone-matched to it (per-channel monotonic LUT from quantiles), so
      that only local structure differs, not tone curve or colour rendering. Then per image:
        detail   mean Laplacian energy in the 15 % most textured tiles of the reference (relative to reference)
        hf_chroma  chroma energy of the high-pass (Cb/Cr) relative to luma high-pass energy, summed over textured
                 near-neutral tiles: false colour / maze / moire plus chroma noise (lower = fewer colour artefacts)
        noise    std of high-passed luma in the 5 % flattest dark tiles (relative to reference)
      `--sheet` writes side-by-side crops (most textured tile, worst false-colour tile, flattest dark tile).

Linear inputs (half-float TIFF from `apple-raw --mode neutral`) are encoded with the sRGB curve before
tone matching. Source of the CIEDE2000 formula: Sharma, Wu, Dalal (2005).
"""
import sys

import numpy as np
import tifffile
from PIL import Image, ImageDraw
from scipy import ndimage

# ---------------------------------------------------------------- loading

REC2020_TO_SRGB = np.array([[1.6605, -0.5876, -0.0728], [-0.1246, 1.1329, -0.0083], [-0.0182, -0.1006, 1.1187]])


def srgb_oetf(x):
    x = np.clip(x, 0, 1)
    return np.where(x <= 0.0031308, 12.92 * x, 1.055 * np.power(x, 1 / 2.4) - 0.055)


def load(path):
    """Return float32 RGB 0…1 (display encoded sRGB)."""
    if path.lower().endswith((".tif", ".tiff")):
        a = tifffile.imread(path)
        a = a[..., :3]
        if a.dtype == np.float16 or a.dtype == np.float32:
            # linear Rec. 2020 from apple-raw (neutral / linear-default): to linear sRGB, then encode
            lin = a.astype(np.float32) @ REC2020_TO_SRGB.T.astype(np.float32)
            return srgb_oetf(lin).astype(np.float32)
        return (a.astype(np.float32) / 65535.0).astype(np.float32)
    im = Image.open(path).convert("RGB")
    return np.asarray(im, dtype=np.float32) / 255.0


def parse(items):
    out = []
    for it in items:
        name, _, path = it.partition("=")
        out.append((name, path))
    return out

# ---------------------------------------------------------------- colour


def srgb_to_lab(rgb):
    c = np.where(rgb <= 0.04045, rgb / 12.92, ((rgb + 0.055) / 1.055) ** 2.4)
    m = np.array([[0.4124564, 0.3575761, 0.1804375], [0.2126729, 0.7151522, 0.0721750], [0.0193339, 0.1191920, 0.9503041]])
    xyz = c @ m.T / np.array([0.95047, 1.0, 1.08883])
    f = np.where(xyz > 216 / 24389, np.cbrt(xyz), (24389 / 27 * xyz + 16) / 116)
    return np.stack([116 * f[..., 1] - 16, 500 * (f[..., 0] - f[..., 1]), 200 * (f[..., 1] - f[..., 2])], -1)


def de2000(l1, l2):
    L1, a1, b1 = l1[..., 0], l1[..., 1], l1[..., 2]
    L2, a2, b2 = l2[..., 0], l2[..., 1], l2[..., 2]
    C1, C2 = np.hypot(a1, b1), np.hypot(a2, b2)
    Cm = (C1 + C2) / 2
    G = 0.5 * (1 - np.sqrt(Cm**7 / (Cm**7 + 25**7)))
    a1p, a2p = (1 + G) * a1, (1 + G) * a2
    C1p, C2p = np.hypot(a1p, b1), np.hypot(a2p, b2)
    h1p, h2p = np.degrees(np.arctan2(b1, a1p)) % 360, np.degrees(np.arctan2(b2, a2p)) % 360
    dLp, dCp = L2 - L1, C2p - C1p
    dh = h2p - h1p
    dh = np.where(dh > 180, dh - 360, np.where(dh < -180, dh + 360, dh))
    dh = np.where(C1p * C2p == 0, 0, dh)
    dHp = 2 * np.sqrt(C1p * C2p) * np.sin(np.radians(dh / 2))
    Lm, Cmp = (L1 + L2) / 2, (C1p + C2p) / 2
    hs = h1p + h2p
    hm = np.where(C1p * C2p == 0, hs, np.where(np.abs(h1p - h2p) <= 180, hs / 2, np.where(hs < 360, (hs + 360) / 2, (hs - 360) / 2)))
    T = 1 - 0.17 * np.cos(np.radians(hm - 30)) + 0.24 * np.cos(np.radians(2 * hm)) + 0.32 * np.cos(np.radians(3 * hm + 6)) - 0.20 * np.cos(np.radians(4 * hm - 63))
    dth = 30 * np.exp(-(((hm - 275) / 25) ** 2))
    Rc = 2 * np.sqrt(Cmp**7 / (Cmp**7 + 25**7))
    Sl = 1 + 0.015 * (Lm - 50) ** 2 / np.sqrt(20 + (Lm - 50) ** 2)
    Sc, Sh = 1 + 0.045 * Cmp, 1 + 0.015 * Cmp * T
    Rt = -np.sin(np.radians(2 * dth)) * Rc
    return np.sqrt((dLp / Sl) ** 2 + (dCp / Sc) ** 2 + (dHp / Sh) ** 2 + Rt * (dCp / Sc) * (dHp / Sh))


def resize(a, w, h):
    im = Image.fromarray((np.clip(a, 0, 1) * 255 + 0.5).astype(np.uint8))
    return np.asarray(im.resize((w, h), Image.Resampling.BOX if (im.width > w) else Image.Resampling.BICUBIC), dtype=np.float32) / 255.0


def colour(argv):
    ref = load(argv[0])
    h, w = ref.shape[:2]
    lref = srgb_to_lab(ref)
    b = int(0.04 * min(h, w))
    print("name\tdE00_mean\tdE00_median\tdE00_p90\tdL_mean\tshift")
    for name, path in parse(argv[1:]):
        img = load(path)
        if (img.shape[0] > img.shape[1]) != (h > w):
            print(f"{name}\torientation differs ({img.shape[1]}x{img.shape[0]} vs {w}x{h})")
            continue
        img = resize(img, w, h)
        lab = srgb_to_lab(img)
        best = None
        for dy in range(-3, 4):
            for dx in range(-3, 4):
                a = lref[b:h - b, b:w - b]
                c = lab[b + dy:h - b + dy, b + dx:w - b + dx]
                d = de2000(a[::4, ::4], c[::4, ::4]).mean()
                if best is None or d < best[0]:
                    best = (d, dy, dx)
        _, dy, dx = best
        d = de2000(lref[b:h - b, b:w - b], lab[b + dy:h - b + dy, b + dx:w - b + dx])
        dl = (lab[b + dy:h - b + dy, b + dx:w - b + dx, 0] - lref[b:h - b, b:w - b, 0]).mean()
        print(f"{name}\t{d.mean():.2f}\t{np.median(d):.2f}\t{np.percentile(d, 90):.2f}\t{dl:+.1f}\t{dx},{dy}")

# ---------------------------------------------------------------- structure


def luma(a):
    return a @ np.array([0.2126, 0.7152, 0.0722], dtype=np.float32)


def register(ref, img):
    """Integer translation of img relative to ref (phase correlation on a central luma crop)."""
    h, w = min(ref.shape[0], img.shape[0]), min(ref.shape[1], img.shape[1])
    s = min(2048, h - 64, w - 64)
    y0, x0 = (h - s) // 2, (w - s) // 2
    A = luma(ref[y0:y0 + s, x0:x0 + s])
    B = luma(img[y0:y0 + s, x0:x0 + s])
    F = np.fft.fft2(A - A.mean()) * np.conj(np.fft.fft2(B - B.mean()))
    r = np.fft.ifft2(F / (np.abs(F) + 1e-9)).real
    dy, dx = np.unravel_index(np.argmax(r), r.shape)
    dy = dy - s if dy > s // 2 else dy
    dx = dx - s if dx > s // 2 else dx
    return -int(dy), -int(dx)


def overlap(ref, img, dy, dx):
    """Common area: ref[y, x] corresponds to img[y + dy, x + dx]."""
    y0, x0 = max(0, -dy), max(0, -dx)
    y1 = min(ref.shape[0], img.shape[0] - dy)
    x1 = min(ref.shape[1], img.shape[1] - dx)
    return ref[y0:y1, x0:x1], img[y0 + dy:y1 + dy, x0 + dx:x1 + dx]


def tone_match(src, ref, n=512):
    """Per-channel monotonic mapping of src onto ref via matched quantiles."""
    out = np.empty_like(src)
    q = np.linspace(0, 100, n)
    for c in range(3):
        qs = np.percentile(src[::3, ::3, c], q)
        qr = np.percentile(ref[::3, ::3, c], q)
        qs, idx = np.unique(qs, return_index=True)
        out[..., c] = np.interp(src[..., c], qs, qr[idx])
    return out


def hp(x, s=1.5):
    return x - ndimage.gaussian_filter(x, s)


def ycc(a):
    y = luma(a)
    return y, a[..., 2] - y, a[..., 0] - y


def tiles(shape, t):
    for y in range(0, shape[0] - t + 1, t):
        for x in range(0, shape[1] - t + 1, t):
            yield y, x


def detail(argv):
    sheet = None
    crop = 320
    rest = []
    i = 0
    while i < len(argv):
        if argv[i] == "--sheet":
            sheet = argv[i + 1]
            i += 2
        elif argv[i] == "--crop":
            crop = int(argv[i + 1])
            i += 2
        else:
            rest.append(argv[i])
            i += 1
    items = parse(rest)
    ref_name, ref_path = items[0]
    ref = load(ref_path)
    imgs = [(ref_name, ref, (0, 0))]
    for name, path in items[1:]:
        img = load(path)
        if img.shape[:2] != ref.shape[:2] and img.shape[:2] == ref.shape[1::-1]:
            print(f"{name}: orientation differs, skipped")
            continue
        dy, dx = register(ref, img)
        imgs.append((name, img, (dy, dx)))
    # common area of all images in reference coordinates
    y0 = max(max(0, -d[0]) for _, _, d in imgs)
    x0 = max(max(0, -d[1]) for _, _, d in imgs)
    y1 = min(min(ref.shape[0], im.shape[0] - d[0]) for _, im, d in imgs)
    x1 = min(min(ref.shape[1], im.shape[1] - d[1]) for _, im, d in imgs)
    al = [(n, im[y0 + d[0]:y1 + d[0], x0 + d[1]:x1 + d[1]], d) for n, im, d in imgs]
    R = al[0][1]
    al = [(n, R if k == 0 else tone_match(im, R), d) for k, (n, im, d) in enumerate(al)]
    # tile classes from the reference
    T = 64
    Y = luma(R)
    lap = ndimage.laplace(Y)
    mean = ndimage.uniform_filter(Y, T)
    tl = list(tiles(Y.shape, T))
    tex = np.array([np.mean(lap[y:y + T, x:x + T] ** 2) for y, x in tl])
    lum = np.array([mean[y + T // 2, x + T // 2] for y, x in tl])
    lowpass_chroma = []
    for y, x in tl:
        p = R[y:y + T, x:x + T].mean((0, 1))
        lowpass_chroma.append(np.ptp(p) / (p.mean() + 1e-3))
    lowpass_chroma = np.array(lowpass_chroma)
    textured = [i for i in np.argsort(-tex)[: max(5, len(tl) * 15 // 100)]]
    neutral_tex = [i for i in textured if lowpass_chroma[i] < 0.25] or textured
    dark = [i for i in range(len(tl)) if np.percentile(lum, 5) < lum[i] < np.percentile(lum, 30)]
    flat_dark = sorted(dark, key=lambda i: tex[i])[: max(5, len(tl) * 5 // 100)]
    print(f"area {x1 - x0}x{y1 - y0} · tiles {len(tl)} · textured {len(textured)} (neutral {len(neutral_tex)}) · flat dark {len(flat_dark)}")
    print("name\tshift\tdetail_rel\thf_chroma\tnoise_rel\tworst_hf_chroma_tile")
    base = {}
    rows = []
    for n, im, d in al:
        y, cb, cr = ycc(im)
        ly = ndimage.laplace(y)
        det = np.mean([np.mean(ly[a:a + T, b:b + T] ** 2) for a, b in (tl[i] for i in textured)])
        hy, hcb, hcr = hp(y), hp(cb), hp(cr)
        # ratio of sums over the textured near-neutral tiles (not a mean of per-tile ratios, which flat tiles dominate)
        e_y = np.array([np.mean(hy[a:a + T, b:b + T] ** 2) for a, b in (tl[i] for i in neutral_tex)])
        e_c = np.array([np.mean(hcb[a:a + T, b:b + T] ** 2 + hcr[a:a + T, b:b + T] ** 2) for a, b in (tl[i] for i in neutral_tex)])
        hfc = float(e_c.sum() / (e_y.sum() + 1e-12))
        worst = neutral_tex[int(np.argmax(e_c))]
        noise = np.mean([np.std(hy[a:a + T, b:b + T]) for a, b in (tl[i] for i in flat_dark)])
        if not base:
            base = {"det": det, "noise": noise}
        rows.append((n, d, det / base["det"], hfc, noise / base["noise"], worst))
        print(f"{n}\t{d[1]},{d[0]}\t{det / base['det']:.3f}\t{hfc:.4f}\t{noise / base['noise']:.3f}\t{tl[worst]}")
    if sheet:
        # crops: most textured, worst false colour (over all images), flattest dark tile
        worst_all = max(rows, key=lambda r: r[3])[5]
        picks = [("texture", tl[textured[0]]), ("false colour", tl[worst_all]), ("dark flat", tl[flat_dark[0]])]
        c = crop
        z = 2
        lab_h = 18
        W = len(al) * c * z + (len(al) - 1) * 6
        H = len(picks) * (c * z + lab_h + 6)
        canvas = Image.new("RGB", (W, H), (20, 20, 20))
        dr = ImageDraw.Draw(canvas)
        for r, (label, (ty, tx)) in enumerate(picks):
            cy = int(np.clip(ty + T // 2 - c // 2, 0, R.shape[0] - c))
            cx = int(np.clip(tx + T // 2 - c // 2, 0, R.shape[1] - c))
            for k, (n, im, _) in enumerate(al):
                part = im[cy:cy + c, cx:cx + c]
                if label == "dark flat":
                    # brighten the dark crop equally for all images (visibility of noise)
                    part = np.clip(part * (0.5 / max(R[cy:cy + c, cx:cx + c].mean(), 1e-3)), 0, 1)
                p = Image.fromarray((np.clip(part, 0, 1) * 255 + 0.5).astype(np.uint8)).resize((c * z, c * z), Image.Resampling.NEAREST)
                X = k * (c * z + 6)
                Yp = r * (c * z + lab_h + 6)
                canvas.paste(p, (X, Yp + lab_h))
                dr.text((X + 4, Yp + 3), f"{n} · {label} @ {cx},{cy}", fill=(230, 230, 230))
        canvas.save(sheet)
        print(f"sheet {sheet}")


if __name__ == "__main__":
    if len(sys.argv) < 3 or sys.argv[1] not in ("colour", "detail"):
        print(__doc__)
        sys.exit(1)
    {"colour": colour, "detail": detail}[sys.argv[1]](sys.argv[2:])

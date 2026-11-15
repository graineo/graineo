# SPDX-License-Identifier: GPL-3.0-only
"""Colour chart measurement: graineo (farbtafel-raw) and camera JPEG against ColorChecker target values (Lab D50, after Nov. 2014).
python measure.py corners_jpeg.json corners_graineo.json OUTPUT.json"""
import sys, json, glob, struct, os
import numpy as np
import colour

F = os.environ.get('COLOR_CHART_DIR', os.getcwd())  # folder with one subfolder per camera
CC = colour.CCS_COLOURCHECKERS['ColorChecker24 - After November 2014']
NAMES = list(CC.data.keys())
REF = np.array([colour.XYZ_to_Lab(colour.xyY_to_XYZ(v), CC.illuminant) for v in CC.data.values()])
D50 = CC.illuminant
D65 = colour.CCS_ILLUMINANTS['CIE 1931 2 Degree Standard Observer']['D65']
M_SRGB = colour.RGB_COLOURSPACES['sRGB'].matrix_RGB_to_XYZ
M_BRAD = colour.adaptation.matrix_chromatic_adaptation_VonKries(colour.xy_to_XYZ(D65), colour.xy_to_XYZ(D50), 'Bradford')


def decode(a):
    a = np.asarray(a, dtype=np.float64)
    s, x = np.sign(a), np.abs(a)
    return s * np.where(x <= 0.04045, x / 12.92, ((x + 0.055) / 1.055) ** 2.4)


def lin_srgb_to_lab(rgb):
    xyz = rgb @ M_SRGB.T @ M_BRAD.T
    return colour.XYZ_to_Lab(xyz, D50)


def load_f32(p):
    b = open(p, 'rb').read()
    w, h = struct.unpack('<II', b[4:12])
    return np.frombuffer(b[12:], '<f4').reshape(h, w, 3).astype(np.float64)


def boxes(c, box=0.45):
    p1, p6, p19, p24 = [np.array(c[i:i + 2]) for i in (0, 2, 4, 6)]
    out = []
    px = np.linalg.norm(p6 - p1) / 5 * box / 2
    py = np.linalg.norm(p19 - p1) / 3 * box / 2
    for r in range(4):
        for k in range(6):
            u, v = k / 5, r / 3
            m = (1 - u) * (1 - v) * p1 + u * (1 - v) * p6 + (1 - u) * v * p19 + u * v * p24
            out.append([m[0] - px, m[1] - py, m[0] + px, m[1] + py])
    return out


def patch_means(lin, c):
    h, w, _ = lin.shape
    res = []
    for x0, y0, x1, y1 in boxes(c):
        a = lin[int(y0 * h):int(np.ceil(y1 * h)), int(x0 * w):int(np.ceil(x1 * w))]
        res.append(a.reshape(-1, 3).mean(0))
    return np.array(res)


def metrics(lab):
    de = colour.delta_E(lab, REF, method='CIE 2000')
    lab_c = lab.copy(); lab_c[:, 0] = REF[:, 0]
    de_color = colour.delta_E(lab_c, REF, method='CIE 2000')
    C = np.hypot(lab[:, 1], lab[:, 2]); Cr = np.hypot(REF[:, 1], REF[:, 2])
    dC = C - Cr
    dL = lab[:, 0] - REF[:, 0]
    chromatic = slice(0, 18); grey = slice(18, 24)
    return {
        'de': de.round(2).tolist(), 'de_color': de_color.round(2).tolist(), 'dC': dC.round(2).tolist(), 'dL': dL.round(2).tolist(),
        'lab': lab.round(2).tolist(),
        'mean': round(float(de.mean()), 2), 'max': round(float(de.max()), 2), 'max_patch': int(de.argmax()) + 1,
        'mean_chromatic': round(float(de[chromatic].mean()), 2), 'skin': round(float(de[0:2].mean()), 2),
        'grey_dL_mean': round(float(np.abs(dL[grey]).mean()), 2), 'grey_C_mean': round(float(C[grey].mean()), 2),
        'dC_chromatic': round(float(dC[chromatic].mean()), 2), 'dC_chromatic_rel': round(float((C[chromatic] / Cr[chromatic]).mean() * 100 - 100), 1),
        'mean_color': round(float(de_color.mean()), 2), 'mean_color_chromatic': round(float(de_color[chromatic].mean()), 2),
    }


def jpeg_lin(p):
    from PIL import Image
    im = Image.open(p)
    icc = im.info.get('icc_profile')
    a = np.asarray(im.convert('RGB'), dtype=np.float64) / 255
    return decode(a), (b'Adobe RGB' in icc) if icc else False


if __name__ == '__main__':
    ej = json.load(open(sys.argv[1])); eg = json.load(open(sys.argv[2]))
    all_res = {}
    for k in eg:
        e = {}
        d = f'{F}/{k}'
        variants = [('graineo', f'{d}/graineo-export.f32', True), ('display', f'{d}/graineo.f32', True), ('matrix', f'{d}/graineo-lin.f32', False),
                     ('matrix_std', f'{d}/graineo-std.f32', False), ('curve', f'{d}/graineo-kurve.f32', False), ('curve_h1', f'{d}/graineo-kurve-h1.f32', False),
                     ('curve_y', f'{d}/graineo-kurve-y.f32', False), ('graineo', f'{d}/graineo-foveon.f32', True)]
        for name, p, enc in variants:
            if not os.path.exists(p):
                continue
            a = load_f32(p)
            lin = decode(a) if enc else a
            e[name] = metrics(lin_srgb_to_lab(patch_means(lin, eg[k])))
        j = glob.glob(f'{d}/*.JPG')
        if j and k in ej:
            lin, adobe = jpeg_lin(j[0])
            if adobe:
                print(k, 'JPEG is Adobe RGB – not supported')
            labj = lin_srgb_to_lab(patch_means(lin, ej[k]))
            e['jpeg'] = metrics(labj)
            # JPEG normalised in exposure to patch 20 (for orientation only): scaling in linear light
            pm = patch_means(lin, ej[k])
            y20 = (pm[19] @ (M_BRAD @ M_SRGB)[1])
            yt = colour.Lab_to_XYZ(REF[19], D50)[1]
            e['jpeg_norm'] = metrics(lin_srgb_to_lab(pm * yt / y20))
        meta = f'{d}/graineo.json'
        if os.path.exists(meta):
            e['meta'] = json.load(open(meta))
        all_res[k] = e
        g = e.get('graineo', {}); jj = e.get('jpeg', {})
        print(f"{k:20s} graineo {g.get('mean','-'):>5} max {g.get('max','-'):>5} chromatic {g.get('mean_chromatic','-'):>5} skin {g.get('skin','-'):>5} dC {g.get('dC_chromatic','-'):>6} | matrix {e.get('matrix',{}).get('mean','-'):>5} dC {e.get('matrix',{}).get('dC_chromatic','-'):>6} | jpeg {jj.get('mean','-'):>5} dC {jj.get('dC_chromatic','-'):>6}")
    json.dump({'reference': {'name': 'ColorChecker24 - After November 2014', 'names': NAMES, 'lab': REF.round(2).tolist()}, 'cameras': all_res}, open(sys.argv[3], 'w'), indent=1, ensure_ascii=False)

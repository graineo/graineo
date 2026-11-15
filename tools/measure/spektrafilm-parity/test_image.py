# SPDX-License-Identifier: GPL-3.0-only
# Synthetic test chart: grey wedge, ColorChecker 24 (linear sRGB), saturated hue ring.
import sys, numpy as np, colour
out = sys.argv[1]
P = 16  # patch size
cols = 24
patches = []
# row 0+1: grey wedge -7..+4 EV around 0.184 (24 steps)
ev = np.linspace(-7, 4, 24)
patches.append([0.184 * 2**e * np.ones(3) for e in ev])
# row 2: ColorChecker 24 in linear sRGB
cc = colour.CCS_COLOURCHECKERS['ColorChecker24 - After November 2014']
xyY = np.array(list(cc.data.values()))
XYZ = colour.xyY_to_XYZ(xyY)
rgb = colour.XYZ_to_RGB(XYZ, 'sRGB', illuminant=cc.illuminant, chromatic_adaptation_transform='Bradford', apply_cctf_encoding=False)
patches.append([np.clip(c, 0, None) for c in rgb])
# row 3,4: hue ring at two saturations, Y≈0.18
for sat in (0.9, 0.5):
    row = []
    for h in np.linspace(0, 1, 24, endpoint=False):
        import colorsys
        c = np.array(colorsys.hsv_to_rgb(h, sat, 1.0))
        c = colour.cctf_decoding(c, 'sRGB') if False else c**2.2
        Y = 0.2126*c[0]+0.7152*c[1]+0.0722*c[2]
        row.append(c * 0.18 / Y)
    patches.append(row)
# row 5: bright highlights (+1..+5 EV) of warm/neutral/cool and a few saturated
row = []
for e in np.linspace(0, 5, 8):
    for base in ([1,1,1],[1,0.8,0.6],[0.6,0.8,1]):
        row.append(0.184*2**e*np.array(base))
patches.append(row)
H = len(patches)*P; W = cols*P
img = np.zeros((H, W, 3))
for r, row in enumerate(patches):
    for c, v in enumerate(row):
        img[r*P:(r+1)*P, c*P:(c+1)*P] = v
np.save(out + '.npy', img)
img.astype('<f8').tofile(out + '.f64')
print(img.shape)

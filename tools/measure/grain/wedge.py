# SPDX-License-Identifier: GPL-3.0-only
"""Usage: wedge.py wedge.png gradient.png
Step wedge (sRGB PNG): 4×4 patches of 600×600, scene luminance from -6 to +3 EV around mid-grey 0.184,
and a horizontal gradient (for the website)."""
import sys, numpy as np
from PIL import Image
def enc(l):
    l = np.clip(l, 0, 1)
    return np.where(l <= 0.0031308, 12.92 * l, 1.055 * l ** (1 / 2.4) - 0.055)
evs = np.linspace(-6, 3, 16)
img = np.zeros((2400, 2400))
for i, ev in enumerate(evs):
    y, x = divmod(i, 4)
    img[y*600:(y+1)*600, x*600:(x+1)*600] = 0.184 * 2.0 ** ev
Image.fromarray((enc(img) * 255 + 0.5).astype(np.uint8)).convert('RGB').save(sys.argv[1])
open(sys.argv[1] + '.ev.txt', 'w').write(' '.join(f'{e:.2f}' for e in evs))
# gradient 2400×600: -6 … +3 EV, logarithmic
ev = np.linspace(-6, 3, 2400)[None, :].repeat(600, 0)
Image.fromarray((enc(0.184 * 2.0 ** ev) * 255 + 0.5).astype(np.uint8)).convert('RGB').save(sys.argv[2])

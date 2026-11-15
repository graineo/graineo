# SPDX-License-Identifier: GPL-3.0-only
# image.py out.png W H title1=file1 title2=file2 ...  (sRGB-encoded f64 images, stacked)
import sys, numpy as np, matplotlib
matplotlib.use('Agg'); import matplotlib.pyplot as plt
out, W, H = sys.argv[1], int(sys.argv[2]), int(sys.argv[3])
items = [a.split('=', 1) for a in sys.argv[4:]]
fig, axs = plt.subplots(len(items), 1, figsize=(W/40, len(items)*H/40+0.4*len(items)), dpi=80)
for ax, (t, f) in zip(np.atleast_1d(axs), items):
    im = np.clip(np.fromfile(f, '<f8').reshape(H, W, 3), 0, 1)
    ax.imshow(im, interpolation='nearest'); ax.set_title(t, fontsize=10, loc='left'); ax.axis('off')
fig.patch.set_facecolor('#808080'); plt.tight_layout(); plt.savefig(out, facecolor=fig.get_facecolor())

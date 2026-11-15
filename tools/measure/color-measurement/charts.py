#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-only
"""Charts for the colour measurement (PNG) from result.json of color_measurement.py.

Usage:  python -I charts.py OUTPUT_FOLDER [TARGET_FOLDER]
Writes (numbers and patch means only, no image content of third-party shots):
  1-sigma-vs-vendor.png          ΔE00 per Sigma camera (mean over the files, dots = individual files)
  2-color-chart-patches.png      ΔE00 per ColorChecker patch, one row per camera (heat map)
  3-color-chart-target-actual.png  colour patches target | graineo as colour tiles (patch means), one chart per camera
  4-camera-jpeg.png              distance to the camera JPEG: look neutral against camera look per camera
  5-map-<name>.png               own shots only: SPP | graineo | ΔE00 map (8×8)
Dependencies: numpy, matplotlib, pillow, colour-science.
"""
import json
import sys
from pathlib import Path

import numpy as np
import matplotlib
matplotlib.use('Agg')
import matplotlib.pyplot as plt  # noqa: E402

BLUE, ORANGE, TEXT, TEXT2, SURFACE, LINE = '#2a78d6', '#eb6834', '#0b0b0b', '#52514e', '#fcfcfb', '#e4e3df'
plt.rcParams.update({'font.size': 10, 'axes.edgecolor': LINE, 'axes.labelcolor': TEXT2, 'xtick.color': TEXT2, 'ytick.color': TEXT2,
                     'figure.facecolor': SURFACE, 'axes.facecolor': SURFACE, 'axes.spines.top': False, 'axes.spines.right': False})


def lab_to_srgb(lab):
    import colour
    d50 = colour.CCS_ILLUMINANTS['CIE 1931 2 Degree Standard Observer']['D50']
    xyz = colour.Lab_to_XYZ(np.asarray(lab), d50)
    rgb = colour.XYZ_to_sRGB(xyz, d50, chromatic_adaptation_transform='Bradford')
    return np.clip(rgb, 0, 1)


def sigma(e, target):
    res = [r for r in e['spp'] if 'error' not in r]
    group_order = {'Merrill': 0, 'Quattro': 1, 'Classic': 2}
    cams = sorted({(r['group'], r['camera']) for r in res}, key=lambda x: (group_order.get(x[0], 9), x[1]))
    fig, ax = plt.subplots(figsize=(8, 0.42 * len(cams) + 1.2))
    xr = max(r['de_mean'] for r in res) + 0.3
    for i, (g, k) in enumerate(cams):
        v = [r['de_mean'] for r in res if r['camera'] == k]
        ax.barh(i, np.mean(v), color=BLUE, height=0.55)
        ax.scatter(v, [i] * len(v), s=10, color=TEXT, zorder=3, alpha=0.6)
        ax.text(xr, i, f'{np.mean(v):.2f}' + f'  (n={len(v)})', va='center', fontsize=9, color=TEXT)
    ax.set_yticks(range(len(cams)), [f'{k}' for g, k in cams])
    ax.invert_yaxis()
    for x, t in ((1, 'invisible'), (2, 'barely visible')):
        ax.axvline(x, color=LINE, lw=1, zorder=0)
    ax.set_xlabel('ΔE00 to the manufacturer software (mean per file; bar = mean, dots = files)')
    ax.set_title('Sigma: distance to the manufacturer software', loc='left', color=TEXT)
    fig.tight_layout()
    fig.savefig(target / '1-sigma-vs-vendor.png', dpi=150)
    plt.close(fig)


def color_chart(e, target):
    ft = e['color_chart']
    ok = {k: v for k, v in ft['cameras'].items() if 'graineo' in v}
    de = np.array([v['graineo']['de'] for v in ok.values()])
    fig, ax = plt.subplots(figsize=(10, 0.4 * len(ok) + 1.6))
    im = ax.imshow(de, cmap='Blues', vmin=0, vmax=12, aspect='auto')
    for i in range(de.shape[0]):
        for j in range(24):
            ax.text(j, i, f'{de[i, j]:.0f}', ha='center', va='center', fontsize=7, color='white' if de[i, j] > 7 else TEXT)
    ax.set_yticks(range(len(ok)), [v['camera'] for v in ok.values()])
    ax.set_xticks(range(24), [str(i + 1) for i in range(24)])
    ax.set_xlabel('Patch (1–2 skin, 13–18 saturated, 19–24 greyscale)')
    ax.set_title('ColorChecker: ΔE00 per patch to the target value (white balance 20–22, exposure patch 22)', loc='left', color=TEXT)
    fig.colorbar(im, ax=ax, fraction=0.025, label='ΔE00')
    fig.tight_layout()
    fig.savefig(target / '2-color-chart-patches.png', dpi=150)
    plt.close(fig)
    # target | actual as tiles
    ref = lab_to_srgb(ft['reference']['lab'])
    n = len(ok)
    cols = 4
    nrows = (n + cols - 1) // cols
    fig, axs = plt.subplots(nrows, cols, figsize=(2.6 * cols, 2.5 * nrows + 0.5))
    axs = np.atleast_1d(axs).ravel()
    for ax in axs[n:]:
        ax.set_axis_off()
    for ax, (k, v) in zip(axs, ok.items()):
        actual = lab_to_srgb(v['graineo']['lab'])
        tile = np.ones((4 * 12 + 3, 6 * 12 + 5, 3)) * 0.1
        for f in range(24):
            r, c = divmod(f, 6)
            y, x = r * 13, c * 13
            tile[y:y + 12, x:x + 12] = ref[f]
            tile[y + 6:y + 12, x:x + 12] = actual[f]  # lower half = graineo
        ax.imshow(tile)
        ax.set_axis_off()
        ax.set_title(f"{v['camera']}\nΔE00 {v['graineo']['mean']:.1f}", fontsize=9, color=TEXT)
    fig.suptitle('ColorChecker: target on top, graineo below (patch means)', x=0.01, ha='left', color=TEXT)
    fig.tight_layout()
    fig.savefig(target / '3-color-chart-target-actual.png', dpi=150)
    plt.close(fig)


def camera_jpeg(e, target):
    res = e['camera_jpeg']
    cams = sorted({r['camera'] for r in res})
    fig, ax = plt.subplots(figsize=(8, 0.6 * len(cams) + 1.3))
    for i, k in enumerate(cams):
        v = [r for r in res if r['camera'] == k]
        a, b = np.mean([r['neutral']['de'] for r in v]), np.mean([r['look']['de'] for r in v])
        ax.barh(i - 0.17, a, height=0.32, color=BLUE, label='Look neutral' if i == 0 else None)
        ax.barh(i + 0.17, b, height=0.32, color=ORANGE, label='with camera look' if i == 0 else None)
        ax.text(a + 0.08, i - 0.17, f'{a:.1f}', va='center', fontsize=8, color=TEXT)
        ax.text(b + 0.08, i + 0.17, f'{b:.1f}' + f'  (n={len(v)})', va='center', fontsize=8, color=TEXT)
    ax.set_yticks(range(len(cams)), cams)
    ax.invert_yaxis()
    ax.legend(frameon=False, loc='lower right')
    ax.set_xlabel('ΔE00 to the camera JPEG of the same shot (mean, exposure matched)')
    ax.set_title('Other cameras: distance to the camera JPEG', loc='left', color=TEXT)
    fig.tight_layout()
    fig.savefig(target / '4-camera-jpeg.png', dpi=150)
    plt.close(fig)


def maps(out_dir, e, target, names):
    from PIL import Image
    for n in names:
        b = out_dir / 'spp' / 'images'
        if not (b / f'{n}-de8.npy').exists():
            continue
        de = np.load(b / f'{n}-de8.npy').astype(np.float32)
        r = [x for x in e['spp'] if x['name'] == n][0]
        fig, axs = plt.subplots(1, 3, figsize=(12, 3.4))
        for ax, (t, img) in zip(axs, (('Manufacturer software', Image.open(b / f'{n}-ref.jpg')), ('graineo', Image.open(b / f'{n}-graineo.jpg')))):
            ax.imshow(img)
            ax.set_title(t, fontsize=9, color=TEXT)
            ax.set_axis_off()
        im = axs[2].imshow(de, cmap='magma', vmin=0, vmax=5)
        axs[2].set_axis_off()
        axs[2].set_title(f"ΔE00 (8×8), mean {r['de8_mean']:.2f}", fontsize=9, color=TEXT)
        fig.colorbar(im, ax=axs[2], fraction=0.035)
        fig.suptitle(f"{r['camera']} – own shot", x=0.01, ha='left', color=TEXT)
        fig.tight_layout()
        fig.savefig(target / f'5-map-{n}.png', dpi=130)
        plt.close(fig)


if __name__ == '__main__':
    out_dir = Path(sys.argv[1])
    target = Path(sys.argv[2]) if len(sys.argv) > 2 else out_dir
    target.mkdir(parents=True, exist_ok=True)
    e = json.loads((out_dir / 'result.json').read_text())
    if 'spp' in e:
        sigma(e, target)
    if 'color_chart' in e:
        color_chart(e, target)
    if 'camera_jpeg' in e:
        camera_jpeg(e, target)
    if 'spp' in e:
        maps(out_dir, e, target, sys.argv[3].split(',') if len(sys.argv) > 3 else [r['name'] for r in e['spp'] if r.get('own')][:3])

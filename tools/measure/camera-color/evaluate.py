"""Evaluate `graineo-dev camera-color` runs: distance of the app's development to the camera JPEG,
per group (camera and picture style), training set against hold-out set.

Usage:
    python3 -I evaluate.py RESULTS.tsv [RESULTS2.tsv …] [--min-gain 0.03] [--min-share 0.6]

RESULTS.tsv is the stdout of `graineo-dev camera-color --list PAIRS.tsv --sat F1,F2,…` (lines IMG/POOL/SECTOR);
the split column must be `train` or `holdout`. Several files are merged (e.g. one per camera).

Decision rule (fixed before looking at the hold-out numbers):
1. On the training set pick the saturation variant with the lowest pooled ΔE00 median (ties: lower 90th
   percentile). Variant `code` is the value the app uses today.
2. Accept it only if on the hold-out set, against `code`:
   - the pooled median falls by at least --min-gain (relative, default 3 %),
   - the pooled 90th percentile does not rise (tolerance 0.02 ΔE00),
   - at least --min-share (default 60 %) of the hold-out photos get a lower mean ΔE00.
Otherwise the code value stays. Prints one table per group and the sector table (chroma ratio and hue
difference per 20° hue sector of the reference) for `code` on all photos, which shows whether the
remaining error depends on the hue (a uniform saturation factor cannot fix that).
"""
import sys
from collections import defaultdict


def load(paths):
    pool, img, sec = {}, defaultdict(dict), defaultdict(list)
    for p in paths:
        for line in open(p, encoding='utf-8'):
            t = line.rstrip('\n').split('\t')
            if t[0] == 'POOL':
                g, sp, v = t[1], t[2], t[3]
                pool[(g, sp, v)] = dict(n=int(t[4]), median=float(t[5]), p90=float(t[6]), mean=float(t[7]), mean_img=float(t[8]), dc=float(t[9]), dl=float(t[10]))
            elif t[0] == 'IMG':
                g, sp, f, v = t[1], t[2], t[3], t[4]
                img[(g, sp, f)][v] = dict(ev=float(t[5]), mean=float(t[6]), median=float(t[7]), p90=float(t[8]), dl=float(t[9]), dc=float(t[10]))
            elif t[0] == 'SECTOR':
                sec[(t[1], t[2], t[3])].append((int(t[4]), int(t[5]), float(t[6]), float(t[7])))
    return pool, img, sec


def arg(name, default):
    if name in sys.argv:
        return float(sys.argv[sys.argv.index(name) + 1])
    return default


def main():
    files = [a for i, a in enumerate(sys.argv[1:], 1) if not a.startswith('--') and not sys.argv[i - 1].startswith('--')]
    min_gain, min_share = arg('--min-gain', 0.03), arg('--min-share', 0.6)
    pool, img, sec = load(files)
    groups = sorted({k[0] for k in pool})
    for g in groups:
        variants = sorted({k[2] for k in pool if k[0] == g})
        train = {v: pool[(g, 'train', v)] for v in variants if (g, 'train', v) in pool}
        hold = {v: pool[(g, 'holdout', v)] for v in variants if (g, 'holdout', v) in pool}
        print(f'\n## {g}')
        print('| variant | train n | train median | train p90 | train ΔC* | holdout n | holdout median | holdout p90 | holdout ΔC* |')
        print('|---|---:|---:|---:|---:|---:|---:|---:|---:|')
        for v in variants:
            a, b = train.get(v), hold.get(v)
            fa = f"{a['n']} | {a['median']:.2f} | {a['p90']:.2f} | {a['dc']:+.2f}" if a else '– | – | – | –'
            fb = f"{b['n']} | {b['median']:.2f} | {b['p90']:.2f} | {b['dc']:+.2f}" if b else '– | – | – | –'
            print(f'| {v} | {fa} | {fb} |')
        if 'code' not in train or 'code' not in hold:
            print('no training or hold-out set: no decision')
            continue
        best = min(train, key=lambda v: (train[v]['median'], train[v]['p90']))
        c, b = hold['code'], hold[best]
        photos = [d for (gg, sp, _), d in img.items() if gg == g and sp == 'holdout' and best in d and 'code' in d]
        share = sum(d[best]['mean'] < d['code']['mean'] for d in photos) / max(1, len(photos))
        gain = (c['median'] - b['median']) / c['median'] if c['median'] > 0 else 0.0
        ok = best != 'code' and gain >= min_gain and b['p90'] <= c['p90'] + 0.02 and share >= min_share
        print(f"\ntraining choice: {best}; hold-out median {c['median']:.2f} → {b['median']:.2f} ({100 * gain:+.1f} %), "
              f"p90 {c['p90']:.2f} → {b['p90']:.2f}, photos better {100 * share:.0f} % of {len(photos)}"
              f" → {'ACCEPT' if ok else 'keep code value'}")
        rows = defaultdict(lambda: [0, 0.0, 0.0])
        for sp in ('train', 'holdout'):
            for h, n, cr, dh in sec.get((g, sp, 'code'), []):
                r = rows[h]
                r[0] += n
                r[1] += n * cr
                r[2] += n * dh
        if rows:
            print('\nhue sectors of the reference (code, all photos): from° n chroma-ratio Δh°')
            print('  '.join(f'{h}:{r[0]}:{r[1] / r[0]:.2f}:{r[2] / r[0]:+.1f}' for h, r in sorted(rows.items()) if r[0] >= 200))


if __name__ == '__main__':
    main()

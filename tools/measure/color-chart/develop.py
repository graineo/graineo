# SPDX-License-Identifier: GPL-3.0-only
"""Calls graineo-dev color-chart-raw per camera. python develop.py corners_graineo.json [--raw] [camera …]
--raw: without adjustment (as shot) to graineo-raw.f32 – only for reading off the corners."""
import sys, json, glob, subprocess, os
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from measure import boxes, F

BIN = os.environ.get('GRAINEO_DEV', 'graineo-dev')  # test program (path or in PATH)
corners = json.load(open(sys.argv[1]))
raw_mode = '--raw' in sys.argv
choice = [a for a in sys.argv[2:] if not a.startswith('--')] or list(corners)
env = dict(os.environ, GRAINEO_DATA_DIR=os.environ.get('GRAINEO_DATA_DIR', '/tmp/graineo-measure-data'),
           GRAINEO_LIBRARY_NO_SYNC='1', GRAINEO_LIBRARY_SIDECARS='0', GRAINEO_NO_PHOTOKIT='1')
for k in choice:
    raws = [p for p in glob.glob(f'{F}/{k}/*') if p.split('.')[-1] in ('DNG', 'RAF', 'CR3', 'ARW')]
    if not raws:
        continue
    b = boxes(corners[k])
    r = lambda i: ','.join(f'{v:.5f}' for v in b[i])
    out = f'{F}/{k}/graineo-raw' if raw_mode else f'{F}/{k}/graineo'
    args = [BIN, 'color-chart-raw', raws[0], out, '--patch22', r(21), '--patch20', r(19), '--l20', '81.29'] + (['--no-sync'] if raw_mode else [])
    p = subprocess.run(args, capture_output=True, text=True, env=env)
    line = [l for l in p.stdout.splitlines() if l.startswith('COLOR_CHART')]
    if not line:
        print(k, 'ERROR', p.stderr[-800:])
        continue
    j = json.loads(line[0].split('\t', 1)[1])
    if not raw_mode:
        json.dump(j, open(f'{out}.json', 'w'), indent=1, ensure_ascii=False)
    print(f"{k:20s} WB {j['white_balance']['kelvin']:.0f} K / {j['white_balance']['tint']:+.1f} (as shot {j['as_shot']['kelvin']:.0f}/{j['as_shot']['tint']:+.1f}) EV {j['exposure_ev']:+.2f} F20 {[round(v,2) for v in j['patch20_lab']]} F22 {[round(v,2) for v in j['patch22_lab']]} fwd {j['matrix']['forward']} std {j['standard_correction']} {j['dng_profile']}")

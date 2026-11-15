# SPDX-License-Identifier: GPL-3.0-only
"""Check images: one crop per camera with the 24 measurement boxes. python test_images.py CORNERS.json SOURCE(jpeg|f32) [camera …]"""
import sys, json, glob, subprocess, os
F = os.environ.get('COLOR_CHART_DIR', os.getcwd())  # folder with one subfolder per camera
here = os.path.dirname(os.path.abspath(__file__))
os.makedirs(f'{F}/crops', exist_ok=True)
corners = json.load(open(sys.argv[1]))
kind = sys.argv[2]
for k in (sys.argv[3:] or corners):
    c = corners[k]
    if kind == 'jpeg':
        image = glob.glob(f'{F}/{k}/*.JPG')[0]
    else:
        image = {'raw': f'{F}/{k}/graineo-raw.f32', 'sig': f'{F}/{k}/graineo-foveon.f32', 'exp': f'{F}/{k}/graineo-export.f32'}.get(kind, f'{F}/{k}/graineo.f32')
    xs, ys = c[0::2], c[1::2]
    dx, dy = (max(xs) - min(xs)) * 0.18, (max(ys) - min(ys)) * 0.25
    args = [sys.executable, '-W', 'ignore', f'{here}/crop.py', image, f'{F}/crops/{kind}-{k}.jpg',
            str(min(xs) - dx), str(min(ys) - dy), str(max(xs) + dx), str(max(ys) + dy), ','.join(map(str, c))]
    subprocess.run(args, check=True)

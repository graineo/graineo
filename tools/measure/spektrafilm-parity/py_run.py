# SPDX-License-Identifier: GPL-3.0-only
# Run the original spektrafilm on an image, dump all taps. Usage: py_run.py cfg.json outprefix
import sys, json, numpy as np
sys.path.insert(0, sys.argv[3])
from spektrafilm.runtime.params_builder import init_params, digest_params
from spektrafilm.runtime.pipeline import SimulationPipeline
cfg = json.load(open(sys.argv[1])); out = sys.argv[2]
img = np.load(cfg['input'])
p = init_params(cfg['film'], cfg['print'])
def patch(obj, d):
    for k, v in d.items():
        cur = getattr(obj, k)
        if isinstance(v, dict) and not isinstance(cur, dict):
            import dataclasses
            if dataclasses.is_dataclass(cur) and getattr(type(cur), '__dataclass_params__').frozen:
                setattr(obj, k, dataclasses.replace(cur, **v))
            else:
                patch(cur, v)
        else:
            setattr(obj, k, tuple(v) if isinstance(v, list) else v)
patch(p, cfg.get('params', {}))
p = digest_params(p)
pipe = SimulationPipeline(p)
taps = ['log_e_film', 'cmy_film', 'rgb_out'] if p.io.scan_film else ['log_e_film', 'cmy_film', 'log_e_print', 'cmy_print', 'rgb_out']
for t in taps:
    np.random.seed(0)
    r = pipe.process(img, collect=t)
    np.asarray(r, dtype='<f8').tofile(f'{out}.{t}.f64')
print('ok', taps)

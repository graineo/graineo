#!/usr/bin/env python3
"""Undo crate::tr("…") → "…": i18n-remove.py file [line ...] (without lines: the whole file)."""
import re, sys, pathlib
p = pathlib.Path('crates/app/src') / sys.argv[1]
lines = {int(x) for x in sys.argv[2:]}
s = p.read_text().split('\n')
n = 0
for i, ln in enumerate(s, 1):
    if lines and i not in lines: continue
    new = re.sub(r'crate::tr\(("(?:\\.|[^"\\])*")\)', r'\1', ln)
    if new != ln: s[i - 1] = new; n += 1
p.write_text('\n'.join(s))
print(sys.argv[1], 'lines changed:', n)

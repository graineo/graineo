#!/usr/bin/env python3
"""Undo tr(…) in calls of the slider family (slider*, regler*, v, f …): names and hints are needed
there in German for the help search and are only translated when displayed (lr.rs, help.rs)."""
import re, pathlib
root = pathlib.Path('crates/app/src')
FAM = {'slider', 'slider_hint', 'slider_grad', 'slider_full', 'v', 'f', 'regler', 'regler100', 'regler100_f32', 'regler100_f', 'hue_range', 'color_wheel', 'fuer_regler', 'section', 'section_reset', 'section_switch', 'subsection', 'group_label'}
def callee(s, pos):
    depth = 0; i = pos - 1
    while i > 0:
        c = s[i]
        if c in ')]}': depth += 1
        elif c in '([{':
            if depth == 0:
                if c != '(': return None
                m = re.search(r'([\w:]+)\s*!?\s*$', s[max(0, i - 60):i])
                return m.group(1).split('::')[-1] if m else None
            depth -= 1
        i -= 1
n = 0
for p in sorted(root.rglob('*.rs')):
    s = p.read_text(); out = []; last = 0
    for m in re.finditer(r'crate::tr\(("(?:\\.|[^"\\])*")\)', s):
        if callee(s, m.start()) in FAM:
            out.append(s[last:m.start()]); out.append(m.group(1)); last = m.end(); n += 1
    out.append(s[last:])
    if len(out) > 1: p.write_text(''.join(out))
print('reverted', n)

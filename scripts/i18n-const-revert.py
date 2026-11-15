#!/usr/bin/env python3
"""Removes crate::tr(…) again where the compiler rejects it in constants (E0015), and reports other errors."""
import json, subprocess, sys, re, pathlib, os
root = pathlib.Path(__file__).resolve().parents[1]
env = dict(os.environ, PATH=os.path.expanduser('~/.rustup/toolchains/stable-aarch64-apple-darwin/bin') + ':' + os.environ['PATH'])
for round_no in range(8):
    p = subprocess.run(['cargo', 'build', '--release', '-j', '4', '-p', 'graineo', '--message-format=json'], cwd=root, env=env, capture_output=True, text=True)
    errors = []
    for ln in p.stdout.splitlines():
        try: m = json.loads(ln)
        except Exception: continue
        if m.get('reason') != 'compiler-message': continue
        d = m['message']
        if d['level'] != 'error': continue
        errors.append(d)
    if not errors:
        print('no errors after round', round_no); sys.exit(0)
    const_errors = [d for d in errors if (d.get('code') or {}).get('code') == 'E0015']
    other = [d for d in errors if d not in const_errors]
    if not const_errors:
        for d in other[:40]:
            sp = d['spans'][0] if d['spans'] else {}
            print('ERROR', (d.get('code') or {}).get('code'), d['message'][:120], sp.get('file_name'), sp.get('line_start'))
        sys.exit(1)
    # replace back to front per file
    per_file = {}
    for d in const_errors:
        for sp in d['spans']:
            if sp.get('is_primary'):
                per_file.setdefault(sp['file_name'], set()).add((sp['line_start'], sp['column_start'], sp['line_end'], sp['column_end']))
    n = 0
    for f, spans in per_file.items():
        path = root / f
        lines = path.read_text().split('\n')
        for ls, cs, le, ce in sorted(spans, reverse=True):
            if ls != le: continue
            ln = lines[ls - 1]
            part = ln[cs - 1:ce - 1]
            mm = re.fullmatch(r'crate::tr\((.*)\)', part, re.S)
            if mm:
                lines[ls - 1] = ln[:cs - 1] + mm.group(1) + ln[ce - 1:]; n += 1
        path.write_text('\n'.join(lines))
    print('round', round_no, 'reverted:', n, 'other errors:', len(other))
    if n == 0:
        for d in other[:30]:
            sp = d['spans'][0] if d['spans'] else {}
            print('ERROR', (d.get('code') or {}).get('code'), d['message'][:120], sp.get('file_name'), sp.get('line_start'))
        sys.exit(1)

#!/usr/bin/env python3
"""Key list for locales/en-ui.tsv from the source code.

Usage:
  i18n-keys.py --list file       writes "number<TAB>key" (escaped) for all entries still missing
  i18n-keys.py --import numbers.tsv keys.txt   appends "number<TAB>English" lines as entries to en-ui.tsv
  i18n-keys.py --missing         counts missing entries (English)
  i18n-keys.py --languages       counts missing entries per language (en, ja, fr, es; exit 1 if any)
Keys: all tr(…)/trf!(…) texts, all German texts in help.rs, shortcuts.rs and extension_texts.rs, and the raw texts at the
call sites of the translating helper functions in lr.rs/dialog.rs.
"""
import re, sys, pathlib
root = pathlib.Path(__file__).resolve().parents[1]
src = root / 'crates/app/src'
LANGS = ['en', 'ja', 'fr', 'es']
tsv = root / 'crates/app/locales/en-ui.tsv'
LEX = r"//[^\n]*|/\*.*?\*/|(?:b?r(?P<h>\#*)\"(?P<raw>.*?)\"(?P=h))|(?:b?\"(?P<s>(?:\\.|[^\"\\])*)\")|'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'"
lex = re.compile(LEX, re.S)
def entesc(t):
    out = []; i = 0
    while i < len(t):
        c = t[i]
        if c != '\\': out.append(c); i += 1; continue
        n = t[i + 1]
        if n == 'n': out.append('\n'); i += 2
        elif n == 't': out.append('\t'); i += 2
        elif n in '"\\\'': out.append(n); i += 2
        elif n == 'u':
            m = re.match(r'\\u\{([0-9a-fA-F]+)\}', t[i:]); out.append(chr(int(m.group(1), 16))); i += m.end()
        elif n == '\n':
            i += 2
            while i < len(t) and t[i] in ' \t\n': i += 1
        else: out.append(n); i += 2
    return ''.join(out)
def mask(t): return t.replace('\\', '\\\\').replace('\n', '\\n').replace('\t', '\\t')
def unmask(t):
    out = []; i = 0
    while i < len(t):
        if t[i] == '\\' and i + 1 < len(t):
            out.append({'n': '\n', 't': '\t', '\\': '\\'}.get(t[i + 1], t[i + 1])); i += 2
        else: out.append(t[i]); i += 1
    return ''.join(out)
def tests(s):
    sp = []
    brace = re.compile(LEX + r'|[{};]', re.S)
    for m in re.finditer(r'#\[cfg\(test\)\]', s):
        d = 0
        for t in brace.finditer(s, m.end()):
            g = t.group()
            if len(g) != 1: continue
            if g == ';' and d == 0: sp.append((m.start(), t.end())); break
            if g == '{': d += 1
            elif g == '}':
                d -= 1
                if d == 0: sp.append((m.start(), t.end())); break
    return sp
def is_german(t):
    if len(re.findall(r'[A-Za-zÄÖÜäöüß]', t)) < 2: return False
    if re.fullmatch(r'[a-z0-9_.:/\-{}<>+*#%@\[\]|&!~]+', t): return False
    if t.startswith(('http', '/', '~', '.')): return False
    return True
def placeholders_only(t): return re.fullmatch(r'(\{[a-z0-9_:.]*\}|[\s·–—,.:;()/%|+\-×]|\d)*', t) is not None
HELPER_FILES = {'help.rs', 'shortcuts.rs', 'extension_texts.rs'}
def source_keys():
    keys = {}
    for p in sorted(src.rglob('*.rs')):
        rel = p.relative_to(src).as_posix()
        if rel.startswith('i18n/') or p.name in ('ui_test.rs', 'prefs_i18n.rs'): continue
        s = p.read_text(); ts = tests(s)
        for m in lex.finditer(s):
            if not (m.group('s') is not None or m.group('raw') is not None): continue
            if any(x <= m.start() < y for x, y in ts): continue
            t = entesc(m.group('s')) if m.group('s') is not None else m.group('raw')
            before = s[max(0, m.start() - 24):m.start()]
            in_tr = re.search(r'(?:\btr|\btrf!)\s*\(\s*$', before) is not None
            if in_tr or (p.name in HELPER_FILES and is_german(t) and not placeholders_only(t)):
                if is_german(t) and not placeholders_only(t): keys.setdefault(t, rel)
    return keys
def existing(f=None):
    d = {}
    f = f or tsv
    if f.exists():
        for z in f.read_text().split('\n'):
            if not z or (z.startswith('#') and not z.startswith('##')) or '\t' not in z: continue
            k, v = z.split('\t', 1); d[unmask(k)] = unmask(v)
    return d
if __name__ == '__main__':
    a = sys.argv[1:]
    keys = source_keys(); have = existing()
    missing = [k for k in keys if k not in have]
    if a[:1] == ['--missing']:
        print(len(keys), 'keys,', len(missing), 'missing')
    elif a[:1] == ['--languages']:
        # missing keys per language (en, ja, fr, es); exit code 1 if any is missing
        bad = 0
        for lg in LANGS:
            h = existing(root / f'crates/app/locales/{lg}-ui.tsv')
            m = [k for k in keys if k not in h]
            bad += len(m)
            print(f'{lg}: {len(keys)} keys, {len(m)} missing')
            for k in m[:10]: print('   ', mask(k)[:90])
        sys.exit(1 if bad else 0)
    elif a[:1] == ['--list']:
        pathlib.Path(a[1]).write_text('\n'.join(f'{i}\t{mask(k)}' for i, k in enumerate(missing)) + '\n')
        print(len(missing), 'entries in', a[1])
    elif a[:1] == ['--import']:
        numbered = {}
        for z in pathlib.Path(a[1]).read_text().split('\n'):
            if '\t' in z: i, v = z.split('\t', 1); numbered[int(i)] = v
        key_lines = pathlib.Path(a[2]).read_text().split('\n')
        by_number = {int(z.split('\t', 1)[0]): unmask(z.split('\t', 1)[1]) for z in key_lines if '\t' in z}
        with tsv.open('a') as f:
            for i, v in numbered.items():
                if i in by_number and v.strip() and v != by_number[i]:
                    f.write(f'{mask(by_number[i])}\t{v}\n')
        print(len(numbered), 'imported')

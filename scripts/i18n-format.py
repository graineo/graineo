#!/usr/bin/env python3
"""format!("German text {x} …", …) → crate::trf!("German text {x} …", x = …) (display texts; logging stays).

Usage: scripts/i18n-format.py [--write] [--file part]; without --write only a list.
Format specs ({:.1}, {:>5}) are filled in beforehand with format!, so the German text stays unchanged.
"""
import argparse, pathlib, re, sys
ap = argparse.ArgumentParser(); ap.add_argument('--write', action='store_true'); ap.add_argument('--file'); a = ap.parse_args()
root = pathlib.Path(__file__).resolve().parents[1] / 'crates/app/src'
LEX = r"//[^\n]*|/\*.*?\*/|(?:b?r(?P<h>\#*)\".*?\"(?P=h))|(?:b?\"(?:\\.|[^\"\\])*\")|'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'"
lex = re.compile(LEX, re.S); brace = re.compile(LEX + r'|[{};]', re.S)
WORDS = set('abbrechen abzug aktiv anzeigen aufnahme bearbeiten belichtung bereit bewertung bild bilder bildschirm breite dunkelkammer datei dateien datenblatt drehen drucken eigene einstellungen einzelbild entwickeln export exportieren farbe farben fehler film filme filmprofile filter filmstreifen fokus foto fotos grad grau herunterladen hinweis helligkeit hilfe histogramm infos kamera kein keine korn kontrast kopieren laden lichter liste maske masken mediathek minuten mitgelieferte nachbelichten name neu neue neues ordner papier papiere profil profile raster rezept rezepte speichern schatten schwarz schwarzwert schärfe start suche temperatur tiefen tonung vergleich vergleichen verlauf verlaufen verwalten vorschau wählen zeigen ziel zurück und oder nicht mit von für wird sind ist werden alle ohne nur dann noch auch'.split())
def is_german(text):
    """Display text with at least one word (texts without spaces are usually identifiers/file names)."""
    text = re.sub(r'\{[^{}]*\}', '', text)
    if re.search('[ÄÖÜäöüß]', text): return True
    if ' ' not in text.strip() and not (text.strip().endswith(':') and text.strip()[:1].isupper()): return False
    return re.search(r'[A-Za-z]{4,}', text) is not None

def tests(s):
    sp = []
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
LOG = re.compile(r'(?:\blog|eprintln!|println!|diagnose|expect|panic!|assert\w*!|unreachable!|json!|anyhow!|\.join|with_file_name|join|PathBuf::from|app_file|with_extension|set_title|file_name|set_file_name|GRAINEO)\s*\(?\s*&?\s*$')
def split_args(s, i):
    """from i (after the opening parenthesis) to the matching closing one: list of top-level arguments and end position"""
    depth = 0; parts = []; start = i; pos = i
    for t in re.finditer(LEX + r'|[()\[\]{},]', s[i:], re.S):
        g = t.group(); at = i + t.start()
        if len(g) > 1 or g in ('"',): continue
        if g in '([{': depth += 1
        elif g in ')]}':
            if depth == 0 and g == ')':
                parts.append(s[start:at]); return [x.strip() for x in parts if x.strip()], at + 1
            depth -= 1
        elif g == ',' and depth == 0:
            parts.append(s[start:at]); start = at + 1
    return None, None
total = 0; log_n = 0
for p in sorted(root.rglob('*.rs')):
    rel = p.relative_to(root).as_posix()
    if rel.startswith('i18n/') or p.name in ('ui_test.rs', 'film_look_tests.rs', 'test_matrix.rs', 'stress.rs', 'test_image.rs'): continue
    if a.file and a.file not in rel: continue
    s = p.read_text(); ts = tests(s)
    out = []; last = 0; n = 0
    for m in re.finditer(r'(?<![\w:])format!\s*\(\s*(?=")', s):
        if any(x <= m.start() < y for x, y in ts): continue
        # read the literal
        lm = re.compile(r'"(?:\\.|[^"\\])*"').match(s, m.end())
        if not lm: continue
        lit = lm.group()[1:-1]
        if not is_german(lit): continue
        before = re.sub(r'\s+', ' ', s[max(0, m.start() - 40):m.start()])
        if LOG.search(before):
            log_n += 1; continue
        # arguments after the literal
        rest = s[lm.end():]
        m_comma = re.match(r'\s*,', rest)
        if m_comma:
            args, end = split_args(s, lm.end() + m_comma.end())
        else:
            args, end = [], None
            m2 = re.match(r'\s*\)', rest)
            if not m2: continue
            end = lm.end() + m2.end()
        if end is None: continue
        positional = []; named = {}
        for x in args:
            m_named = re.fullmatch(r'([A-Za-z_]\w*)\s*=\s*(.+)', x, re.S)
            if m_named and not x.strip().startswith('=='): named[m_named.group(1)] = m_named.group(2).strip()
            else: positional.append(x)
        new = []; pos = 0; used = []; auto_names = iter('abcdefghijklmnopqrstuvwxyz')
        ok = True
        def substitute(mt):
            global ok
            g = mt.group(0)
            if g in ('{{', '}}'): return g
            inner = mt.group(1)
            name, _, spec = inner.partition(':')
            name = name.strip()
            if name == '':
                if pos_idx[0] >= len(positional): ok = False; return g
                expr = positional[pos_idx[0]]; pos_idx[0] += 1
                key = next(auto_names)
            elif name.isdigit():
                if int(name) >= len(positional): ok = False; return g
                expr = positional[int(name)]; key = 'p' + name
            else:
                key = name; expr = named.get(name, name)
            if spec:
                expr = f'format!("{{:{spec}}}", {expr})'
            if key not in [k for k, _ in used]: used.append((key, expr))
            return '{' + key + '}'
        pos_idx = [0]
        new_text = re.sub(r'\{\{|\}\}|\{([^{}]*)\}', substitute, lit)
        if not ok: continue
        # remaining named arguments are not needed
        if any(k not in [u[0] for u in used] for k in named): pass
        replacement = 'crate::trf!("' + new_text + '"' + ''.join(f', {k} = {e}' for k, e in used) + ')'
        out.append(s[last:m.start()]); out.append(replacement); last = end; n += 1
        if not a.write: print(f'{rel}:{s.count(chr(10), 0, m.start()) + 1}\t{replacement[:150]}')
    out.append(s[last:])
    total += n
    if a.write and n: p.write_text(''.join(out))
print({'converted': total, 'log_skipped': log_n}, file=sys.stderr)

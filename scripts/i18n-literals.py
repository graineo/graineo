#!/usr/bin/env python3
"""Find German UI texts in crates/app/src (without test code and without i18n/).

Usage: scripts/i18n-literals.py [--tsv file.tsv] [--only-open] [--file part]
Output per hit: file:line  class  text. Classes:
  tr      already inside tr(…)/trf!(…)
  display direct display call (label, button, RichText::new …) – belongs in tr
  helper  argument of an lr::/dialog:: helper function (translated there)
  other   other context (helper function, list, assignment …) – check by hand
"""
import argparse, pathlib, re, sys
ap = argparse.ArgumentParser()
ap.add_argument('--tsv', type=pathlib.Path)
ap.add_argument('--only-open', action='store_true')
ap.add_argument('--file')
a = ap.parse_args()
root = pathlib.Path(__file__).resolve().parents[1] / 'crates/app/src'
LEX = r"//[^\n]*|/\*.*?\*/|(?:b?r(?P<h>\#*)\".*?\"(?P=h))|(?:b?\"(?:\\.|[^\"\\])*\")|'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'"
lex = re.compile(LEX, re.S)
brace = re.compile(LEX + r'|[{};]', re.S)
WORDS = set('abbrechen abzug aktiv anzeigen aufnahme bearbeiten belichtung bereit bewertung bild bilder bildschirm breite dunkelkammer datei dateien datenblatt drehen drucken eigene einstellungen einzelbild entwickeln export exportieren farbe farben fehler film filme filmprofile filter filmstreifen fokus foto fotos grad grau herunterladen hinweis helligkeit hilfe histogramm infos kamera kein keine korn kontrast kopieren laden lichter liste maske masken mediathek minuten mitgelieferte nachbelichten name neu neue neues ordner papier papiere profil profile raster rezept rezepte speichern schatten schwarz schwarzwert schärfe start suche temperatur tiefen tonung vergleich vergleichen verlauf verlaufen verwalten vorschau wählen zeigen ziel zurück und oder nicht mit von für wird sind ist werden alle ohne nur dann noch auch'.split())
DISPLAY = re.compile(r'(?:\.label|\.button|\.small_button|\.heading|RichText::new|Button::new|\.on_hover_text|\.on_disabled_hover_text|\.hint_text|\.selected_text|\.colored_label\([^,]+,|\.checkbox\([^,]+,|\.selectable_label\([^,]+,|\.selectable_value\([^,]+,[^,]+,)\s*\($' )
def mask_tests(s):
    spans = []
    for m in re.finditer(r'#\[cfg\(test\)\]', s):
        depth = 0
        for t in brace.finditer(s, m.end()):
            g = t.group()
            if len(g) != 1: continue
            if g == ';' and depth == 0:
                spans.append((m.start(), t.end())); break
            if g == '{': depth += 1
            elif g == '}':
                depth -= 1
                if depth == 0:
                    spans.append((m.start(), t.end())); break
    for x, y in reversed(spans):
        s = s[:x] + re.sub(r'[^\n]', ' ', s[x:y]) + s[y:]
    return s
def is_german(text):
    text = re.sub(r'\{[^{}]*\}', '', text)
    if re.search('[ÄÖÜäöüß]', text): return True
    toks = re.findall(r'[A-Za-zÄÖÜäöüß]+', text.lower())
    return any(t in WORDS for t in toks) and len(text) > 2
hits = []
for p in sorted(root.rglob('*.rs')):
    rel = p.relative_to(root).as_posix()
    if rel.startswith('i18n/') or rel in ('ui_test.rs', 'film_look_tests.rs', 'test_matrix.rs', 'stress.rs', 'test_image.rs'): continue
    if a.file and a.file not in rel: continue
    s = mask_tests(p.read_text())
    for m in lex.finditer(s):
        t = m.group()
        if t.startswith('//') or t.startswith('/*') or t.startswith("'"): continue
        text = t[1:-1] if t.startswith('"') else t
        if not is_german(text): continue
        if re.fullmatch(r'[a-z0-9_.:/\-{}<>+*#%]+', text): continue   # identifier
        before = s[max(0, m.start() - 70):m.start()]
        before1 = re.sub(r'\s+', ' ', before)
        if re.search(r'(?:\btr|\btrf!)\s*\($', before1) or re.search(r'trf!\($', before1): kl = 'tr'
        elif DISPLAY.search(before1.rstrip() if before1.rstrip().endswith('(') else before1): kl = 'display'
        elif re.search(r'(?:^|[^\w])(?:log|eprintln!|println!|panic!|assert\w*!|expect|unreachable!)\s*\(?$', before1) or 'log(&' in before1[-12:]: kl = 'log'
        elif re.search(r'(?:section_reset|section_switch|section|slider_hint|slider_grad|slider|group_label|subsection|hilfe::\w+|field_label|dialog_heading)\s*\([^"]*$', before1): kl = 'inner'
        elif re.search(r'(?:lr::|dialog::|hilfe::|crate::lr::|crate::dialog::)\w+\s*\([^"]*$', before1): kl = 'helper'
        else: kl = 'other'
        hits.append((rel, s.count('\n', 0, m.start()) + 1, kl, text))
if a.tsv:
    a.tsv.write_text('\n'.join(f'{r}\t{l}\t{k}\t{t}' for r, l, k, t in hits))
for r, l, k, t in hits:
    if a.only_open and k == 'tr': continue
    print(f'{r}:{l}\t{k}\t{t[:110]!r}')
import collections
c = collections.Counter(k for _, _, k, _ in hits)
print('TOTAL', dict(c), 'distinct', len(set(t for _, _, _, t in hits)), file=sys.stderr)

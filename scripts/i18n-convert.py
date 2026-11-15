#!/usr/bin/env python3
"""Wrap German UI texts in crates/app/src in crate::tr(…) (display; keys stay German).

Usage: scripts/i18n-convert.py [--write] [--file part] [--report]
Without --write only counts. Skipped (by hand or in the helper function): comparisons, search functions,
match patterns, constants, arguments of format!/log, arguments of helper functions with lookup
(section*, slider*, group_label, subsection, hilfe::).
"""
import argparse, pathlib, re, sys, importlib.util
ap = argparse.ArgumentParser()
ap.add_argument('--write', action='store_true')
ap.add_argument('--file')
ap.add_argument('--report', action='store_true')
a = ap.parse_args()
here = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('lit', here / 'i18n-literals.py')
root = here.parent / 'crates/app/src'
LEX = r"//[^\n]*|/\*.*?\*/|(?:b?r(?P<h>\#*)\".*?\"(?P=h))|(?:b?\"(?:\\.|[^\"\\])*\")|'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'"
lex = re.compile(LEX, re.S)
brace = re.compile(LEX + r'|[{};]', re.S)
WORDS = set('abbrechen abzug aktiv anzeigen aufnahme bearbeiten belichtung bereit bewertung bild bilder bildschirm breite dunkelkammer datei dateien datenblatt drehen drucken eigene einstellungen einzelbild entwickeln export exportieren farbe farben fehler film filme filmprofile filter filmstreifen fokus foto fotos grad grau herunterladen hinweis helligkeit hilfe histogramm infos kamera kein keine korn kontrast kopieren laden lichter liste maske masken mediathek minuten mitgelieferte nachbelichten name neu neue neues ordner papier papiere profil profile raster rezept rezepte speichern schatten schwarz schwarzwert schärfe start suche temperatur tiefen tonung vergleich vergleichen verlauf verlaufen verwalten vorschau wählen zeigen ziel zurück und oder nicht mit von für wird sind ist werden alle ohne nur dann noch auch'.split())
def is_german(text):
    text = re.sub(r'\{[^{}]*\}', '', text)
    if re.search('[ÄÖÜäöüß]', text): return True
    toks = re.findall(r'[A-Za-zÄÖÜäöüß]+', text.lower())
    return any(t in WORDS for t in toks) and len(text) > 2
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
    return spans
SKIP_FILES = {'lr.rs', 'help.rs', 'shortcuts.rs', 'ui_test.rs', 'film_look_tests.rs', 'test_matrix.rs', 'stress.rs', 'test_image.rs', 'prefs_i18n.rs'}
LOOKUP = re.compile(r'(?:section_reset|section_switch|section|slider_hint|slider_grad|slider|group_label|subsection|abschnitt_merken|hilfe::\w+|fuer_regler|fuer_abschnitt|paste::\w+)\s*\([^"]*$')
SEARCH = re.compile(r'(?:==|!=|starts_with|ends_with|contains|strip_prefix|strip_suffix|find|rfind|split|replace|trim_matches|trim_start_matches|trim_end_matches|matches!|eq_ignore_ascii_case|splitn|rsplit|rsplit_once|split_once|get|entry|insert|remove|contains_key|join_|var|set_var|env|extension|with_extension|file_name|join|push_str|parse)\s*\(?\s*$|(?:==|!=)\s*$')
FORMAT = re.compile(r'(?:format!|println!|eprintln!|print!|write!|writeln!|panic!|assert!|assert_eq!|assert_ne!|debug_assert!|unreachable!|todo!|expect|log|diagnose|crate::log|json!|anyhow!|bail!|context|with_context)\s*\(?\s*(?:[^"(]*,\s*)?$')
report = {'wrapped': 0}
counters = {}
for p in sorted(root.rglob('*.rs')):
    rel = p.relative_to(root).as_posix()
    if rel.startswith('i18n/') or p.name in SKIP_FILES: continue
    if a.file and a.file not in rel: continue
    orig = p.read_text()
    tests = mask_tests(orig)
    def in_test(pos): return any(x <= pos < y for x, y in tests)
    out = []; last = 0; n = 0
    for m in lex.finditer(orig):
        t = m.group()
        if t.startswith('//') or t.startswith('/*') or t.startswith('b') or t.startswith('r') or t.startswith("'"): continue
        if in_test(m.start()): continue
        text = t[1:-1]
        if not is_german(text): continue
        if re.fullmatch(r'[a-z0-9_.:/\-{}<>+*#%]+', text): continue   # identifier, not a display text
        before = re.sub(r'\s+', ' ', orig[max(0, m.start() - 110):m.start()])
        after = orig[m.end():m.end() + 24]
        reason = None
        if re.search(r'(?:\btr|\btrf!)\s*\($', before): continue
        if re.match(r'\s*(?:\||=>|\.\.=|\.\.|if\b)', after) : reason = 'pattern'
        elif re.match(r'\s*(?:==|!=)', after): reason = 'comparison'
        elif SEARCH.search(before): reason = 'search'
        elif FORMAT.search(before): reason = 'format/log'
        elif LOOKUP.search(before): reason = 'lookup'
        else:
            # start of statement: const/static?
            k = max(orig.rfind(';', 0, m.start()), orig.rfind('{', 0, m.start()), orig.rfind('}', 0, m.start()))
            head = orig[k + 1:m.start()].lstrip()
            if re.match(r'(?:pub(?:\([a-z]+\))? )?(?:const|static) ', head): reason = 'constant'
            elif re.match(r'(?:#\[|use |mod )', head): reason = 'attribute'
        if reason:
            counters[reason] = counters.get(reason, 0) + 1
            if a.report: print(f'{rel}:{orig.count(chr(10), 0, m.start()) + 1}\t{reason}\t{text[:90]!r}')
            continue
        if a.report: print(f'{rel}:{orig.count(chr(10), 0, m.start()) + 1}\tWRAP\t{before[-60:]!r} ⟦{text[:50]}⟧')
        out.append(orig[last:m.start()]); out.append('crate::tr(' + t + ')'); last = m.end(); n += 1
    out.append(orig[last:])
    report['wrapped'] += n
    if a.write and n: p.write_text(''.join(out))
print(report, counters, file=sys.stderr)

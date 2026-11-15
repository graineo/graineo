#!/usr/bin/env python3
"""Second pass: wrap text literals passed directly to a display function in crate::tr(…) – even without
recognized German (words like "Lichthof" are in no word list). Without --write only count/list."""
import re, pathlib, sys
write = '--write' in sys.argv
root = pathlib.Path('crates/app/src')
LEX = r"//[^\n]*|/\*.*?\*/|(?:b?r(?P<h>\#*)\".*?\"(?P=h))|(?:b?\"(?:\\.|[^\"\\])*\")|'(?:\\u\{[0-9a-fA-F]+\}|\\.|[^\\'])'"
lex = re.compile(LEX, re.S); brace = re.compile(LEX + r'|[{};]', re.S)
DISPLAY_FUNCS = {'label', 'button', 'small_button', 'heading', 'on_hover_text', 'on_disabled_hover_text', 'hint_text', 'selected_text', 'colored_label', 'checkbox', 'selectable_label', 'hinweis', 'chip', 'option', 'ergebnis', 'fortschritt', 'field_label', 'tipp', 'icon_button_tipp', 'hauptknopf', 'hauptknopf_min', 'kopf', 'zeile', 'wert', 'gruppe', 'bestaetigen', 'zerstoerend', 'badge', 'pill', 'dialog_heading', 'kategorie', 'eingabe', 'row', 'liste', 'schalter', 'toggle', 'segmented', 'choice_card', 'list_item', 'strong', 'add_filter', 'set_title', 'Status', 'Fehler', 'Hinweis'}
SPECIAL = {'new': ('RichText', 'Button', 'Label', 'ProgressBar'), 'zeigen': None, 'text': None, 'selectable_value': 'letzte', 'radio_value': 'letzte', 'anzahl': None}
def callee(s, pos):
    depth = 0; i = pos - 1
    while i > 0:
        c = s[i]
        if c in ')]}': depth += 1
        elif c in '([{':
            if depth == 0:
                if c != '(': return None
                m = re.search(r'([\w:]+)\s*!?\s*$', s[max(0, i - 60):i])
                return m.group(1) if m else None
            depth -= 1
        i -= 1
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
SKIP = {'lr.rs', 'ui_test.rs', 'film_look_tests.rs', 'test_matrix.rs', 'stress.rs', 'test_image.rs', 'prefs_i18n.rs', 'help.rs', 'shortcuts.rs'}
total = 0
for p in sorted(root.rglob('*.rs')):
    rel = p.relative_to(root).as_posix()
    if rel.startswith('i18n/') or p.name in SKIP: continue
    s = p.read_text(); ts = tests(s); out = []; last = 0; n = 0
    for m in lex.finditer(s):
        t = m.group()
        if not t.startswith('"'): continue
        if any(x <= m.start() < y for x, y in ts): continue
        text = t[1:-1]
        if len(re.findall(r'[A-Za-zÄÖÜäöüß]', text)) < 2: continue
        if re.fullmatch(r'[a-z0-9_.:/\-{}<>+*#%@\[\]|&!~ ]+', text) and ' ' not in text.strip(): continue
        if '\\u{' in text or re.fullmatch(r'[a-z][A-Za-z0-9_]*', text): continue
        if re.search(r'(?:\btr|\btrf!)\s*\(\s*$', s[max(0, m.start() - 12):m.start()]): continue
        c = callee(s, m.start())
        if not c: continue
        short = c.split('::')[-1]
        ok = short in DISPLAY_FUNCS
        if short == 'new': ok = c.split('::')[-2:-1] and c.split('::')[-2] in SPECIAL['new']
        if short in ('text', 'zeigen', 'anzahl'): ok = True
        if short in ('selectable_value', 'radio_value'):
            ok = re.match(r'\s*[,)]', s[m.end():m.end() + 3]) is not None and re.match(r'\s*\)', s[m.end():m.end() + 3]) is not None
        if short == 'text' and 'painter' not in c and 'dialog' not in c and not c.startswith('p.') and not c.startswith('ui.painter') and '.' not in c and '::' not in c:
            ok = True
        if not ok: continue
        # identifiers as the first argument text (e.g. zeigen(ctx, "karte", …)) are lower case and were filtered above
        out.append(s[last:m.start()]); out.append('crate::tr(' + t + ')'); last = m.end(); n += 1
        if not write: print(f'{rel}:{s.count(chr(10), 0, m.start()) + 1}\t{short}\t{text[:70]!r}')
    out.append(s[last:]); total += n
    if write and n: p.write_text(''.join(out))
print({'wrapped': total}, file=sys.stderr)

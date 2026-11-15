#!/usr/bin/env python3
"""Static audit: German-looking string literals that are NOT in the English dictionary.

Finds the texts that reach the display without `tr(…)` / `trf!(…)` (constants, parameters, `format!` results,
error texts of the library crates). Usage:
  i18n-static.py [--crates]     list (app sources; with --crates also the crates that deliver UI text)
  i18n-static.py --count       only the number
A hit is a candidate: check whether it is displayed. Log lines, keys, identifiers: add them to
crates/app/locales/i18n-ignore.txt (one literal per line, exact text, or `file.rs:text`).
"""
import re, sys, pathlib, importlib.util
here = pathlib.Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location('keys_mod', here / 'i18n-keys.py')
keys_mod = importlib.util.module_from_spec(spec)
spec.loader.exec_module(keys_mod)
root = keys_mod.root
WORDS = set('der die das den dem des ein eine einen einem einer und oder nicht kein keine mit von für auf aus bei nach über unter zum zur ist sind wird werden wurde kann können muss noch nur auch wie wenn als dass bild bilder foto fotos datei dateien ordner fehler öffnen speichern löschen wählen zurück abbrechen schließen fehlt fehlen gespeichert geladen läuft zeile spalte regler stufe sammlung bibliothek einstellungen vorschau entwickeln gesichter kamera objektiv'.split())


def is_german(t):
    if not keys_mod.is_german(t) or keys_mod.placeholders_only(t):
        return False
    if any(c in t for c in 'äöüÄÖÜß'):
        return True
    return any(w.lower() in WORDS for w in re.findall(r'[A-Za-zÄÖÜäöüß]+', t))


def ignore_list():
    f = root / 'crates/app/locales/i18n-ignore.txt'
    if not f.exists():
        return set()
    return {z for z in f.read_text().split('\n') if z and not z.startswith('#')}


SKIP_BEFORE = re.compile(
    r'(?:\bcrate::log|\blog|println!|eprintln!|panic!|assert\w*!|expect|debug_assert\w*!|diagnose|unreachable!|todo!|trace!|dbg!)\s*\(?\s*$'
    r'|(?:\bextension|\bjoin|\bfile_name|\bget|\bremove|\binsert|\bcontains_key|\bset|\bvalue|\bsetting|\bkey|\bstarts_with|\bends_with|\bcontains|\bfind|\bsplit|\bstrip_prefix|\bstrip_suffix|\bsplit_once|\bvar|\bget_or)\s*\(\s*$')


def scan(dirs):
    have = keys_mod.existing()
    ign = ignore_list()
    out = []
    for d in dirs:
        for p in sorted(d.rglob('*.rs')):
            rel = p.relative_to(root).as_posix()
            if '/i18n/' in rel or p.name in ('ui_test.rs', 'prefs_i18n.rs') or p.name.endswith('tests.rs') or p.name.endswith('_test.rs'):
                continue
            s = p.read_text()
            ts = keys_mod.tests(s)
            for m in keys_mod.lex.finditer(s):
                if not (m.group('s') is not None or m.group('raw') is not None):
                    continue
                if any(x <= m.start() < y for x, y in ts):
                    continue
                t = keys_mod.entesc(m.group('s')) if m.group('s') is not None else m.group('raw')
                line = s.count('\n', 0, m.start()) + 1
                before = s[max(0, m.start() - 60):m.start()]
                if t in have or t in ign or f'{p.name}:{t}' in ign:
                    continue
                if SKIP_BEFORE.search(before):
                    continue
                if not is_german(t):
                    continue
                out.append((rel, line, t))
    return out


if __name__ == '__main__':
    a = sys.argv[1:]
    dirs = [root / 'crates/app/src']
    if '--crates' in a:
        dirs += [root / f'crates/{c}/src' for c in ('x3f', 'library', 'develop', 'rawdev', 'raw', 'sensor_view')]
    r = scan(dirs)
    if '--count' in a:
        print(len(r))
    else:
        for rel, line, t in r:
            print(f'{rel}:{line}: {t!r}')
        print(len(r), 'candidates')

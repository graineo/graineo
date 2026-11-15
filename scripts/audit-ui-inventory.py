#!/usr/bin/env python3
"""Heuristic UI text inventory; no translation and no complete visibility analysis."""
import argparse
import re, pathlib, collections, tomllib, json, subprocess
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--revision', help='read a git revision instead of the working files')
parser.add_argument('--literals', type=pathlib.Path, help='optionally save candidates as TSV')
args = parser.parse_args()
root=pathlib.Path(__file__).resolve().parents[1]
# Heuristic: detect comments before strings; deliberately skip attributed test modules.
lex=re.compile(r'//[^\n]*|/\*.*?\*/|(?:b?r(?P<h>\#*)".*?"(?P=h))|(?:b?"(?:\\.|[^"\\])*")',re.S)
words=set('abbrechen abzug abzüge aktiv anzeigen aufnahme bearbeiten belichtung bereit bewertung bild bilder bildschirm breite dunkelkammer datei dateien datenblatt drehen drucken eigene einstellungen einzelbild entwickeln export exportieren farbe farben fehler film filme filmprofile filter filmstreifen fokus foto fotos grad grau herunterladen hinweis helligkeit hilfe histogramm infos kamera kein keine korn kontrast kopieren laden lichter liste maske masken mediathek minuten mitgelieferte nachbelichten name neues ordner papier papiere profil profile raster rezept rezepte speichern schatten schwarz schwarzwert schärfe start suche temperatur tiefen tonung vergleich vergleichen verlauf verlaufen verwalten vorschau wählen zeigen ziel zurück'.split())
entries=[]; all_literals=0; hover=0
files = ([root / f for f in subprocess.check_output(['git','ls-tree','-r','--name-only',args.revision,'--','crates/app/src'],cwd=root,text=True).splitlines() if f.endswith('.rs')]
         if args.revision else sorted((root/'crates/app/src').rglob('*.rs')))
for p in files:
 s=subprocess.check_output(["git", "show", args.revision+":"+str(p.relative_to(root))],cwd=root,text=True) if args.revision else p.read_text()
 if p.name=='ui_test.rs': continue
 # Omit only the attributed test item, including test modules embedded before production UI.
 spans=[]
 bracelex=re.compile(lex.pattern+r'|[{};]',re.S)
 for attr in re.finditer(r'#\[cfg\(test\)\]',s):
  depth=0
  for token in bracelex.finditer(s,attr.end()):
   t=token.group()
   if len(t)!=1: continue
   if t==';' and depth==0:
    spans.append((attr.start(),token.end())); break
   if t=='{': depth+=1
   elif t=='}':
    depth-=1
    if depth==0:
     spans.append((attr.start(),token.end())); break
 for a,b in reversed(spans): s=s[:a]+re.sub(r'[^\n]',' ',s[a:b])+s[b:]
 for m in lex.finditer(s):
  t=m.group()
  if t.startswith('//') or t.startswith('/*'): continue
  all_literals+=1
  text=t.strip('"'); tokens=set(re.findall(r'[A-Za-zÄÖÜäöüß]+',text.lower()))
  german=bool(re.search('[ÄÖÜäöüß]',text)) or bool(tokens&words)
  if german: entries.append((str(p.relative_to(root)),s.count('\n',0,m.start())+1,text))
 hover+=len(re.findall(r'\.on_hover_(?:text|ui)\s*\(',s))
if args.literals: args.literals.write_text('\n'.join(f'{p}\t{line}\t{t}' for p,line,t in entries))
lock_text = subprocess.check_output(['git','show',args.revision+':Cargo.lock'],cwd=root,text=True) if args.revision else (root/'Cargo.lock').read_text()
lock=tomllib.loads(lock_text); pkgs=lock['package']
selected={'image','rawler','rusqlite','libsqlite3-sys','egui','eframe','wgpu','serde_json','roxmltree','flate2','zip','libc','reqwest','ureq','openssl','rustls','muda','rfd','sha2'}
print(json.dumps({'rust_files':len(files),'all_literals':all_literals,'german_candidates':len(entries),'distinct_candidates':len(set(t for _,_,t in entries)),'hover_calls':hover,'german_by_file':dict(collections.Counter(p for p,_,_ in entries).most_common(12)),'dependencies':len(pkgs),'selected_versions':[(p['name'],p['version']) for p in pkgs if p['name'] in selected]},ensure_ascii=False,indent=2))

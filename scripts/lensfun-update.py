#!/usr/bin/env python3
"""Build crates/raw/data/lensfun-db.xml.gz from a checkout of the lensfun database.

Usage: scripts/lensfun-update.py <lensfun checkout>/data/db <commit> <date>

All files of data/db are merged into one <lensdatabase> document. Changes to the data:
comments, indentation and translated names other than English (lang="en" stays) are
removed, everything else is taken over unchanged. One own camera entry is appended
(Ricoh GR II, same lens as the GR). The result is gzip-compressed (the app unpacks it
once on first use). Data license: CC BY-SA 3.0, (c) the lensfun contributors.
"""
import gzip
import pathlib
import re
import sys

OWN = """<!-- own addition (not from lensfun): the Ricoh GR II has the lens of the GR -->
<camera><maker>Ricoh Imaging Company, Ltd.</maker><model>GR II</model><mount>ricohGR</mount><cropfactor>1.523</cropfactor></camera>"""


def main() -> None:
    db, commit, date = pathlib.Path(sys.argv[1]), sys.argv[2], sys.argv[3]
    body = []
    for f in sorted(db.glob("*.xml")):
        t = f.read_text(encoding="utf-8")
        m = re.search(r"<lensdatabase[^>]*>(.*)</lensdatabase>", t, re.S)
        if not m:
            continue
        t = re.sub(r"<!--.*?-->", "", m.group(1), flags=re.S)
        t = re.sub(r'\s*<(\w+) lang="(?!en")[^"]*">[^<]*</\1>', "", t)
        t = re.sub(r">\s+<", "><", t).strip()
        body.append(f"<!-- {f.name} -->{t}")
    head = f"""<?xml version="1.0" encoding="UTF-8"?>
<!--
  lensfun database (https://github.com/lensfun/lensfun, data/db, commit {commit}, {date}).
  License of the data: Creative Commons Attribution-ShareAlike 3.0 (CC BY-SA 3.0),
  https://creativecommons.org/licenses/by-sa/3.0/ - (c) the lensfun contributors.
  Changes: all files merged into one; comments, indentation and non-English translated
  names removed, elements otherwise unchanged; in addition one own camera entry "GR II"
  (same lens as the GR, marked as such). This file is also licensed under CC BY-SA 3.0
  and is a separate data file (not part of the program code). Built by scripts/lensfun-update.py.
-->
<lensdatabase version="2">
"""
    xml = head + "\n".join(body) + "\n" + OWN + "\n</lensdatabase>\n"
    out = pathlib.Path(__file__).resolve().parent.parent / "crates/raw/data/lensfun-db.xml.gz"
    raw = xml.encode("utf-8")
    out.write_bytes(gzip.compress(raw, compresslevel=9, mtime=0))
    print(f"{len(raw)} bytes raw, {out.stat().st_size} bytes compressed -> {out}")


if __name__ == "__main__":
    main()

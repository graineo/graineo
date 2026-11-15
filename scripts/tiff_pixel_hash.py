#!/usr/bin/env python3
"""Checksum of only the image data of a TIFF (strips/tiles of the first page), independent of the program identifier."""
import struct, sys, hashlib
def pixel_hash(path):
    d = open(path, 'rb').read()
    e = '<' if d[:2] == b'II' else '>'
    big = struct.unpack(e + 'H', d[2:4])[0] == 43
    if big:
        off = struct.unpack(e + 'Q', d[8:16])[0]; n = struct.unpack(e + 'Q', d[off:off + 8])[0]; p = off + 8; size = 20
    else:
        off = struct.unpack(e + 'I', d[4:8])[0]; n = struct.unpack(e + 'H', d[off:off + 2])[0]; p = off + 2; size = 12
    tags = {}
    tsz = {3: 2, 4: 4, 16: 8}
    for i in range(n):
        q = p + i * size
        if big:
            tag, typ, cnt = struct.unpack(e + 'HHQ', d[q:q + 12]); val = d[q + 12:q + 20]; inl = 8
        else:
            tag, typ, cnt = struct.unpack(e + 'HHI', d[q:q + 8]); val = d[q + 8:q + 12]; inl = 4
        if tag in (273, 279, 324, 325):
            w = tsz[typ]; fmt = {2: 'H', 4: 'I', 8: 'Q'}[w]
            raw = val if cnt * w <= inl else d[struct.unpack(e + ('Q' if big else 'I'), val)[0]:][:cnt * w]
            tags[tag] = list(struct.unpack(e + fmt * cnt, raw[:cnt * w]))
    offs, cnts = (tags[273], tags[279]) if 273 in tags else (tags[324], tags[325])
    h = hashlib.sha256()
    for o, c in zip(offs, cnts):
        h.update(d[o:o + c])
    return h.hexdigest()[:16]
for f in sys.argv[1:]:
    print(pixel_hash(f))

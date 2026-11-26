#!/bin/bash
# Test charts for the HEIC decoder (heif_lib.rs): 600×400, top row red, green, blue patches
# (sRGB primaries), bottom half mid grey. Needs python3 and heif-enc (libheif examples, e.g. brew install libheif).
#   srgb.heic      8 bit, nclx BT.709
#   p3.heic        8 bit, the same colours converted to Display P3, nclx primaries 12
#   p3-10bit.heic  10 bit, as p3.heic
#   rot90.heic     srgb.heic turned 90° clockwise through an irot property (decoded 400×600)
#   thumb.heic     srgb.heic with an embedded 120×80 thumbnail (thmb item)
set -euo pipefail
cd "$(dirname "$0")"
TMP=$(mktemp -d)
python3 -I - "$TMP" <<'EOF'
import struct, sys, zlib
out = sys.argv[1]
def enc(c): return 12.92 * c if c <= 0.0031308 else 1.055 * c ** (1 / 2.4) - 0.055
def png(name, cols, bits):
    w, h = 600, 400
    rows = []
    for y in range(h):
        r = bytearray(b'\0')
        for x in range(w):
            c = cols[x // 200] if y < 200 else (0.2, 0.2, 0.2)
            if bits == 8:
                r += bytes(int(enc(v) * 255 + 0.5) for v in c)
            else:
                r += b''.join(struct.pack('>H', int(enc(v) * 65535 + 0.5)) for v in c)
        rows.append(bytes(r))
    def chunk(t, d): return struct.pack('>I', len(d)) + t + d + struct.pack('>I', zlib.crc32(t + d) & 0xffffffff)
    data = b'\x89PNG\r\n\x1a\n' + chunk(b'IHDR', struct.pack('>IIBBBBB', w, h, bits, 2, 0, 0, 0)) + chunk(b'IDAT', zlib.compress(b''.join(rows))) + chunk(b'IEND', b'')
    open(f'{out}/{name}', 'wb').write(data)
srgb = [(1, 0, 0), (0, 1, 0), (0, 0, 1)]
# linear sRGB primaries in linear Display P3
p3 = [(0.8225, 0.0332, 0.0171), (0.1774, 0.9669, 0.0724), (0.0, 0.0, 0.9108)]
png('srgb.png', srgb, 8)
png('p3.png', p3, 8)
png('p3-16.png', p3, 16)
EOF
heif-enc -q 92 --colour_primaries 1 --transfer_characteristic 13 -o srgb.heic "$TMP/srgb.png" > /dev/null
heif-enc -q 92 --colour_primaries 12 --transfer_characteristic 13 -o p3.heic "$TMP/p3.png" > /dev/null
heif-enc -q 92 -b 10 --colour_primaries 12 --transfer_characteristic 13 -o p3-10bit.heic "$TMP/p3-16.png" > /dev/null
heif-enc -q 92 --rotate-cw 90 --colour_primaries 1 --transfer_characteristic 13 -o rot90.heic "$TMP/srgb.png" > /dev/null
heif-enc -q 92 --thumb 120 --colour_primaries 1 --transfer_characteristic 13 -o thumb.heic "$TMP/srgb.png" > /dev/null
rm -rf "$TMP"
ls -l ./*.heic

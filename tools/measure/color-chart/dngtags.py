# SPDX-License-Identifier: GPL-3.0-only
"""Reads the colour tags of a DNG file (IFD0 + SubIFDs only, read access)."""
import sys, struct
TAGS = {50721: 'ColorMatrix1', 50722: 'ColorMatrix2', 50723: 'CameraCalibration1', 50724: 'CameraCalibration2', 50727: 'AnalogBalance',
        50728: 'AsShotNeutral', 50730: 'BaselineExposure', 50778: 'CalibrationIlluminant1', 50779: 'CalibrationIlluminant2',
        50964: 'ForwardMatrix1', 50965: 'ForwardMatrix2', 50936: 'ProfileName', 50938: 'ProfileHueSatMapData1', 50940: 'ProfileToneCurve',
        50982: 'ProfileLookTableData', 50706: 'DNGVersion', 271: 'Make', 272: 'Model', 305: 'Software'}
SIZES = {1: 1, 2: 1, 3: 2, 4: 4, 5: 8, 7: 1, 8: 2, 9: 4, 10: 8, 11: 4, 12: 8}


def read(path):
    b = open(path, 'rb').read()
    e = '<' if b[:2] == b'II' else '>'
    out = {}

    def val(t, n, off):
        if t in (5, 10):
            f = 'I' if t == 5 else 'i'
            v = struct.unpack(e + f * (2 * n), b[off:off + 8 * n])
            return [v[2 * i] / v[2 * i + 1] if v[2 * i + 1] else 0 for i in range(n)]
        if t == 2:
            return b[off:off + n].split(b'\0')[0].decode('latin1').strip()
        f = {1: 'B', 3: 'H', 4: 'I', 7: 'B', 8: 'h', 9: 'i', 11: 'f', 12: 'd'}[t]
        return list(struct.unpack(e + f * n, b[off:off + SIZES[t] * n]))

    def ifd(off, depth=0):
        n = struct.unpack(e + 'H', b[off:off + 2])[0]
        for i in range(n):
            tag, t, cnt, v = struct.unpack(e + 'HHI4s', b[off + 2 + 12 * i: off + 14 + 12 * i])
            size = SIZES.get(t, 1) * cnt
            p = off + 2 + 12 * i + 8 if size <= 4 else struct.unpack(e + 'I', v)[0]
            if tag in TAGS and TAGS[tag] not in out:
                out[TAGS[tag]] = val(t, min(cnt, 9) if t != 2 else cnt, p) if tag not in (50938, 50982, 50940) else f'{cnt} values'
            if tag == 330 and depth < 2:
                for s in val(t, cnt, p):
                    ifd(s, depth + 1)

    ifd(struct.unpack(e + 'I', b[4:8])[0])
    return out


if __name__ == '__main__':
    for p in sys.argv[1:]:
        print(p.split('/')[-1])
        for k, v in read(p).items():
            print('  ', k, [round(x, 4) for x in v] if isinstance(v, list) else v)

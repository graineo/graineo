//! RAW format 6 of the older Foveon cameras (SD9, SD10, SD14): 10-bit Huffman with an explicit code table
//! and a difference table.
//!
//! Section content after the 28-byte image header:
//! ```text
//! i16 diff[1024]       value added for each symbol ("DPCM table")
//! u32 code[1024]       per symbol: code length in the top 5 bits, code in the low 27 bits
//! data                 Huffman-coded rows, each starting at a byte boundary
//! u32 row_start[rows]  offset of each row in `data` (at the very end of the section)
//! ```
//! Each row holds `columns × 3` symbols in pixel order (three layers per pixel); codes are read most
//! significant bit first, bits past the end of a row read as zero. The value of a layer is the running
//! sum of `diff[symbol]` along the row, starting at zero at every row (16-bit arithmetic).
//!
//! Decoding uses a two-level lookup (a [`FIRST_BITS`]-bit first level, sub-tables for longer codes) and a
//! 64-bit bit reader, on one thread. Codes are up to 24 bits long; a single table over 24 bits (64 MB) would
//! have to be built for every file and misses the processor cache on almost every symbol. The two-level table
//! gives exactly the same result as that single table, including tables whose codes overlap (a later symbol
//! wins).

use crate::error::{corrupt, Result};
use crate::raw::RawImage;

const SYMBOLS: usize = 1024;
/// Longest supported code.
const MAX_LEN: u32 = 24;
/// Bits of the first lookup level.
const FIRST_BITS: u32 = 12;
/// Marks a first-level entry that points to a sub-table (`sym` is then the sub-table number).
const SUB: u8 = 0xff;

/// Lookup entry: symbol and code length; length 0 = no code.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Entry {
    sym: u16,
    len: u8,
}

const NONE: Entry = Entry { sym: 0, len: 0 };

/// Prefix decoder for an explicit code table.
struct Decoder {
    /// bits of the first level (`min(longest code, FIRST_BITS)`)
    first_bits: u32,
    /// bits of each sub-table (`longest code - first_bits`)
    sub_bits: u32,
    /// first level over the leading `first_bits` bits
    first: Vec<Entry>,
    /// all sub-tables one after the other, `1 << sub_bits` entries each
    subs: Vec<Entry>,
}

impl Decoder {
    fn new(codes: &[u32]) -> Result<Self> {
        let max = codes.iter().map(|c| c >> 27).max().unwrap_or(0);
        if max == 0 || max > MAX_LEN {
            return corrupt(format!("Huffman-Tabelle: Codelänge {max} nicht unterstützt"));
        }
        let first_bits = max.min(FIRST_BITS);
        let sub_bits = max - first_bits;
        let sub_size = 1usize << sub_bits;
        let mut first = vec![NONE; 1 << first_bits];
        let mut subs: Vec<Entry> = Vec::new();
        // Symbols are entered in order and overwrite what earlier ones entered, as in a single table over the
        // longest code (a sub-table stands for the `sub_size` entries of that table behind its prefix).
        for (s, c) in codes.iter().enumerate() {
            let len = c >> 27;
            if len == 0 {
                continue;
            }
            let code = c & 0x07ff_ffff;
            if code >> len != 0 {
                return corrupt("Huffman-Tabelle: Code länger als angegeben");
            }
            let e = Entry { sym: s as u16, len: len as u8 };
            if len <= first_bits {
                let begin = (code << (first_bits - len)) as usize;
                for f in &mut first[begin..begin + (1usize << (first_bits - len))] {
                    if f.len == SUB {
                        let o = usize::from(f.sym) * sub_size;
                        subs[o..o + sub_size].fill(e);
                    } else {
                        *f = e;
                    }
                }
            } else {
                let rest_bits = len - first_bits;
                let f = &mut first[(code >> rest_bits) as usize];
                if f.len != SUB {
                    // split: the sub-table starts with what the first level held for the whole prefix
                    let n = subs.len() / sub_size;
                    subs.resize(subs.len() + sub_size, *f);
                    *f = Entry { sym: n as u16, len: SUB };
                }
                let o = usize::from(f.sym) * sub_size;
                let rest = code & ((1 << rest_bits) - 1);
                let begin = o + (rest << (max - len)) as usize;
                subs[begin..begin + (1usize << (max - len))].fill(e);
            }
        }
        Ok(Self { first_bits, sub_bits, first, subs })
    }

    /// Entry for the code at the top of `window` (the next bits of the stream, most significant bit first).
    #[inline(always)]
    fn lookup(&self, window: u64) -> Entry {
        let e = self.first[(window >> (64 - self.first_bits)) as usize];
        if e.len != SUB {
            return e;
        }
        // a sub-table exists only for codes longer than `first_bits`, so `sub_bits ≥ 1`
        let i = ((window << self.first_bits) >> (64 - self.sub_bits)) as usize;
        self.subs[(usize::from(e.sym) << self.sub_bits) + i]
    }
}

/// Bit reader over one row, most significant bit first; bits past the end of the row read as zero.
struct Bits<'a> {
    row: &'a [u8],
    /// next byte to load
    pos: usize,
    /// loaded bits, left-aligned
    acc: u64,
    /// number of valid bits in `acc`
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(row: &'a [u8]) -> Self {
        Self { row, pos: 0, acc: 0, count: 0 }
    }

    /// Ensure at least 56 valid bits (more than the longest code).
    #[inline(always)]
    fn refill(&mut self) {
        if self.count >= 56 {
            return;
        }
        if let Some(w) = self.row.get(self.pos..self.pos + 8) {
            // eight bytes at once: count the whole bytes that fit; the bits of a partly taken byte are loaded again
            // at the same place next time (the same bits, so the OR does not change them)
            let w = u64::from_be_bytes(w.try_into().unwrap());
            self.acc |= w >> self.count;
            let take = (63 - self.count) >> 3;
            self.pos += take as usize;
            self.count += take * 8;
        } else {
            while self.count < 56 {
                let b = self.row.get(self.pos).copied().unwrap_or(0);
                self.acc |= u64::from(b) << (56 - self.count);
                self.pos += 1;
                self.count += 8;
            }
        }
    }

    #[inline(always)]
    fn consume(&mut self, n: u32) {
        self.acc <<= n;
        self.count -= n;
    }
}

/// Decodes the content of a format-6 RAW section (without the 28-byte header).
pub fn decode(body: &[u8], columns: usize, rows: usize) -> Result<RawImage> {
    let tab_end = SYMBOLS * 2 + SYMBOLS * 4;
    let pointer_length = rows * 4;
    if body.len() < tab_end + pointer_length {
        return corrupt("RAW-Format 6: Abschnitt zu kurz");
    }
    let diff: Vec<i16> = (0..SYMBOLS).map(|i| i16::from_le_bytes([body[i * 2], body[i * 2 + 1]])).collect();
    let codes: Vec<u32> = (0..SYMBOLS)
        .map(|i| {
            let o = SYMBOLS * 2 + i * 4;
            u32::from_le_bytes([body[o], body[o + 1], body[o + 2], body[o + 3]])
        })
        .collect();
    let dec = Decoder::new(&codes)?;
    let data = &body[tab_end..body.len() - pointer_length];
    let pointer = &body[body.len() - pointer_length..];
    let start = |r: usize| -> usize {
        if r == rows {
            data.len()
        } else {
            u32::from_le_bytes([pointer[r * 4], pointer[r * 4 + 1], pointer[r * 4 + 2], pointer[r * 4 + 3]]) as usize
        }
    };
    let n = columns * rows;
    let mut planes = [vec![0u16; n], vec![0u16; n], vec![0u16; n]];
    let [p0, p1, p2] = &mut planes;
    for r in 0..rows {
        let (a, b) = (start(r), start(r + 1));
        if a > b || b > data.len() {
            return corrupt(format!("RAW-Format 6: Zeile {r} außerhalb der Daten"));
        }
        let mut bits = Bits::new(&data[a..b]);
        let mut sum = [0i16; 3];
        let out = r * columns..(r + 1) * columns;
        for ((v0, v1), v2) in p0[out.clone()].iter_mut().zip(&mut p1[out.clone()]).zip(&mut p2[out]) {
            for (s, v) in sum.iter_mut().zip([v0, v1, v2]) {
                bits.refill();
                let e = dec.lookup(bits.acc);
                if e.len == 0 {
                    return corrupt(format!("RAW-Format 6: ungültiger Code in Zeile {r}"));
                }
                bits.consume(u32::from(e.len));
                *s = s.wrapping_add(diff[usize::from(e.sym)]);
                *v = *s as u16;
            }
        }
    }
    Ok(RawImage { columns, rows, planes })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The previous decoder (one table over the longest code, bit-by-bit reader), as the reference.
    fn decode_reference(body: &[u8], columns: usize, rows: usize) -> Option<[Vec<u16>; 3]> {
        let tab_end = SYMBOLS * 6;
        let pointer_length = rows * 4;
        if body.len() < tab_end + pointer_length {
            return None;
        }
        let diff: Vec<i16> = (0..SYMBOLS).map(|i| i16::from_le_bytes([body[i * 2], body[i * 2 + 1]])).collect();
        let codes: Vec<u32> = (0..SYMBOLS).map(|i| u32::from_le_bytes(body[SYMBOLS * 2 + i * 4..][..4].try_into().unwrap())).collect();
        let max = codes.iter().map(|c| c >> 27).max().unwrap_or(0);
        if max == 0 || max > 24 {
            return None;
        }
        let mut table = vec![(0u16, 0u8); 1 << max];
        for (s, c) in codes.iter().enumerate() {
            let len = c >> 27;
            if len == 0 {
                continue;
            }
            let code = c & 0x07ff_ffff;
            if code >> len != 0 {
                return None;
            }
            let begin = (code << (max - len)) as usize;
            for e in &mut table[begin..begin + (1usize << (max - len))] {
                *e = (s as u16, len as u8);
            }
        }
        let data = &body[tab_end..body.len() - pointer_length];
        let pointer = &body[body.len() - pointer_length..];
        let start = |r: usize| if r == rows { data.len() } else { u32::from_le_bytes(pointer[r * 4..][..4].try_into().unwrap()) as usize };
        let n = columns * rows;
        let mut planes = [vec![0u16; n], vec![0u16; n], vec![0u16; n]];
        for r in 0..rows {
            let (a, b) = (start(r), start(r + 1));
            if a > b || b > data.len() {
                return None;
            }
            let row = &data[a..b];
            let mut pos = 0usize;
            let bits = |pos: usize| -> u32 {
                let mut v = 0u32;
                for k in 0..max as usize {
                    let p = pos + k;
                    v = (v << 1) | u32::from(row.get(p >> 3).map_or(0, |by| (by >> (7 - (p & 7))) & 1));
                }
                v
            };
            let mut sum = [0i16; 3];
            for x in 0..columns {
                for (c, s) in sum.iter_mut().enumerate() {
                    let (sym, len) = table[bits(pos) as usize];
                    if len == 0 {
                        return None;
                    }
                    pos += usize::from(len);
                    *s = s.wrapping_add(diff[usize::from(sym)]);
                    planes[c][r * columns + x] = *s as u16;
                }
            }
        }
        Some(planes)
    }

    /// Small random generator (xorshift), reproducible.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }
        fn below(&mut self, n: u64) -> u64 {
            self.next() % n
        }
    }

    /// Writes bits most significant bit first.
    #[derive(Default)]
    struct Writer {
        bytes: Vec<u8>,
        bits: usize,
    }

    impl Writer {
        fn put(&mut self, code: u32, len: u32) {
            for k in (0..len).rev() {
                if self.bits % 8 == 0 {
                    self.bytes.push(0);
                }
                if (code >> k) & 1 == 1 {
                    *self.bytes.last_mut().unwrap() |= 0x80 >> (self.bits % 8);
                }
                self.bits += 1;
            }
        }
    }

    /// Section body for code table `codes` ((length, code) per symbol) and the rows `rows_data` (bytes per row).
    fn section(codes: &[(u32, u32)], diff: &[i16], rows_data: &[Vec<u8>]) -> Vec<u8> {
        let mut body = Vec::new();
        for i in 0..SYMBOLS {
            body.extend_from_slice(&diff.get(i).copied().unwrap_or(0).to_le_bytes());
        }
        for i in 0..SYMBOLS {
            let (len, code) = codes.get(i).copied().unwrap_or((0, 0));
            body.extend_from_slice(&((len << 27) | code).to_le_bytes());
        }
        let mut starts = Vec::new();
        let mut data = Vec::new();
        for r in rows_data {
            starts.push(data.len() as u32);
            data.extend_from_slice(r);
        }
        body.extend_from_slice(&data);
        for s in starts {
            body.extend_from_slice(&s.to_le_bytes());
        }
        body
    }

    /// Complete prefix code with lengths 1 … 24: symbol k (k < 24) = k ones and a zero, symbol 24 = 24 ones; the
    /// longest codes are 24 bits.
    fn unary_code() -> Vec<(u32, u32)> {
        let mut c: Vec<(u32, u32)> = (0..24u32).map(|k| (k + 1, ((1u32 << (k + 1)) - 1) ^ 1)).collect();
        c.push((24, (1 << 24) - 1));
        c
    }

    /// Encodes the symbols `syms` (row by row, `columns × 3` each) with `codes`.
    fn encode_rows(codes: &[(u32, u32)], syms: &[Vec<usize>]) -> Vec<Vec<u8>> {
        syms.iter()
            .map(|row| {
                let mut w = Writer::default();
                for &s in row {
                    w.put(codes[s].1, codes[s].0);
                }
                w.bytes
            })
            .collect()
    }

    fn check_same(body: &[u8], columns: usize, rows: usize) -> Option<[Vec<u16>; 3]> {
        let new = decode(body, columns, rows).ok().map(|r| r.planes);
        let old = decode_reference(body, columns, rows);
        assert_eq!(new, old, "two-level decoder differs from the single-table reference");
        new
    }

    #[test]
    fn decodes_codes_up_to_24_bits() {
        let codes = unary_code();
        let diff: Vec<i16> = (0..25).map(|k| (k as i16 - 12) * 37).collect();
        let (columns, rows) = (17, 5);
        let mut rng = Rng(0x1234_5678_9abc_def1);
        // every length occurs, the 24-bit codes often
        let syms: Vec<Vec<usize>> = (0..rows).map(|_| (0..columns * 3).map(|_| if rng.below(3) == 0 { 22 + rng.below(3) as usize } else { rng.below(25) as usize }).collect()).collect();
        let body = section(&codes, &diff, &encode_rows(&codes, &syms));
        let planes = check_same(&body, columns, rows).expect("decodes");
        for (r, row) in syms.iter().enumerate() {
            let mut sum = [0i16; 3];
            for x in 0..columns {
                for c in 0..3 {
                    sum[c] = sum[c].wrapping_add(diff[row[x * 3 + c]]);
                    assert_eq!(planes[c][r * columns + x], sum[c] as u16);
                }
            }
        }
    }

    #[test]
    fn long_rows_use_the_word_reader() {
        // rows much longer than 8 bytes: the reader loads eight bytes at once and falls back to single bytes at the end
        let codes = unary_code();
        let diff: Vec<i16> = (0..25).map(|k| k as i16 * 3 - 30).collect();
        let (columns, rows) = (400, 3);
        let mut rng = Rng(0xfeed_beef_0bad_c0de);
        let syms: Vec<Vec<usize>> = (0..rows).map(|_| (0..columns * 3).map(|_| rng.below(25) as usize).collect()).collect();
        let body = section(&codes, &diff, &encode_rows(&codes, &syms));
        check_same(&body, columns, rows).expect("decodes");
    }

    #[test]
    fn short_codes_only_use_one_level() {
        // longest code below the first level: no sub-tables
        let codes: Vec<(u32, u32)> = (0..8u32).map(|k| (3, k)).collect();
        let diff: Vec<i16> = (0..8).map(|k| k * 1000 - 3000).collect();
        let syms = vec![(0..30).map(|i| i % 8).collect::<Vec<_>>(); 3];
        let body = section(&codes, &diff, &encode_rows(&codes, &syms));
        let words: Vec<u32> = body[SYMBOLS * 2..SYMBOLS * 6].chunks(4).map(|b| u32::from_le_bytes(b.try_into().unwrap())).collect();
        let dec = Decoder::new(&words).unwrap();
        assert!(dec.subs.is_empty());
        assert_eq!(dec.first_bits, 3);
        check_same(&body, 10, 3).expect("decodes");
    }

    #[test]
    fn random_tables_and_streams_match_the_reference() {
        let mut rng = Rng(0x0dd_ba11_cafe_f00d);
        for round in 0..300 {
            // random code lengths up to 24; overlapping and incomplete tables included (a later symbol wins, gaps
            // are invalid codes)
            let n = 2 + rng.below(60) as usize;
            let codes: Vec<(u32, u32)> = (0..n)
                .map(|_| {
                    let len = 1 + rng.below(24) as u32;
                    (len, (rng.next() as u32) & ((1u32 << len) - 1))
                })
                .collect();
            let diff: Vec<i16> = (0..n).map(|_| rng.next() as i16).collect();
            let (columns, rows) = (1 + rng.below(9) as usize, 1 + rng.below(4) as usize);
            let rows_data: Vec<Vec<u8>> = if round % 2 == 0 {
                // rows written with the table (they may still contain codes a later symbol overwrote)
                let syms: Vec<Vec<usize>> = (0..rows).map(|_| (0..columns * 3).map(|_| rng.below(n as u64) as usize).collect()).collect();
                encode_rows(&codes, &syms)
            } else {
                // random bytes, including empty and short rows (bits past the end read as zero)
                (0..rows).map(|_| (0..rng.below(20)).map(|_| rng.next() as u8).collect()).collect()
            };
            let body = section(&codes, &diff, &rows_data);
            check_same(&body, columns, rows);
        }
    }

    #[test]
    fn empty_and_truncated_rows_read_zero_bits() {
        let codes = unary_code();
        let diff: Vec<i16> = (0..25).map(|k| k as i16 + 1).collect();
        // all-zero bits decode as symbol 0 ("0"), so an empty row is valid
        let body = section(&codes, &diff, &[Vec::new(), vec![0xff], vec![0xff, 0xff, 0xff, 0xfe]]);
        let planes = check_same(&body, 4, 3).expect("decodes");
        assert_eq!(planes[0][0..4], [1, 2, 3, 4]);
    }

    #[test]
    fn errors_as_before() {
        let codes = unary_code();
        let diff = vec![1i16; 25];
        let rows = encode_rows(&codes, &[vec![0; 6]]);
        let body = section(&codes, &diff, &rows);
        // section shorter than its tables
        assert!(decode(&body[..100], 2, 1).is_err());
        // row pointer past the data
        let mut bad = body.clone();
        let p = bad.len() - 4;
        bad[p..].copy_from_slice(&1000u32.to_le_bytes());
        assert!(decode(&bad, 2, 1).is_err());
        // invalid code: only "1" is a code, the stream starts with 0
        let only_one: Vec<(u32, u32)> = vec![(0, 0), (1, 1)];
        let body = section(&only_one, &diff, &[vec![0x00]]);
        assert!(check_same(&body, 1, 1).is_none());
        // code longer than its length; no code at all; length above 24
        assert!(decode(&section(&[(2, 7)], &diff, &[vec![0]]), 1, 1).is_err());
        assert!(decode(&section(&[], &diff, &[vec![0]]), 1, 1).is_err());
        let mut body = section(&[(1, 0)], &diff, &[vec![0]]);
        body[SYMBOLS * 2..SYMBOLS * 2 + 4].copy_from_slice(&(25u32 << 27).to_le_bytes());
        assert!(decode(&body, 1, 1).is_err());
    }
}

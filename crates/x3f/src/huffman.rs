//! Huffman decoding of the TRUE compression.
//!
//! The table consists of pairs `(code length, code)`; the code is stored left-aligned
//! in a byte, so the code length is at most 8. The index of an
//! entry is the decoded symbol: the number of following difference bits
//! (as in DC coding in JPEG).

use crate::bits::BitReader;
use crate::error::{corrupt, Result};

#[derive(Clone, Copy, Default)]
struct Slot {
    len: u8,
    symbol: u8,
}

pub struct TrueHuffman {
    /// Lookup table over the next 8 bits.
    table: [Slot; 256],
}

impl TrueHuffman {
    /// `pairs` = (code_size, code) in file order, without the terminating entry.
    pub fn new(pairs: &[(u8, u8)]) -> Result<Self> {
        let mut table = [Slot::default(); 256];
        for (symbol, &(len, code)) in pairs.iter().enumerate() {
            if len == 0 {
                continue;
            }
            if len > 8 || symbol > 255 {
                return corrupt(format!("ungültiger Huffman-Code (Länge {len})"));
            }
            // the symbol is the number of difference bits that follow: more than 16 never occurs in real files
            // (and more than 31 would shift out of range)
            if symbol > 16 {
                return corrupt(format!("ungültiger Huffman-Code (Symbol {symbol})"));
            }
            let prefix = (code as usize) >> (8 - len);
            let span = 1usize << (8 - len);
            let start = prefix << (8 - len);
            for slot in &mut table[start..start + span] {
                *slot = Slot { len, symbol: symbol as u8 };
            }
        }
        Ok(Self { table })
    }

    /// Is the next code in the stream a code of the table? (Bits that match no
    /// code only occur in damaged data.)
    #[inline(always)]
    pub fn next_is_valid(&self, br: &mut BitReader) -> bool {
        self.table[br.peek(8) as usize].len != 0
    }

    /// Decode one difference.
    #[inline(always)]
    pub fn decode_diff(&self, br: &mut BitReader) -> i32 {
        self.decode_diff_counting(br, &mut 0)
    }

    /// Like [`Self::decode_diff`], counts bit patterns that are no code of the table in `invalid`
    /// (they only occur in damaged data; the result stays the same).
    #[inline(always)]
    pub fn decode_diff_counting(&self, br: &mut BitReader, invalid: &mut usize) -> i32 {
        let slot = self.table[br.peek(8) as usize];
        *invalid += (slot.len == 0) as usize;
        br.consume(slot.len.max(1) as u32);
        let bits = slot.symbol as u32;
        if bits == 0 {
            return 0;
        }
        let v = br.read(bits) as i32;
        // A leading 0 means a negative difference (JPEG-like coding).
        if v >> (bits - 1) == 0 {
            v - ((1 << bits) - 1)
        } else {
            v
        }
    }
}

/// Result of [`scan_true_stream`]: how well a compressed plane fits its declared geometry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StreamCheck {
    /// first row containing a bit pattern that is no code of the table
    pub first_invalid_row: Option<usize>,
    /// number of such codes
    pub invalid_codes: usize,
    /// first row with a value outside 0 … `max_value`
    pub first_out_of_range_row: Option<usize>,
    /// number of values outside 0 … `max_value`
    pub out_of_range: usize,
    /// bits needed for all rows
    pub bits_used: u64,
    /// bits available in the plane
    pub bits_available: u64,
}

impl StreamCheck {
    /// The plane decodes consistently: every code is valid and the stream does not need more bits
    /// than the plane holds. (Values out of range and unused bits are only reported: they are
    /// not proven to be impossible in every camera generation.)
    pub fn is_clean(&self) -> bool {
        self.invalid_codes == 0 && self.bits_used <= self.bits_available
    }

    /// First damaged row (invalid code or value out of range), if any.
    pub fn first_bad_row(&self) -> Option<usize> {
        match (self.first_invalid_row, self.first_out_of_range_row) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }
}

/// Decode a TRUE stream like [`decode_true_stream`], but only collect plausibility data
/// (no output). Used to tell damaged files apart from intact ones.
pub fn scan_true_stream(huff: &TrueHuffman, data: &[u8], seed: i32, rows: usize, cols: usize, max_value: i32) -> StreamCheck {
    let mut br = BitReader::new(data);
    let mut row_start = [[seed; 2]; 2];
    let mut check = StreamCheck { bits_available: data.len() as u64 * 8, ..Default::default() };
    for row in 0..rows {
        let odd_row = row & 1;
        let mut acc = [0i32; 2];
        for col in 0..cols {
            let odd_col = col & 1;
            if !huff.next_is_valid(&mut br) {
                check.invalid_codes += 1;
                check.first_invalid_row.get_or_insert(row);
            }
            let diff = huff.decode_diff(&mut br);
            let prev = if col < 2 { row_start[odd_row][odd_col] } else { acc[odd_col] };
            let value = prev + diff;
            acc[odd_col] = value;
            if col < 2 {
                row_start[odd_row][odd_col] = value;
            }
            if !(0..=max_value).contains(&value) {
                check.out_of_range += 1;
                check.first_out_of_range_row.get_or_insert(row);
            }
        }
    }
    check.bits_used = br.bit_position();
    check
}

/// Decode a stream of prediction differences in the TRUE scheme.
///
/// Each of the four positions of a 2×2 block forms its own predictor:
/// a value is the sum of the difference and the previous value of the same
/// row/column parity. The first two columns of a row are predicted from
/// the start of the second-to-last row of the same parity.
///
/// Returns the first row containing a bit pattern that is no code of the table (`None` for
/// intact data).
pub fn decode_true_stream(
    huff: &TrueHuffman,
    data: &[u8],
    seed: i32,
    rows: usize,
    cols: usize,
    mut sink: impl FnMut(usize, usize, i32) -> bool,
) -> Result<Option<usize>> {
    let mut br = BitReader::new(data);
    let mut row_start = [[seed; 2]; 2];
    let mut invalid = 0usize;
    let mut first_invalid = None;
    'rows: for row in 0..rows {
        if invalid > 0 && first_invalid.is_none() {
            first_invalid = Some(row.saturating_sub(1));
        }
        // past the end of the data only zero bits follow: stop instead of decoding nothing for a long time
        // (damaged geometry in the header)
        if br.overrun() {
            break;
        }
        let odd_row = row & 1;
        let mut acc = [0i32; 2];
        for col in 0..cols {
            if col & 0xfff == 0xfff && br.overrun() {
                break 'rows;
            }
            let odd_col = col & 1;
            let diff = huff.decode_diff_counting(&mut br, &mut invalid);
            let prev = if col < 2 { row_start[odd_row][odd_col] } else { acc[odd_col] };
            let value = prev + diff;
            acc[odd_col] = value;
            if col < 2 {
                row_start[odd_row][odd_col] = value;
            }
            if !sink(row, col, value) {
                break 'rows;
            }
        }
    }
    if invalid > 0 && first_invalid.is_none() {
        first_invalid = Some(rows.saturating_sub(1));
    }
    // invalid codes say more about where the damage starts than the overrun they cause
    if first_invalid.is_none() && br.overrun() {
        return corrupt("TRUE-Datenstrom endet vorzeitig");
    }
    Ok(first_invalid)
}

//! Bit reader (MSB first), as used by the TRUE compression.

pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u64,
    nbits: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0, acc: 0, nbits: 0 }
    }

    #[inline(always)]
    fn refill(&mut self) {
        while self.nbits <= 56 {
            // Past the end of the data, zero bits are shifted in; the caller
            // detects overruns via `overrun()`.
            let byte = self.data.get(self.pos).copied().unwrap_or(0);
            self.acc |= (byte as u64) << (56 - self.nbits);
            self.pos += 1;
            self.nbits += 8;
        }
    }

    /// Peek at the next `n` bits (n ≤ 32) without consuming them.
    #[inline(always)]
    pub fn peek(&mut self, n: u32) -> u32 {
        if self.nbits < n {
            self.refill();
        }
        (self.acc >> (64 - n)) as u32
    }

    #[inline(always)]
    pub fn consume(&mut self, n: u32) {
        self.acc <<= n;
        self.nbits -= n;
    }

    #[inline(always)]
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek(n);
        self.consume(n);
        v
    }

    /// Bits consumed so far.
    pub fn bit_position(&self) -> u64 {
        self.pos as u64 * 8 - self.nbits as u64
    }

    /// Were more bytes read than are available?
    pub fn overrun(&self) -> bool {
        self.pos > self.data.len() + 8
    }
}

//! Bit readers over in-memory packets with a 64-bit cache register.
//!
//! Both readers keep up to 64 upcoming bits in `bit_buf` and refill it without
//! branches while at least 8 bytes remain: one unaligned 8-byte load, then
//! `byte_pos += (63 - bits_left) >> 3` and `bits_left |= 56`, which leaves
//! `56..=63` valid bits. The bits of that load past `bits_left` are the true
//! next bits of the stream, so a later refill rewrites them with identical
//! values. The last `1..=7` bytes are loaded one at a time, never reading past
//! the slice.
//!
//! Invariant: the number of consumed bits is `byte_pos * 8 - bits_left`.

use crate::error::{Error, ErrorKind};

/// Largest width accepted by `peek_padded`: one refill guarantees 56 bits.
const MAX_PEEK_BITS: u32 = 56;
/// Largest width read from the cache register in one step.
const MAX_STEP_BITS: u32 = 56;
/// Largest width accepted by `read_bits`.
const MAX_READ_BITS: u32 = 64;

fn eof() -> Error {
    ErrorKind::UnexpectedEof.into()
}

/// Mask of the low `n` bits, `n` in `0..=63`.
const fn low_mask(n: u32) -> u64 {
    (1u64 << n) - 1
}

/// Bit reader for most-significant-bit-first streams (FLAC, MPEG audio, AAC,
/// ALAC, Opus).
///
/// Short input never causes a panic: a read past the end returns
/// [`ErrorKind::UnexpectedEof`] and leaves the reader unchanged.
///
/// ```
/// use akousma_core::io::BitReaderMsb;
///
/// let mut bits = BitReaderMsb::new(&[0b1011_0010, 0b1000_0000]);
/// assert_eq!(bits.read_bits(3)?, 0b101);
/// assert_eq!(bits.read_signed(4)?, -7); // 0b1001
/// assert_eq!(bits.read_unary_zeros(16)?, 1); // "01"
/// assert_eq!(bits.bits_remaining(), 7);
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct BitReaderMsb<'a> {
    /// Input, truncated to the bytes that hold readable bits.
    data: &'a [u8],
    /// Index of the next byte not yet loaded into `bit_buf`.
    byte_pos: usize,
    /// Upcoming bits, aligned to bit 63.
    bit_buf: u64,
    /// Number of valid bits at the top of `bit_buf`, `0..=63`.
    bits_left: u32,
    /// Total number of readable bits from the start of `data`.
    end_bit: u64,
}

impl<'a> BitReaderMsb<'a> {
    /// Largest `n` accepted by [`BitReaderMsb::peek_padded`].
    pub const MAX_PEEK_BITS: u32 = MAX_PEEK_BITS;

    /// Creates a reader over all bits of `data`.
    #[must_use]
    pub fn new(data: &'a [u8]) -> Self {
        Self::with_bit_limit(data, u64::MAX)
    }

    /// Creates a reader over the first `max_bits` bits of `data` (all of
    /// `data` if it is shorter).
    ///
    /// Bits after the limit are never returned, even when they share a byte
    /// with readable bits: MPEG audio Layer III ends a granule's Huffman data
    /// after `part2_3_length` bits, in the middle of a byte.
    #[must_use]
    pub fn with_bit_limit(data: &'a [u8], max_bits: u64) -> Self {
        let data_bits = (data.len() as u64).saturating_mul(8);
        let end_bit = max_bits.min(data_bits);
        // `end_bit <= data.len() * 8`, so the byte count fits in `usize`.
        let bytes = usize::try_from(end_bit.div_ceil(8)).map_or(data.len(), |b| b.min(data.len()));
        Self {
            data: &data[..bytes],
            byte_pos: 0,
            bit_buf: 0,
            bits_left: 0,
            end_bit,
        }
    }

    /// Number of bits consumed so far.
    #[must_use]
    pub fn bit_position(&self) -> u64 {
        self.byte_pos as u64 * 8 - u64::from(self.bits_left)
    }

    /// Number of bits left before the end or the bit limit.
    #[must_use]
    pub fn bits_remaining(&self) -> u64 {
        self.end_bit - self.bit_position()
    }

    /// Returns `true` if the position is a multiple of 8 bits.
    #[must_use]
    pub const fn is_byte_aligned(&self) -> bool {
        self.bits_left & 7 == 0
    }

    /// Loads more bits; afterwards `bits_left >= 56` or every remaining byte
    /// is loaded. A single call is enough: with `bits_left >= 56` the
    /// branchless step would advance by zero bytes.
    #[inline]
    fn refill(&mut self) {
        if self.bits_left >= MAX_STEP_BITS {
            return;
        }
        if let Some(word) = self.data[self.byte_pos..].first_chunk::<8>() {
            self.bit_buf |= u64::from_be_bytes(*word) >> self.bits_left;
            self.byte_pos += ((63 - self.bits_left) >> 3) as usize;
            self.bits_left |= 56;
        } else {
            while self.bits_left <= 56 && self.byte_pos < self.data.len() {
                self.bit_buf |= u64::from(self.data[self.byte_pos]) << (56 - self.bits_left);
                self.byte_pos += 1;
                self.bits_left += 8;
            }
        }
    }

    /// Drops `n <= bits_left` bits from the register.
    #[inline]
    fn consume_loaded(&mut self, n: u32) {
        self.bit_buf = self.bit_buf.checked_shl(n).unwrap_or(0);
        self.bits_left -= n;
    }

    /// Reads `n <= 56` bits; the caller has checked `n <= bits_remaining()`.
    #[inline]
    fn take_bits(&mut self, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        if self.bits_left < n {
            self.refill();
        }
        let value = self.bit_buf >> (64 - n);
        self.consume_loaded(n);
        value
    }

    /// Reads `n` bits (`0..=64`) as an unsigned integer, first bit most
    /// significant. `read_bits(0)` returns `0`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnexpectedEof`] if fewer than `n` bits remain (the reader
    /// is unchanged).
    ///
    /// # Panics
    ///
    /// If `n > 64`. Widths come from the decoder, which validates any width
    /// derived from stream fields before reading.
    #[inline]
    pub fn read_bits(&mut self, n: u32) -> Result<u64, Error> {
        assert!(n <= MAX_READ_BITS, "read width {n} exceeds {MAX_READ_BITS}");
        if u64::from(n) > self.bits_remaining() {
            return Err(eof());
        }
        if n <= MAX_STEP_BITS {
            Ok(self.take_bits(n))
        } else {
            let high = self.take_bits(32);
            let low = self.take_bits(n - 32);
            Ok((high << (n - 32)) | low)
        }
    }

    /// Reads one bit.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnexpectedEof`] at the end of the input.
    #[inline]
    pub fn read_bit(&mut self) -> Result<bool, Error> {
        self.read_bits(1).map(|bit| bit != 0)
    }

    /// Reads `n` bits (`0..=64`) as a two's complement signed integer.
    /// `read_signed(0)` returns `0`.
    ///
    /// # Errors
    ///
    /// Same as [`BitReaderMsb::read_bits`].
    ///
    /// # Panics
    ///
    /// If `n > 64`.
    #[inline]
    pub fn read_signed(&mut self, n: u32) -> Result<i64, Error> {
        let value = self.read_bits(n)?;
        if n == 0 {
            return Ok(0);
        }
        let shift = 64 - n;
        Ok(((value << shift) as i64) >> shift)
    }

    /// Returns the next `n` bits (`0..=56`) without consuming them. Bits past
    /// the end or the bit limit read as zero.
    ///
    /// Intended for table-driven Huffman decoding: the last code word of a
    /// packet may be shorter than the table width.
    ///
    /// # Panics
    ///
    /// If `n > 56` ([`BitReaderMsb::MAX_PEEK_BITS`]).
    #[inline]
    pub fn peek_padded(&mut self, n: u32) -> u64 {
        assert!(n <= MAX_PEEK_BITS, "peek width {n} exceeds {MAX_PEEK_BITS}");
        if n == 0 {
            return 0;
        }
        if self.bits_left < n {
            self.refill();
        }
        let value = self.bit_buf >> (64 - n);
        let remaining = self.bits_remaining();
        if remaining < u64::from(n) {
            let pad = n - remaining as u32;
            (value >> pad) << pad
        } else {
            value
        }
    }

    /// Skips `n` bits.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnexpectedEof`] if fewer than `n` bits remain (the reader
    /// is unchanged).
    pub fn skip_bits(&mut self, n: u64) -> Result<(), Error> {
        if n > self.bits_remaining() {
            return Err(eof());
        }
        if n <= u64::from(self.bits_left) {
            self.consume_loaded(n as u32);
        } else {
            let target = self.bit_position() + n;
            // `target <= end_bit <= data.len() * 8`.
            self.byte_pos = (target / 8) as usize;
            self.bit_buf = 0;
            self.bits_left = 0;
            self.take_bits((target % 8) as u32);
        }
        Ok(())
    }

    /// Skips to the next byte boundary, or to the bit limit if it comes first.
    #[inline]
    pub fn align_to_byte(&mut self) {
        // `byte_pos * 8` is a multiple of 8, so `bits_left & 7` bits remain in
        // the current byte.
        let skip = u64::from(self.bits_left & 7).min(self.bits_remaining());
        self.consume_loaded(skip as u32);
    }

    /// Reads a unary code: counts `0` bits up to the terminating `1`, consumes
    /// both and returns the count. Used for the quotient of Rice codes.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::CorruptFrame`] as soon as more than `max_zeros` zeros are
    /// seen, which bounds the work on corrupt input. [`ErrorKind::UnexpectedEof`]
    /// if the input ends before the terminating `1`. On error the reader is
    /// unchanged.
    pub fn read_unary_zeros(&mut self, max_zeros: u32) -> Result<u32, Error> {
        let saved = (self.byte_pos, self.bit_buf, self.bits_left);
        let mut zeros = 0u64;
        loop {
            self.refill();
            let available = u64::from(self.bits_left).min(self.bits_remaining()) as u32;
            if available == 0 {
                (self.byte_pos, self.bit_buf, self.bits_left) = saved;
                return Err(eof());
            }
            let leading = self.bit_buf.leading_zeros().min(available);
            zeros += u64::from(leading);
            if zeros > u64::from(max_zeros) {
                (self.byte_pos, self.bit_buf, self.bits_left) = saved;
                return Err(ErrorKind::CorruptFrame.into());
            }
            if leading < available {
                self.consume_loaded(leading + 1);
                return Ok(zeros as u32);
            }
            self.consume_loaded(available);
        }
    }
}

/// Bit reader for least-significant-bit-first streams (Vorbis, WavPack).
///
/// Bits are taken from the least significant end of each byte, and the first
/// bit read becomes the least significant bit of the result (Vorbis I §2.1).
/// Short input never causes a panic: a read past the end returns
/// [`ErrorKind::UnexpectedEof`] and leaves the reader unchanged.
///
/// ```
/// use akousma_core::io::BitReaderLsb;
///
/// // Vorbis I §2.1.6 example.
/// let mut bits = BitReaderLsb::new(&[0b1111_1100, 0b0100_1000, 0b1100_1110, 0b0000_0110]);
/// assert_eq!(bits.read_bits(4)?, 12);
/// assert_eq!(bits.read_bits(3)?, 7);
/// assert_eq!(bits.read_bits(7)?, 17);
/// assert_eq!(bits.read_bits(13)?, 6969);
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct BitReaderLsb<'a> {
    data: &'a [u8],
    /// Index of the next byte not yet loaded into `bit_buf`.
    byte_pos: usize,
    /// Upcoming bits, aligned to bit 0.
    bit_buf: u64,
    /// Number of valid bits at the bottom of `bit_buf`, `0..=63`.
    bits_left: u32,
}

impl<'a> BitReaderLsb<'a> {
    /// Largest `n` accepted by [`BitReaderLsb::peek_padded`].
    pub const MAX_PEEK_BITS: u32 = MAX_PEEK_BITS;

    /// Creates a reader over all bits of `data`.
    #[must_use]
    pub const fn new(data: &'a [u8]) -> Self {
        Self {
            data,
            byte_pos: 0,
            bit_buf: 0,
            bits_left: 0,
        }
    }

    /// Number of bits consumed so far.
    #[must_use]
    pub fn bit_position(&self) -> u64 {
        self.byte_pos as u64 * 8 - u64::from(self.bits_left)
    }

    /// Number of bits left before the end of the input.
    #[must_use]
    pub fn bits_remaining(&self) -> u64 {
        self.data.len() as u64 * 8 - self.bit_position()
    }

    /// Loads more bits; afterwards `bits_left >= 56` or every remaining byte
    /// is loaded.
    #[inline]
    fn refill(&mut self) {
        if self.bits_left >= MAX_STEP_BITS {
            return;
        }
        if let Some(word) = self.data[self.byte_pos..].first_chunk::<8>() {
            self.bit_buf |= u64::from_le_bytes(*word) << self.bits_left;
            self.byte_pos += ((63 - self.bits_left) >> 3) as usize;
            self.bits_left |= 56;
        } else {
            while self.bits_left <= 56 && self.byte_pos < self.data.len() {
                self.bit_buf |= u64::from(self.data[self.byte_pos]) << self.bits_left;
                self.byte_pos += 1;
                self.bits_left += 8;
            }
        }
    }

    /// Drops `n <= bits_left` bits from the register.
    #[inline]
    fn consume_loaded(&mut self, n: u32) {
        self.bit_buf = self.bit_buf.checked_shr(n).unwrap_or(0);
        self.bits_left -= n;
    }

    /// Reads `n <= 56` bits; the caller has checked `n <= bits_remaining()`.
    #[inline]
    fn take_bits(&mut self, n: u32) -> u64 {
        if n == 0 {
            return 0;
        }
        if self.bits_left < n {
            self.refill();
        }
        let value = self.bit_buf & low_mask(n);
        self.consume_loaded(n);
        value
    }

    /// Reads `n` bits (`0..=64`); the first bit read is the least significant
    /// bit of the result. `read_bits(0)` returns `0`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnexpectedEof`] if fewer than `n` bits remain (the reader
    /// is unchanged).
    ///
    /// # Panics
    ///
    /// If `n > 64`. Widths come from the decoder, which validates any width
    /// derived from stream fields before reading.
    #[inline]
    pub fn read_bits(&mut self, n: u32) -> Result<u64, Error> {
        assert!(n <= MAX_READ_BITS, "read width {n} exceeds {MAX_READ_BITS}");
        if u64::from(n) > self.bits_remaining() {
            return Err(eof());
        }
        if n <= MAX_STEP_BITS {
            Ok(self.take_bits(n))
        } else {
            let low = self.take_bits(32);
            let high = self.take_bits(n - 32);
            Ok(low | (high << 32))
        }
    }

    /// Reads one bit.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnexpectedEof`] at the end of the input.
    #[inline]
    pub fn read_bit(&mut self) -> Result<bool, Error> {
        self.read_bits(1).map(|bit| bit != 0)
    }

    /// Returns the next `n` bits (`0..=56`) without consuming them, in the
    /// same order as [`BitReaderLsb::read_bits`]. Bits past the end read as
    /// zero.
    ///
    /// # Panics
    ///
    /// If `n > 56` ([`BitReaderLsb::MAX_PEEK_BITS`]).
    #[inline]
    pub fn peek_padded(&mut self, n: u32) -> u64 {
        assert!(n <= MAX_PEEK_BITS, "peek width {n} exceeds {MAX_PEEK_BITS}");
        if self.bits_left < n {
            self.refill();
        }
        let remaining = self.bits_remaining();
        let width = if remaining < u64::from(n) {
            remaining as u32
        } else {
            n
        };
        self.bit_buf & low_mask(width)
    }

    /// Skips `n` bits.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnexpectedEof`] if fewer than `n` bits remain (the reader
    /// is unchanged).
    pub fn skip_bits(&mut self, n: u64) -> Result<(), Error> {
        if n > self.bits_remaining() {
            return Err(eof());
        }
        if n <= u64::from(self.bits_left) {
            self.consume_loaded(n as u32);
        } else {
            let target = self.bit_position() + n;
            // `target <= data.len() * 8`.
            self.byte_pos = (target / 8) as usize;
            self.bit_buf = 0;
            self.bits_left = 0;
            self.take_bits((target % 8) as u32);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference model: bit `i` of an MSB-first stream.
    fn msb_bit(data: &[u8], i: u64) -> u64 {
        u64::from(data[(i / 8) as usize] >> (7 - i % 8)) & 1
    }

    /// Reference model: bit `i` of an LSB-first stream.
    fn lsb_bit(data: &[u8], i: u64) -> u64 {
        u64::from(data[(i / 8) as usize] >> (i % 8)) & 1
    }

    fn msb_ref(data: &[u8], pos: u64, n: u32) -> u64 {
        (0..u64::from(n)).fold(0, |acc, i| (acc << 1) | msb_bit(data, pos + i))
    }

    fn lsb_ref(data: &[u8], pos: u64, n: u32) -> u64 {
        (0..u64::from(n)).fold(0, |acc, i| acc | (lsb_bit(data, pos + i) << i))
    }

    /// Packs a string of `0`/`1` into bytes, first character as the most
    /// significant bit of the first byte; the last byte is zero-padded.
    fn msb_bytes(bits: &str) -> Vec<u8> {
        let mut out = vec![0u8; bits.len().div_ceil(8)];
        for (i, c) in bits.bytes().enumerate() {
            if c == b'1' {
                out[i / 8] |= 0x80 >> (i % 8);
            }
        }
        out
    }

    fn pattern(len: usize) -> Vec<u8> {
        (0..len)
            .map(|i| (i as u8).wrapping_mul(37).wrapping_add(11))
            .collect()
    }

    const WIDTHS: [u32; 14] = [0, 1, 32, 56, 57, 64, 0, 3, 13, 64, 7, 1, 56, 9];

    fn check_widths_msb(data: &[u8]) {
        let mut r = BitReaderMsb::new(data);
        let mut pos = 0;
        for n in WIDTHS {
            assert_eq!(
                r.read_bits(n).unwrap(),
                msb_ref(data, pos, n),
                "msb width {n} at {pos}"
            );
            pos += u64::from(n);
            assert_eq!(r.bit_position(), pos);
        }
    }

    fn check_widths_lsb(data: &[u8]) {
        let mut r = BitReaderLsb::new(data);
        let mut pos = 0;
        for n in WIDTHS {
            assert_eq!(
                r.read_bits(n).unwrap(),
                lsb_ref(data, pos, n),
                "lsb width {n} at {pos}"
            );
            pos += u64::from(n);
            assert_eq!(r.bit_position(), pos);
        }
    }

    #[test]
    fn bitreader_msb_and_lsb_edges() {
        // Hand-computed values.
        let bytes = [
            0x12, 0x34, 0x56, 0x78, 0x9A, 0xBC, 0xDE, 0xF0, 0x0F, 0xED, 0xCB,
        ];
        let mut r = BitReaderMsb::new(&bytes);
        assert_eq!(r.read_bits(0).unwrap(), 0);
        assert_eq!(r.read_bits(4).unwrap(), 0x1);
        assert_eq!(r.read_bits(32).unwrap(), 0x2345_6789);
        assert_eq!(r.read_bits(28).unwrap(), 0xABC_DEF0);
        assert_eq!(r.read_bits(8).unwrap(), 0x0F);
        let mut r = BitReaderMsb::new(&bytes);
        assert_eq!(r.read_bits(64).unwrap(), 0x1234_5678_9ABC_DEF0);
        let mut r = BitReaderMsb::new(&bytes);
        r.read_bits(4).unwrap();
        assert_eq!(r.read_bits(57).unwrap(), 0x0234_5678_9ABC_DEF0 >> 3);
        let mut r = BitReaderLsb::new(&bytes);
        assert_eq!(r.read_bits(4).unwrap(), 0x2);
        assert_eq!(r.read_bits(8).unwrap(), 0x41);
        let mut r = BitReaderLsb::new(&bytes);
        assert_eq!(r.read_bits(64).unwrap(), 0xF0DE_BC9A_7856_3412);
        let mut r = BitReaderLsb::new(&bytes);
        assert_eq!(
            r.read_bits(57).unwrap(),
            0xF0DE_BC9A_7856_3412 & low_mask(57)
        );

        // Widths 0, 1, 32, 56, 57, 64 across 8-byte word boundaries.
        let data = pattern(64);
        check_widths_msb(&data);
        check_widths_lsb(&data);
        let mut r = BitReaderMsb::new(&data);
        r.read_bits(60).unwrap();
        assert_eq!(r.read_bits(8).unwrap(), msb_ref(&data, 60, 8));

        // Signed reads.
        let mut r = BitReaderMsb::new(&[0x80, 0x00, 0x00, 0x7F, 0xFF]);
        assert_eq!(r.read_signed(0).unwrap(), 0);
        assert_eq!(r.read_signed(24).unwrap(), -8_388_608);
        assert_eq!(r.read_signed(8).unwrap(), 127);
        assert_eq!(r.read_signed(1).unwrap(), -1);
        assert_eq!(r.read_signed(7).unwrap(), -1);
        assert_eq!(BitReaderMsb::new(&[0xFF; 8]).read_signed(64).unwrap(), -1);
        let mut r = BitReaderMsb::new(&[0x80, 0, 0, 0, 0, 0, 0, 0, 0x40]);
        assert_eq!(r.read_signed(64).unwrap(), i64::MIN);
        assert_eq!(r.read_signed(2).unwrap(), 1);
        for n in 1..=64u32 {
            // A single leading one is the most negative n-bit value.
            let data = msb_bytes(&format!("1{}", "0".repeat(n as usize - 1)));
            let expected = if n == 64 {
                i64::MIN
            } else {
                -(1i64 << (n - 1))
            };
            assert_eq!(
                BitReaderMsb::new(&data).read_signed(n).unwrap(),
                expected,
                "n = {n}"
            );
        }

        // Tails of 0..=7 bytes: byte-wise loading, exact end, no partial reads.
        let data = pattern(7);
        for len in 0..=7usize {
            let tail = &data[..len];
            let total = len as u32 * 8;
            let mut msb = BitReaderMsb::new(tail);
            let mut lsb = BitReaderLsb::new(tail);
            for i in 0..u64::from(total) {
                assert_eq!(msb.read_bits(1).unwrap(), msb_bit(tail, i));
                assert_eq!(lsb.read_bits(1).unwrap(), lsb_bit(tail, i));
            }
            assert_eq!(msb.read_bits(1).unwrap_err().kind, ErrorKind::UnexpectedEof);
            assert_eq!(lsb.read_bits(1).unwrap_err().kind, ErrorKind::UnexpectedEof);
            assert_eq!(msb.peek_padded(56), 0);
            assert_eq!(lsb.peek_padded(56), 0);
            assert_eq!(msb.read_bits(0).unwrap(), 0);
            assert_eq!(lsb.read_bits(0).unwrap(), 0);

            let mut msb = BitReaderMsb::new(tail);
            let mut lsb = BitReaderLsb::new(tail);
            assert_eq!(
                msb.read_bits(total + 1).unwrap_err().kind,
                ErrorKind::UnexpectedEof
            );
            assert_eq!(
                lsb.read_bits(total + 1).unwrap_err().kind,
                ErrorKind::UnexpectedEof
            );
            assert_eq!(msb.bits_remaining(), u64::from(total));
            assert_eq!(lsb.bits_remaining(), u64::from(total));
            assert_eq!(msb.peek_padded(total), msb_ref(tail, 0, total));
            assert_eq!(lsb.peek_padded(total), lsb_ref(tail, 0, total));
            assert_eq!(msb.read_bits(total).unwrap(), msb_ref(tail, 0, total));
            assert_eq!(lsb.read_bits(total).unwrap(), lsb_ref(tail, 0, total));
        }

        // Bit limit in the middle of a byte: the following bits are masked.
        let data = [0b1011_0111, 0b1110_0001, 0xFF];
        let mut r = BitReaderMsb::with_bit_limit(&data, 11);
        assert_eq!(r.bits_remaining(), 11);
        assert_eq!(r.read_bits(3).unwrap(), 0b101);
        assert_eq!(r.peek_padded(12), 0b1011_1111_0000);
        assert_eq!(r.peek_padded(8), 0b1011_1111);
        assert_eq!(r.read_bits(9).unwrap_err().kind, ErrorKind::UnexpectedEof);
        assert_eq!(r.read_bits(8).unwrap(), 0b1011_1111);
        assert_eq!(r.bits_remaining(), 0);
        assert_eq!(r.peek_padded(5), 0);
        assert_eq!(r.read_bit().unwrap_err().kind, ErrorKind::UnexpectedEof);
        // Set bits after the limit do not terminate a unary code.
        let mut r = BitReaderMsb::with_bit_limit(&[0x00, 0xFF], 8);
        assert_eq!(
            r.read_unary_zeros(100).unwrap_err().kind,
            ErrorKind::UnexpectedEof
        );
        // A limit beyond the data is clamped to the data.
        assert_eq!(
            BitReaderMsb::with_bit_limit(&data, 1000).bits_remaining(),
            24
        );

        // Vorbis I §2.1.6, with zero-width reads in between.
        let mut r = BitReaderLsb::new(&[0b1111_1100, 0b0100_1000, 0b1100_1110, 0b0000_0110]);
        assert_eq!(r.read_bits(4).unwrap(), 12);
        assert_eq!(r.read_bits(0).unwrap(), 0);
        assert_eq!(r.read_bits(3).unwrap(), 7);
        assert_eq!(r.read_bits(7).unwrap(), 17);
        assert_eq!(r.read_bits(0).unwrap(), 0);
        assert_eq!(r.read_bits(13).unwrap(), 6969);
        assert_eq!(r.bits_remaining(), 5);
        assert_eq!(r.read_bits(6).unwrap_err().kind, ErrorKind::UnexpectedEof);
        assert_eq!(r.read_bits(5).unwrap(), 0);

        // Unary codes, including a run that crosses the 64-bit register.
        let data = msb_bytes(&format!("101{}1{}", "0".repeat(70), "1011"));
        let mut r = BitReaderMsb::new(&data);
        assert_eq!(r.read_bits(3).unwrap(), 0b101);
        assert_eq!(r.read_unary_zeros(1000).unwrap(), 70);
        assert_eq!(r.bit_position(), 74);
        assert_eq!(r.read_unary_zeros(0).unwrap(), 0);
        assert_eq!(r.read_unary_zeros(5).unwrap(), 1);
        // The last "1", then the zero padding of the final byte.
        assert_eq!(r.read_bits(2).unwrap(), 0b10);
        // Errors leave the reader where the call started.
        let mut r = BitReaderMsb::new(&data);
        r.read_bits(3).unwrap();
        assert_eq!(
            r.read_unary_zeros(69).unwrap_err().kind,
            ErrorKind::CorruptFrame
        );
        assert_eq!(r.bit_position(), 3);
        assert_eq!(r.read_unary_zeros(70).unwrap(), 70);
        let mut r = BitReaderMsb::new(&[0u8; 32]);
        r.read_bits(5).unwrap();
        assert_eq!(
            r.read_unary_zeros(u32::MAX).unwrap_err().kind,
            ErrorKind::UnexpectedEof
        );
        assert_eq!(r.bit_position(), 5);
        assert_eq!(r.read_bits(56).unwrap(), 0);
        let mut r = BitReaderMsb::new(&[0u8; 32]);
        assert_eq!(
            r.read_unary_zeros(100).unwrap_err().kind,
            ErrorKind::CorruptFrame
        );
        assert_eq!(r.bit_position(), 0);
        let mut r = BitReaderMsb::new(&[0x00, 0x01]);
        assert_eq!(r.read_unary_zeros(15).unwrap(), 15);
        assert_eq!(r.bits_remaining(), 0);

        // Alignment and skipping.
        let data = pattern(40);
        let mut r = BitReaderMsb::new(&data);
        assert!(r.is_byte_aligned());
        r.read_bits(3).unwrap();
        assert!(!r.is_byte_aligned());
        r.align_to_byte();
        assert_eq!(r.bit_position(), 8);
        r.align_to_byte();
        assert_eq!(r.bit_position(), 8);
        r.skip_bits(100).unwrap();
        assert_eq!(r.read_bits(20).unwrap(), msb_ref(&data, 108, 20));
        assert_eq!(r.skip_bits(193).unwrap_err().kind, ErrorKind::UnexpectedEof);
        r.skip_bits(5).unwrap();
        assert_eq!(r.read_bits(11).unwrap(), msb_ref(&data, 133, 11));
        r.skip_bits(r.bits_remaining()).unwrap();
        assert_eq!(r.bits_remaining(), 0);
        let mut r = BitReaderMsb::with_bit_limit(&data, 10);
        r.read_bits(9).unwrap();
        r.align_to_byte();
        assert_eq!(r.bit_position(), 10);
        let mut r = BitReaderLsb::new(&data);
        r.read_bits(5).unwrap();
        r.skip_bits(77).unwrap();
        assert_eq!(r.read_bits(30).unwrap(), lsb_ref(&data, 82, 30));
        assert_eq!(r.skip_bits(209).unwrap_err().kind, ErrorKind::UnexpectedEof);
        assert_eq!(r.read_bits(16).unwrap(), lsb_ref(&data, 112, 16));
    }

    #[test]
    #[should_panic(expected = "read width 65 exceeds 64")]
    fn msb_read_wider_than_64_panics() {
        let _ = BitReaderMsb::new(&[0; 16]).read_bits(65);
    }

    #[test]
    #[should_panic(expected = "read width 65 exceeds 64")]
    fn lsb_read_wider_than_64_panics() {
        let _ = BitReaderLsb::new(&[0; 16]).read_bits(65);
    }

    #[test]
    #[should_panic(expected = "peek width 57 exceeds 56")]
    fn peek_wider_than_56_panics() {
        BitReaderMsb::new(&[0; 16]).peek_padded(57);
    }

    #[test]
    fn bitreader_refill_at_55_and_56_bits() {
        let data = pattern(32);

        let mut r = BitReaderMsb::new(&data);
        r.take_bits(8);
        assert_eq!((r.bits_left, r.byte_pos), (48, 7));
        r.refill();
        assert_eq!((r.bits_left, r.byte_pos), (56, 8));
        r.refill();
        assert_eq!((r.bits_left, r.byte_pos), (56, 8));
        assert_eq!(r.bit_position(), 8);
        r.take_bits(1);
        assert_eq!(r.bits_left, 55);
        r.refill();
        assert_eq!((r.bits_left, r.byte_pos), (63, 9));
        assert_eq!(r.bit_position(), 9);
        for pos in 9..200 {
            assert_eq!(
                r.read_bits(1).unwrap(),
                msb_bit(&data, pos),
                "msb bit {pos}"
            );
        }

        let mut r = BitReaderLsb::new(&data);
        r.take_bits(8);
        r.refill();
        assert_eq!((r.bits_left, r.byte_pos), (56, 8));
        r.refill();
        assert_eq!((r.bits_left, r.byte_pos), (56, 8));
        r.take_bits(1);
        assert_eq!(r.bits_left, 55);
        r.refill();
        assert_eq!((r.bits_left, r.byte_pos), (63, 9));
        assert_eq!(r.bit_position(), 9);
        for pos in 9..200 {
            assert_eq!(
                r.read_bits(1).unwrap(),
                lsb_bit(&data, pos),
                "lsb bit {pos}"
            );
        }
    }
}

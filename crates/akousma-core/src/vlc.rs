//! Table-driven decoder of variable-length prefix codes (Huffman codes).
//!
//! A [`Codebook`] is a flat array of 4-byte entries split into a root table
//! and sub-tables. One peek of up to `table_bits` bits indexes the root
//! table; codes longer than the root width continue through sub-tables, one
//! peek per level. Each entry is either a leaf (code length within the
//! current table and symbol) or a jump (sub-table width and offset).
//!
//! Codes are written most significant bit first: `code = 0b110, len = 3` is
//! the bit sequence `1, 1, 0`. The [`BitOrder`] of a codebook says how that
//! sequence is packed into bytes: MPEG audio and AAC fill bytes from the most
//! significant bit, Vorbis from the least significant bit.

use crate::error::{Error, ErrorKind};
use crate::io::{BitReaderLsb, BitReaderMsb};
use core::fmt;

/// Longest supported code, in bits.
pub const MAX_CODE_LEN: u8 = 32;
/// Largest symbol value: symbols are stored in 24 bits.
pub const MAX_SYMBOL: u32 = (1 << PAYLOAD_BITS) - 1;
/// Largest table width accepted by the constructors.
pub const MAX_TABLE_BITS: u8 = 16;

/// Bits of an entry that hold a symbol or a sub-table offset.
const PAYLOAD_BITS: u32 = 24;
const PAYLOAD_MASK: u32 = (1 << PAYLOAD_BITS) - 1;
/// Entry bits `24..=30`: code length (leaf) or sub-table width (jump).
const WIDTH_MASK: u32 = 0x7F;
/// Entry bit 31: set for a jump.
const JUMP_FLAG: u32 = 1 << 31;
/// Largest number of entries: offsets are stored in 24 bits.
const MAX_ENTRIES: usize = 1 << PAYLOAD_BITS;
/// Number of distinct codes of `MAX_CODE_LEN` bits; a complete code fills it.
const CODE_SPACE: u64 = 1 << MAX_CODE_LEN;
/// Bits peeked per decoded symbol: enough for the longest code.
const PEEK_BITS: u32 = MAX_CODE_LEN as u32;

/// One table entry: `[jump:1][width:7][payload:24]`. A leaf with width 0
/// marks a bit pattern that no code starts with.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
struct Entry(u32);

impl Entry {
    const NONE: Self = Self(0);

    const fn leaf(len: u32, symbol: u32) -> Self {
        Self((len << PAYLOAD_BITS) | symbol)
    }

    const fn jump(width: u32, offset: u32) -> Self {
        Self(JUMP_FLAG | (width << PAYLOAD_BITS) | offset)
    }

    const fn is_jump(self) -> bool {
        self.0 & JUMP_FLAG != 0
    }

    const fn width(self) -> u32 {
        (self.0 >> PAYLOAD_BITS) & WIDTH_MASK
    }

    const fn payload(self) -> u32 {
        self.0 & PAYLOAD_MASK
    }
}

/// How code bits are packed into bytes.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum BitOrder {
    /// Most significant bit of each byte first ([`BitReaderMsb`]).
    Msb,
    /// Least significant bit of each byte first ([`BitReaderLsb`]).
    Lsb,
}

/// A code word: `len` bits of `code`, most significant first, and the symbol
/// it decodes to.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct CodeWord {
    /// Code bits, right-aligned.
    pub code: u32,
    /// Code length in bits, `1..=32`.
    pub len: u8,
    /// Decoded value, at most [`MAX_SYMBOL`].
    pub symbol: u32,
}

/// Reason a set of codes was rejected.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CodebookError {
    /// A length is above 32, an explicit code has length 0, or a code does
    /// not fit in its length.
    InvalidLength,
    /// A symbol is above [`MAX_SYMBOL`].
    SymbolOutOfRange,
    /// Two codes collide or one is a prefix of another; for code lengths,
    /// no free code of the requested length is left.
    Overspecified,
    /// The codes leave bit patterns undecodable (the Kraft sum is below 1).
    Underpopulated,
    /// The tables need more than `2^24` entries.
    TableTooLarge,
}

impl fmt::Display for CodebookError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidLength => "invalid code length",
            Self::SymbolOutOfRange => "symbol out of range",
            Self::Overspecified => "overspecified prefix code",
            Self::Underpopulated => "underpopulated prefix code",
            Self::TableTooLarge => "code tables too large",
        })
    }
}

impl std::error::Error for CodebookError {}

impl From<CodebookError> for Error {
    /// Every codebook error is [`ErrorKind::InvalidData`].
    fn from(_: CodebookError) -> Self {
        ErrorKind::InvalidData.into()
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for crate::io::BitReaderMsb<'_> {}
    impl Sealed for crate::io::BitReaderLsb<'_> {}
}

/// Bit readers a [`Codebook`] can decode from.
pub trait BitSource: sealed::Sealed {
    /// Bit order of the reader.
    const ORDER: BitOrder;
    /// The next `n` bits (`n <= 56`), zero-padded past the end.
    fn peek_bits(&mut self, n: u32) -> u64;
    /// Consumes `n` bits.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnexpectedEof`] if fewer than `n` bits remain.
    fn consume_bits(&mut self, n: u32) -> Result<(), Error>;
}

impl BitSource for BitReaderMsb<'_> {
    const ORDER: BitOrder = BitOrder::Msb;

    #[inline]
    fn peek_bits(&mut self, n: u32) -> u64 {
        self.peek_padded(n)
    }

    #[inline]
    fn consume_bits(&mut self, n: u32) -> Result<(), Error> {
        self.skip_bits(u64::from(n))
    }
}

impl BitSource for BitReaderLsb<'_> {
    const ORDER: BitOrder = BitOrder::Lsb;

    #[inline]
    fn peek_bits(&mut self, n: u32) -> u64 {
        self.peek_padded(n)
    }

    #[inline]
    fn consume_bits(&mut self, n: u32) -> Result<(), Error> {
        self.skip_bits(u64::from(n))
    }
}

/// Decoding tables for one prefix code.
///
/// The code must be complete (every bit pattern starts with a code), with
/// two exceptions that Vorbis requires: an empty code, which fails to decode
/// any symbol, and a single code of length 1, which decodes from either bit
/// value.
///
/// ```ignore
/// use akousma_core::io::BitReaderMsb;
/// use akousma_core::vlc::{BitOrder, Codebook, CodeWord};
///
/// // MPEG audio Layer III Huffman table 1; symbols are `x << 4 | y`.
/// let words = [
///     CodeWord { code: 0b1, len: 1, symbol: 0x00 },
///     CodeWord { code: 0b001, len: 3, symbol: 0x01 },
///     CodeWord { code: 0b01, len: 2, symbol: 0x10 },
///     CodeWord { code: 0b000, len: 3, symbol: 0x11 },
/// ];
/// let book = Codebook::new(&words, BitOrder::Msb, 9)?;
/// let mut bits = BitReaderMsb::new(&[0b1_01_000_00, 0b1_0000000]);
/// assert_eq!(book.decode(&mut bits)?, 0x00);
/// assert_eq!(book.decode(&mut bits)?, 0x10);
/// assert_eq!(book.decode(&mut bits)?, 0x11);
/// assert_eq!(book.decode(&mut bits)?, 0x01);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[derive(Clone, Debug)]
pub struct Codebook {
    entries: Vec<Entry>,
    root_bits: u32,
    order: BitOrder,
}

impl Codebook {
    /// Builds tables for explicit code words.
    ///
    /// `table_bits` caps the width of every table; codes longer than the root
    /// table take one more lookup per `table_bits` bits.
    ///
    /// # Errors
    ///
    /// See [`CodebookError`].
    ///
    /// # Panics
    ///
    /// If `table_bits` is not in `1..=16` ([`MAX_TABLE_BITS`]).
    pub fn new(words: &[CodeWord], order: BitOrder, table_bits: u8) -> Result<Self, CodebookError> {
        assert!(
            (1..=MAX_TABLE_BITS).contains(&table_bits),
            "table width {table_bits} is outside 1..={MAX_TABLE_BITS}"
        );
        let mut sorted = Vec::with_capacity(words.len());
        for &word in words {
            if word.len == 0 || word.len > MAX_CODE_LEN || u64::from(word.code) >> word.len != 0 {
                return Err(CodebookError::InvalidLength);
            }
            if word.symbol > MAX_SYMBOL {
                return Err(CodebookError::SymbolOutOfRange);
            }
            sorted.push(Code::from(word));
        }
        sorted.sort_unstable_by_key(|c| (c.start(), c.len));

        let mut covered_until = 0;
        for c in &sorted {
            if c.start() < covered_until {
                return Err(CodebookError::Overspecified);
            }
            covered_until = c.start() + c.span();
        }
        let total: u64 = sorted.iter().map(Code::span).sum();

        match sorted.as_slice() {
            [] => Ok(Self {
                entries: vec![Entry::NONE; 2],
                root_bits: 1,
                order,
            }),
            [single] if single.len == 1 => Ok(Self {
                entries: vec![Entry::leaf(1, single.symbol); 2],
                root_bits: 1,
                order,
            }),
            _ if total != CODE_SPACE => Err(CodebookError::Underpopulated),
            _ => {
                let max_len = sorted.iter().map(|c| c.len).fold(0, u32::max);
                let root_bits = max_len.min(u32::from(table_bits));
                let mut entries = Vec::new();
                fill_table(
                    &mut entries,
                    &sorted,
                    root_bits,
                    u32::from(table_bits),
                    order,
                )?;
                Ok(Self {
                    entries,
                    root_bits,
                    order,
                })
            }
        }
    }

    /// Builds tables from code lengths, one per symbol (`0` marks an unused
    /// symbol). Symbols take codes in ascending symbol order, each the
    /// lowest-valued free code of its length (Vorbis I specification,
    /// section 3.2.1). For lengths sorted in ascending order this is the
    /// canonical Huffman code.
    ///
    /// # Errors
    ///
    /// See [`CodebookError`].
    ///
    /// # Panics
    ///
    /// If `table_bits` is not in `1..=16` ([`MAX_TABLE_BITS`]).
    pub fn from_lengths(
        lengths: &[u8],
        order: BitOrder,
        table_bits: u8,
    ) -> Result<Self, CodebookError> {
        // free[d]: a free code of length d whose whole subtree is unused. At
        // most one exists per length, and longer free codes have lower
        // values, so the lowest free code of length `len` grows from the
        // longest free code not longer than `len`.
        let mut free = [None; MAX_CODE_LEN as usize + 1];
        free[0] = Some(0u32);
        let mut words = Vec::new();
        for (symbol, &len) in lengths.iter().enumerate() {
            if len == 0 {
                continue;
            }
            if len > MAX_CODE_LEN {
                return Err(CodebookError::InvalidLength);
            }
            if symbol > MAX_SYMBOL as usize {
                return Err(CodebookError::SymbolOutOfRange);
            }
            let len_index = usize::from(len);
            let Some((depth, mut code)) = (0..=len_index)
                .rev()
                .find_map(|d| free[d].take().map(|c| (d, c)))
            else {
                return Err(CodebookError::Overspecified);
            };
            for slot in &mut free[depth + 1..=len_index] {
                code <<= 1;
                *slot = Some(code | 1);
            }
            words.push(CodeWord {
                code,
                len,
                symbol: symbol as u32,
            });
        }
        Self::new(&words, order, table_bits)
    }

    /// Bit order the tables were built for.
    #[must_use]
    pub const fn order(&self) -> BitOrder {
        self.order
    }

    /// Decodes one symbol.
    ///
    /// The lookup peeks 32 bits at once, zero-padded past the end of the
    /// input, and consumes only the bits of the matched code. A short final
    /// code therefore decodes even when fewer bits than the table width
    /// remain.
    ///
    /// # Errors
    ///
    /// * [`ErrorKind::UnexpectedEof`] if the input ends inside a code;
    /// * [`ErrorKind::CorruptFrame`] if the codebook is empty.
    ///
    /// On error the reader is unchanged.
    ///
    /// # Panics
    ///
    /// If the reader's bit order differs from [`Codebook::order`].
    #[inline]
    pub fn decode<R: BitSource>(&self, bits: &mut R) -> Result<u32, Error> {
        assert!(
            R::ORDER == self.order,
            "codebook built for {:?} bit order",
            self.order
        );
        let window = bits.peek_bits(PEEK_BITS);
        let mut used = 0;
        let mut width = self.root_bits;
        let mut base = 0;
        loop {
            // Tables are built so that `used + width <= 32`.
            let index = match R::ORDER {
                BitOrder::Msb => window >> (PEEK_BITS - used - width),
                BitOrder::Lsb => window >> used,
            } & ((1 << width) - 1);
            // Every table holds `2^width` entries.
            let entry = self.entries[base + index as usize];
            if entry.is_jump() {
                used += width;
                base = entry.payload() as usize;
                width = entry.width();
            } else if entry.width() == 0 {
                return Err(ErrorKind::CorruptFrame.into());
            } else {
                bits.consume_bits(used + entry.width())?;
                return Ok(entry.payload());
            }
        }
    }
}

/// A validated code word.
#[derive(Copy, Clone, Debug)]
struct Code {
    code: u32,
    len: u32,
    symbol: u32,
}

impl From<CodeWord> for Code {
    fn from(word: CodeWord) -> Self {
        Self {
            code: word.code,
            len: u32::from(word.len),
            symbol: word.symbol,
        }
    }
}

impl Code {
    /// First 32-bit pattern that starts with this code.
    fn start(&self) -> u64 {
        u64::from(self.code) << (u32::from(MAX_CODE_LEN) - self.len)
    }

    /// Number of 32-bit patterns that start with this code.
    fn span(&self) -> u64 {
        1 << (u32::from(MAX_CODE_LEN) - self.len)
    }
}

/// Table index of the `len`-bit code `code` with `pad` bits after it.
fn table_index(code: u32, len: u32, pad_value: u32, pad: u32, order: BitOrder) -> usize {
    let index = match order {
        BitOrder::Msb => (code << pad) | pad_value,
        BitOrder::Lsb => (code.reverse_bits() >> (u32::BITS - len)) | (pad_value << len),
    };
    index as usize
}

/// Appends a table of `2^width` entries for `codes` (sorted by start, a
/// complete prefix code relative to this table) and returns its offset.
fn fill_table(
    entries: &mut Vec<Entry>,
    codes: &[Code],
    width: u32,
    table_bits: u32,
    order: BitOrder,
) -> Result<u32, CodebookError> {
    let offset = entries.len();
    let size = 1usize << width;
    if offset + size > MAX_ENTRIES {
        return Err(CodebookError::TableTooLarge);
    }
    entries.resize(offset + size, Entry::NONE);
    let mut rest = codes;
    while let Some((first, _)) = rest.split_first() {
        if first.len <= width {
            let pad = width - first.len;
            for pad_value in 0..1u32 << pad {
                let index = table_index(first.code, first.len, pad_value, pad, order);
                entries[offset + index] = Entry::leaf(first.len, first.symbol);
            }
            rest = &rest[1..];
            continue;
        }
        let prefix_of = |c: &Code| c.code >> (c.len - width);
        let prefix = prefix_of(first);
        let group_len = rest
            .iter()
            .take_while(|c| c.len > width && prefix_of(c) == prefix)
            .count();
        let (group, tail) = rest.split_at(group_len);
        let sub: Vec<Code> = group
            .iter()
            .map(|c| Code {
                code: c.code & ((1u32 << (c.len - width)) - 1),
                len: c.len - width,
                symbol: c.symbol,
            })
            .collect();
        let sub_width = sub.iter().map(|c| c.len).fold(0, u32::max).min(table_bits);
        let sub_offset = fill_table(entries, &sub, sub_width, table_bits, order)?;
        entries[offset + table_index(prefix, width, 0, 0, order)] =
            Entry::jump(sub_width, sub_offset);
        rest = tail;
    }
    // `offset + size <= 2^24`, so the offset fits in the 24-bit payload.
    Ok(offset as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Packs code words into bytes in the given bit order.
    fn encode(words: &[CodeWord], order: BitOrder) -> Vec<u8> {
        let mut out = Vec::new();
        let mut pos = 0usize;
        for w in words {
            for i in (0..w.len).rev() {
                if pos.is_multiple_of(8) {
                    out.push(0);
                }
                if (w.code >> i) & 1 == 1 {
                    let bit = match order {
                        BitOrder::Msb => 0x80 >> (pos % 8),
                        BitOrder::Lsb => 1 << (pos % 8),
                    };
                    *out.last_mut().unwrap() |= bit;
                }
                pos += 1;
            }
        }
        out
    }

    fn decode_all(book: &Codebook, data: &[u8], count: usize) -> Vec<u32> {
        match book.order() {
            BitOrder::Msb => {
                let mut r = BitReaderMsb::new(data);
                (0..count).map(|_| book.decode(&mut r).unwrap()).collect()
            }
            BitOrder::Lsb => {
                let mut r = BitReaderLsb::new(data);
                (0..count).map(|_| book.decode(&mut r).unwrap()).collect()
            }
        }
    }

    fn word(code: u32, len: u8, symbol: u32) -> CodeWord {
        CodeWord { code, len, symbol }
    }

    /// Code words of the example in Vorbis I section 3.2.1.
    fn vorbis_example() -> [CodeWord; 8] {
        [
            word(0b00, 2, 0),
            word(0b0100, 4, 1),
            word(0b0101, 4, 2),
            word(0b0110, 4, 3),
            word(0b0111, 4, 4),
            word(0b10, 2, 5),
            word(0b110, 3, 6),
            word(0b111, 3, 7),
        ]
    }

    fn check_round_trip(book: &Codebook, words: &[CodeWord]) {
        let message: Vec<CodeWord> = words.iter().chain(words.iter().rev()).copied().collect();
        let data = encode(&message, book.order());
        let symbols: Vec<u32> = message.iter().map(|w| w.symbol).collect();
        assert_eq!(decode_all(book, &data, message.len()), symbols);
    }

    #[test]
    fn vlc_codebook_msb_and_lsb() {
        let lengths = [2, 4, 4, 4, 4, 2, 3, 3];
        let words = vorbis_example();
        for order in [BitOrder::Msb, BitOrder::Lsb] {
            // Root tables of 1 to 4 bits: 4-bit codes take up to 4 lookups.
            for table_bits in [1, 2, 3, 4, 9] {
                let from_lengths = Codebook::from_lengths(&lengths, order, table_bits).unwrap();
                check_round_trip(&from_lengths, &words);
                let explicit = Codebook::new(&words, order, table_bits).unwrap();
                check_round_trip(&explicit, &words);
            }
        }

        // The same lengths reordered (lewton test of unordered codebooks).
        let reordered = [
            word(0b00, 2, 0),
            word(0b0100, 4, 1),
            word(0b0101, 4, 2),
            word(0b10, 2, 3),
            word(0b0110, 4, 4),
            word(0b0111, 4, 5),
            word(0b110, 3, 6),
            word(0b111, 3, 7),
        ];
        let book = Codebook::from_lengths(&[2, 4, 4, 2, 4, 4, 3, 3], BitOrder::Lsb, 3).unwrap();
        check_round_trip(&book, &reordered);

        // Vorbis I section 2.1.6 packing: bits 1,1,0 then 0,1 fill the first
        // byte from bit 0: 0b10_011 = 0x13.
        let book = Codebook::from_lengths(&[2, 4, 4, 4, 4, 2, 3, 3], BitOrder::Lsb, 9).unwrap();
        let mut r = BitReaderLsb::new(&[0x13]);
        assert_eq!(book.decode(&mut r).unwrap(), 6);
        assert_eq!(r.bit_position(), 3);

        // Codes of 1 to 32 bits: 0, 10, 110, ..., 1^31 0, 1^32.
        let lengths: Vec<u8> = (1..=32).chain([32]).collect();
        let mut deep: Vec<CodeWord> = (0..32u32)
            .map(|i| word((((1u64 << i) - 1) << 1) as u32, i as u8 + 1, i))
            .collect();
        deep.push(word(u32::MAX, 32, 32));
        for order in [BitOrder::Msb, BitOrder::Lsb] {
            for table_bits in [1, 4, 7, 16] {
                let book = Codebook::from_lengths(&lengths, order, table_bits).unwrap();
                check_round_trip(&book, &deep);
                let book = Codebook::new(&deep, order, table_bits).unwrap();
                check_round_trip(&book, &deep);
            }
        }
    }

    #[test]
    fn rejects_malformed_codes() {
        // A ninth code cannot fit in the complete Vorbis example.
        let over = Codebook::from_lengths(&[2, 4, 4, 4, 4, 2, 3, 3, 3], BitOrder::Lsb, 9);
        assert_eq!(over.unwrap_err(), CodebookError::Overspecified);
        assert_eq!(
            Codebook::from_lengths(&[1, 1, 1], BitOrder::Msb, 4).unwrap_err(),
            CodebookError::Overspecified
        );
        // Without its last code the example is unfinished.
        let under = Codebook::from_lengths(&[2, 4, 4, 4, 4, 2, 3], BitOrder::Lsb, 9);
        assert_eq!(under.unwrap_err(), CodebookError::Underpopulated);
        // A single code must have length 1.
        assert_eq!(
            Codebook::from_lengths(&[2], BitOrder::Lsb, 9).unwrap_err(),
            CodebookError::Underpopulated
        );
        assert_eq!(
            Codebook::from_lengths(&[33], BitOrder::Lsb, 9).unwrap_err(),
            CodebookError::InvalidLength
        );

        let dup = [word(0b0, 1, 0), word(0b0, 1, 1)];
        assert_eq!(
            Codebook::new(&dup, BitOrder::Msb, 4).unwrap_err(),
            CodebookError::Overspecified
        );
        let prefix = [word(0b0, 1, 0), word(0b01, 2, 1), word(0b1, 1, 2)];
        assert_eq!(
            Codebook::new(&prefix, BitOrder::Msb, 4).unwrap_err(),
            CodebookError::Overspecified
        );
        let hole = [word(0b0, 1, 0), word(0b10, 2, 1)];
        assert_eq!(
            Codebook::new(&hole, BitOrder::Msb, 4).unwrap_err(),
            CodebookError::Underpopulated
        );
        let zero_len = [word(0, 0, 0)];
        assert_eq!(
            Codebook::new(&zero_len, BitOrder::Msb, 4).unwrap_err(),
            CodebookError::InvalidLength
        );
        let too_wide = [word(0b10, 1, 0), word(0b1, 1, 1)];
        assert_eq!(
            Codebook::new(&too_wide, BitOrder::Msb, 4).unwrap_err(),
            CodebookError::InvalidLength
        );
        let big_symbol = [word(0, 1, 0), word(1, 1, MAX_SYMBOL + 1)];
        assert_eq!(
            Codebook::new(&big_symbol, BitOrder::Msb, 4).unwrap_err(),
            CodebookError::SymbolOutOfRange
        );
        let max_symbol = [word(0, 1, 0), word(1, 1, MAX_SYMBOL)];
        let book = Codebook::new(&max_symbol, BitOrder::Msb, 4).unwrap();
        assert_eq!(
            book.decode(&mut BitReaderMsb::new(&[0x80])).unwrap(),
            MAX_SYMBOL
        );
        assert_eq!(
            Error::from(CodebookError::Overspecified).kind,
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn degenerate_codebooks() {
        // A single used entry of length 1 decodes from either bit and
        // consumes one bit (Vorbis I errata 20150226).
        let book = Codebook::from_lengths(&[0, 0, 1, 0], BitOrder::Lsb, 9).unwrap();
        let mut r = BitReaderLsb::new(&[0b10]);
        assert_eq!(book.decode(&mut r).unwrap(), 2);
        assert_eq!(book.decode(&mut r).unwrap(), 2);
        assert_eq!(r.bit_position(), 2);
        let mut empty_input = BitReaderLsb::new(&[]);
        assert_eq!(
            book.decode(&mut empty_input).unwrap_err().kind,
            ErrorKind::UnexpectedEof
        );

        // An empty codebook builds but decodes nothing.
        for lengths in [&[][..], &[0, 0, 0][..]] {
            let book = Codebook::from_lengths(lengths, BitOrder::Lsb, 9).unwrap();
            let mut r = BitReaderLsb::new(&[0xFF, 0x00]);
            assert_eq!(
                book.decode(&mut r).unwrap_err().kind,
                ErrorKind::CorruptFrame
            );
            assert_eq!(r.bit_position(), 0);
        }
        let empty = Codebook::new(&[], BitOrder::Msb, 4).unwrap();
        assert_eq!(
            empty.decode(&mut BitReaderMsb::new(&[0])).unwrap_err().kind,
            ErrorKind::CorruptFrame
        );
    }

    #[test]
    fn short_tail_and_truncated_code() {
        let book = Codebook::new(&vorbis_example(), BitOrder::Msb, 9).unwrap();
        // Three bits "110" before the bit limit: decodes although the root
        // table is 4 bits wide; the following bits are outside the limit.
        let mut r = BitReaderMsb::with_bit_limit(&[0b1101_1111], 3);
        assert_eq!(book.decode(&mut r).unwrap(), 6);
        assert_eq!(
            book.decode(&mut r).unwrap_err().kind,
            ErrorKind::UnexpectedEof
        );
        // "01" then the end: a 4-bit code is cut off.
        let mut r = BitReaderMsb::with_bit_limit(&[0b0100_0000], 2);
        assert_eq!(
            book.decode(&mut r).unwrap_err().kind,
            ErrorKind::UnexpectedEof
        );
        assert_eq!(r.bit_position(), 0);

        // A long code cut off after two jumps leaves the reader unchanged.
        let deep_lengths: Vec<u8> = (1..=32).chain([32]).collect();
        let deep = Codebook::from_lengths(&deep_lengths, BitOrder::Msb, 8).unwrap();
        let mut r = BitReaderMsb::new(&[0xFF, 0xFF]);
        assert_eq!(
            deep.decode(&mut r).unwrap_err().kind,
            ErrorKind::UnexpectedEof
        );
        assert_eq!(r.bit_position(), 0);
        let mut r = BitReaderLsb::new(&[0xFF, 0xFF, 0xFF]);
        let deep_lsb = Codebook::from_lengths(&deep_lengths, BitOrder::Lsb, 8).unwrap();
        assert_eq!(
            deep_lsb.decode(&mut r).unwrap_err().kind,
            ErrorKind::UnexpectedEof
        );
        assert_eq!(r.bit_position(), 0);
        // 32 ones decode to the last symbol.
        let mut r = BitReaderMsb::new(&[0xFF; 4]);
        assert_eq!(deep.decode(&mut r).unwrap(), 32);
    }

    #[test]
    #[should_panic(expected = "codebook built for Msb bit order")]
    fn order_mismatch_panics() {
        let book = Codebook::new(&vorbis_example(), BitOrder::Msb, 9).unwrap();
        let _ = book.decode(&mut BitReaderLsb::new(&[0]));
    }

    #[test]
    #[should_panic(expected = "table width 17 is outside 1..=16")]
    fn wide_table_panics() {
        let _ = Codebook::new(&vorbis_example(), BitOrder::Msb, 17);
    }

    /// Deterministic xorshift generator for the randomized test.
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        fn below(&mut self, n: usize) -> usize {
            (self.next() % n as u64) as usize
        }
    }

    #[test]
    fn random_complete_codes_round_trip() {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
        for _ in 0..40 {
            // Grow a complete code by splitting random leaves.
            let mut leaves = vec![(0u32, 0u8)];
            let splits = 1 + rng.below(300);
            for _ in 0..splits {
                let i = rng.below(leaves.len());
                let (code, len) = leaves[i];
                if len == MAX_CODE_LEN {
                    continue;
                }
                leaves[i] = (code << 1, len + 1);
                leaves.push(((code << 1) | 1, len + 1));
            }
            let words: Vec<CodeWord> = leaves
                .iter()
                .enumerate()
                .map(|(s, &(code, len))| word(code, len, (s as u32 * 7919) & MAX_SYMBOL))
                .collect();
            let message: Vec<CodeWord> = (0..500).map(|_| words[rng.below(words.len())]).collect();
            let symbols: Vec<u32> = message.iter().map(|w| w.symbol).collect();
            for order in [BitOrder::Msb, BitOrder::Lsb] {
                let table_bits = 1 + rng.below(usize::from(MAX_TABLE_BITS)) as u8;
                let book = Codebook::new(&words, order, table_bits).unwrap();
                let data = encode(&message, order);
                assert_eq!(decode_all(&book, &data, message.len()), symbols);
            }
        }
    }
}

//! Table-driven CRC-8, CRC-16 and CRC-32.
//!
//! CRC-16 and CRC-32 process four bytes per step with four lookup tables
//! (slice-by-4); the remaining one to three bytes use the first table.

use super::Checksum;

/// CRC-8 polynomial `x^8 + x^2 + x + 1` (RFC 9639 §9.1.8).
const CRC8_POLY: u8 = 0x07;
/// CRC-16 polynomial `x^16 + x^15 + x^2 + 1` (RFC 9639 §9.3, ISO/IEC 11172-3).
const CRC16_POLY: u16 = 0x8005;
/// CRC-32 polynomial `0x04C11DB7`, most significant bit first (RFC 3533 §6,
/// ISO/IEC 13818-1).
const CRC32_POLY: u32 = 0x04C1_1DB7;
/// The same CRC-32 polynomial with reversed bit order (ISO 3309).
const CRC32_POLY_REFLECTED: u32 = 0xEDB8_8320;

static CRC8_TABLE: [u8; 256] = crc8_table();
static CRC16_TABLES: [[u16; 256]; 4] = crc16_tables();
static CRC32_TABLES: [[u32; 256]; 4] = crc32_tables();
static CRC32_REFLECTED_TABLES: [[u32; 256]; 4] = crc32_reflected_tables();

const fn crc8_table() -> [u8; 256] {
    let mut table = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut reg = i as u8;
        let mut bit = 0;
        while bit < 8 {
            reg = if reg & 0x80 != 0 {
                (reg << 1) ^ CRC8_POLY
            } else {
                reg << 1
            };
            bit += 1;
        }
        table[i] = reg;
        i += 1;
    }
    table
}

/// `tables[0][b]` is the register after feeding byte `b` into a zero
/// register; `tables[k][b]` additionally feeds `k` zero bytes.
const fn crc16_tables() -> [[u16; 256]; 4] {
    let mut tables = [[0u16; 256]; 4];
    let mut i = 0;
    while i < 256 {
        let mut reg = (i as u16) << 8;
        let mut bit = 0;
        while bit < 8 {
            reg = if reg & 0x8000 != 0 {
                (reg << 1) ^ CRC16_POLY
            } else {
                reg << 1
            };
            bit += 1;
        }
        tables[0][i] = reg;
        i += 1;
    }
    let mut k = 1;
    while k < 4 {
        let mut i = 0;
        while i < 256 {
            let prev = tables[k - 1][i];
            tables[k][i] = (prev << 8) ^ tables[0][(prev >> 8) as usize];
            i += 1;
        }
        k += 1;
    }
    tables
}

/// Same layout as [`crc16_tables`] for the most-significant-bit-first CRC-32.
const fn crc32_tables() -> [[u32; 256]; 4] {
    let mut tables = [[0u32; 256]; 4];
    let mut i = 0;
    while i < 256 {
        let mut reg = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            reg = if reg & 0x8000_0000 != 0 {
                (reg << 1) ^ CRC32_POLY
            } else {
                reg << 1
            };
            bit += 1;
        }
        tables[0][i] = reg;
        i += 1;
    }
    let mut k = 1;
    while k < 4 {
        let mut i = 0;
        while i < 256 {
            let prev = tables[k - 1][i];
            tables[k][i] = (prev << 8) ^ tables[0][(prev >> 24) as usize];
            i += 1;
        }
        k += 1;
    }
    tables
}

/// Same layout as [`crc16_tables`] for the reflected (least significant bit
/// first) CRC-32.
const fn crc32_reflected_tables() -> [[u32; 256]; 4] {
    let mut tables = [[0u32; 256]; 4];
    let mut i = 0;
    while i < 256 {
        let mut reg = i as u32;
        let mut bit = 0;
        while bit < 8 {
            reg = if reg & 1 != 0 {
                (reg >> 1) ^ CRC32_POLY_REFLECTED
            } else {
                reg >> 1
            };
            bit += 1;
        }
        tables[0][i] = reg;
        i += 1;
    }
    let mut k = 1;
    while k < 4 {
        let mut i = 0;
        while i < 256 {
            let prev = tables[k - 1][i];
            tables[k][i] = (prev >> 8) ^ tables[0][(prev & 0xFF) as usize];
            i += 1;
        }
        k += 1;
    }
    tables
}

/// CRC-8 of the FLAC frame header (RFC 9639 §9.1.8): polynomial `0x07`, most
/// significant bit first, initial value `0`, no final XOR.
///
/// Feeding the header followed by its stored CRC byte yields `0`.
///
/// ```ignore
/// use akousma_core::checksum::{Checksum, Crc8};
///
/// let mut crc = Crc8::new();
/// crc.update(b"123456789");
/// assert_eq!(crc.value(), 0xF4);
/// crc.update(&[0xF4]);
/// assert_eq!(crc.value(), 0);
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Crc8 {
    state: u8,
}

impl Crc8 {
    /// Creates a CRC-8 with initial value `0`.
    #[must_use]
    pub const fn new() -> Self {
        Self { state: 0 }
    }

    /// Returns the CRC of the bytes fed so far.
    #[must_use]
    pub const fn value(&self) -> u8 {
        self.state
    }
}

impl Default for Crc8 {
    fn default() -> Self {
        Self::new()
    }
}

impl Checksum for Crc8 {
    fn update(&mut self, bytes: &[u8]) {
        let mut state = self.state;
        for &byte in bytes {
            state = CRC8_TABLE[usize::from(state ^ byte)];
        }
        self.state = state;
    }
}

/// CRC-16 with polynomial `0x8005`, most significant bit first, no final XOR.
///
/// FLAC (RFC 9639 §9.3) starts from `0x0000`; MPEG audio and ADTS headers
/// (ISO/IEC 11172-3, 13818-7) start from `0xFFFF`. Feeding the protected bytes
/// followed by the stored big-endian CRC yields `0` for both.
///
/// ```ignore
/// use akousma_core::checksum::{Checksum, Crc16};
///
/// let mut flac = Crc16::flac();
/// flac.update(b"123456789");
/// assert_eq!(flac.value(), 0xFEE8);
///
/// let mut mpeg = Crc16::mpeg();
/// mpeg.update(b"123456789");
/// assert_eq!(mpeg.value(), 0xAEE7);
/// mpeg.update(&0xAEE7u16.to_be_bytes());
/// assert_eq!(mpeg.value(), 0);
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Crc16 {
    state: u16,
}

impl Crc16 {
    /// Creates a CRC-16 with initial value `0x0000` (FLAC).
    #[must_use]
    pub const fn new() -> Self {
        Self::with_init(0)
    }

    /// Creates a CRC-16 with the given initial register value.
    #[must_use]
    pub const fn with_init(init: u16) -> Self {
        Self { state: init }
    }

    /// FLAC frame footer CRC (initial value `0x0000`).
    #[must_use]
    pub const fn flac() -> Self {
        Self::with_init(0)
    }

    /// MPEG audio / ADTS header CRC (initial value `0xFFFF`).
    #[must_use]
    pub const fn mpeg() -> Self {
        Self::with_init(0xFFFF)
    }

    /// Returns the CRC of the bytes fed so far.
    #[must_use]
    pub const fn value(&self) -> u16 {
        self.state
    }
}

impl Default for Crc16 {
    fn default() -> Self {
        Self::new()
    }
}

impl Checksum for Crc16 {
    fn update(&mut self, bytes: &[u8]) {
        let [t0, t1, t2, t3] = &CRC16_TABLES;
        let mut state = self.state;
        let (quads, tail) = bytes.as_chunks::<4>();
        for quad in quads {
            let x = u32::from_be_bytes(*quad) ^ (u32::from(state) << 16);
            let [b0, b1, b2, b3] = x.to_be_bytes();
            state = t3[usize::from(b0)]
                ^ t2[usize::from(b1)]
                ^ t1[usize::from(b2)]
                ^ t0[usize::from(b3)];
        }
        for &byte in tail {
            let [hi, _] = state.to_be_bytes();
            state = (state << 8) ^ t0[usize::from(hi ^ byte)];
        }
        self.state = state;
    }
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
enum BitOrder {
    MsbFirst,
    Reflected,
}

/// CRC-32 with polynomial `0x04C11DB7` in one of three standard
/// parameterisations.
///
/// | Constructor | Bit order | Initial value | Final XOR |
/// | :--- | :--- | :--- | :--- |
/// | [`Crc32::ogg`] | MSB first | `0x00000000` | `0x00000000` |
/// | [`Crc32::mpeg2`] | MSB first | `0xFFFFFFFF` | `0x00000000` |
/// | [`Crc32::ieee`] | reflected | `0xFFFFFFFF` | `0xFFFFFFFF` |
///
/// An Ogg page stores its CRC little-endian inside the header (bytes
/// `22..26`); those four bytes must be fed as zeros when verifying.
///
/// ```ignore
/// use akousma_core::checksum::{Checksum, Crc32};
///
/// let mut crc = Crc32::ieee();
/// crc.update(b"The quick brown fox jumps over the lazy dog");
/// assert_eq!(crc.value(), 0x414F_A339);
/// ```
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub struct Crc32 {
    state: u32,
    order: BitOrder,
    xorout: u32,
}

impl Crc32 {
    /// Ogg page CRC (RFC 3533 §6).
    #[must_use]
    pub const fn ogg() -> Self {
        Self {
            state: 0,
            order: BitOrder::MsbFirst,
            xorout: 0,
        }
    }

    /// MPEG-2 transport stream section CRC (ISO/IEC 13818-1).
    #[must_use]
    pub const fn mpeg2() -> Self {
        Self {
            state: u32::MAX,
            order: BitOrder::MsbFirst,
            xorout: 0,
        }
    }

    /// ISO 3309 / IEEE 802.3 CRC used by ID3v2, gzip and PNG.
    #[must_use]
    pub const fn ieee() -> Self {
        Self {
            state: u32::MAX,
            order: BitOrder::Reflected,
            xorout: u32::MAX,
        }
    }

    /// Returns the CRC of the bytes fed so far, with the final XOR applied.
    #[must_use]
    pub const fn value(&self) -> u32 {
        self.state ^ self.xorout
    }
}

impl Checksum for Crc32 {
    fn update(&mut self, bytes: &[u8]) {
        let (quads, tail) = bytes.as_chunks::<4>();
        let mut state = self.state;
        match self.order {
            BitOrder::MsbFirst => {
                let [t0, t1, t2, t3] = &CRC32_TABLES;
                for quad in quads {
                    let [b0, b1, b2, b3] = (u32::from_be_bytes(*quad) ^ state).to_be_bytes();
                    state = t3[usize::from(b0)]
                        ^ t2[usize::from(b1)]
                        ^ t1[usize::from(b2)]
                        ^ t0[usize::from(b3)];
                }
                for &byte in tail {
                    let [hi, ..] = state.to_be_bytes();
                    state = (state << 8) ^ t0[usize::from(hi ^ byte)];
                }
            }
            BitOrder::Reflected => {
                let [t0, t1, t2, t3] = &CRC32_REFLECTED_TABLES;
                for quad in quads {
                    let [b0, b1, b2, b3] = (u32::from_le_bytes(*quad) ^ state).to_le_bytes();
                    state = t3[usize::from(b0)]
                        ^ t2[usize::from(b1)]
                        ^ t1[usize::from(b2)]
                        ^ t0[usize::from(b3)];
                }
                for &byte in tail {
                    let [lo, ..] = state.to_le_bytes();
                    state = (state >> 8) ^ t0[usize::from(lo ^ byte)];
                }
            }
        }
        self.state = state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::io::{ByteReader, CrcMonitorReader};
    use std::io::Cursor;

    const CHECK_INPUT: &[u8] = b"123456789";

    fn checksum_of<C: Checksum>(mut crc: C, bytes: &[u8]) -> C {
        crc.update(bytes);
        crc
    }

    /// Feeds `bytes` in every split `bytes[..i]` + `bytes[i..]` and one byte at
    /// a time, and checks that all results equal the one-shot result.
    fn assert_split_invariant<C: Checksum + Copy + PartialEq + core::fmt::Debug>(
        init: C,
        bytes: &[u8],
    ) {
        let whole = checksum_of(init, bytes);
        for i in 0..=bytes.len() {
            let mut split = init;
            split.update(&bytes[..i]);
            split.update(&bytes[i..]);
            assert_eq!(split, whole, "split at {i}");
        }
        let mut bytewise = init;
        for byte in bytes {
            bytewise.update(core::slice::from_ref(byte));
        }
        assert_eq!(bytewise, whole);
    }

    #[test]
    fn crc_all_variants_and_monitor() {
        // Check values of the CRC catalogue parameterisations.
        assert_eq!(checksum_of(Crc8::new(), CHECK_INPUT).value(), 0xF4);
        assert_eq!(checksum_of(Crc16::flac(), CHECK_INPUT).value(), 0xFEE8);
        assert_eq!(checksum_of(Crc16::mpeg(), CHECK_INPUT).value(), 0xAEE7);
        assert_eq!(
            checksum_of(Crc16::with_init(0xFFFF), CHECK_INPUT),
            checksum_of(Crc16::mpeg(), CHECK_INPUT)
        );
        assert_eq!(checksum_of(Crc32::ogg(), CHECK_INPUT).value(), 0x89A1_897F);
        assert_eq!(
            checksum_of(Crc32::mpeg2(), CHECK_INPUT).value(),
            0x0376_E6E7
        );
        assert_eq!(checksum_of(Crc32::ieee(), CHECK_INPUT).value(), 0xCBF4_3926);

        // Empty input leaves the initial value (after the final XOR).
        assert_eq!(Crc8::new().value(), 0);
        assert_eq!(Crc16::mpeg().value(), 0xFFFF);
        assert_eq!(Crc32::mpeg2().value(), 0xFFFF_FFFF);
        assert_eq!(Crc32::ieee().value(), 0);

        // A 1000-byte input exercises many slice-by-4 steps.
        let long: Vec<u8> = (0..1000u32)
            .map(|i| (i.wrapping_mul(31) ^ (i >> 3)) as u8)
            .collect();
        assert_eq!(checksum_of(Crc8::new(), &long).value(), LONG_CRC8);
        assert_eq!(checksum_of(Crc16::flac(), &long).value(), LONG_CRC16_FLAC);
        assert_eq!(checksum_of(Crc16::mpeg(), &long).value(), LONG_CRC16_MPEG);
        assert_eq!(checksum_of(Crc32::ogg(), &long).value(), LONG_CRC32_OGG);
        assert_eq!(checksum_of(Crc32::mpeg2(), &long).value(), LONG_CRC32_MPEG2);
        assert_eq!(checksum_of(Crc32::ieee(), &long).value(), LONG_CRC32_IEEE);

        let sample = &long[..23];
        assert_split_invariant(Crc8::new(), sample);
        assert_split_invariant(Crc16::flac(), sample);
        assert_split_invariant(Crc16::mpeg(), sample);
        assert_split_invariant(Crc32::ogg(), sample);
        assert_split_invariant(Crc32::mpeg2(), sample);
        assert_split_invariant(Crc32::ieee(), sample);

        // Residues: message followed by its own CRC.
        let mut framed = CHECK_INPUT.to_vec();
        framed.extend_from_slice(&0x0376_E6E7u32.to_be_bytes());
        assert_eq!(checksum_of(Crc32::mpeg2(), &framed).value(), 0);
        let mut framed = CHECK_INPUT.to_vec();
        framed.extend_from_slice(&0xCBF4_3926u32.to_le_bytes());
        assert_eq!(checksum_of(Crc32::ieee(), &framed).value(), 0x2144_DF1C);

        // A FLAC-style header (CRC-8 byte) and footer (big-endian CRC-16) read
        // through the monitor leave a zero register.
        let mut stream = CHECK_INPUT.to_vec();
        stream.push(0xF4);
        let mut reader = ByteReader::new(CrcMonitorReader::new(Cursor::new(&stream), Crc8::new()));
        assert_eq!(reader.read_array::<9>().unwrap(), *b"123456789");
        assert_eq!(reader.get_ref().checksum().value(), 0xF4);
        assert_eq!(reader.read_u8().unwrap(), 0xF4);
        assert_eq!(reader.get_ref().checksum().value(), 0);

        let mut stream = CHECK_INPUT.to_vec();
        stream.extend_from_slice(&0xFEE8u16.to_be_bytes());
        let mut reader =
            ByteReader::new(CrcMonitorReader::new(Cursor::new(&stream), Crc16::flac()));
        reader.skip(9).unwrap();
        assert_eq!(reader.read_u16_be().unwrap(), 0xFEE8);
        let (_, crc) = reader.into_inner().into_parts();
        assert_eq!(crc.value(), 0);
    }

    // Reference values for the 1000-byte input of `crc_all_variants_and_monitor`,
    // computed bit by bit from the polynomial definitions (IEEE also by zlib).
    const LONG_CRC8: u8 = 0x74;
    const LONG_CRC16_FLAC: u16 = 0x3928;
    const LONG_CRC16_MPEG: u16 = 0x13F8;
    const LONG_CRC32_OGG: u32 = 0x67FB_18EC;
    const LONG_CRC32_MPEG2: u32 = 0x99EC_3773;
    const LONG_CRC32_IEEE: u32 = 0x33D6_BB82;
}

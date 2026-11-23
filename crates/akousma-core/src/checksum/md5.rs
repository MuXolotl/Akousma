//! MD5 message digest (RFC 1321).

use super::Checksum;
use crate::error::{Error, ErrorKind};

/// Per-step left-rotation amounts (RFC 1321 §3.4).
const SHIFTS: [[u32; 4]; 4] = [
    [7, 12, 17, 22],
    [5, 9, 14, 20],
    [4, 11, 16, 23],
    [6, 10, 15, 21],
];

/// Additive constants `floor(2^32 * |sin(i + 1)|)` (RFC 1321 §3.4).
const K: [u32; 64] = [
    0xD76A_A478,
    0xE8C7_B756,
    0x2420_70DB,
    0xC1BD_CEEE,
    0xF57C_0FAF,
    0x4787_C62A,
    0xA830_4613,
    0xFD46_9501,
    0x6980_98D8,
    0x8B44_F7AF,
    0xFFFF_5BB1,
    0x895C_D7BE,
    0x6B90_1122,
    0xFD98_7193,
    0xA679_438E,
    0x49B4_0821,
    0xF61E_2562,
    0xC040_B340,
    0x265E_5A51,
    0xE9B6_C7AA,
    0xD62F_105D,
    0x0244_1453,
    0xD8A1_E681,
    0xE7D3_FBC8,
    0x21E1_CDE6,
    0xC337_07D6,
    0xF4D5_0D87,
    0x455A_14ED,
    0xA9E3_E905,
    0xFCEF_A3F8,
    0x676F_02D9,
    0x8D2A_4C8A,
    0xFFFA_3942,
    0x8771_F681,
    0x6D9D_6122,
    0xFDE5_380C,
    0xA4BE_EA44,
    0x4BDE_CFA9,
    0xF6BB_4B60,
    0xBEBF_BC70,
    0x289B_7EC6,
    0xEAA1_27FA,
    0xD4EF_3085,
    0x0488_1D05,
    0xD9D4_D039,
    0xE6DB_99E5,
    0x1FA2_7CF8,
    0xC4AC_5665,
    0xF429_2244,
    0x432A_FF97,
    0xAB94_23A7,
    0xFC93_A039,
    0x655B_59C3,
    0x8F0C_CC92,
    0xFFEF_F47D,
    0x8584_5DD1,
    0x6FA8_7E4F,
    0xFE2C_E6E0,
    0xA301_4314,
    0x4E08_11A1,
    0xF753_7E82,
    0xBD3A_F235,
    0x2AD7_D2BB,
    0xEB86_D391,
];

/// Initial chaining values A, B, C, D (RFC 1321 §3.3).
const INIT: [u32; 4] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476];

const BLOCK_BYTES: usize = 64;
/// Byte offset inside the final block where the 64-bit message length starts.
const LENGTH_OFFSET: usize = 56;
/// Staging buffer size for [`Md5::update_pcm`]: a multiple of 1, 2, 3 and 4
/// byte samples and of the block size.
const PCM_STAGING_BYTES: usize = 960;

/// Streaming MD5 digest (RFC 1321).
///
/// The message length is counted in a `u64`, so streams longer than 512 MiB
/// (2^32 bits) are handled.
///
/// ```ignore
/// use akousma_core::checksum::{Checksum, Md5};
///
/// let mut md5 = Md5::new();
/// md5.update(b"message ");
/// md5.update(b"digest");
/// assert_eq!(
///     md5.finalize(),
///     [
///         0xF9, 0x6B, 0x69, 0x7D, 0x7C, 0xB7, 0x93, 0x8D,
///         0x52, 0x5A, 0x2F, 0x31, 0xAA, 0xF1, 0x61, 0xD0,
///     ]
/// );
/// ```
#[derive(Clone, Debug)]
pub struct Md5 {
    state: [u32; 4],
    block: [u8; BLOCK_BYTES],
    block_len: usize,
    total_bytes: u64,
}

impl Md5 {
    /// Creates a digest of the empty message.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            state: INIT,
            block: [0; BLOCK_BYTES],
            block_len: 0,
            total_bytes: 0,
        }
    }

    /// Feeds interleaved signed PCM samples packed as FLAC does for its
    /// `STREAMINFO` signature (RFC 9639 §8.2).
    ///
    /// Each sample holds a value in the signed `bits_per_sample` range,
    /// right-aligned in the `i32`. It is written as `ceil(bits_per_sample / 8)`
    /// little-endian two's complement bytes, so the upper bits carry the sign
    /// extension. This applies to 8-bit audio as well.
    ///
    /// ```ignore
    /// use akousma_core::checksum::Md5;
    ///
    /// // RFC 9639 Appendix D.1: one 16-bit stereo frame.
    /// let mut md5 = Md5::new();
    /// md5.update_pcm(&[25588, 10416], 16)?;
    /// assert_eq!(
    ///     md5.finalize(),
    ///     [
    ///         0x3E, 0x84, 0xB4, 0x18, 0x07, 0xDC, 0x69, 0x03,
    ///         0x07, 0x58, 0x6A, 0x3D, 0xAD, 0x1A, 0x2E, 0x0F,
    ///     ]
    /// );
    /// # Ok::<(), akousma_core::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidData`] if `bits_per_sample` is not in `1..=32`.
    pub fn update_pcm(&mut self, interleaved: &[i32], bits_per_sample: u32) -> Result<(), Error> {
        match bits_per_sample {
            1..=8 => self.pack_pcm::<1>(interleaved),
            9..=16 => self.pack_pcm::<2>(interleaved),
            17..=24 => self.pack_pcm::<3>(interleaved),
            25..=32 => self.pack_pcm::<4>(interleaved),
            _ => return Err(ErrorKind::InvalidData.into()),
        }
        Ok(())
    }

    /// Writes the low `W` little-endian bytes of every sample.
    fn pack_pcm<const W: usize>(&mut self, samples: &[i32]) {
        let mut staging = [0u8; PCM_STAGING_BYTES];
        for group in samples.chunks(PCM_STAGING_BYTES / W) {
            let (slots, _) = staging.as_chunks_mut::<W>();
            for (slot, sample) in slots.iter_mut().zip(group) {
                slot.copy_from_slice(&sample.to_le_bytes()[..W]);
            }
            self.update(&staging[..group.len() * W]);
        }
    }

    /// Completes the digest and returns its 16 bytes.
    #[must_use]
    pub fn finalize(mut self) -> [u8; 16] {
        let bit_len = self.total_bytes.wrapping_mul(8);
        // Padding: 0x80, then zeros up to 56 mod 64 (one or two final blocks).
        let pad_len = if self.block_len < LENGTH_OFFSET {
            LENGTH_OFFSET - self.block_len
        } else {
            LENGTH_OFFSET + BLOCK_BYTES - self.block_len
        };
        let mut padding = [0u8; BLOCK_BYTES];
        padding[0] = 0x80;
        self.update(&padding[..pad_len]);
        self.update(&bit_len.to_le_bytes());
        debug_assert_eq!(self.block_len, 0);

        let mut digest = [0u8; 16];
        let (words, _) = digest.as_chunks_mut::<4>();
        for (out, word) in words.iter_mut().zip(self.state) {
            *out = word.to_le_bytes();
        }
        digest
    }
}

impl Default for Md5 {
    fn default() -> Self {
        Self::new()
    }
}

impl Checksum for Md5 {
    fn update(&mut self, bytes: &[u8]) {
        self.total_bytes = self.total_bytes.wrapping_add(bytes.len() as u64);
        let mut input = bytes;

        if self.block_len > 0 {
            let take = (BLOCK_BYTES - self.block_len).min(input.len());
            let (head, rest) = input.split_at(take);
            self.block[self.block_len..self.block_len + take].copy_from_slice(head);
            self.block_len += take;
            input = rest;
            if self.block_len < BLOCK_BYTES {
                return;
            }
            compress(&mut self.state, &self.block);
            self.block_len = 0;
        }

        let (blocks, rest) = input.as_chunks::<BLOCK_BYTES>();
        for block in blocks {
            compress(&mut self.state, block);
        }
        self.block[..rest.len()].copy_from_slice(rest);
        self.block_len = rest.len();
    }
}

/// One MD5 operation: `b + ((a + f + k + m) <<< shift)`.
#[inline]
const fn step(a: u32, b: u32, f: u32, k: u32, m: u32, shift: u32) -> u32 {
    b.wrapping_add(
        a.wrapping_add(f)
            .wrapping_add(k)
            .wrapping_add(m)
            .rotate_left(shift),
    )
}

/// Applies the four rounds of RFC 1321 §3.4 to one 64-byte block.
fn compress(state: &mut [u32; 4], block: &[u8; BLOCK_BYTES]) {
    let mut m = [0u32; 16];
    let (words, _) = block.as_chunks::<4>();
    for (dst, word) in m.iter_mut().zip(words) {
        *dst = u32::from_le_bytes(*word);
    }

    let [mut a, mut b, mut c, mut d] = *state;
    // Round functions F, G, H, I and message index orders (RFC 1321 §3.4).
    for i in 0..16 {
        let f = (b & c) | (!b & d);
        (a, b, c, d) = (d, step(a, b, f, K[i], m[i], SHIFTS[0][i % 4]), b, c);
    }
    for (i, &k) in K.iter().enumerate().take(32).skip(16) {
        let f = (b & d) | (c & !d);
        let g = (5 * i + 1) % 16;
        (a, b, c, d) = (d, step(a, b, f, k, m[g], SHIFTS[1][i % 4]), b, c);
    }
    for (i, &k) in K.iter().enumerate().take(48).skip(32) {
        let f = b ^ c ^ d;
        let g = (3 * i + 5) % 16;
        (a, b, c, d) = (d, step(a, b, f, k, m[g], SHIFTS[2][i % 4]), b, c);
    }
    for (i, &k) in K.iter().enumerate().take(64).skip(48) {
        let f = c ^ (b | !d);
        let g = (7 * i) % 16;
        (a, b, c, d) = (d, step(a, b, f, k, m[g], SHIFTS[3][i % 4]), b, c);
    }

    state[0] = state[0].wrapping_add(a);
    state[1] = state[1].wrapping_add(b);
    state[2] = state[2].wrapping_add(c);
    state[3] = state[3].wrapping_add(d);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(digest: [u8; 16]) -> String {
        digest.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn md5_hex(bytes: &[u8]) -> String {
        let mut md5 = Md5::new();
        md5.update(bytes);
        hex(md5.finalize())
    }

    fn md5_pcm_hex(samples: &[i32], bits_per_sample: u32) -> String {
        let mut md5 = Md5::new();
        md5.update_pcm(samples, bits_per_sample).unwrap();
        hex(md5.finalize())
    }

    #[test]
    fn md5_rfc1321_and_flac_d1() {
        // RFC 1321 Appendix A.5 test suite.
        let suite: [(&[u8], &str); 7] = [
            (b"", "d41d8cd98f00b204e9800998ecf8427e"),
            (b"a", "0cc175b9c0f1b6a831c399e269772661"),
            (b"abc", "900150983cd24fb0d6963f7d28e17f72"),
            (b"message digest", "f96b697d7cb7938d525a2f31aaf161d0"),
            (
                b"abcdefghijklmnopqrstuvwxyz",
                "c3fcd3d76192e4007dfb496cca67e13b",
            ),
            (
                b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789",
                "d174ab98d277d9f5a5611c2c9f419d9f",
            ),
            (
                b"12345678901234567890123456789012345678901234567890123456789012345678901234567890",
                "57edf4a22be3c955ac49da2e2107b67a",
            ),
        ];
        for (message, expected) in suite {
            assert_eq!(md5_hex(message), expected);
            // Byte-at-a-time feeding gives the same digest.
            let mut md5 = Md5::new();
            for byte in message {
                md5.update(core::slice::from_ref(byte));
            }
            assert_eq!(hex(md5.finalize()), expected);
        }

        // Padding boundaries: 55 bytes fit one final block, 56..=63 need two.
        // Reference digests of bytes 0, 1, 2, ... from Python `hashlib`.
        let ramp: Vec<u8> = (0..=255).collect();
        let boundaries = [
            (55, "6912ee65fff2d9f9ce2508cddf8bcda0"),
            (56, "51fdd1acda72405dfdfa03fcb85896d7"),
            (63, "48a6295221902e8e0938f773a7185e72"),
            (64, "b2d3f56bc197fd985d5965079b5e7148"),
            (65, "8bd7053801c768420faf816fadba971c"),
            (127, "8402b21e7bc7906493bae0dac017f1f9"),
            (128, "37eff01866ba3f538421b30b7cbefcac"),
        ];
        for (len, expected) in boundaries {
            assert_eq!(md5_hex(&ramp[..len]), expected, "length {len}");
            let (head, tail) = ramp[..len].split_at(len / 3);
            let mut md5 = Md5::new();
            md5.update(head);
            md5.update(tail);
            assert_eq!(hex(md5.finalize()), expected, "split length {len}");
        }

        // RFC 9639 Appendix D.1: samples 25588, 10416 -> bytes F4 63 B0 28.
        assert_eq!(
            md5_hex(&[0xF4, 0x63, 0xB0, 0x28]),
            "3e84b41807dc690307586a3dad1a2e0f"
        );
        assert_eq!(
            md5_pcm_hex(&[25588, 10416], 16),
            "3e84b41807dc690307586a3dad1a2e0f"
        );

        // Sign-extended little-endian packing; references are the digests of
        // the hand-packed bytes noted next to each case.
        // 80 7F FF
        assert_eq!(
            md5_pcm_hex(&[-128, 127, -1], 8),
            "006a57995fe9cfa1da8f5f0aae5bf8eb"
        );
        // 00 F8 FF 07
        assert_eq!(
            md5_pcm_hex(&[-2048, 2047], 12),
            "b419ddb965f5efb510d4f616ac468288"
        );
        // 00 00 F8 FF FF 07
        assert_eq!(
            md5_pcm_hex(&[-524_288, 524_287], 20),
            "eaecf79ee53eeabf533ece3cf4a33ad6"
        );
        // 00 00 80 FF FF 7F FF FF FF 01 00 00
        assert_eq!(
            md5_pcm_hex(&[-8_388_608, 8_388_607, -1, 1], 24),
            "d549d260733512d16e5911ecd49dacfe"
        );
        // 00 00 00 80 FF FF FF 7F
        assert_eq!(
            md5_pcm_hex(&[i32::MIN, i32::MAX], 32),
            "f80a0433ca4951811cb36f607903fc4c"
        );
        // 2000 24-bit samples span several staging buffers.
        let long: Vec<i32> = (0..2000)
            .map(|i| (i * 7919) % 16_777_216 - 8_388_608)
            .collect();
        assert_eq!(md5_pcm_hex(&long, 24), "d5b187a593197711889aa03a8a8723ba");

        assert_eq!(
            Md5::new().update_pcm(&[0], 0).unwrap_err().kind,
            ErrorKind::InvalidData
        );
        assert_eq!(
            Md5::new().update_pcm(&[0], 33).unwrap_err().kind,
            ErrorKind::InvalidData
        );
    }
}

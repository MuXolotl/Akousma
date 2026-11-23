//! The `fmt ` chunk: `WAVEFORMATEX`, `WAVEFORMATEXTENSIBLE` and the mapping
//! of format tags to codecs.

use akousma_core::channel::ChannelMask;
use akousma_core::codec::{CodecId, CodecParams};
use akousma_core::{Error, ErrorKind};

use crate::adpcm::BlockLayout;
use crate::adpcm_ms;

/// `WAVE_FORMAT_PCM`.
const TAG_PCM: u16 = 0x0001;
/// `WAVE_FORMAT_ADPCM` (Microsoft ADPCM).
const TAG_MS_ADPCM: u16 = 0x0002;
/// `WAVE_FORMAT_IEEE_FLOAT`.
const TAG_FLOAT: u16 = 0x0003;
/// `WAVE_FORMAT_ALAW`.
const TAG_ALAW: u16 = 0x0006;
/// `WAVE_FORMAT_MULAW`.
const TAG_MULAW: u16 = 0x0007;
/// `WAVE_FORMAT_DVI_ADPCM` (IMA ADPCM).
const TAG_IMA_ADPCM: u16 = 0x0011;
/// `WAVE_FORMAT_MPEG` (MPEG-1 Layer I and II).
const TAG_MPEG: u16 = 0x0050;
/// `WAVE_FORMAT_MPEGLAYER3`.
const TAG_MP3: u16 = 0x0055;
/// AAC (RFC 2361 registry).
const TAG_AAC: u16 = 0x00FF;
/// Opus.
const TAG_OPUS: u16 = 0x704F;
/// FLAC.
const TAG_FLAC: u16 = 0xF1AC;
/// `WAVE_FORMAT_EXTENSIBLE`.
const TAG_EXTENSIBLE: u16 = 0xFFFE;

/// Length of `WAVEFORMAT` plus `wBitsPerSample`.
const FMT_MIN_LEN: usize = 16;
/// Offset of the extension after `cbSize`.
const EXTENSION_OFFSET: usize = 18;
/// Length of a `WAVEFORMATEXTENSIBLE` and of its extension.
const EXTENSIBLE_LEN: usize = 40;
const EXTENSIBLE_CB_SIZE: u16 = 22;
/// Offset of `nCodecDelay` in `MPEGLAYER3WAVEFORMAT`, and the `cbSize` it
/// needs.
const MP3_DELAY_OFFSET: usize = 28;
const MP3_CB_SIZE: u16 = 12;

/// Largest sample rate accepted.
const MAX_SAMPLE_RATE: u32 = 7_680_000;
/// Speaker bits defined by `dwChannelMask`; higher bits (such as
/// `SPEAKER_ALL`) are dropped.
const SPEAKER_BITS: u32 = 0x0003_FFFF;

/// Bytes 2..16 of `KSDATAFORMAT_SUBTYPE_*` GUIDs derived from a format tag
/// (`xxxxxxxx-0000-0010-8000-00AA00389B71`).
const SUBTYPE_WAVEFORMATEX: [u8; 14] = [
    0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xAA, 0x00, 0x38, 0x9B, 0x71,
];
/// Bytes 2..16 of the Ambisonic B-Format GUIDs
/// (`0000000x-0721-11D3-8644-C8C1CA000000`).
const SUBTYPE_AMBISONIC: [u8; 14] = [
    0x00, 0x00, 0x21, 0x07, 0xD3, 0x11, 0x86, 0x44, 0xC8, 0xC1, 0xCA, 0x00, 0x00, 0x00,
];

/// How the demuxer cuts the `data` chunk into packets.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) enum Layout {
    /// Fixed-size frames of `frame_bytes` bytes.
    Pcm { frame_bytes: usize },
    /// Independent ADPCM blocks.
    Adpcm(BlockLayout),
    /// A compressed stream without fixed frames, passed on as it is.
    Stream,
}

/// A parsed `fmt ` chunk.
#[derive(Clone, Debug)]
pub(crate) struct Format {
    pub params: CodecParams,
    pub layout: Layout,
}

/// Reads a `u16` or `u32` at a fixed offset of a chunk that has been checked
/// to be long enough.
struct Fields<'a> {
    bytes: &'a [u8],
    big_endian: bool,
}

impl Fields<'_> {
    fn u16(&self, at: usize) -> u16 {
        let b = [self.bytes[at], self.bytes[at + 1]];
        if self.big_endian {
            u16::from_be_bytes(b)
        } else {
            u16::from_le_bytes(b)
        }
    }

    fn u32(&self, at: usize) -> u32 {
        let b = [
            self.bytes[at],
            self.bytes[at + 1],
            self.bytes[at + 2],
            self.bytes[at + 3],
        ];
        if self.big_endian {
            u32::from_be_bytes(b)
        } else {
            u32::from_le_bytes(b)
        }
    }
}

/// Parses the payload of a `fmt ` chunk.
pub(crate) fn parse(fmt: &[u8], big_endian: bool) -> Result<Format, Error> {
    let invalid = || Error::from(ErrorKind::InvalidHeader);
    if fmt.len() < FMT_MIN_LEN {
        return Err(invalid());
    }
    let f = Fields {
        bytes: fmt,
        big_endian,
    };
    let tag = f.u16(0);
    let channels = f.u16(2);
    let sample_rate = f.u32(4);
    let block_align = f.u16(12);
    let bits = f.u16(14);
    // Divisors are checked before any division.
    if !(1..=64).contains(&channels)
        || !(1..=MAX_SAMPLE_RATE).contains(&sample_rate)
        || block_align == 0
    {
        return Err(invalid());
    }
    let channels = channels as u8;
    let cb_size = if fmt.len() >= EXTENSION_OFFSET {
        f.u16(16)
    } else {
        0
    };
    let extension = &fmt[fmt.len().min(EXTENSION_OFFSET)..];
    let extension = &extension[..extension.len().min(usize::from(cb_size))];

    let (tag, valid_bits, channel_mask, samples_per_block) = if tag == TAG_EXTENSIBLE {
        if big_endian {
            return Err(ErrorKind::UnsupportedCodec.into());
        }
        if fmt.len() < EXTENSIBLE_LEN || cb_size < EXTENSIBLE_CB_SIZE {
            return Err(invalid());
        }
        let samples = f.u16(18);
        let mask = f.u32(20) & SPEAKER_BITS;
        let guid = &fmt[24..40];
        let base = u16::from_le_bytes([guid[0], guid[1]]);
        // All 16 bytes are checked: a vendor GUID that starts with a known
        // tag is not that format.
        let known = guid[2..] == SUBTYPE_WAVEFORMATEX
            || (guid[2..] == SUBTYPE_AMBISONIC && matches!(base, TAG_PCM | TAG_FLOAT));
        if !known {
            return Err(ErrorKind::UnsupportedCodec.into());
        }
        let tag = base;
        let valid = match samples {
            0 => bits,
            v if v > bits && matches!(tag, TAG_PCM | TAG_FLOAT) => return Err(invalid()),
            v => v,
        };
        // A mask may name fewer speakers than channels (the rest are
        // unlabeled), but not more.
        let mask = if mask.count_ones() <= u32::from(channels) {
            ChannelMask::from_raw(mask)
        } else {
            ChannelMask::EMPTY
        };
        (tag, valid, mask, Some(samples))
    } else {
        let mask = match channels {
            1 => ChannelMask::MONO,
            2 => ChannelMask::STEREO,
            _ => ChannelMask::EMPTY,
        };
        (tag, bits, mask, None)
    };

    let mut params = CodecParams {
        codec: CodecId::PcmS16Le,
        sample_rate,
        channels,
        channel_mask,
        bits_per_sample: 0,
        block_align,
        delay: 0,
        padding: 0,
        total_frames: None,
        extra_data: Vec::new(),
    };
    let fixed_width = matches!(
        tag,
        TAG_PCM | TAG_FLOAT | TAG_ALAW | TAG_MULAW | TAG_IMA_ADPCM | TAG_MS_ADPCM
    );
    if fixed_width && bits == 0 {
        return Err(invalid());
    }
    if big_endian && !matches!(tag, TAG_PCM | TAG_FLOAT | TAG_ALAW | TAG_MULAW) {
        return Err(ErrorKind::UnsupportedCodec.into());
    }
    let layout = match tag {
        TAG_PCM => {
            let (codec, width) = pcm_codec(bits, block_align, channels, big_endian)?;
            params.codec = codec;
            params.bits_per_sample = valid_bits.min(width as u16 * 8).min(32) as u8;
            Layout::Pcm {
                frame_bytes: width * usize::from(channels),
            }
        }
        TAG_FLOAT => {
            let (codec, width) = match (bits, big_endian) {
                (32, false) => (CodecId::PcmF32Le, 4),
                (32, true) => (CodecId::PcmF32Be, 4),
                (64, false) => (CodecId::PcmF64Le, 8),
                (64, true) => (CodecId::PcmF64Be, 8),
                _ => return Err(ErrorKind::UnsupportedFeature.into()),
            };
            params.codec = codec;
            params.bits_per_sample = bits as u8;
            Layout::Pcm {
                frame_bytes: width * usize::from(channels),
            }
        }
        TAG_ALAW | TAG_MULAW => {
            // One byte per sample on disk, also when `wBitsPerSample` gives
            // the decoded width of 16.
            let ch = u16::from(channels);
            if block_align != ch && block_align != 2 * ch {
                return Err(invalid());
            }
            params.codec = if tag == TAG_ALAW {
                CodecId::G711Alaw
            } else {
                CodecId::G711Mulaw
            };
            params.bits_per_sample = 16;
            params.block_align = ch;
            Layout::Pcm {
                frame_bytes: usize::from(channels),
            }
        }
        TAG_IMA_ADPCM | TAG_MS_ADPCM => {
            if bits != 4 {
                return Err(ErrorKind::UnsupportedFeature.into());
            }
            params.bits_per_sample = 16;
            if tag == TAG_IMA_ADPCM {
                params.codec = CodecId::AdpcmImaWav;
                // Samples per block from the extensible header or the
                // `IMAADPCMWAVEFORMAT` extension; computed if absent or 0.
                let declared = samples_per_block
                    .or_else(|| extension.first_chunk::<2>().map(|b| u16::from_le_bytes(*b)));
                params.extra_data = match declared {
                    Some(spb) => spb.to_le_bytes().to_vec(),
                    None => Vec::new(),
                };
                let layout = BlockLayout::ima(&params)?;
                // Store the samples per block also when computed.
                params.extra_data = u16::try_from(layout.samples_per_block)
                    .map_err(|_| invalid())?
                    .to_le_bytes()
                    .to_vec();
                Layout::Adpcm(layout)
            } else {
                params.codec = CodecId::AdpcmMs;
                params.extra_data = match samples_per_block {
                    Some(spb) => adpcm_ms::ms_standard_extension(spb),
                    None => extension.to_vec(),
                };
                Layout::Adpcm(adpcm_ms::layout(&params)?.0)
            }
        }
        TAG_MPEG | TAG_MP3 | TAG_AAC | TAG_OPUS | TAG_FLAC => {
            params.codec = match tag {
                TAG_MPEG => CodecId::Mp2,
                TAG_MP3 => CodecId::Mp3,
                TAG_AAC => CodecId::Aac,
                TAG_OPUS => CodecId::Opus,
                _ => CodecId::Flac,
            };
            if tag == TAG_FLAC {
                params.bits_per_sample = u8::try_from(bits).map_err(|_| invalid())?;
            }
            if tag == TAG_MP3 && cb_size >= MP3_CB_SIZE && fmt.len() >= MP3_DELAY_OFFSET + 2 {
                params.delay = u32::from(f.u16(MP3_DELAY_OFFSET));
            }
            params.extra_data = extension.to_vec();
            Layout::Stream
        }
        _ => return Err(ErrorKind::UnsupportedCodec.into()),
    };
    Ok(Format { params, layout })
}

/// Codec and container bytes of integer PCM.
///
/// A width that is a multiple of 8 gives the container size. Otherwise (for
/// example 12 or 20 bits) the block alignment gives it if it is a plausible
/// container (1 to 4 bytes per sample holding the width), else the width is
/// rounded up to whole bytes.
fn pcm_codec(
    bits: u16,
    block_align: u16,
    channels: u8,
    big_endian: bool,
) -> Result<(CodecId, usize), Error> {
    let ch = u16::from(channels);
    let per_sample = block_align / ch;
    let width = if bits.is_multiple_of(8) {
        bits / 8
    } else if block_align.is_multiple_of(ch)
        && (1..=4).contains(&per_sample)
        && per_sample * 8 >= bits
    {
        per_sample
    } else {
        bits.div_ceil(8)
    };
    let codec = match (width, big_endian) {
        (1, _) => CodecId::PcmU8,
        (2, false) => CodecId::PcmS16Le,
        (2, true) => CodecId::PcmS16Be,
        (3, false) => CodecId::PcmS24Le,
        (3, true) => CodecId::PcmS24Be,
        (4, false) => CodecId::PcmS32Le,
        (4, true) => CodecId::PcmS32Be,
        (8, false) => CodecId::PcmS64Le,
        (8, true) => CodecId::PcmS64Be,
        _ => return Err(ErrorKind::UnsupportedFeature.into()),
    };
    Ok((codec, usize::from(width)))
}

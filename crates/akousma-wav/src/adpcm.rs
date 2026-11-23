//! Block decoder of IMA ADPCM (`0x0011`) and the block layout it shares
//! with Microsoft ADPCM (`0x0002`).
//!
//! Both codecs store 4-bit codes in independent blocks of
//! [`CodecParams::block_align`] bytes. Packets may contain several blocks,
//! and decoding can start at a block boundary. A final short block, when
//! present, yields only the frames represented by its available bytes.
//!
//! | | IMA ADPCM | Microsoft ADPCM |
//! | :--- | :--- | :--- |
//! | Channels | `1..=64` | `1..=2` |
//! | Block header | per channel: predictor `i16`, step index `u8`, reserved `u8` | per field across channels: predictor index `u8`, delta `i16`, sample 1 `i16`, sample 2 `i16` |
//! | First output frames | the predictor | sample 2, then sample 1 |
//! | Body | 4-byte words per channel in turn, low nibble first | one nibble per channel in turn, high nibble first |
//! | `extra_data` | samples per block (`u16` LE), optional | `ADPCMWAVEFORMAT` extension: samples per block, coefficient count, coefficient pairs (`i16` LE) |

use akousma_core::audio::{AudioBuf, AudioPlanes};
use akousma_core::codec::{CodecId, CodecParams, Decoder, Packet};
use akousma_core::{DecodeOptions, Error, ErrorKind};

/// Step sizes of the IMA ADPCM quantizer. The decoder reads [`IMA_DIFFS`],
/// which is built from these steps; nothing else uses them.
const IMA_STEPS: [i32; 89] = [
    7, 8, 9, 10, 11, 12, 13, 14, 16, 17, 19, 21, 23, 25, 28, 31, 34, 37, 41, 45, 50, 55, 60, 66,
    73, 80, 88, 97, 107, 118, 130, 143, 157, 173, 190, 209, 230, 253, 279, 307, 337, 371, 408, 449,
    494, 544, 598, 658, 724, 796, 876, 963, 1060, 1166, 1282, 1411, 1552, 1707, 1878, 2066, 2272,
    2499, 2749, 3024, 3327, 3660, 4026, 4428, 4871, 5358, 5894, 6484, 7132, 7845, 8630, 9493,
    10442, 11487, 12635, 13899, 15289, 16818, 18500, 20350, 22385, 24623, 27086, 29794, 32767,
];
/// Largest IMA ADPCM step index.
const IMA_MAX_INDEX: u8 = 88;
/// Codes per step index: 3 magnitude bits and a sign bit.
const IMA_CODES: usize = 16;
/// Signed difference of every code at every step index, at
/// `index * 16 + code`. `i32`: magnitudes reach 61 436 (step 32 767, code 7).
const IMA_DIFFS: [i32; IMA_STEPS.len() * IMA_CODES] = ima_diffs();
/// Change of the step index for each nibble.
const IMA_INDEX_CHANGE: [i32; 16] = [-1, -1, -1, -1, 2, 4, 6, 8, -1, -1, -1, -1, 2, 4, 6, 8];
/// Header bytes per channel of an IMA ADPCM block.
const IMA_HEADER: usize = 4;
/// Samples per 4-byte word of an IMA ADPCM block body.
const IMA_WORD_SAMPLES: usize = 8;
/// Header bytes per channel of a Microsoft ADPCM block.
pub(crate) const MS_HEADER: usize = 7;

/// Builds [`IMA_DIFFS`] with the shift-add of the IMA recommendation
/// (Revision 3.00, Appendix D, section 6.2): each term is truncated on its
/// own.
const fn ima_diffs() -> [i32; IMA_STEPS.len() * IMA_CODES] {
    let mut diffs = [0; IMA_STEPS.len() * IMA_CODES];
    let mut i = 0;
    while i < diffs.len() {
        let (step, code) = (IMA_STEPS[i / IMA_CODES], i % IMA_CODES);
        let mut diff = step >> 3;
        if code & 4 != 0 {
            diff += step;
        }
        if code & 2 != 0 {
            diff += step >> 1;
        }
        if code & 1 != 0 {
            diff += step >> 2;
        }
        diffs[i] = if code & 8 != 0 { -diff } else { diff };
        i += 1;
    }
    diffs
}

/// Geometry of the blocks of an ADPCM stream.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub(crate) struct BlockLayout {
    pub block_align: usize,
    pub channels: usize,
    pub samples_per_block: u32,
    pub ima: bool,
}

impl BlockLayout {
    /// Validates the parameters of an IMA ADPCM stream.
    pub(crate) fn ima(params: &CodecParams) -> Result<Self, Error> {
        let channels = usize::from(params.channels);
        let block_align = usize::from(params.block_align);
        if !(1..=64).contains(&channels) || block_align < IMA_HEADER * channels {
            return Err(ErrorKind::InvalidHeader.into());
        }
        let body = block_align - IMA_HEADER * channels;
        // Mono bodies may end in a partial word; with more channels only
        // whole groups of one word per channel hold a frame of every channel.
        let max = if channels == 1 {
            1 + body * 2
        } else {
            1 + body / (IMA_HEADER * channels) * IMA_WORD_SAMPLES
        };
        let declared = match params.extra_data.first_chunk::<2>() {
            Some(&spb) if u16::from_le_bytes(spb) != 0 => usize::from(u16::from_le_bytes(spb)),
            _ => body * 2 / channels + 1,
        };
        Self::checked(block_align, channels, declared, max, true)
    }

    /// Checks the samples per block against the largest number `max` a
    /// block holds.
    pub(crate) fn checked(
        block_align: usize,
        channels: usize,
        spb: usize,
        max: usize,
        ima: bool,
    ) -> Result<Self, Error> {
        if spb == 0 || spb > max {
            return Err(ErrorKind::InvalidHeader.into());
        }
        Ok(Self {
            block_align,
            channels,
            samples_per_block: u32::try_from(spb).map_err(|_| ErrorKind::InvalidHeader)?,
            ima,
        })
    }

    /// Frames decoded from a block of `bytes` bytes (the last block of a
    /// stream may be short).
    pub(crate) fn frames_in(&self, bytes: usize) -> u32 {
        let spb = self.samples_per_block;
        if bytes >= self.block_align {
            return spb;
        }
        let ch = self.channels;
        let frames = if self.ima {
            match bytes.checked_sub(IMA_HEADER * ch) {
                None => 0,
                Some(body) if ch == 1 => 1 + body * 2,
                Some(body) => 1 + body / (IMA_HEADER * ch) * IMA_WORD_SAMPLES,
            }
        } else {
            match bytes.checked_sub(MS_HEADER * ch) {
                None => 0,
                Some(_) if spb == 1 => 1,
                Some(body) => 2 + body * 2 / ch,
            }
        };
        // `frames` is below `samples_per_block` or saturates to it.
        u32::try_from(frames).map_or(spb, |frames| frames.min(spb))
    }

    /// Frames decoded from `bytes` bytes of consecutive blocks.
    pub(crate) fn frames_in_run(&self, bytes: u64) -> Option<u64> {
        let align = self.block_align as u64;
        let full = bytes / align;
        let rest = usize::try_from(bytes % align).ok()?;
        full.checked_mul(u64::from(self.samples_per_block))?
            .checked_add(u64::from(self.frames_in(rest)))
    }

    /// Total frames of a packet, checked against the buffer limits.
    pub(crate) fn packet_frames(&self, data: &[u8]) -> Result<usize, Error> {
        let frames = self
            .frames_in_run(data.len() as u64)
            .ok_or(ErrorKind::LimitExceeded)?;
        usize::try_from(frames).map_err(|_| ErrorKind::LimitExceeded.into())
    }
}

/// Makes `out` an empty 16-bit buffer of the stream's layout, unless it
/// already is one.
pub(crate) fn prepare(params: &CodecParams, out: &mut AudioBuf) -> Result<(), Error> {
    const BITS: u8 = 16;
    let matches = out.plane::<i16>(0).is_some()
        && out.channels() == params.channels
        && out.sample_rate() == params.sample_rate
        && out.channel_mask() == params.channel_mask.fit(params.channels)
        && out.bits_per_sample() == BITS;
    if !matches {
        let planes = (0..params.channels).map(|_| Vec::new()).collect();
        *out = AudioBuf::new(
            AudioPlanes::I16(planes),
            params.sample_rate,
            params.channel_mask,
            BITS,
        )?;
    }
    Ok(())
}

/// Decoder of IMA ADPCM as stored in WAV files ([`CodecId::AdpcmImaWav`]).
///
/// Output is 16-bit; `out` is reshaped as described for
/// [`PcmDecoder`](akousma_core::pcm::PcmDecoder).
///
/// ```
/// use akousma_core::audio::AudioBuf;
/// use akousma_core::channel::ChannelMask;
/// use akousma_core::codec::{CodecId, CodecParams, Decoder, Packet};
/// use akousma_core::DecodeOptions;
/// use akousma_wav::AdpcmImaDecoder;
///
/// let params = CodecParams {
///     codec: CodecId::AdpcmImaWav,
///     sample_rate: 8_000,
///     channels: 1,
///     channel_mask: ChannelMask::MONO,
///     bits_per_sample: 16,
///     block_align: 8,
///     delay: 0,
///     padding: 0,
///     total_frames: None,
///     extra_data: Vec::new(),
/// };
/// let mut decoder = AdpcmImaDecoder::new(&params, &DecodeOptions::new())?;
/// // Predictor 100, step index 0 (step 7), then nibbles 0x7 and 0x8.
/// let block = vec![100, 0, 0, 0, 0x87, 0, 0, 0];
/// let mut buf = AudioBuf::i16(1, 8_000, ChannelMask::MONO)?;
/// decoder.decode(&Packet { data: block, ..Packet::default() }, &mut buf)?;
/// // 0x7: diff = 0 + 1 + 3 + 7 = 11; the step index rises to 8 (step 16).
/// // 0x8: diff = -(16 >> 3) = -2.
/// assert_eq!(&buf.plane::<i16>(0).unwrap()[..3], &[100, 111, 109]);
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct AdpcmImaDecoder {
    params: CodecParams,
    layout: BlockLayout,
    apply_trim: bool,
}

impl AdpcmImaDecoder {
    /// Creates a decoder for `params`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnsupportedCodec`] for another codec;
    /// [`ErrorKind::InvalidHeader`] if the channel count is not in `1..=64`,
    /// a block is shorter than its header, or the samples per block in
    /// `extra_data` do not fit in a block.
    pub fn new(params: &CodecParams, options: &DecodeOptions) -> Result<Self, Error> {
        if params.codec != CodecId::AdpcmImaWav {
            return Err(ErrorKind::UnsupportedCodec.into());
        }
        Ok(Self {
            params: params.clone(),
            layout: BlockLayout::ima(params)?,
            apply_trim: options.apply_trim,
        })
    }

    /// Creates a boxed decoder for a [`CodecRegistry`](akousma_core::CodecRegistry).
    ///
    /// # Errors
    ///
    /// Same as [`AdpcmImaDecoder::new`].
    pub fn boxed(params: &CodecParams, options: &DecodeOptions) -> Result<Box<dyn Decoder>, Error> {
        Ok(Box::new(Self::new(params, options)?))
    }
}

impl Decoder for AdpcmImaDecoder {
    fn params(&self) -> &CodecParams {
        &self.params
    }

    /// Decodes every block of `packet`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidData`] for a step index above 88 in a block
    /// header.
    fn decode(&mut self, packet: &Packet, out: &mut AudioBuf) -> Result<(), Error> {
        prepare(&self.params, out)?;
        let layout = self.layout;
        out.reset_window(layout.packet_frames(&packet.data)?)?;
        let planes = out.planes_mut::<i16>().ok_or(ErrorKind::InvalidData)?;
        for (channel, plane) in planes.enumerate() {
            let mut pos = 0;
            for block in packet.data.chunks(layout.block_align) {
                let frames = layout.frames_in(block.len()) as usize;
                if frames > 0 {
                    ima_channel(
                        block,
                        channel,
                        layout.channels,
                        &mut plane[pos..pos + frames],
                    )?;
                    pos += frames;
                }
            }
        }
        if self.apply_trim {
            out.apply_trim(packet.trim_start, packet.trim_end);
        }
        Ok(())
    }

    fn reset(&mut self) {}
}

/// Decodes channel `channel` of one IMA ADPCM block into `out`, which
/// holds the frames the block covers.
fn ima_channel(
    block: &[u8],
    channel: usize,
    channels: usize,
    out: &mut [i16],
) -> Result<(), Error> {
    let [lo, hi, index, _reserved] = block.as_chunks::<IMA_HEADER>().0[channel];
    if index > IMA_MAX_INDEX {
        return Err(ErrorKind::InvalidData.into());
    }
    let (first, rest) = out.split_first_mut().ok_or(ErrorKind::InvalidData)?;
    *first = i16::from_le_bytes([lo, hi]);
    let mut predictor = i32::from(*first);
    let mut index = i32::from(index);
    let mut rest = rest.iter_mut();
    let group = IMA_HEADER * channels;
    'words: for words in block[group..].chunks(group) {
        // A short last group of a mono block holds part of a word.
        let Some(word) = words.get(IMA_HEADER * channel..) else {
            break;
        };
        for &byte in word.iter().take(IMA_HEADER) {
            for nibble in [byte & 0x0F, byte >> 4] {
                let Some(sample) = rest.next() else {
                    break 'words;
                };
                let diff = IMA_DIFFS[index as usize * IMA_CODES + usize::from(nibble)];
                predictor = (predictor + diff).clamp(i16::MIN.into(), i16::MAX.into());
                index =
                    (index + IMA_INDEX_CHANGE[usize::from(nibble)]).clamp(0, IMA_MAX_INDEX.into());
                *sample = predictor as i16;
            }
        }
    }
    // `frames_in` counts only frames whose nibbles are in the block.
    if rest.len() != 0 {
        return Err(ErrorKind::CorruptFrame.into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ima_diffs_follow_the_recommendation() {
        // Every entry against the arithmetic of the IMA recommendation
        // (Revision 3.00, Appendix D, section 6.2), evaluated at run time.
        assert_eq!(IMA_DIFFS.len(), 89 * 16);
        for (i, &entry) in IMA_DIFFS.iter().enumerate() {
            let (step, code) = (IMA_STEPS[i / 16], i % 16);
            let mut difference = 0;
            if code & 4 != 0 {
                difference += step;
            }
            if code & 2 != 0 {
                difference += step >> 1;
            }
            if code & 1 != 0 {
                difference += step >> 2;
            }
            difference += step >> 3;
            if code & 8 != 0 {
                difference = -difference;
            }
            assert_eq!(entry, difference, "step index {}, code {code}", i / 16);
        }
    }
}

//! Block decoder of Microsoft ADPCM (`0x0002`).

use akousma_core::audio::AudioBuf;
use akousma_core::codec::{CodecId, CodecParams, Decoder, Packet};
use akousma_core::{DecodeOptions, Error, ErrorKind};

use crate::adpcm::{BlockLayout, MS_HEADER, prepare};

/// Adaptation factors of the Microsoft ADPCM delta, in 1/256.
const MS_ADAPTATION: [i64; 16] = [
    230, 230, 230, 230, 307, 409, 512, 614, 768, 614, 512, 409, 307, 230, 230, 230,
];
/// The seven predictor coefficient pairs of Microsoft ADPCM, in 1/256.
const MS_DEFAULT_COEFFS: [[i64; 2]; 7] = [
    [256, 0],
    [512, -256],
    [0, 0],
    [192, 64],
    [240, 0],
    [460, -208],
    [392, -232],
];
/// Largest number of coefficient pairs accepted in the format extension.
const MS_MAX_COEFFS: usize = 256;
/// Smallest and largest Microsoft ADPCM delta after adaptation.
const MS_MIN_DELTA: i64 = 16;
const MS_MAX_DELTA: i64 = 0x7FFF_FFFF;

/// Length of the fixed part of the `ADPCMWAVEFORMAT` extension: samples
/// per block and number of coefficient pairs.
const MS_EXTENSION_HEADER: usize = 4;
/// Length of one coefficient pair.
const MS_COEFF_LEN: usize = 4;

/// Validates the parameters of a Microsoft ADPCM stream and returns its
/// block layout and coefficient pairs.
///
/// `extra_data` is the `ADPCMWAVEFORMAT` extension, which must hold the
/// samples per block (1 or more, fitting in a block), the number of pairs
/// (1 to 256) and all the pairs; bytes after them are ignored. Nothing is
/// substituted for a missing or damaged extension. (In
/// `WAVEFORMATEXTENSIBLE`, which has no room for a table, the `fmt ` reader
/// builds the extension from the standard pairs; see
/// [`ms_standard_extension`].)
pub(crate) fn layout(params: &CodecParams) -> Result<(BlockLayout, Vec<[i64; 2]>), Error> {
    let channels = usize::from(params.channels);
    let block_align = usize::from(params.block_align);
    if !(1..=2).contains(&channels) {
        return Err(ErrorKind::UnsupportedFeature.into());
    }
    if block_align < MS_HEADER * channels {
        return Err(ErrorKind::InvalidHeader.into());
    }
    let max = 2 + (block_align - MS_HEADER * channels) * 2 / channels;
    let extra = &params.extra_data;
    let invalid = || Error::from(ErrorKind::InvalidHeader);
    let &[spb_lo, spb_hi, count_lo, count_hi] = extra
        .first_chunk::<MS_EXTENSION_HEADER>()
        .ok_or_else(invalid)?;
    let samples_per_block = usize::from(u16::from_le_bytes([spb_lo, spb_hi]));
    let count = usize::from(u16::from_le_bytes([count_lo, count_hi]));
    if !(1..=MS_MAX_COEFFS).contains(&count) {
        return Err(invalid());
    }
    let table = extra
        .get(MS_EXTENSION_HEADER..MS_EXTENSION_HEADER + MS_COEFF_LEN * count)
        .ok_or_else(invalid)?;
    let coeffs = table
        .as_chunks::<MS_COEFF_LEN>()
        .0
        .iter()
        .map(|&[a, b, c, d]| {
            [
                i64::from(i16::from_le_bytes([a, b])),
                i64::from(i16::from_le_bytes([c, d])),
            ]
        })
        .collect();
    let layout = BlockLayout::checked(block_align, channels, samples_per_block, max, false)?;
    Ok((layout, coeffs))
}

/// `ADPCMWAVEFORMAT` extension with the seven standard coefficient pairs,
/// for Microsoft ADPCM in `WAVEFORMATEXTENSIBLE`.
///
/// `WAVEFORMATEXTENSIBLE` carries the samples per block in place of the
/// valid bits and has no room for a coefficient table, so the standard
/// pairs, which every encoder writes, are the only ones it can describe.
/// This is the only case in which a table is supplied: a plain
/// `ADPCMWAVEFORMAT` must carry its own table, and a missing or empty one is
/// an invalid header rather than a reason to substitute the standard pairs.
pub(crate) fn ms_standard_extension(samples_per_block: u16) -> Vec<u8> {
    let mut extension = samples_per_block.to_le_bytes().to_vec();
    extension.extend_from_slice(&(MS_DEFAULT_COEFFS.len() as u16).to_le_bytes());
    for coeff in MS_DEFAULT_COEFFS.as_flattened() {
        // The standard coefficients fit in `i16`.
        extension.extend_from_slice(&(*coeff as i16).to_le_bytes());
    }
    extension
}

/// Decoder of Microsoft ADPCM ([`CodecId::AdpcmMs`]), mono or stereo.
///
/// Coefficient pairs and samples per block come from the `ADPCMWAVEFORMAT`
/// extension in `extra_data`, which is required. Output is 16-bit; `out` is
/// reshaped as described for
/// [`PcmDecoder`](akousma_core::pcm::PcmDecoder).
///
/// ```
/// use akousma_core::audio::AudioBuf;
/// use akousma_core::channel::ChannelMask;
/// use akousma_core::codec::{CodecId, CodecParams, Decoder, Packet};
/// use akousma_core::DecodeOptions;
/// use akousma_wav::AdpcmMsDecoder;
///
/// let params = CodecParams {
///     codec: CodecId::AdpcmMs,
///     sample_rate: 8_000,
///     channels: 1,
///     channel_mask: ChannelMask::MONO,
///     bits_per_sample: 16,
///     block_align: 8,
///     delay: 0,
///     padding: 0,
///     total_frames: None,
///     // `ADPCMWAVEFORMAT`: 4 samples per block, 1 coefficient pair (256, 0).
///     extra_data: vec![4, 0, 1, 0, 0, 1, 0, 0],
/// };
/// let mut decoder = AdpcmMsDecoder::new(&params, &DecodeOptions::new())?;
/// // Predictor 0, delta 16, sample 1 = 200, sample 2 = 100, nibbles 0x1 and 0xF.
/// let block = vec![0, 16, 0, 200, 0, 100, 0, 0x1F];
/// let mut buf = AudioBuf::i16(1, 8_000, ChannelMask::MONO)?;
/// decoder.decode(&Packet { data: block, ..Packet::default() }, &mut buf)?;
/// // 200 * 256 / 256 = 200, plus 1 * 16; the delta stays at 16 (230 * 16 >> 8 = 14, raised to 16).
/// // 216 * 256 / 256 = 216, plus -1 * 16.
/// assert_eq!(buf.plane::<i16>(0), Some(&[100, 200, 216, 200][..]));
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct AdpcmMsDecoder {
    params: CodecParams,
    layout: BlockLayout,
    coeffs: Vec<[i64; 2]>,
    apply_trim: bool,
}

impl AdpcmMsDecoder {
    /// Creates a decoder for `params`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnsupportedCodec`] for another codec;
    /// [`ErrorKind::UnsupportedFeature`] for more than two channels (no
    /// specification defines their block layout); [`ErrorKind::InvalidHeader`]
    /// if a block is shorter than its header, the extension is missing or
    /// shorter than 4 bytes, declares 0 or more than 256 coefficient pairs or
    /// holds fewer pairs than it declares, or the samples per block are 0 or
    /// do not fit in a block.
    pub fn new(params: &CodecParams, options: &DecodeOptions) -> Result<Self, Error> {
        if params.codec != CodecId::AdpcmMs {
            return Err(ErrorKind::UnsupportedCodec.into());
        }
        let (layout, coeffs) = layout(params)?;
        Ok(Self {
            params: params.clone(),
            layout,
            coeffs,
            apply_trim: options.apply_trim,
        })
    }

    /// Creates a boxed decoder for a [`CodecRegistry`](akousma_core::CodecRegistry).
    ///
    /// # Errors
    ///
    /// Same as [`AdpcmMsDecoder::new`].
    pub fn boxed(params: &CodecParams, options: &DecodeOptions) -> Result<Box<dyn Decoder>, Error> {
        Ok(Box::new(Self::new(params, options)?))
    }
}

impl Decoder for AdpcmMsDecoder {
    fn params(&self) -> &CodecParams {
        &self.params
    }

    /// Decodes every block of `packet`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidData`] for a predictor index outside the
    /// coefficient table.
    fn decode(&mut self, packet: &Packet, out: &mut AudioBuf) -> Result<(), Error> {
        prepare(&self.params, out)?;
        let layout = self.layout;
        out.reset_window(layout.packet_frames(&packet.data)?)?;
        let mut planes = out.planes_mut::<i16>().ok_or(ErrorKind::InvalidData)?;
        let first = planes.next().ok_or(ErrorKind::InvalidData)?;
        let mut second = planes.next();
        let mut pos = 0;
        for block in packet.data.chunks(layout.block_align) {
            let frames = layout.frames_in(block.len()) as usize;
            if frames == 0 {
                continue;
            }
            let range = pos..pos + frames;
            match second.as_deref_mut() {
                None => ms_block(block, &self.coeffs, [&mut first[range]])?,
                Some(right) => ms_block(
                    block,
                    &self.coeffs,
                    [&mut first[range.clone()], &mut right[range]],
                )?,
            }
            pos += frames;
        }
        drop(planes);
        if self.apply_trim {
            out.apply_trim(packet.trim_start, packet.trim_end);
        }
        Ok(())
    }

    fn reset(&mut self) {}
}

/// State of one Microsoft ADPCM channel.
struct MsChannel {
    coeffs: [i64; 2],
    delta: i64,
    sample1: i64,
    sample2: i64,
}

impl MsChannel {
    fn step(&mut self, nibble: u8) -> i16 {
        let signed = i64::from(((nibble << 4) as i8) >> 4);
        // A division by 256, which truncates toward zero, as the Multimedia
        // Data Standards Update (1994) writes it; a shift by 8 would round
        // negative predictions down. `i64` keeps the products of custom
        // coefficients and large deltas from overflowing.
        let predicted = (self.sample1 * self.coeffs[0] + self.sample2 * self.coeffs[1]) / 256;
        let sample = (predicted + signed * self.delta).clamp(i16::MIN.into(), i16::MAX.into());
        self.sample2 = self.sample1;
        self.sample1 = sample;
        self.delta = ((MS_ADAPTATION[usize::from(nibble)] * self.delta) >> 8)
            .clamp(MS_MIN_DELTA, MS_MAX_DELTA);
        sample as i16
    }
}

/// Decodes one Microsoft ADPCM block of `CH` channels into `outs`, which
/// hold the frames the block covers (at least one, so the block holds its
/// whole header).
fn ms_block<const CH: usize>(
    block: &[u8],
    coeffs: &[[i64; 2]],
    mut outs: [&mut [i16]; CH],
) -> Result<(), Error> {
    // The header groups each field across channels: `CH` predictor bytes,
    // then `CH` deltas, `CH` first samples and `CH` second samples.
    let field = |offset: usize| i64::from(i16::from_le_bytes([block[offset], block[offset + 1]]));
    let mut state: [MsChannel; CH] = core::array::from_fn(|c| MsChannel {
        coeffs: [0, 0],
        delta: field(CH + 2 * c),
        sample1: field(3 * CH + 2 * c),
        sample2: field(5 * CH + 2 * c),
    });
    for (c, channel) in state.iter_mut().enumerate() {
        channel.coeffs = *coeffs
            .get(usize::from(block[c]))
            .ok_or(ErrorKind::InvalidData)?;
    }
    let frames = outs[0].len();
    // Sample 2 is the earlier one.
    for (out, channel) in outs.iter_mut().zip(&state) {
        out[0] = channel.sample2 as i16;
        if frames > 1 {
            out[1] = channel.sample1 as i16;
        }
    }
    let mut nibbles = block[MS_HEADER * CH..]
        .iter()
        .flat_map(|&byte| [byte >> 4, byte & 0x0F]);
    for frame in 2..frames {
        for (out, channel) in outs.iter_mut().zip(state.iter_mut()) {
            let nibble = nibbles.next().ok_or(ErrorKind::CorruptFrame)?;
            out[frame] = channel.step(nibble);
        }
    }
    Ok(())
}

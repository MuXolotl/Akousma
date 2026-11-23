//! Decoder of uncompressed and companded PCM, shared by every container that
//! stores raw samples.
//!
//! | Codec | Bytes per sample | Planes | Conversion |
//! | :--- | :---: | :--- | :--- |
//! | [`PcmU8`](CodecId::PcmU8) | 1 | `I16` | offset binary: `(byte ^ 0x80) as i8 << 8` |
//! | [`PcmS16Le`](CodecId::PcmS16Le), [`PcmS16Be`](CodecId::PcmS16Be) | 2 | `I16` | unchanged |
//! | [`PcmS24Le`](CodecId::PcmS24Le), [`PcmS24Be`](CodecId::PcmS24Be) | 3 | `I32` | placed in the top 24 bits |
//! | [`PcmS32Le`](CodecId::PcmS32Le), [`PcmS32Be`](CodecId::PcmS32Be) | 4 | `I32` | unchanged |
//! | [`PcmS64Le`](CodecId::PcmS64Le), [`PcmS64Be`](CodecId::PcmS64Be) | 8 | `I32` | top 32 bits |
//! | [`PcmF32Le`](CodecId::PcmF32Le), [`PcmF32Be`](CodecId::PcmF32Be) | 4 | `F32` | `NaN` to `0.0`, `±inf` to `±1.0` |
//! | [`PcmF64Le`](CodecId::PcmF64Le), [`PcmF64Be`](CodecId::PcmF64Be) | 8 | `F32` | `as f32`, then as `PcmF32` |
//! | [`G711Alaw`](CodecId::G711Alaw), [`G711Mulaw`](CodecId::G711Mulaw) | 1 | `I16` | ITU-T G.711 expansion table |
//!
//! Integer PCM values narrower than the decoded container are expected to
//! be aligned to its most significant bit, as RIFF and AIFF store them, so
//! they need no shift. Float samples keep finite values outside
//! `[-1.0, 1.0]`; non-finite values are replaced before later processing.
//! A finite `f64` beyond the `f32` range becomes infinite on conversion and
//! is replaced in the same way.

use crate::audio::{AudioBuf, AudioPlanes, Sample};
use crate::codec::{CodecId, CodecParams, Decoder, Packet};
use crate::error::{Error, ErrorKind};
use crate::options::DecodeOptions;

/// Codecs decoded by [`PcmDecoder`].
pub const PCM_CODECS: &[CodecId] = &[
    CodecId::PcmU8,
    CodecId::PcmS16Le,
    CodecId::PcmS16Be,
    CodecId::PcmS24Le,
    CodecId::PcmS24Be,
    CodecId::PcmS32Le,
    CodecId::PcmS32Be,
    CodecId::PcmS64Le,
    CodecId::PcmS64Be,
    CodecId::PcmF32Le,
    CodecId::PcmF32Be,
    CodecId::PcmF64Le,
    CodecId::PcmF64Be,
    CodecId::G711Alaw,
    CodecId::G711Mulaw,
];

/// Bytes per sample of one channel for a codec of [`PCM_CODECS`], `None`
/// for other codecs.
///
/// ```
/// use akousma_core::codec::CodecId;
/// use akousma_core::pcm::sample_bytes;
///
/// assert_eq!(sample_bytes(CodecId::PcmS24Le), Some(3));
/// assert_eq!(sample_bytes(CodecId::G711Mulaw), Some(1));
/// assert_eq!(sample_bytes(CodecId::Flac), None);
/// ```
#[must_use]
pub const fn sample_bytes(codec: CodecId) -> Option<usize> {
    match codec {
        CodecId::PcmU8 | CodecId::G711Alaw | CodecId::G711Mulaw => Some(1),
        CodecId::PcmS16Le | CodecId::PcmS16Be => Some(2),
        CodecId::PcmS24Le | CodecId::PcmS24Be => Some(3),
        CodecId::PcmS32Le | CodecId::PcmS32Be | CodecId::PcmF32Le | CodecId::PcmF32Be => Some(4),
        CodecId::PcmS64Le | CodecId::PcmS64Be | CodecId::PcmF64Le | CodecId::PcmF64Be => Some(8),
        _ => None,
    }
}

/// ITU-T G.711 A-law expansion of one code word to 16 bits: even bits are
/// inverted (`^ 0x55`), bit 7 set means positive.
const fn alaw_to_i16(code: u8) -> i16 {
    let a = code ^ 0x55;
    let mantissa = ((a & 0x0F) as i32) << 4;
    let segment = (a & 0x70) >> 4;
    let magnitude = match segment {
        0 => mantissa + 8,
        _ => (mantissa + 0x108) << (segment - 1),
    };
    if a & 0x80 != 0 {
        magnitude as i16
    } else {
        -magnitude as i16
    }
}

/// ITU-T G.711 mu-law expansion of one code word to 16 bits: all bits are
/// inverted, the bias is 132 (`0x84`), bit 7 set (after inversion) means
/// negative.
const fn mulaw_to_i16(code: u8) -> i16 {
    const BIAS: i32 = 0x84;
    let u = !code;
    let magnitude = ((((u & 0x0F) as i32) << 3) + BIAS) << ((u & 0x70) >> 4);
    if u & 0x80 != 0 {
        (BIAS - magnitude) as i16
    } else {
        (magnitude - BIAS) as i16
    }
}

/// Expansion table of A-law (`mu_law == false`) or mu-law.
const fn expansion_table(mu_law: bool) -> [i16; 256] {
    let mut table = [0; 256];
    let mut code = 0;
    while code < 256 {
        table[code] = if mu_law {
            mulaw_to_i16(code as u8)
        } else {
            alaw_to_i16(code as u8)
        };
        code += 1;
    }
    table
}

/// G.711 A-law code word to 16-bit sample.
pub const ALAW_TO_I16: [i16; 256] = expansion_table(false);

/// G.711 mu-law code word to 16-bit sample.
pub const MULAW_TO_I16: [i16; 256] = expansion_table(true);

/// Replaces values that are not finite: `NaN` by `0.0`, `±inf` by `±1.0`.
#[inline]
fn sanitize(x: f32) -> f32 {
    if x.is_finite() {
        x
    } else if x.is_nan() {
        0.0
    } else {
        x.signum()
    }
}

/// Decoder of the codecs in [`PCM_CODECS`].
///
/// Each packet holds whole interleaved frames; the decoder writes them to
/// planes. If `out` does not have the sample type, channel count, sample
/// rate, channel mask and sample width of the stream, it is replaced by an
/// empty buffer that has them. The buffer's capacity is reused for packets
/// that fit; a larger packet may grow it.
///
/// ```
/// use akousma_core::audio::AudioBuf;
/// use akousma_core::channel::ChannelMask;
/// use akousma_core::codec::{CodecId, CodecParams, Decoder, Packet};
/// use akousma_core::pcm::PcmDecoder;
/// use akousma_core::DecodeOptions;
///
/// let params = CodecParams {
///     codec: CodecId::PcmS16Le,
///     sample_rate: 48_000,
///     channels: 2,
///     channel_mask: ChannelMask::STEREO,
///     bits_per_sample: 16,
///     block_align: 4,
///     delay: 0,
///     padding: 0,
///     total_frames: None,
///     extra_data: Vec::new(),
/// };
/// let mut decoder = PcmDecoder::new(&params, &DecodeOptions::new())?;
/// let packet = Packet { data: vec![0x01, 0x00, 0x00, 0x80, 0xFF, 0x7F, 0xFF, 0xFF], ..Packet::default() };
/// let mut buf = AudioBuf::i16(1, 8_000, ChannelMask::MONO)?;
/// decoder.decode(&packet, &mut buf)?;
/// assert_eq!(buf.plane::<i16>(0), Some(&[1, i16::MAX][..]));
/// assert_eq!(buf.plane::<i16>(1), Some(&[i16::MIN, -1][..]));
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct PcmDecoder {
    params: CodecParams,
    frame_bytes: usize,
    apply_trim: bool,
}

impl PcmDecoder {
    /// Creates a decoder for `params`.
    ///
    /// # Errors
    ///
    /// - [`ErrorKind::UnsupportedCodec`] if `params.codec` is not in
    ///   [`PCM_CODECS`].
    /// - [`ErrorKind::InvalidData`] if the channel count is not in `1..=64`,
    ///   the sample rate is zero, or `bits_per_sample` is zero or wider than
    ///   the decoded sample (8 for `PcmU8`, 16 for the other `I16` codecs,
    ///   32 for the `I32` codecs).
    pub fn new(params: &CodecParams, options: &DecodeOptions) -> Result<Self, Error> {
        let width = sample_bytes(params.codec).ok_or(ErrorKind::UnsupportedCodec)?;
        let max_bits = match params.codec {
            CodecId::PcmU8 => 8,
            CodecId::PcmF32Le | CodecId::PcmF32Be | CodecId::PcmF64Le | CodecId::PcmF64Be => 64,
            _ if width <= 2 => 16,
            _ => 32,
        };
        let valid = (1..=crate::audio::MAX_CHANNELS).contains(&params.channels)
            && params.sample_rate > 0
            && (1..=max_bits).contains(&params.bits_per_sample);
        if !valid {
            return Err(ErrorKind::InvalidData.into());
        }
        Ok(Self {
            params: params.clone(),
            frame_bytes: width * usize::from(params.channels),
            apply_trim: options.apply_trim,
        })
    }

    /// Creates a boxed decoder; a [`DecoderFactory`](crate::codec::DecoderFactory)
    /// for a [`CodecRegistry`](crate::codec::CodecRegistry).
    ///
    /// # Errors
    ///
    /// Same as [`PcmDecoder::new`].
    pub fn boxed(params: &CodecParams, options: &DecodeOptions) -> Result<Box<dyn Decoder>, Error> {
        Ok(Box::new(Self::new(params, options)?))
    }

    /// Makes `out` an empty buffer of the stream's layout with planes of
    /// `T`, unless it already is one.
    fn prepare<T: Sample>(
        &self,
        out: &mut AudioBuf,
        planes: fn(Vec<Vec<T>>) -> AudioPlanes,
    ) -> Result<(), Error> {
        let p = &self.params;
        let bits = p.bits_per_sample.min(32);
        let matches = out.plane::<T>(0).is_some()
            && out.channels() == p.channels
            && out.sample_rate() == p.sample_rate
            && out.channel_mask() == p.channel_mask.fit(p.channels)
            && out.bits_per_sample() == bits;
        if !matches {
            let empty = (0..p.channels).map(|_| Vec::new()).collect();
            *out = AudioBuf::new(planes(empty), p.sample_rate, p.channel_mask, bits)?;
        }
        Ok(())
    }
}

impl Decoder for PcmDecoder {
    fn params(&self) -> &CodecParams {
        &self.params
    }

    /// Decodes the whole frames of `packet`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::CorruptFrame`] if the packet does not hold a whole
    /// number of frames; [`ErrorKind::LimitExceeded`] if the planes cannot
    /// grow.
    fn decode(&mut self, packet: &Packet, out: &mut AudioBuf) -> Result<(), Error> {
        let data = &packet.data[..];
        if !data.len().is_multiple_of(self.frame_bytes) {
            return Err(ErrorKind::CorruptFrame.into());
        }
        let frames = data.len() / self.frame_bytes;
        match self.params.codec {
            CodecId::PcmU8 => {
                self.prepare(out, AudioPlanes::I16)?;
                write::<i16, 1, 2>(data, frames, out, |[b]| i16::from((b ^ 0x80) as i8) << 8)
            }
            CodecId::G711Alaw => {
                self.prepare(out, AudioPlanes::I16)?;
                write::<i16, 1, 2>(data, frames, out, |[b]| ALAW_TO_I16[usize::from(b)])
            }
            CodecId::G711Mulaw => {
                self.prepare(out, AudioPlanes::I16)?;
                write::<i16, 1, 2>(data, frames, out, |[b]| MULAW_TO_I16[usize::from(b)])
            }
            CodecId::PcmS16Le => {
                self.prepare(out, AudioPlanes::I16)?;
                write::<i16, 2, 4>(data, frames, out, i16::from_le_bytes)
            }
            CodecId::PcmS16Be => {
                self.prepare(out, AudioPlanes::I16)?;
                write::<i16, 2, 4>(data, frames, out, i16::from_be_bytes)
            }
            CodecId::PcmS24Le => {
                self.prepare(out, AudioPlanes::I32)?;
                write::<i32, 3, 6>(data, frames, out, |[a, b, c]| {
                    i32::from_le_bytes([0, a, b, c])
                })
            }
            CodecId::PcmS24Be => {
                self.prepare(out, AudioPlanes::I32)?;
                write::<i32, 3, 6>(data, frames, out, |[a, b, c]| {
                    i32::from_be_bytes([a, b, c, 0])
                })
            }
            CodecId::PcmS32Le => {
                self.prepare(out, AudioPlanes::I32)?;
                write::<i32, 4, 8>(data, frames, out, i32::from_le_bytes)
            }
            CodecId::PcmS32Be => {
                self.prepare(out, AudioPlanes::I32)?;
                write::<i32, 4, 8>(data, frames, out, i32::from_be_bytes)
            }
            CodecId::PcmS64Le => {
                self.prepare(out, AudioPlanes::I32)?;
                write::<i32, 8, 16>(data, frames, out, |b| (i64::from_le_bytes(b) >> 32) as i32)
            }
            CodecId::PcmS64Be => {
                self.prepare(out, AudioPlanes::I32)?;
                write::<i32, 8, 16>(data, frames, out, |b| (i64::from_be_bytes(b) >> 32) as i32)
            }
            CodecId::PcmF32Le => {
                self.prepare(out, AudioPlanes::F32)?;
                write::<f32, 4, 8>(data, frames, out, |b| sanitize(f32::from_le_bytes(b)))
            }
            CodecId::PcmF32Be => {
                self.prepare(out, AudioPlanes::F32)?;
                write::<f32, 4, 8>(data, frames, out, |b| sanitize(f32::from_be_bytes(b)))
            }
            CodecId::PcmF64Le => {
                self.prepare(out, AudioPlanes::F32)?;
                write::<f32, 8, 16>(
                    data,
                    frames,
                    out,
                    |b| sanitize(f64::from_le_bytes(b) as f32),
                )
            }
            CodecId::PcmF64Be => {
                self.prepare(out, AudioPlanes::F32)?;
                write::<f32, 8, 16>(
                    data,
                    frames,
                    out,
                    |b| sanitize(f64::from_be_bytes(b) as f32),
                )
            }
            // `new` accepts only the codecs above.
            _ => Err(ErrorKind::UnsupportedCodec.into()),
        }?;
        if self.apply_trim {
            out.apply_trim(packet.trim_start, packet.trim_end);
        }
        Ok(())
    }

    fn reset(&mut self) {}
}

/// Writes `frames` interleaved frames of samples of `W` bytes from `data`
/// to the planes of `out`. `F` is the size of a stereo frame, `2 * W`.
///
/// Mono and stereo, almost all real files, get loops over fixed-size arrays
/// with no bounds checks inside; other channel counts are written plane by
/// plane.
fn write<T: Sample, const W: usize, const F: usize>(
    data: &[u8],
    frames: usize,
    out: &mut AudioBuf,
    convert: impl Fn([u8; W]) -> T,
) -> Result<(), Error> {
    out.reset_window(frames)?;
    let channels = usize::from(out.channels());
    let mut planes = out.planes_mut::<T>().ok_or(ErrorKind::InvalidData)?;
    match channels {
        1 => {
            let mono = planes.next().ok_or(ErrorKind::InvalidData)?;
            let (samples, _) = data.as_chunks::<W>();
            for (o, &s) in mono.iter_mut().zip(samples) {
                *o = convert(s);
            }
        }
        2 => {
            let left = planes.next().ok_or(ErrorKind::InvalidData)?;
            let right = planes.next().ok_or(ErrorKind::InvalidData)?;
            let (pairs, _) = data.as_chunks::<F>();
            for ((l, r), pair) in left.iter_mut().zip(right.iter_mut()).zip(pairs) {
                if let [a, b] = pair.as_chunks::<W>().0 {
                    *l = convert(*a);
                    *r = convert(*b);
                }
            }
        }
        _ => {
            let frame_bytes = W * channels;
            for (channel, plane) in planes.enumerate() {
                for (o, frame) in plane.iter_mut().zip(data.chunks_exact(frame_bytes)) {
                    *o = convert(frame.as_chunks::<W>().0[channel]);
                }
            }
        }
    }
    Ok(())
}

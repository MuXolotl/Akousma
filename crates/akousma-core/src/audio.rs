//! Planar buffer of decoded samples with an `O(1)` gapless trim window.

use crate::channel::{ChannelMask, ChannelPosition};
use crate::error::{Error, ErrorKind};

/// Largest number of channels in an [`AudioBuf`].
pub const MAX_CHANNELS: u8 = 64;

/// Scale from `i16` to `[-1.0, 1.0)`: `2^-15`, exact in `f32`.
const I16_TO_F32: f32 = 1.0 / 32_768.0;
/// Scale from MSB-aligned `i32` to `[-1.0, 1.0)`: `2^-31`, exact in `f32`.
const I32_TO_F32: f32 = 1.0 / 2_147_483_648.0;
/// Scale from `[-1.0, 1.0)` to the `i16` range.
const F32_TO_I16: f32 = 32_768.0;
/// `i16::MAX` and `i16::MIN` as `f32`.
const I16_MAX_F32: f32 = 32_767.0;
const I16_MIN_F32: f32 = -32_768.0;
/// `1.5 * 2^23`. For `|x| < 2^22`, `x + ROUND_MAGIC` lies in `[2^23, 2^24)`,
/// where the spacing of `f32` values is 1: the addition rounds `x` half to
/// even and the low mantissa bits hold `2^22 + round(x)`.
const ROUND_MAGIC: f32 = 12_582_912.0;
/// Shift from an MSB-aligned `i32` to `i16`.
const I32_TO_I16_SHIFT: u32 = 16;
/// Frames per block in the stereo interleaving loop.
const STEREO_BLOCK: usize = 8;

/// Planar sample storage: one vector per channel, all of one sample type.
///
/// `I32` samples are aligned to the most significant bit: a sample of
/// `bits_per_sample` bits is stored as `sample << (32 - bits_per_sample)`, so
/// conversions to `f32` and `i16` do not depend on the source width, and
/// `value >> (32 - bits_per_sample)` restores the original sample.
///
/// The enum is non-exhaustive: a sample type may be added in a later release,
/// so a `match` outside this crate needs a wildcard arm.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum AudioPlanes {
    /// 16-bit integer samples.
    I16(Vec<Vec<i16>>),
    /// MSB-aligned integer samples of up to 32 bits.
    I32(Vec<Vec<i32>>),
    /// Floating-point samples, nominally in `[-1.0, 1.0]`. Values outside that
    /// range are kept (lossy decoders legitimately overshoot).
    F32(Vec<Vec<f32>>),
}

impl AudioPlanes {
    fn len(&self) -> usize {
        match self {
            Self::I16(p) => p.len(),
            Self::I32(p) => p.len(),
            Self::F32(p) => p.len(),
        }
    }
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for i16 {}
    impl Sealed for i32 {}
    impl Sealed for f32 {}
}

/// Sample types stored in [`AudioPlanes`]: `i16`, `i32` and `f32`.
pub trait Sample: Copy + Default + sealed::Sealed + 'static {
    /// The planes if `planes` holds this sample type.
    fn planes(planes: &AudioPlanes) -> Option<&[Vec<Self>]>;
    /// The planes if `planes` holds this sample type.
    fn planes_mut(planes: &mut AudioPlanes) -> Option<&mut [Vec<Self>]>;
}

impl Sample for i16 {
    fn planes(planes: &AudioPlanes) -> Option<&[Vec<Self>]> {
        match planes {
            AudioPlanes::I16(p) => Some(p),
            AudioPlanes::I32(_) | AudioPlanes::F32(_) => None,
        }
    }
    fn planes_mut(planes: &mut AudioPlanes) -> Option<&mut [Vec<Self>]> {
        match planes {
            AudioPlanes::I16(p) => Some(p),
            AudioPlanes::I32(_) | AudioPlanes::F32(_) => None,
        }
    }
}

impl Sample for i32 {
    fn planes(planes: &AudioPlanes) -> Option<&[Vec<Self>]> {
        match planes {
            AudioPlanes::I32(p) => Some(p),
            AudioPlanes::I16(_) | AudioPlanes::F32(_) => None,
        }
    }
    fn planes_mut(planes: &mut AudioPlanes) -> Option<&mut [Vec<Self>]> {
        match planes {
            AudioPlanes::I32(p) => Some(p),
            AudioPlanes::I16(_) | AudioPlanes::F32(_) => None,
        }
    }
}

impl Sample for f32 {
    fn planes(planes: &AudioPlanes) -> Option<&[Vec<Self>]> {
        match planes {
            AudioPlanes::F32(p) => Some(p),
            AudioPlanes::I16(_) | AudioPlanes::I32(_) => None,
        }
    }
    fn planes_mut(planes: &mut AudioPlanes) -> Option<&mut [Vec<Self>]> {
        match planes {
            AudioPlanes::F32(p) => Some(p),
            AudioPlanes::I16(_) | AudioPlanes::I32(_) => None,
        }
    }
}

/// Reusable buffer of decoded audio owned by the caller.
///
/// A decoder writes a whole decoded frame with
/// [`reset_window`](AudioBuf::reset_window) and
/// [`planes_mut`](AudioBuf::planes_mut), then applies the packet's gapless
/// trim with [`apply_trim`](AudioBuf::apply_trim). The trim only moves the
/// visible window `frame_offset..frame_offset + frames`; no sample is moved.
/// Reusing the planes avoids allocations while later packets fit their
/// existing capacity; a larger packet may grow that capacity.
///
/// Planes are in canonical order (see [`crate::channel`]).
///
/// ```
/// use akousma_core::audio::AudioBuf;
/// use akousma_core::channel::{ChannelMask, ChannelPosition};
///
/// let mut buf = AudioBuf::i16(2, 44_100, ChannelMask::STEREO)?;
/// buf.reset_window(4)?;
/// for (ch, plane) in buf.planes_mut::<i16>().expect("i16 planes").enumerate() {
///     for (i, s) in plane.iter_mut().enumerate() {
///         *s = (ch * 100 + i) as i16;
///     }
/// }
/// buf.apply_trim(1, 1);
/// assert_eq!(buf.plane_by_pos::<i16>(ChannelPosition::FrontRight), Some(&[101, 102][..]));
/// let mut out = [0i16; 4];
/// assert_eq!(buf.copy_interleaved_i16(&mut out), 4);
/// assert_eq!(out, [1, 101, 2, 102]);
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct AudioBuf {
    sample_rate: u32,
    channels: u8,
    channel_mask: ChannelMask,
    bits_per_sample: u8,
    frame_offset: usize,
    frames: usize,
    planes: AudioPlanes,
}

impl AudioBuf {
    /// Creates a buffer over `planes`, one vector per channel, with an empty
    /// window. Existing vectors are reused as capacity.
    ///
    /// The channel mask is passed through [`ChannelMask::fit`].
    /// `bits_per_sample` is the significant width of the source samples.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidData`] if the number of planes is not in
    /// `1..=64`, or if `bits_per_sample` is zero or wider than the sample type
    /// for `I16` (16) and `I32` (32) planes.
    pub fn new(
        planes: AudioPlanes,
        sample_rate: u32,
        channel_mask: ChannelMask,
        bits_per_sample: u8,
    ) -> Result<Self, Error> {
        let channels = match u8::try_from(planes.len()) {
            Ok(channels) if (1..=MAX_CHANNELS).contains(&channels) => channels,
            _ => return Err(ErrorKind::InvalidData.into()),
        };
        let bits_valid = match planes {
            AudioPlanes::I16(_) => (1..=i16::BITS).contains(&u32::from(bits_per_sample)),
            AudioPlanes::I32(_) => (1..=i32::BITS).contains(&u32::from(bits_per_sample)),
            AudioPlanes::F32(_) => true,
        };
        if !bits_valid {
            return Err(ErrorKind::InvalidData.into());
        }
        Ok(Self {
            sample_rate,
            channels,
            channel_mask: channel_mask.fit(channels),
            bits_per_sample,
            frame_offset: 0,
            frames: 0,
            planes,
        })
    }

    /// Creates a buffer of `channels` empty `f32` planes with an empty window.
    ///
    /// Shorthand for [`AudioBuf::new`] with [`AudioPlanes::F32`] and
    /// `bits_per_sample` of 32. The channel mask is passed through
    /// [`ChannelMask::fit`].
    ///
    /// ```
    /// use akousma_core::audio::AudioBuf;
    /// use akousma_core::channel::{ChannelMask, ChannelPosition};
    ///
    /// let mut buf = AudioBuf::f32(2, 48_000, ChannelMask::STEREO)?;
    /// assert_eq!((buf.channels(), buf.sample_rate()), (2, 48_000));
    /// assert_eq!(buf.plane::<f32>(0), Some(&[][..]));
    /// buf.reset_window(2)?;
    /// buf.plane_mut::<f32>(1).expect("stereo").copy_from_slice(&[0.25, -0.25]);
    /// assert_eq!(
    ///     buf.plane_by_pos::<f32>(ChannelPosition::FrontRight),
    ///     Some(&[0.25, -0.25][..])
    /// );
    /// # Ok::<(), akousma_core::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidData`] if `channels` is not in `1..=64`.
    pub fn f32(channels: u8, sample_rate: u32, channel_mask: ChannelMask) -> Result<Self, Error> {
        let planes = vec![Vec::new(); usize::from(channels)];
        Self::new(AudioPlanes::F32(planes), sample_rate, channel_mask, 32)
    }

    /// Creates a buffer of `channels` empty `i16` planes with an empty window.
    ///
    /// Shorthand for [`AudioBuf::new`] with [`AudioPlanes::I16`] and
    /// `bits_per_sample` of 16. The channel mask is passed through
    /// [`ChannelMask::fit`].
    ///
    /// ```
    /// use akousma_core::audio::AudioBuf;
    /// use akousma_core::channel::ChannelMask;
    ///
    /// let mut buf = AudioBuf::i16(2, 48_000, ChannelMask::STEREO)?;
    /// buf.reset_window(1)?;
    /// buf.plane_mut::<i16>(1).expect("stereo").copy_from_slice(&[-32_768]);
    /// let mut out = [0i16; 2];
    /// assert_eq!(buf.copy_interleaved_i16(&mut out), 2);
    /// assert_eq!(out, [0, -32_768]);
    /// # Ok::<(), akousma_core::Error>(())
    /// ```
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidData`] if `channels` is not in `1..=64`.
    pub fn i16(channels: u8, sample_rate: u32, channel_mask: ChannelMask) -> Result<Self, Error> {
        let planes = vec![Vec::new(); usize::from(channels)];
        Self::new(AudioPlanes::I16(planes), sample_rate, channel_mask, 16)
    }

    /// Sample rate in hertz.
    #[must_use]
    pub const fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    /// Number of planes, `1..=64`.
    #[must_use]
    pub const fn channels(&self) -> u8 {
        self.channels
    }

    /// Positions of the first planes.
    #[must_use]
    pub const fn channel_mask(&self) -> ChannelMask {
        self.channel_mask
    }

    /// Significant width of the source samples.
    #[must_use]
    pub const fn bits_per_sample(&self) -> u8 {
        self.bits_per_sample
    }

    /// Number of frames in the visible window.
    #[must_use]
    pub const fn frames(&self) -> usize {
        self.frames
    }

    /// Returns `true` if the visible window is empty.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.frames == 0
    }

    /// Sets the window to `0..frames`, growing every plane to at least
    /// `frames` samples. Samples inside the window are unspecified until the
    /// decoder writes them.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::LimitExceeded`] if memory for the planes cannot be
    /// reserved. The window and its samples stay unchanged, though capacities
    /// of earlier planes may have grown before a later reservation failed.
    pub fn reset_window(&mut self, frames: usize) -> Result<(), Error> {
        fn grow<T: Sample>(planes: &mut [Vec<T>], frames: usize) -> Result<(), Error> {
            for plane in planes.iter_mut() {
                let missing = frames.saturating_sub(plane.len());
                plane
                    .try_reserve_exact(missing)
                    .map_err(|_| Error::from(ErrorKind::LimitExceeded))?;
            }
            for plane in planes {
                if plane.len() < frames {
                    plane.resize(frames, T::default());
                }
            }
            Ok(())
        }
        match &mut self.planes {
            AudioPlanes::I16(p) => grow(p, frames)?,
            AudioPlanes::I32(p) => grow(p, frames)?,
            AudioPlanes::F32(p) => grow(p, frames)?,
        }
        self.frame_offset = 0;
        self.frames = frames;
        Ok(())
    }

    /// Drops `trim_start` frames from the start and `trim_end` frames from the
    /// end of the window in `O(1)`. Trims larger than the window leave an
    /// empty window.
    pub fn apply_trim(&mut self, trim_start: u32, trim_end: u32) {
        // A trim that does not fit in `usize` exceeds any window.
        let start = usize::try_from(trim_start).map_or(self.frames, |s| s.min(self.frames));
        let rest = self.frames - start;
        let end = usize::try_from(trim_end).map_or(rest, |e| e.min(rest));
        self.frame_offset += start;
        self.frames = rest - end;
    }

    fn window(&self) -> core::ops::Range<usize> {
        self.frame_offset..self.frame_offset + self.frames
    }

    /// The visible window of plane `channel`, or `None` if the planes do not
    /// hold `T` or `channel` is out of range.
    #[must_use]
    pub fn plane<T: Sample>(&self, channel: usize) -> Option<&[T]> {
        let window = self.window();
        T::planes(&self.planes)?.get(channel).map(|p| &p[window])
    }

    /// The visible window of plane `channel`, mutable, or `None` if the planes
    /// do not hold `T` or `channel` is out of range.
    #[must_use]
    pub fn plane_mut<T: Sample>(&mut self, channel: usize) -> Option<&mut [T]> {
        let window = self.window();
        T::planes_mut(&mut self.planes)?
            .get_mut(channel)
            .map(|p| &mut p[window])
    }

    /// The visible windows of all planes in canonical order, or `None` if the
    /// planes do not hold `T`.
    #[must_use]
    pub fn planes_mut<T: Sample>(&mut self) -> Option<impl ExactSizeIterator<Item = &mut [T]>> {
        let window = self.window();
        let planes = T::planes_mut(&mut self.planes)?;
        Some(planes.iter_mut().map(move |p| &mut p[window.clone()]))
    }

    /// The visible window of the plane carrying `pos`, or `None` if the
    /// channel mask does not contain `pos` or the planes do not hold `T`.
    #[must_use]
    pub fn plane_by_pos<T: Sample>(&self, pos: ChannelPosition) -> Option<&[T]> {
        self.plane(self.channel_mask.plane_index(pos)?)
    }

    /// Number of samples in the visible window over all channels.
    #[must_use]
    pub const fn samples(&self) -> usize {
        self.frames * self.channels as usize
    }

    /// Writes the visible window to `out` as interleaved `f32` and returns the
    /// number of samples written, [`AudioBuf::samples`].
    ///
    /// `i16` samples are scaled by `2^-15` and `i32` samples by `2^-31`, so
    /// the minimum maps to `-1.0`. `f32` samples are copied unchanged.
    ///
    /// # Panics
    ///
    /// If `out` is shorter than [`AudioBuf::samples`].
    pub fn copy_interleaved_f32(&self, out: &mut [f32]) -> usize {
        let out = self.output(out);
        let window = self.window();
        match &self.planes {
            AudioPlanes::I16(p) => {
                interleave_converting(p, window, out, |s| f32::from(s) * I16_TO_F32);
            }
            AudioPlanes::I32(p) => interleave(p, window, out, |s| s as f32 * I32_TO_F32),
            AudioPlanes::F32(p) => interleave(p, window, out, |s| s),
        }
        out.len()
    }

    /// Writes the visible window to `out` as interleaved `i16` and returns the
    /// number of samples written, [`AudioBuf::samples`].
    ///
    /// `i32` samples keep their top 16 bits (an arithmetic shift, rounding
    /// toward negative infinity). `f32` samples are scaled by `32768`,
    /// rounded half to even and saturated: `NaN` becomes `0`, values at or
    /// above `1.0` become `32767` and values at or below `-1.0` become
    /// `-32768`. No dither is applied.
    ///
    /// # Panics
    ///
    /// If `out` is shorter than [`AudioBuf::samples`].
    pub fn copy_interleaved_i16(&self, out: &mut [i16]) -> usize {
        let out = self.output(out);
        let window = self.window();
        match &self.planes {
            AudioPlanes::I16(p) => interleave(p, window, out, |s| s),
            AudioPlanes::I32(p) => interleave(p, window, out, |s| (s >> I32_TO_I16_SHIFT) as i16),
            AudioPlanes::F32(p) => interleave(p, window, out, f32_to_i16),
        }
        out.len()
    }

    fn output<'o, T>(&self, out: &'o mut [T]) -> &'o mut [T] {
        let samples = self.samples();
        assert!(
            out.len() >= samples,
            "output holds {} samples, the window needs {samples}",
            out.len()
        );
        &mut out[..samples]
    }
}

/// `(sample * 32768.0).round_ties_even() as i16`, computed without a
/// rounding instruction or a saturating cast so that the loop vectorizes on
/// baseline x86-64 (SSE2), where `round_ties_even` is a library call.
#[inline]
fn f32_to_i16(sample: f32) -> i16 {
    let x = sample * F32_TO_I16;
    let clamped = if x.is_nan() {
        0.0
    } else {
        x.clamp(I16_MIN_F32, I16_MAX_F32)
    };
    // Two's complement of `round(clamped)` in the low bits.
    (clamped + ROUND_MAGIC)
        .to_bits()
        .wrapping_sub(ROUND_MAGIC.to_bits()) as i16
}

/// Interleaves `window` of every plane into `out`, which holds exactly
/// `window.len() * planes.len()` samples.
fn interleave<S: Copy, D: Copy>(
    planes: &[Vec<S>],
    window: core::ops::Range<usize>,
    out: &mut [D],
    convert: impl Fn(S) -> D,
) {
    match planes {
        [mono] => {
            for (o, &s) in out.iter_mut().zip(&mono[window]) {
                *o = convert(s);
            }
        }
        [left, right] => {
            let (left, right) = (&left[window.clone()], &right[window]);
            let (left_blocks, left_tail) = left.as_chunks::<STEREO_BLOCK>();
            let (right_blocks, right_tail) = right.as_chunks::<STEREO_BLOCK>();
            let (out_blocks, out_tail) = out.as_chunks_mut::<{ 2 * STEREO_BLOCK }>();
            for ((l, r), o) in left_blocks.iter().zip(right_blocks).zip(out_blocks) {
                for i in 0..STEREO_BLOCK {
                    o[2 * i] = convert(l[i]);
                    o[2 * i + 1] = convert(r[i]);
                }
            }
            let (out_pairs, _) = out_tail.as_chunks_mut::<2>();
            for ((&l, &r), o) in left_tail.iter().zip(right_tail).zip(out_pairs) {
                *o = [convert(l), convert(r)];
            }
        }
        _ => {
            let channels = planes.len();
            for (ch, plane) in planes.iter().enumerate() {
                let samples = out.iter_mut().skip(ch).step_by(channels);
                for (o, &s) in samples.zip(&plane[window.clone()]) {
                    *o = convert(s);
                }
            }
        }
    }
}

/// [`interleave`] for planes converted to `f32`. Stereo is a plain zipped
/// loop, which the compiler vectorizes where the blocked loop of
/// [`interleave`] stays scalar; other layouts use [`interleave`].
fn interleave_converting<S: Copy>(
    planes: &[Vec<S>],
    window: core::ops::Range<usize>,
    out: &mut [f32],
    convert: impl Fn(S) -> f32,
) {
    if let [left, right] = planes {
        let (left, right) = (&left[window.clone()], &right[window]);
        // No block loop: over pairs the compiler turns the zip into a
        // vectorized loop; the blocked loop above stays scalar for `f32`.
        let (pairs, _) = out.as_chunks_mut::<2>();
        for ((&l, &r), o) in left.iter().zip(right).zip(pairs) {
            *o = [convert(l), convert(r)];
        }
    } else {
        interleave(planes, window, out, convert);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_i16(frames: usize) -> AudioBuf {
        let mut buf = AudioBuf::new(
            AudioPlanes::I16(vec![Vec::new(); 2]),
            48_000,
            ChannelMask::STEREO,
            16,
        )
        .unwrap();
        buf.reset_window(frames).unwrap();
        for (ch, plane) in buf.planes_mut::<i16>().unwrap().enumerate() {
            for (i, s) in plane.iter_mut().enumerate() {
                *s = i16::try_from(i).unwrap() * if ch == 0 { 1 } else { -1 };
            }
        }
        buf
    }

    fn f32_mono(samples: &[f32]) -> AudioBuf {
        let mut buf = AudioBuf::new(
            AudioPlanes::F32(vec![Vec::new()]),
            48_000,
            ChannelMask::MONO,
            32,
        )
        .unwrap();
        buf.reset_window(samples.len()).unwrap();
        buf.plane_mut::<f32>(0).unwrap().copy_from_slice(samples);
        buf
    }

    #[test]
    fn audiobuf_o1_gapless_and_clamp() {
        // MPEG Layer III frame of 1152 samples, delay 100, padding 52.
        let mut buf = stereo_i16(1152);
        let ptr = buf.plane::<i16>(0).unwrap().as_ptr();
        buf.apply_trim(100, 52);
        assert_eq!(buf.frames(), 1000);
        let left = buf.plane::<i16>(0).unwrap();
        assert_eq!((left[0], left[999]), (100, 1099));
        assert_eq!(buf.plane::<i16>(1).unwrap()[0], -100);
        // The window moved inside the same allocation.
        assert_eq!(ptr.wrapping_add(100), left.as_ptr());

        // Trims larger than the frame: 576 + 800 > 1152.
        let mut short = stereo_i16(1152);
        short.apply_trim(576, 800);
        assert_eq!(short.frames(), 0);
        assert!(short.is_empty());
        assert_eq!(short.plane::<i16>(0), Some(&[][..]));
        assert_eq!(short.copy_interleaved_i16(&mut []), 0);
        short.apply_trim(u32::MAX, u32::MAX);
        assert_eq!(short.frames(), 0);
        let mut end_only = stereo_i16(10);
        end_only.apply_trim(0, u32::MAX);
        assert_eq!(end_only.frames(), 0);
        let mut start_only = stereo_i16(10);
        start_only.apply_trim(9, 0);
        assert_eq!(start_only.plane::<i16>(0), Some(&[9][..]));
        // Consecutive trims accumulate.
        start_only.apply_trim(0, 0);
        assert_eq!(start_only.plane::<i16>(0), Some(&[9][..]));

        // Reusing the buffer for a smaller frame does not reallocate.
        buf.reset_window(512).unwrap();
        assert_eq!(buf.frames(), 512);
        assert_eq!(buf.plane::<i16>(0).unwrap().as_ptr(), ptr);

        // f32 -> i16: saturation, NaN, and ties to even (k + 0.5) / 32768.
        let input = [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            1.5,
            -1.5,
            1.0,
            -1.0,
            0.5 / 32_768.0,
            1.5 / 32_768.0,
            2.5 / 32_768.0,
            -0.5 / 32_768.0,
            -1.5 / 32_768.0,
            32_766.5 / 32_768.0,
        ];
        let expected = [
            0, 32_767, -32_768, 32_767, -32_768, 32_767, -32_768, 0, 2, 2, 0, -2, 32_766,
        ];
        let mut out = [0i16; 13];
        assert_eq!(f32_mono(&input).copy_interleaved_i16(&mut out), 13);
        assert_eq!(out, expected);

        // f32 output keeps overshoot unchanged.
        let mut floats = [0f32; 3];
        f32_mono(&[1.05, -1.08, 0.25]).copy_interleaved_f32(&mut floats);
        assert_eq!(floats, [1.05, -1.08, 0.25]);

        // i32 (MSB-aligned) extremes.
        let mut wide = AudioBuf::new(
            AudioPlanes::I32(vec![Vec::new()]),
            96_000,
            ChannelMask::MONO,
            24,
        )
        .unwrap();
        wide.reset_window(4).unwrap();
        wide.plane_mut::<i32>(0).unwrap().copy_from_slice(&[
            i32::MIN,
            8_388_607 << 8,
            -1 << 8,
            1 << 8,
        ]);
        let mut narrow = [0i16; 4];
        wide.copy_interleaved_i16(&mut narrow);
        // Arithmetic shift: -1 in 24 bits becomes -1 in 16 bits, +1 becomes 0.
        assert_eq!(narrow, [-32_768, 32_767, -1, 0]);
        let mut floats = [0f32; 4];
        wide.copy_interleaved_f32(&mut floats);
        // 8388607 / 8388608 and 1 / 8388608 are exact in f32.
        assert_eq!(
            floats,
            [
                -1.0,
                8_388_607.0 / 8_388_608.0,
                -1.0 / 8_388_608.0,
                1.0 / 8_388_608.0
            ]
        );

        // i16 -> f32 boundaries.
        let mut ends = AudioBuf::new(
            AudioPlanes::I16(vec![Vec::new()]),
            8000,
            ChannelMask::MONO,
            16,
        )
        .unwrap();
        ends.reset_window(2).unwrap();
        ends.plane_mut::<i16>(0)
            .unwrap()
            .copy_from_slice(&[i16::MIN, i16::MAX]);
        let mut floats = [0f32; 2];
        ends.copy_interleaved_f32(&mut floats);
        assert_eq!(floats, [-1.0, 32_767.0 / 32_768.0]);
    }

    #[test]
    fn f32_to_i16_matches_round_ties_even() {
        // Reference: the standard library rounding and saturating cast. Every
        // 251st bit pattern covers all exponents, signs, infinities and NaNs.
        let reference = |s: f32| (s * F32_TO_I16).round_ties_even() as i16;
        for bits in (0..=u32::MAX).step_by(251) {
            let s = f32::from_bits(bits);
            assert_eq!(f32_to_i16(s), reference(s), "{bits:#010x}");
        }
        // Every tie `k + 0.5` and its neighbours across the i16 range.
        for k in -32_769..=32_768 {
            for x in [k as f32 + 0.5, k as f32 + 0.499_99, k as f32 + 0.500_01] {
                let s = x / F32_TO_I16;
                assert_eq!(f32_to_i16(s), reference(s), "{x}");
            }
        }
    }

    /// Reference interleaving with explicit indices.
    fn interleaved_reference(buf: &AudioBuf) -> Vec<i32> {
        let mut out = Vec::new();
        for f in 0..buf.frames() {
            for ch in 0..usize::from(buf.channels()) {
                out.push(buf.plane::<i32>(ch).unwrap()[f]);
            }
        }
        out
    }

    #[test]
    fn interleave_paths_match_reference() {
        // Frame counts around the 8-frame stereo block, and 1 to 8 channels.
        for channels in 1..=8usize {
            for frames in [0usize, 1, 7, 8, 9, 16, 17, 33] {
                let mut buf = AudioBuf::new(
                    AudioPlanes::I32(vec![Vec::new(); channels]),
                    44_100,
                    ChannelMask::EMPTY,
                    32,
                )
                .unwrap();
                buf.reset_window(frames + 3).unwrap();
                for (ch, plane) in buf.planes_mut::<i32>().unwrap().enumerate() {
                    for (i, s) in plane.iter_mut().enumerate() {
                        *s = i32::try_from(ch * 1000 + i).unwrap() << 16;
                    }
                }
                buf.apply_trim(2, 1);
                let expected = interleaved_reference(&buf);
                // Extra room in the output stays untouched.
                let mut out = vec![i16::MIN; expected.len() + 5];
                assert_eq!(buf.copy_interleaved_i16(&mut out), expected.len());
                for (o, e) in out.iter().zip(&expected) {
                    assert_eq!(i32::from(*o), e >> 16, "{channels} ch, {frames} frames");
                }
                assert!(out[expected.len()..].iter().all(|&s| s == i16::MIN));
            }
        }
    }

    #[test]
    fn i16_planes_to_f32_match_reference() {
        for channels in 1..=3usize {
            for frames in [0usize, 1, 7, 8, 9, 16, 17, 33] {
                let mut buf = AudioBuf::new(
                    AudioPlanes::I16(vec![Vec::new(); channels]),
                    44_100,
                    ChannelMask::EMPTY,
                    16,
                )
                .unwrap();
                buf.reset_window(frames + 3).unwrap();
                for (ch, plane) in buf.planes_mut::<i16>().unwrap().enumerate() {
                    for (i, s) in plane.iter_mut().enumerate() {
                        *s = i16::try_from(ch * 1000 + i).unwrap().wrapping_mul(997);
                    }
                }
                buf.apply_trim(2, 1);
                let mut expected = Vec::new();
                for f in 0..buf.frames() {
                    for ch in 0..channels {
                        expected.push(f32::from(buf.plane::<i16>(ch).unwrap()[f]) / 32_768.0);
                    }
                }
                let mut out = vec![f32::NAN; expected.len() + 5];
                assert_eq!(buf.copy_interleaved_f32(&mut out), expected.len());
                for (o, e) in out.iter().zip(&expected) {
                    assert_eq!(o.to_bits(), e.to_bits(), "{channels} ch, {frames} frames");
                }
                assert!(out[expected.len()..].iter().all(|s| s.is_nan()));
            }
        }
    }

    #[test]
    #[should_panic(expected = "output holds 3 samples, the window needs 4")]
    fn short_output_panics() {
        stereo_i16(2).copy_interleaved_i16(&mut [0; 3]);
    }

    #[test]
    fn shorthands_match_new() {
        for channels in [1u8, 2, 6, 64] {
            let f = AudioBuf::f32(channels, 96_000, ChannelMask::STEREO).unwrap();
            assert_eq!((f.channels(), f.sample_rate()), (channels, 96_000));
            assert_eq!(f.bits_per_sample(), 32);
            assert_eq!(f.channel_mask(), ChannelMask::STEREO.fit(channels));
            assert!(f.plane::<f32>(usize::from(channels) - 1).is_some());
            assert!(f.plane::<f32>(usize::from(channels)).is_none());

            let i = AudioBuf::i16(channels, 8_000, ChannelMask::MONO).unwrap();
            assert_eq!((i.channels(), i.sample_rate()), (channels, 8_000));
            assert_eq!(i.bits_per_sample(), 16);
            // The mask goes through `fit`, exactly as in `new`.
            assert_eq!(i.channel_mask(), ChannelMask::MONO.fit(channels));
        }
        // An empty window, like `new`.
        let buf = AudioBuf::f32(2, 44_100, ChannelMask::STEREO).unwrap();
        assert_eq!((buf.frames(), buf.samples()), (0, 0));

        // The same range check as `new`.
        assert_eq!(
            AudioBuf::f32(0, 44_100, ChannelMask::EMPTY)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidData
        );
        assert_eq!(
            AudioBuf::i16(65, 44_100, ChannelMask::EMPTY)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidData
        );
        assert_eq!(
            AudioBuf::i16(u8::MAX, 44_100, ChannelMask::EMPTY)
                .unwrap_err()
                .kind,
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn construction_and_typed_access() {
        let none: Vec<Vec<i16>> = Vec::new();
        let err = AudioBuf::new(AudioPlanes::I16(none), 1, ChannelMask::EMPTY, 16).unwrap_err();
        assert_eq!(err.kind, ErrorKind::InvalidData);
        let too_many = AudioPlanes::F32(vec![Vec::new(); 65]);
        assert!(AudioBuf::new(too_many, 1, ChannelMask::EMPTY, 32).is_err());
        let max = AudioPlanes::F32(vec![Vec::new(); 64]);
        assert_eq!(
            AudioBuf::new(max, 1, ChannelMask::EMPTY, 32)
                .unwrap()
                .channels(),
            64
        );
        let wide = AudioPlanes::I16(vec![Vec::new()]);
        assert!(AudioBuf::new(wide, 1, ChannelMask::MONO, 17).is_err());
        let zero = AudioPlanes::I32(vec![Vec::new()]);
        assert!(AudioBuf::new(zero, 1, ChannelMask::MONO, 0).is_err());
        let full = AudioPlanes::I32(vec![Vec::new()]);
        assert!(AudioBuf::new(full, 1, ChannelMask::MONO, 32).is_ok());

        // A mask with more positions than planes is dropped.
        let buf = AudioBuf::new(
            AudioPlanes::I16(vec![Vec::new(); 2]),
            48_000,
            ChannelMask::from_raw(0x3F),
            16,
        )
        .unwrap();
        assert_eq!(buf.channel_mask(), ChannelMask::EMPTY);
        assert_eq!(buf.plane_by_pos::<i16>(ChannelPosition::FrontLeft), None);
        assert_eq!(buf.sample_rate(), 48_000);
        assert_eq!(buf.bits_per_sample(), 16);

        // Partial mask on three planes: FLC | FRC, third plane unlabeled.
        let mut buf = AudioBuf::new(
            AudioPlanes::I16(vec![Vec::new(); 3]),
            48_000,
            ChannelMask::from_raw(0xC0),
            16,
        )
        .unwrap();
        buf.reset_window(1).unwrap();
        for (ch, plane) in buf.planes_mut::<i16>().unwrap().enumerate() {
            plane[0] = i16::try_from(ch).unwrap() + 10;
        }
        assert_eq!(
            buf.plane_by_pos::<i16>(ChannelPosition::FrontLeftCenter),
            Some(&[10][..])
        );
        assert_eq!(
            buf.plane_by_pos::<i16>(ChannelPosition::FrontRightCenter),
            Some(&[11][..])
        );
        assert_eq!(buf.plane_by_pos::<i16>(ChannelPosition::FrontLeft), None);
        assert_eq!(buf.plane::<i16>(2), Some(&[12][..]));
        assert_eq!(buf.plane::<i16>(3), None);
        assert_eq!(buf.plane::<f32>(0), None);
        assert!(buf.planes_mut::<i32>().is_none());
        assert_eq!(buf.planes_mut::<i16>().unwrap().len(), 3);
    }
}

//! In-place complex fast Fourier transform.

use super::Complex32;
use crate::error::{Error, ErrorKind};

/// Complex forward FFT of a fixed length,
/// `F[m] = sum(Z[p] * exp(-2 * pi * i * p * m / len))`, computed in place.
///
/// Lengths are powers of two from 1 to [`Fft::MAX_LEN`]. Twiddle factors are
/// computed in `f64` when the transform is created, stored as `f32` and laid
/// out stage by stage, so that each stage reads them in order.
/// [`Fft::process`] does not allocate.
///
/// ```ignore
/// use akousma_core::math::{Complex32, Fft};
///
/// let fft = Fft::new(4)?;
/// let mut data = [Complex32::new(1.0, 0.0); 4];
/// fft.process(&mut data)?;
/// assert_eq!(data[0], Complex32::new(4.0, 0.0));
/// assert_eq!(data[1], Complex32::default());
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct Fft {
    len: usize,
    /// `exp(-2 * pi * i * k / size)` for `k` in `0..size / 2`, for every
    /// stage size `4, 8, ..., len`, in this order. Stage size 2 needs none.
    twiddles: Vec<Complex32>,
    /// Bit-reversed index of every position.
    bit_reversed: Vec<u16>,
}

impl Fft {
    /// Largest supported length.
    pub const MAX_LEN: usize = 8192;

    /// Prepares a transform of `len` points.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnsupportedFeature`] if `len` is not a power of two in
    /// `1..=`[`Fft::MAX_LEN`].
    pub fn new(len: usize) -> Result<Self, Error> {
        if !len.is_power_of_two() || len > Self::MAX_LEN {
            return Err(ErrorKind::UnsupportedFeature.into());
        }
        let bits = len.trailing_zeros();
        let bit_reversed = (0..len)
            .map(|i| {
                // `len <= 8192`, so an index fits in 13 bits.
                if bits == 0 {
                    0
                } else {
                    (i as u16).reverse_bits() >> (u16::BITS - bits)
                }
            })
            .collect();
        let mut twiddles = Vec::with_capacity(len.saturating_sub(2));
        let mut size = 4;
        while size <= len {
            let step = core::f64::consts::TAU / size as f64;
            twiddles.extend((0..size / 2).map(|k| Complex32::expi_neg(step * k as f64)));
            size *= 2;
        }
        Ok(Self {
            len,
            twiddles,
            bit_reversed,
        })
    }

    /// Number of points.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len
    }

    /// Always `false`: a transform has at least one point.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Replaces `data` by its forward transform, without scaling.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidData`] if `data.len()` differs from
    /// [`Fft::len`]; `data` is then left unchanged.
    pub fn process(&self, data: &mut [Complex32]) -> Result<(), Error> {
        if data.len() != self.len {
            return Err(ErrorKind::InvalidData.into());
        }
        for (i, &j) in self.bit_reversed.iter().enumerate() {
            let j = usize::from(j);
            if i < j {
                data.swap(i, j);
            }
        }
        // Stage size 2: every twiddle is 1.
        for [a, b] in data.as_chunks_mut::<2>().0 {
            (*a, *b) = (*a + *b, *a - *b);
        }
        let mut size = 4;
        let mut twiddles = &self.twiddles[..];
        while size <= self.len {
            let half = size / 2;
            let (stage, rest) = twiddles.split_at(half);
            twiddles = rest;
            for block in data.chunks_exact_mut(size) {
                let (lower, upper) = block.split_at_mut(half);
                for ((a, b), &w) in lower.iter_mut().zip(upper.iter_mut()).zip(stage) {
                    let t = *b * w;
                    *b = *a - t;
                    *a = *a + t;
                }
            }
            size *= 2;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::super::noise;
    use super::*;

    #[test]
    fn fft_matches_direct_dft() {
        let mut len = 1;
        while len <= Fft::MAX_LEN {
            let fft = Fft::new(len).unwrap();
            assert_eq!(fft.len(), len);
            let re = noise(len as u32, len);
            let im = noise(len as u32 + 7, len);
            let input: Vec<_> = re
                .iter()
                .zip(&im)
                .map(|(&r, &i)| Complex32::new(r, i))
                .collect();
            let mut output = input.clone();
            fft.process(&mut output).unwrap();
            // The direct DFT is slow; check every bin up to 1024 points and
            // 65 spread bins above.
            let bins: Vec<usize> = if len <= 1024 {
                (0..len).collect()
            } else {
                (0..len).step_by(len / 64).chain([len - 1]).collect()
            };
            for m in bins {
                let (er, ei) = dft_bin(&input, m);
                let tolerance = 1e-6 * (len as f64).sqrt() * (len as f64).log2().max(1.0);
                assert!(
                    (f64::from(output[m].re) - er).abs() < tolerance
                        && (f64::from(output[m].im) - ei).abs() < tolerance,
                    "len {len} bin {m}: {:?} vs ({er}, {ei})",
                    output[m]
                );
            }
            len *= 2;
        }
    }

    /// One bin of the direct DFT, in `f64`.
    fn dft_bin(input: &[Complex32], m: usize) -> (f64, f64) {
        let n = input.len();
        input
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(re, im), (p, z)| {
                let angle = -core::f64::consts::TAU * ((p * m) % n) as f64 / n as f64;
                let (sin, cos) = angle.sin_cos();
                let (zr, zi) = (f64::from(z.re), f64::from(z.im));
                (re + zr * cos - zi * sin, im + zr * sin + zi * cos)
            })
    }

    #[test]
    fn fft_rejects_unsupported_lengths() {
        for len in [0, 3, 6, 12, 8193, 16_384] {
            assert_eq!(
                Fft::new(len).unwrap_err().kind,
                ErrorKind::UnsupportedFeature
            );
        }
        let fft = Fft::new(8).unwrap();
        let mut short = [Complex32::new(1.0, 0.0); 4];
        assert_eq!(
            fft.process(&mut short).unwrap_err().kind,
            ErrorKind::InvalidData
        );
        assert_eq!(short, [Complex32::new(1.0, 0.0); 4]);
        // An impulse transforms to a constant, a constant to an impulse.
        let mut impulse = [Complex32::default(); 8];
        impulse[0] = Complex32::new(1.0, 0.0);
        fft.process(&mut impulse).unwrap();
        assert_eq!(impulse, [Complex32::new(1.0, 0.0); 8]);
    }
}

//! Inverse MDCT with a fused window and overlap-add step.

use super::{Complex32, Fft};
use crate::error::{Error, ErrorKind};

/// Inverse modified discrete cosine transform of `m` coefficients into
/// `2 * m` samples,
/// `y[n] = scale * sum(X[k] * cos(pi / m * (n + 0.5 + m / 2) * (k + 0.5)))`
/// for `n` in `0..2 * m`.
///
/// The transform runs as a pre-rotation, a complex [`Fft`] of `m / 2`
/// points and a post-rotation. [`Imdct::process`] fuses the post-rotation
/// with the window and the overlap-add of consecutive blocks: it writes the
/// `m` finished samples straight into the output and keeps the windowed
/// second half for the next block, without an intermediate `2 * m` buffer.
/// [`Imdct::imdct_raw`] returns all `2 * m` samples without a window, for
/// codecs whose blocks change size (Vorbis).
///
/// `m` is a power of two from 4 to [`Imdct::MAX_COEFFS`]. With a window that
/// satisfies the Princen-Bradley condition (any [`WindowShape`]) and a
/// forward transform `X[k] = sum(w[n] * x[n] * cos(...))` without scaling,
/// `scale = 2 / m` reconstructs the signal exactly. Neither method
/// allocates.
///
/// [`WindowShape`]: super::WindowShape
///
/// ```ignore
/// use akousma_core::math::{Imdct, WindowShape};
///
/// let m = 8;
/// let mut imdct = Imdct::new(m, 1.0)?;
/// let window = WindowShape::Sine.generate(2 * m)?;
/// let mut overlap = vec![0.0; m];
/// let mut out = vec![0.0; m];
///
/// // A block of zeros after silence is silence.
/// imdct.process(&[0.0; 8], &window, &mut overlap, &mut out)?;
/// assert!(out.iter().chain(&overlap).all(|&s| s == 0.0));
///
/// // Without a window, the first coefficient gives a cosine.
/// let mut raw = vec![0.0; 2 * m];
/// let mut coeffs = [0.0; 8];
/// coeffs[0] = 1.0;
/// imdct.imdct_raw(&coeffs, &mut raw)?;
/// let expected = (std::f32::consts::PI / 8.0 * (0.5 + 4.0) * 0.5).cos();
/// assert!((raw[0] - expected).abs() < 1e-6);
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Clone, Debug)]
pub struct Imdct {
    m: usize,
    fft: Fft,
    /// `scale * exp(-i * pi * (m / 2 + 0.5) * (2 * p + 0.5) / m)`.
    pre: Vec<Complex32>,
    /// `exp(-i * pi * k / m)`.
    post: Vec<Complex32>,
    /// FFT buffer of `m / 2` points.
    scratch: Vec<Complex32>,
}

impl Imdct {
    /// Largest supported number of coefficients (window length 8192).
    pub const MAX_COEFFS: usize = 4096;

    /// Prepares a transform of `m` coefficients with output scale `scale`.
    ///
    /// The scale is folded into the pre-rotation table, so it costs nothing
    /// per sample.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnsupportedFeature`] if `m` is not a power of two in
    /// `4..=`[`Imdct::MAX_COEFFS`].
    pub fn new(m: usize, scale: f32) -> Result<Self, Error> {
        if !m.is_power_of_two() || !(4..=Self::MAX_COEFFS).contains(&m) {
            return Err(ErrorKind::UnsupportedFeature.into());
        }
        let half = m / 2;
        let mf = m as f64;
        let center = mf / 2.0 + 0.5;
        let pre = (0..half)
            .map(|p| {
                let angle = core::f64::consts::PI * center * (2.0 * p as f64 + 0.5) / mf;
                Complex32::expi_neg(angle) * scale
            })
            .collect();
        let post = (0..half)
            .map(|k| Complex32::expi_neg(core::f64::consts::PI * k as f64 / mf))
            .collect();
        Ok(Self {
            m,
            fft: Fft::new(half)?,
            pre,
            post,
            scratch: vec![Complex32::default(); half],
        })
    }

    /// Number of coefficients `m`; a block produces `m` finished samples.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.m
    }

    /// Always `false`: a transform has at least four coefficients.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Transforms `coeffs`, applies `window` and overlaps with the previous
    /// block.
    ///
    /// With `y` the `2 * m` samples of the transform, this writes
    /// `out[n] = overlap[n] + y[n] * window[n]` and then
    /// `overlap[n] = y[m + n] * window[m + n]` for `n` in `0..m`. `window`
    /// may be any shape, including the asymmetric transition windows of AAC;
    /// `overlap` starts as zeros.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidData`] unless `coeffs`, `overlap` and `out` have
    /// [`Imdct::len`] elements and `window` twice as many; nothing is
    /// written then.
    pub fn process(
        &mut self,
        coeffs: &[f32],
        window: &[f32],
        overlap: &mut [f32],
        out: &mut [f32],
    ) -> Result<(), Error> {
        let m = self.m;
        if coeffs.len() != m || window.len() != 2 * m || overlap.len() != m || out.len() != m {
            return Err(ErrorKind::InvalidData.into());
        }
        self.transform(coeffs)?;
        let quarter = m / 2;
        let (win_first, win_second) = window.split_at(m);
        // Output index pairs: `(i1, i0) = (L - 2 - 2q, L - 1 - 2q)` walk down
        // the first half from `L = m / 2`, `(i2, i3) = (L + 2q, L + 2q + 1)`
        // walk up the second half.
        let (out_low, out_high) = out.split_at_mut(quarter);
        let (ovl_low, ovl_high) = overlap.split_at_mut(quarter);
        // Every slice length here is even, so the chunk remainders are empty.
        let low = pairs_mut(out_low)
            .rev()
            .zip(pairs_mut(ovl_low).rev())
            .zip(pairs(&win_first[..quarter]).rev())
            .zip(pairs(&win_second[..quarter]).rev());
        let high = pairs_mut(out_high)
            .zip(pairs_mut(ovl_high))
            .zip(pairs(&win_first[quarter..]))
            .zip(pairs(&win_second[quarter..]));
        let steps = low.zip(high).zip(rotated(&self.scratch, &self.post));
        for (((((o_lo, v_lo), w1_lo), w2_lo), (((o_hi, v_hi), w1_hi), w2_hi)), (w_lo, w_hi)) in
            steps
        {
            // `o_lo = [i1, i0]`, `o_hi = [i2, i3]`.
            o_lo[1] = v_lo[1] - w_hi.re * w1_lo[1];
            v_lo[1] = w_hi.im * w2_lo[1];
            o_lo[0] = v_lo[0] + w_lo.re * w1_lo[0];
            v_lo[0] = w_lo.im * w2_lo[0];
            o_hi[0] = v_hi[0] + w_hi.re * w1_hi[0];
            v_hi[0] = w_hi.im * w2_hi[0];
            o_hi[1] = v_hi[1] - w_lo.re * w1_hi[1];
            v_hi[1] = w_lo.im * w2_hi[1];
        }
        Ok(())
    }

    /// Transforms `coeffs` into the `2 * m` samples `y` without a window.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::InvalidData`] unless `coeffs` has [`Imdct::len`]
    /// elements and `out` twice as many; nothing is written then.
    pub fn imdct_raw(&mut self, coeffs: &[f32], out: &mut [f32]) -> Result<(), Error> {
        let m = self.m;
        if coeffs.len() != m || out.len() != 2 * m {
            return Err(ErrorKind::InvalidData.into());
        }
        self.transform(coeffs)?;
        let quarter = m / 2;
        let (first, second) = out.split_at_mut(m);
        let (first_low, first_high) = first.split_at_mut(quarter);
        let (second_low, second_high) = second.split_at_mut(quarter);
        let steps = pairs_mut(first_low)
            .rev()
            .zip(pairs_mut(first_high))
            .zip(pairs_mut(second_low).rev())
            .zip(pairs_mut(second_high));
        for ((((y_lo, y_hi), z_lo), z_hi), (w_lo, w_hi)) in
            steps.zip(rotated(&self.scratch, &self.post))
        {
            y_lo[1] = -w_hi.re;
            y_lo[0] = w_lo.re;
            y_hi[0] = w_hi.re;
            y_hi[1] = -w_lo.re;
            z_lo[1] = w_hi.im;
            z_lo[0] = w_lo.im;
            z_hi[0] = w_hi.im;
            z_hi[1] = w_lo.im;
        }
        Ok(())
    }

    /// Pre-rotation and FFT into `scratch`.
    fn transform(&mut self, coeffs: &[f32]) -> Result<(), Error> {
        // `Z[p] = (X[2p] + i * X[m - 1 - 2p]) * pre[p]`. The sign of the
        // imaginary part is `(-1)^(m / 2)`, which is `+1` for every
        // supported `m`.
        let even = pairs(coeffs).map(|pair| pair[0]);
        let odd_reversed = pairs(coeffs).rev().map(|pair| pair[1]);
        for (((z, re), im), &pre) in self
            .scratch
            .iter_mut()
            .zip(even)
            .zip(odd_reversed)
            .zip(&self.pre)
        {
            *z = Complex32::new(re, im) * pre;
        }
        self.fft.process(&mut self.scratch)
    }
}

/// Consecutive pairs of an even-length slice.
fn pairs(values: &[f32]) -> core::slice::Iter<'_, [f32; 2]> {
    values.as_chunks::<2>().0.iter()
}

/// Consecutive mutable pairs of an even-length slice.
fn pairs_mut(values: &mut [f32]) -> core::slice::IterMut<'_, [f32; 2]> {
    values.as_chunks_mut::<2>().0.iter_mut()
}

/// Post-rotated FFT bins in the order the output needs them: for each `q` in
/// `0..m / 4`, the bins `m / 4 - 1 - q` and `m / 4 + q` of the `m / 2` bins.
fn rotated<'a>(
    bins: &'a [Complex32],
    post: &'a [Complex32],
) -> impl Iterator<Item = (Complex32, Complex32)> + 'a {
    let quarter = bins.len() / 2;
    let (bins_low, bins_high) = bins.split_at(quarter);
    let (post_low, post_high) = post.split_at(quarter);
    let low = bins_low.iter().rev().zip(post_low.iter().rev());
    let high = bins_high.iter().zip(post_high);
    low.zip(high)
        .map(|((&f_lo, &p_lo), (&f_hi, &p_hi))| (f_lo * p_lo, f_hi * p_hi))
}

#[cfg(test)]
mod tests {
    use super::super::{WindowShape, noise};
    use super::*;

    /// Direct inverse transform in `f64`.
    fn imdct_direct(coeffs: &[f32], scale: f64) -> Vec<f64> {
        let m = coeffs.len();
        let mf = m as f64;
        (0..2 * m)
            .map(|n| {
                let phase = core::f64::consts::PI / mf * (n as f64 + 0.5 + mf / 2.0);
                scale
                    * coeffs
                        .iter()
                        .enumerate()
                        .map(|(k, &x)| f64::from(x) * (phase * (k as f64 + 0.5)).cos())
                        .sum::<f64>()
            })
            .collect()
    }

    /// Direct forward transform in `f64` of `2 * m` windowed samples.
    fn mdct_direct(samples: &[f64]) -> Vec<f32> {
        let m = samples.len() / 2;
        let mf = m as f64;
        (0..m)
            .map(|k| {
                samples
                    .iter()
                    .enumerate()
                    .map(|(n, &x)| {
                        let phase = core::f64::consts::PI / mf * (n as f64 + 0.5 + mf / 2.0);
                        x * (phase * (k as f64 + 0.5)).cos()
                    })
                    .sum::<f64>() as f32
            })
            .collect()
    }

    #[test]
    fn imdct_matches_direct_formula() {
        for m in [4, 8, 16, 64, 128, 512, 1024, 4096] {
            let mut imdct = Imdct::new(m, 0.5).unwrap();
            let coeffs = noise(m as u32, m);
            let expected = imdct_direct(&coeffs, 0.5);
            let tolerance = 2e-7 * (m as f64).sqrt() * (m as f64).log2();

            let mut raw = vec![0.0; 2 * m];
            imdct.imdct_raw(&coeffs, &mut raw).unwrap();
            for (n, (&got, &want)) in raw.iter().zip(&expected).enumerate() {
                assert!((f64::from(got) - want).abs() < tolerance, "m {m} n {n}");
            }

            // Fused path with a random window and overlap.
            let window = noise(m as u32 + 1, 2 * m);
            let start = noise(m as u32 + 2, m);
            let mut overlap = start.clone();
            let mut out = vec![0.0; m];
            imdct
                .process(&coeffs, &window, &mut overlap, &mut out)
                .unwrap();
            for n in 0..m {
                let first = f64::from(start[n]) + expected[n] * f64::from(window[n]);
                let second = expected[m + n] * f64::from(window[m + n]);
                assert!((f64::from(out[n]) - first).abs() < tolerance, "m {m} n {n}");
                assert!((f64::from(overlap[n]) - second).abs() < tolerance);
            }
        }
    }

    #[test]
    fn fft_and_fused_imdct_tdac() {
        let shapes = [
            WindowShape::Sine,
            WindowShape::Vorbis,
            WindowShape::Kbd(4.0),
            WindowShape::Kbd(6.0),
        ];
        // The direct forward transform in the test is quadratic; the largest
        // size is checked against the direct inverse formula above.
        for m in [4, 128, 256, 1024] {
            for shape in shapes {
                let window = shape.generate(2 * m).unwrap();
                let mut imdct = Imdct::new(m, 2.0 / m as f32).unwrap();
                let blocks = 4;
                let signal: Vec<f64> = noise(m as u32 * 3, (blocks + 1) * m)
                    .into_iter()
                    .map(f64::from)
                    .collect();
                let mut overlap = vec![0.0; m];
                let mut out = vec![0.0; m];
                let mut worst = 0.0f64;
                for block in 0..blocks {
                    let frame: Vec<f64> = signal[block * m..(block + 2) * m]
                        .iter()
                        .zip(&window)
                        .map(|(&x, &w)| x * f64::from(w))
                        .collect();
                    let coeffs = mdct_direct(&frame);
                    imdct
                        .process(&coeffs, &window, &mut overlap, &mut out)
                        .unwrap();
                    // Block `b` completes samples `b * m..(b + 1) * m`; the
                    // first block lacks its left neighbour.
                    if block > 0 {
                        for (&got, &want) in out.iter().zip(&signal[block * m..]) {
                            worst = worst.max((f64::from(got) - want).abs());
                        }
                    }
                }
                assert!(worst < 1e-5, "m {m} {shape:?}: {worst}");
            }
        }

        // Asymmetric pair, as AAC switches block sizes: a start window
        // (sine rise; flat top, the fall of a short sine window, zeros)
        // followed by a stop window whose rise mirrors that fall.
        let m = 256;
        let short = WindowShape::Sine.generate(64).unwrap();
        let sine = WindowShape::Sine.generate(2 * m).unwrap();
        let mut start = sine.clone();
        // Second half: 1.0, then the falling half of the short window, then 0.
        let flat = (m - 32) / 2;
        start[m..m + flat].fill(1.0);
        start[m + flat..m + flat + 32].copy_from_slice(&short[32..]);
        start[m + flat + 32..].fill(0.0);
        let mut stop = sine;
        stop[..flat].fill(0.0);
        stop[flat..flat + 32].copy_from_slice(&short[..32]);
        stop[flat + 32..m].fill(1.0);
        let mut imdct = Imdct::new(m, 2.0 / m as f32).unwrap();
        let signal: Vec<f64> = noise(99, 3 * m).into_iter().map(f64::from).collect();
        let mut overlap = vec![0.0; m];
        let mut out = vec![0.0; m];
        for (block, window) in [&start, &stop].into_iter().enumerate() {
            let frame: Vec<f64> = signal[block * m..(block + 2) * m]
                .iter()
                .zip(window)
                .map(|(&x, &w)| x * f64::from(w))
                .collect();
            let coeffs = mdct_direct(&frame);
            imdct
                .process(&coeffs, window, &mut overlap, &mut out)
                .unwrap();
        }
        for (&got, &want) in out.iter().zip(&signal[m..]) {
            assert!((f64::from(got) - want).abs() < 1e-5);
        }
    }

    #[test]
    fn imdct_rejects_bad_sizes() {
        for m in [0, 1, 2, 3, 6, 8192] {
            assert_eq!(
                Imdct::new(m, 1.0).unwrap_err().kind,
                ErrorKind::UnsupportedFeature
            );
        }
        let mut imdct = Imdct::new(8, 1.0).unwrap();
        assert_eq!(imdct.len(), 8);
        let mut overlap = [7.0; 8];
        let mut out = [7.0; 8];
        let window = [1.0; 16];
        let errors = [
            imdct.process(&[0.0; 4], &window, &mut overlap, &mut out),
            imdct.process(&[0.0; 8], &window[..8], &mut overlap, &mut out),
            imdct.process(&[0.0; 8], &window, &mut overlap[..4], &mut out),
            imdct.process(&[0.0; 8], &window, &mut overlap, &mut out[..4]),
            imdct.imdct_raw(&[0.0; 8], &mut out),
        ];
        for result in errors {
            assert_eq!(result.unwrap_err().kind, ErrorKind::InvalidData);
        }
        assert_eq!((overlap, out), ([7.0; 8], [7.0; 8]));
    }
}

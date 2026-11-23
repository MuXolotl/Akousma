//! Transforms for frequency-domain codecs.
//!
//! * [`Complex32`]: a complex number of two `f32`.
//! * [`Fft`]: an in-place complex fast Fourier transform.
//! * [`Imdct`]: an inverse modified discrete cosine transform whose last
//!   step also applies the window and adds the overlap of the previous
//!   block, writing finished samples in one pass.
//! * [`WindowShape`]: the sine, Vorbis and Kaiser-Bessel-derived windows.
//!
//! Every table is computed in `f64` and stored as `f32`. Once created, the
//! transforms do not allocate.
//!
//! Lengths supported by [`Fft`] and [`Imdct`] are listed on their
//! constructors; other lengths fail with
//! [`ErrorKind::UnsupportedFeature`].

mod fft;
mod imdct;

use core::ops::{Add, Mul, Neg, Sub};

use crate::error::{Error, ErrorKind};

pub(crate) use fft::Fft;

/// A complex number with `f32` parts.
///
/// ```ignore
/// use akousma_core::math::Complex32;
///
/// let i = Complex32::new(0.0, 1.0);
/// assert_eq!(i * i, Complex32::new(-1.0, 0.0));
/// assert_eq!(Complex32::new(1.0, 2.0).conj(), Complex32::new(1.0, -2.0));
/// ```
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Complex32 {
    /// Real part.
    pub re: f32,
    /// Imaginary part.
    pub im: f32,
}

impl Complex32 {
    /// Creates `re + i * im`.
    #[must_use]
    pub const fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    /// Complex conjugate, `re - i * im`.
    #[must_use]
    pub const fn conj(self) -> Self {
        Self::new(self.re, -self.im)
    }

    /// `exp(-i * angle)` rounded from `f64`.
    fn expi_neg(angle: f64) -> Self {
        let (sin, cos) = angle.sin_cos();
        Self::new(cos as f32, -sin as f32)
    }
}

impl Add for Complex32 {
    type Output = Self;

    fn add(self, rhs: Self) -> Self {
        Self::new(self.re + rhs.re, self.im + rhs.im)
    }
}

impl Sub for Complex32 {
    type Output = Self;

    fn sub(self, rhs: Self) -> Self {
        Self::new(self.re - rhs.re, self.im - rhs.im)
    }
}

impl Mul for Complex32 {
    type Output = Self;

    fn mul(self, rhs: Self) -> Self {
        Self::new(
            self.re * rhs.re - self.im * rhs.im,
            self.re * rhs.im + self.im * rhs.re,
        )
    }
}

impl Mul<f32> for Complex32 {
    type Output = Self;

    fn mul(self, rhs: f32) -> Self {
        Self::new(self.re * rhs, self.im * rhs)
    }
}

impl Neg for Complex32 {
    type Output = Self;

    fn neg(self) -> Self {
        Self::new(-self.re, -self.im)
    }
}

/// Shape of a window for [`Imdct::process`].
///
/// A window of length `2 * m` has values `w[n]` for `n` in `0..2 * m`. All
/// three shapes are symmetric (`w[2 * m - 1 - n] == w[n]`) and satisfy the
/// Princen-Bradley condition `w[n]^2 + w[n + m]^2 == 1`, so overlapping
/// blocks reconstruct the signal.
///
/// ```ignore
/// use akousma_core::math::WindowShape;
///
/// let w = WindowShape::Sine.generate(8)?;
/// assert_eq!(w.len(), 8);
/// for n in 0..4 {
///     assert!((w[n] * w[n] + w[n + 4] * w[n + 4] - 1.0).abs() < 1e-6);
///     assert_eq!(w[n], w[7 - n]);
/// }
/// # Ok::<(), akousma_core::Error>(())
/// ```
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum WindowShape {
    /// `w[n] = sin(pi / (2 * m) * (n + 0.5))`.
    Sine,
    /// `w[n] = sin(pi / 2 * sin^2(pi / (2 * m) * (n + 0.5)))`, the window of
    /// Vorbis I.
    Vorbis,
    /// Kaiser-Bessel-derived window with the given `alpha` (AAC uses `4.0`
    /// for long and `6.0` for short blocks):
    /// `w[n] = sqrt(sum(v[0..=n]) / sum(v[0..=m]))` for `n < m`, with
    /// `v[j] = I0(pi * alpha * sqrt(1 - (2 * j / m - 1)^2))`, where `I0` is
    /// the modified Bessel function of the first kind of order zero.
    Kbd(f64),
}

impl WindowShape {
    /// Computes the window of length `len`.
    ///
    /// # Errors
    ///
    /// [`ErrorKind::UnsupportedFeature`] if `len` is zero or odd, and
    /// [`ErrorKind::InvalidData`] if a [`WindowShape::Kbd`] alpha is negative
    /// or not finite.
    pub fn generate(self, len: usize) -> Result<Vec<f32>, Error> {
        if len == 0 || !len.is_multiple_of(2) {
            return Err(ErrorKind::UnsupportedFeature.into());
        }
        let m = len / 2;
        let mut window = vec![0.0f32; len];
        let (rising, falling) = window.split_at_mut(m);
        match self {
            Self::Sine | Self::Vorbis => {
                let step = core::f64::consts::PI / (2 * m) as f64;
                for (n, w) in rising.iter_mut().enumerate() {
                    let sine = (step * (n as f64 + 0.5)).sin();
                    let value = if self == Self::Sine {
                        sine
                    } else {
                        (core::f64::consts::FRAC_PI_2 * sine * sine).sin()
                    };
                    *w = value as f32;
                }
            }
            Self::Kbd(alpha) => {
                if !alpha.is_finite() || alpha < 0.0 {
                    return Err(ErrorKind::InvalidData.into());
                }
                let kernel = |j: usize| {
                    let x = 2.0 * j as f64 / m as f64 - 1.0;
                    bessel_i0(core::f64::consts::PI * alpha * (1.0 - x * x).max(0.0).sqrt())
                };
                let total: f64 = (0..=m).map(kernel).sum();
                let mut sum = 0.0;
                for (j, w) in rising.iter_mut().enumerate() {
                    sum += kernel(j);
                    *w = (sum / total).sqrt() as f32;
                }
            }
        }
        for (w, &mirror) in falling.iter_mut().zip(rising.iter().rev()) {
            *w = mirror;
        }
        Ok(window)
    }
}

/// Deterministic pseudo-random values in `-1.0..1.0` for tests.
#[cfg(test)]
fn noise(seed: u32, len: usize) -> Vec<f32> {
    let mut state = seed.wrapping_mul(2_654_435_761).wrapping_add(1);
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            (state as f32 / u32::MAX as f32) * 2.0 - 1.0
        })
        .collect()
}

/// Modified Bessel function of the first kind of order zero, from its power
/// series `sum(((x / 2)^k / k!)^2)`, summed until a term no longer changes
/// the sum.
fn bessel_i0(x: f64) -> f64 {
    let half = x / 2.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    let mut k = 1.0;
    loop {
        let ratio = half / k;
        term *= ratio * ratio;
        if sum + term == sum {
            return sum;
        }
        sum += term;
        k += 1.0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complex_arithmetic() {
        let a = Complex32::new(1.0, 2.0);
        let b = Complex32::new(3.0, -1.0);
        assert_eq!(a + b, Complex32::new(4.0, 1.0));
        assert_eq!(a - b, Complex32::new(-2.0, 3.0));
        assert_eq!(a * b, Complex32::new(5.0, 5.0));
        assert_eq!(a * 2.0, Complex32::new(2.0, 4.0));
        assert_eq!(-a, Complex32::new(-1.0, -2.0));
        let q = Complex32::expi_neg(core::f64::consts::FRAC_PI_2);
        assert!(q.re.abs() < 1e-7 && (q.im + 1.0).abs() < 1e-7);
    }

    #[test]
    fn bessel_i0_values() {
        // I0(0) = 1; I0(1) and I0(10) from the power series in exact
        // rational arithmetic, rounded to 15 significant digits.
        assert_eq!(bessel_i0(0.0), 1.0);
        assert!((bessel_i0(1.0) - 1.266_065_877_752_01).abs() < 1e-14);
        assert!((bessel_i0(10.0) / 2_815.716_628_466_25 - 1.0).abs() < 1e-14);
    }

    #[test]
    fn windows_are_symmetric_and_power_complementary() {
        for shape in [
            WindowShape::Sine,
            WindowShape::Vorbis,
            WindowShape::Kbd(4.0),
            WindowShape::Kbd(6.0),
            WindowShape::Kbd(0.0),
        ] {
            for len in [2, 8, 256, 2048, 8192] {
                let w = shape.generate(len).unwrap();
                let m = len / 2;
                for n in 0..m {
                    assert_eq!(w[n], w[len - 1 - n]);
                    let power = f64::from(w[n]).powi(2) + f64::from(w[n + m]).powi(2);
                    assert!((power - 1.0).abs() < 1e-6, "{shape:?} {len} {n}");
                    assert!((0.0..=1.0).contains(&w[n]));
                }
                // Rising first half.
                assert!(w[..m].windows(2).all(|pair| pair[0] <= pair[1]));
            }
        }
        // Spot values: sine at n = 0 of a 4-point window is sin(pi / 8),
        // computed in `f64`.
        let sine = WindowShape::Sine.generate(4).unwrap();
        assert_eq!(sine[0], core::f64::consts::FRAC_PI_8.sin() as f32);
        // KBD with alpha 0 has a constant kernel: w[n] = sqrt((n + 1) / (m + 1)).
        let flat = WindowShape::Kbd(0.0).generate(8).unwrap();
        assert!((flat[0] - (1.0f32 / 5.0).sqrt()).abs() < 1e-7);

        for len in [0, 3, 7] {
            assert_eq!(
                WindowShape::Sine.generate(len).unwrap_err().kind,
                ErrorKind::UnsupportedFeature
            );
        }
        for alpha in [-1.0, f64::NAN, f64::INFINITY] {
            assert_eq!(
                WindowShape::Kbd(alpha).generate(8).unwrap_err().kind,
                ErrorKind::InvalidData
            );
        }
    }
}

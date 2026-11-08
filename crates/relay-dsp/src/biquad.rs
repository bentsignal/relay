//! RBJ cookbook biquads in transposed direct form II.

use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FilterKind {
    #[default]
    Peak,
    LowShelf,
    HighShelf,
    LowPass,
    HighPass,
    Notch,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BiquadCoeffs {
    pub b0: f32,
    pub b1: f32,
    pub b2: f32,
    pub a1: f32,
    pub a2: f32,
}

impl BiquadCoeffs {
    pub const IDENTITY: Self = Self { b0: 1.0, b1: 0.0, b2: 0.0, a1: 0.0, a2: 0.0 };

    pub fn design(kind: FilterKind, freq: f32, q: f32, gain_db: f32, sample_rate: f32) -> Self {
        let freq = freq.clamp(10.0, sample_rate * 0.49);
        let q = q.clamp(0.05, 40.0);
        let w0 = 2.0 * PI * freq / sample_rate;
        let (sin, cos) = w0.sin_cos();
        let alpha = sin / (2.0 * q);
        let a = 10f32.powf(gain_db / 40.0);
        let (b0, b1, b2, a0, a1, a2) = match kind {
            FilterKind::Peak => {
                (1.0 + alpha * a, -2.0 * cos, 1.0 - alpha * a, 1.0 + alpha / a, -2.0 * cos, 1.0 - alpha / a)
            }
            FilterKind::LowShelf => {
                let sq = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) - (a - 1.0) * cos + sq),
                    2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                    a * ((a + 1.0) - (a - 1.0) * cos - sq),
                    (a + 1.0) + (a - 1.0) * cos + sq,
                    -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                    (a + 1.0) + (a - 1.0) * cos - sq,
                )
            }
            FilterKind::HighShelf => {
                let sq = 2.0 * a.sqrt() * alpha;
                (
                    a * ((a + 1.0) + (a - 1.0) * cos + sq),
                    -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                    a * ((a + 1.0) + (a - 1.0) * cos - sq),
                    (a + 1.0) - (a - 1.0) * cos + sq,
                    2.0 * ((a - 1.0) - (a + 1.0) * cos),
                    (a + 1.0) - (a - 1.0) * cos - sq,
                )
            }
            FilterKind::LowPass => {
                ((1.0 - cos) / 2.0, 1.0 - cos, (1.0 - cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            FilterKind::HighPass => {
                ((1.0 + cos) / 2.0, -(1.0 + cos), (1.0 + cos) / 2.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha)
            }
            FilterKind::Notch => (1.0, -2.0 * cos, 1.0, 1.0 + alpha, -2.0 * cos, 1.0 - alpha),
        };
        Self { b0: b0 / a0, b1: b1 / a0, b2: b2 / a0, a1: a1 / a0, a2: a2 / a0 }
    }

    /// Magnitude response in dB at `freq`, used by the UI to draw EQ curves.
    pub fn magnitude_db(&self, freq: f32, sample_rate: f32) -> f32 {
        let w = 2.0 * PI * freq / sample_rate;
        let (s1, c1) = w.sin_cos();
        let (s2, c2) = (2.0 * w).sin_cos();
        let nr = self.b0 + self.b1 * c1 + self.b2 * c2;
        let ni = -(self.b1 * s1 + self.b2 * s2);
        let dr = 1.0 + self.a1 * c1 + self.a2 * c2;
        let di = -(self.a1 * s1 + self.a2 * s2);
        let num = nr * nr + ni * ni;
        let den = (dr * dr + di * di).max(1e-20);
        10.0 * (num / den).max(1e-20).log10()
    }
}

/// One stereo biquad section.
#[derive(Clone, Debug)]
pub struct Biquad {
    pub coeffs: BiquadCoeffs,
    z: [[f32; 2]; 2],
}

impl Default for Biquad {
    fn default() -> Self {
        Self { coeffs: BiquadCoeffs::IDENTITY, z: [[0.0; 2]; 2] }
    }
}

impl Biquad {
    pub fn new(coeffs: BiquadCoeffs) -> Self {
        Self { coeffs, z: [[0.0; 2]; 2] }
    }

    pub fn reset(&mut self) {
        self.z = [[0.0; 2]; 2];
    }

    #[inline]
    fn tick(c: &BiquadCoeffs, z: &mut [f32; 2], x: f32) -> f32 {
        let y = c.b0 * x + z[0];
        z[0] = c.b1 * x - c.a1 * y + z[1];
        z[1] = c.b2 * x - c.a2 * y;
        y
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let c = self.coeffs;
        let [zl, zr] = &mut self.z;
        for s in left.iter_mut() {
            *s = Self::tick(&c, zl, *s);
        }
        for s in right.iter_mut() {
            *s = Self::tick(&c, zr, *s);
        }
        // Flush denormals that accumulate in silence.
        for z in [zl, zr] {
            for v in z.iter_mut() {
                if v.abs() < 1e-20 {
                    *v = 0.0;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peak_has_gain_at_center() {
        let c = BiquadCoeffs::design(FilterKind::Peak, 1000.0, 1.0, 6.0, 48000.0);
        assert!((c.magnitude_db(1000.0, 48000.0) - 6.0).abs() < 0.05);
        assert!(c.magnitude_db(50.0, 48000.0).abs() < 0.3);
    }

    #[test]
    fn highpass_attenuates_lows() {
        let c = BiquadCoeffs::design(FilterKind::HighPass, 100.0, 0.707, 0.0, 48000.0);
        assert!(c.magnitude_db(20.0, 48000.0) < -20.0);
        assert!(c.magnitude_db(5000.0, 48000.0).abs() < 0.1);
    }

    #[test]
    fn process_matches_response() {
        let c = BiquadCoeffs::design(FilterKind::Peak, 1000.0, 1.0, -12.0, 48000.0);
        let mut bq = Biquad::new(c);
        let n = 48000;
        let mut l: Vec<f32> = (0..n).map(|i| (2.0 * PI * 1000.0 * i as f32 / 48000.0).sin()).collect();
        let mut r = l.clone();
        bq.process(&mut l, &mut r);
        let peak = l[n / 2..].iter().fold(0f32, |m, s| m.max(s.abs()));
        let db = 20.0 * peak.log10();
        assert!((db + 12.0).abs() < 0.2, "{db}");
    }
}

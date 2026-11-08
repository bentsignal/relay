//! Parametric EQ made of up to [`MAX_EQ_BANDS`] biquads.

use crate::biquad::{Biquad, BiquadCoeffs, FilterKind};
use serde::{Deserialize, Serialize};

pub const MAX_EQ_BANDS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct EqBand {
    pub enabled: bool,
    pub kind: FilterKind,
    pub freq: f32,
    pub q: f32,
    pub gain_db: f32,
}

impl Default for EqBand {
    fn default() -> Self {
        Self {
            enabled: true,
            kind: FilterKind::Peak,
            freq: 1000.0,
            q: 1.0,
            gain_db: 0.0,
        }
    }
}

impl EqBand {
    pub fn coeffs(&self, sample_rate: f32) -> BiquadCoeffs {
        BiquadCoeffs::design(self.kind, self.freq, self.q, self.gain_db, sample_rate)
    }

    /// True when the band changes nothing and can be skipped.
    pub fn is_neutral(&self) -> bool {
        !self.enabled
            || (matches!(
                self.kind,
                FilterKind::Peak | FilterKind::LowShelf | FilterKind::HighShelf
            ) && self.gain_db.abs() < 0.01)
    }

    /// A sensible five-band starting layout.
    pub fn default_layout() -> Vec<EqBand> {
        vec![
            EqBand {
                enabled: false,
                kind: FilterKind::HighPass,
                freq: 80.0,
                q: 0.707,
                gain_db: 0.0,
            },
            EqBand {
                kind: FilterKind::LowShelf,
                freq: 120.0,
                q: 0.707,
                ..Default::default()
            },
            EqBand {
                freq: 500.0,
                ..Default::default()
            },
            EqBand {
                freq: 3000.0,
                ..Default::default()
            },
            EqBand {
                kind: FilterKind::HighShelf,
                freq: 8000.0,
                q: 0.707,
                ..Default::default()
            },
        ]
    }
}

/// Realtime EQ processor. `set_bands` recomputes coefficients without
/// resetting filter state, so parameter drags don't click.
#[derive(Clone, Debug)]
pub struct Eq {
    sample_rate: f32,
    bands: [EqBand; MAX_EQ_BANDS],
    filters: [Biquad; MAX_EQ_BANDS],
    active: [bool; MAX_EQ_BANDS],
    count: usize,
}

impl Eq {
    pub fn new(sample_rate: f32) -> Self {
        Self {
            sample_rate,
            bands: [EqBand::default(); MAX_EQ_BANDS],
            filters: Default::default(),
            active: [false; MAX_EQ_BANDS],
            count: 0,
        }
    }

    pub fn set_bands(&mut self, bands: &[EqBand]) {
        self.count = bands.len().min(MAX_EQ_BANDS);
        for i in 0..MAX_EQ_BANDS {
            if i < self.count {
                let b = bands[i];
                let was_active = self.active[i];
                self.active[i] = !b.is_neutral();
                if self.bands[i] != b || !was_active {
                    self.bands[i] = b;
                    self.filters[i].coeffs = b.coeffs(self.sample_rate);
                    if !was_active {
                        self.filters[i].reset();
                    }
                }
            } else {
                self.active[i] = false;
            }
        }
    }

    pub fn is_bypassed(&self) -> bool {
        !self.active[..self.count].iter().any(|a| *a)
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        for i in 0..self.count {
            if self.active[i] {
                self.filters[i].process(left, right);
            }
        }
    }

    /// Combined magnitude response, for drawing.
    pub fn response_db(bands: &[EqBand], freq: f32, sample_rate: f32) -> f32 {
        bands
            .iter()
            .filter(|b| !b.is_neutral())
            .map(|b| b.coeffs(sample_rate).magnitude_db(freq, sample_rate))
            .sum()
    }
}

//! Realtime-safe DSP for Relay.
//!
//! Everything here is allocation-free after construction and safe to call from
//! an audio thread. Audio is processed as planar stereo blocks (`left`, `right`).

pub mod biquad;
pub mod dynamics;
pub mod eq;
pub mod meter;
pub mod resampler;
pub mod smooth;

pub use biquad::{Biquad, BiquadCoeffs, FilterKind};
pub use dynamics::{Compressor, CompressorParams, Gate, GateParams, Limiter, LimiterParams};
pub use eq::{Eq, EqBand, MAX_EQ_BANDS};
pub use meter::{Meter, MeterReading};
pub use resampler::Resampler;
pub use smooth::SmoothedGain;

/// The engine's internal sample rate.
pub const ENGINE_RATE: u32 = 48_000;

#[inline]
pub fn db_to_gain(db: f32) -> f32 {
    if db <= -120.0 { 0.0 } else { 10f32.powf(db / 20.0) }
}

#[inline]
pub fn gain_to_db(gain: f32) -> f32 {
    if gain <= 1e-6 { -120.0 } else { 20.0 * gain.log10() }
}

/// Planar stereo buffer view used throughout the engine.
pub struct StereoMut<'a> {
    pub left: &'a mut [f32],
    pub right: &'a mut [f32],
}

impl StereoMut<'_> {
    pub fn len(&self) -> usize {
        self.left.len()
    }
    pub fn is_empty(&self) -> bool {
        self.left.is_empty()
    }
}

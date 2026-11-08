//! Stereo-linked gate, compressor and lookahead limiter.

use serde::{Deserialize, Serialize};

#[inline]
fn coef(ms: f32, sample_rate: f32) -> f32 {
    if ms <= 0.0 {
        0.0
    } else {
        (-1.0 / (ms * 0.001 * sample_rate)).exp()
    }
}

#[inline]
fn lin_to_db(x: f32) -> f32 {
    if x <= 1e-9 { -180.0 } else { 20.0 * x.log10() }
}

#[inline]
fn db_to_lin(db: f32) -> f32 {
    10f32.powf(db * 0.05)
}

// ---------------------------------------------------------------- Gate

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct GateParams {
    pub enabled: bool,
    pub threshold_db: f32,
    /// Attenuation applied when closed, as a positive number of dB.
    pub range_db: f32,
    pub attack_ms: f32,
    pub hold_ms: f32,
    pub release_ms: f32,
}

impl Default for GateParams {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -45.0,
            range_db: 60.0,
            attack_ms: 1.0,
            hold_ms: 80.0,
            release_ms: 150.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Gate {
    params: GateParams,
    sample_rate: f32,
    env: f32,
    gain: f32,
    hold_left: u32,
    att: f32,
    rel: f32,
    env_rel: f32,
}

impl Gate {
    pub fn new(sample_rate: f32) -> Self {
        let mut g = Self {
            params: GateParams::default(),
            sample_rate,
            env: 0.0,
            gain: 1.0,
            hold_left: 0,
            att: 0.0,
            rel: 0.0,
            env_rel: 0.0,
        };
        g.set_params(GateParams::default());
        g
    }

    pub fn set_params(&mut self, p: GateParams) {
        self.params = p;
        self.att = coef(p.attack_ms, self.sample_rate);
        self.rel = coef(p.release_ms, self.sample_rate);
        self.env_rel = coef(20.0, self.sample_rate);
    }

    /// Current gain reduction in dB (positive), for display.
    pub fn reduction_db(&self) -> f32 {
        -lin_to_db(self.gain).min(0.0)
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.params.enabled {
            self.gain = 1.0;
            return;
        }
        let thr = db_to_lin(self.params.threshold_db);
        let floor = db_to_lin(-self.params.range_db.abs());
        let hold = (self.params.hold_ms * 0.001 * self.sample_rate) as u32;
        for i in 0..left.len() {
            let x = left[i].abs().max(right[i].abs());
            self.env = if x > self.env {
                x
            } else {
                x + self.env_rel * (self.env - x)
            };
            let target = if self.env >= thr {
                self.hold_left = hold;
                1.0
            } else if self.hold_left > 0 {
                self.hold_left -= 1;
                1.0
            } else {
                floor
            };
            let c = if target > self.gain {
                self.att
            } else {
                self.rel
            };
            self.gain = target + c * (self.gain - target);
            left[i] *= self.gain;
            right[i] *= self.gain;
        }
    }
}

// ---------------------------------------------------------------- Compressor

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CompressorParams {
    pub enabled: bool,
    pub threshold_db: f32,
    pub ratio: f32,
    pub knee_db: f32,
    pub attack_ms: f32,
    pub release_ms: f32,
    pub makeup_db: f32,
}

impl Default for CompressorParams {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_db: -18.0,
            ratio: 3.0,
            knee_db: 6.0,
            attack_ms: 5.0,
            release_ms: 120.0,
            makeup_db: 0.0,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Compressor {
    params: CompressorParams,
    sample_rate: f32,
    att: f32,
    rel: f32,
    /// Smoothed gain reduction in dB (>= 0).
    gr_db: f32,
}

impl Compressor {
    pub fn new(sample_rate: f32) -> Self {
        let mut c = Self {
            params: CompressorParams::default(),
            sample_rate,
            att: 0.0,
            rel: 0.0,
            gr_db: 0.0,
        };
        c.set_params(CompressorParams::default());
        c
    }

    pub fn set_params(&mut self, p: CompressorParams) {
        self.params = p;
        self.att = coef(p.attack_ms, self.sample_rate);
        self.rel = coef(p.release_ms, self.sample_rate);
    }

    pub fn reduction_db(&self) -> f32 {
        self.gr_db
    }

    /// Static gain computer: input level (dB) -> gain reduction (dB, >= 0).
    pub fn static_reduction(p: &CompressorParams, level_db: f32) -> f32 {
        let ratio = p.ratio.max(1.0);
        let slope = 1.0 - 1.0 / ratio;
        let over = level_db - p.threshold_db;
        let knee = p.knee_db.max(0.0);
        if knee > 0.0 && over.abs() <= knee / 2.0 {
            slope * (over + knee / 2.0).powi(2) / (2.0 * knee)
        } else if over > 0.0 {
            slope * over
        } else {
            0.0
        }
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.params.enabled {
            self.gr_db = 0.0;
            return;
        }
        let p = self.params;
        let makeup = p.makeup_db;
        for i in 0..left.len() {
            let x = left[i].abs().max(right[i].abs());
            let target = Self::static_reduction(&p, lin_to_db(x));
            let c = if target > self.gr_db {
                self.att
            } else {
                self.rel
            };
            self.gr_db = target + c * (self.gr_db - target);
            let g = db_to_lin(makeup - self.gr_db);
            left[i] *= g;
            right[i] *= g;
        }
    }
}

// ---------------------------------------------------------------- Limiter

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct LimiterParams {
    pub enabled: bool,
    pub ceiling_db: f32,
    pub release_ms: f32,
}

impl Default for LimiterParams {
    fn default() -> Self {
        Self {
            enabled: false,
            ceiling_db: -1.0,
            release_ms: 60.0,
        }
    }
}

const LOOKAHEAD: usize = 48; // 1 ms at 48 kHz

/// Brickwall limiter with 1 ms lookahead. Adds exactly [`LOOKAHEAD`] samples of
/// latency while enabled.
#[derive(Clone, Debug)]
pub struct Limiter {
    params: LimiterParams,
    sample_rate: f32,
    rel: f32,
    delay_l: [f32; LOOKAHEAD],
    delay_r: [f32; LOOKAHEAD],
    need: [f32; LOOKAHEAD],
    pos: usize,
    gain: f32,
}

impl Limiter {
    pub fn new(sample_rate: f32) -> Self {
        let mut l = Self {
            params: LimiterParams::default(),
            sample_rate,
            rel: 0.0,
            delay_l: [0.0; LOOKAHEAD],
            delay_r: [0.0; LOOKAHEAD],
            need: [1.0; LOOKAHEAD],
            pos: 0,
            gain: 1.0,
        };
        l.set_params(LimiterParams::default());
        l
    }

    pub fn set_params(&mut self, p: LimiterParams) {
        if p.enabled != self.params.enabled {
            self.delay_l = [0.0; LOOKAHEAD];
            self.delay_r = [0.0; LOOKAHEAD];
            self.need = [1.0; LOOKAHEAD];
            self.gain = 1.0;
        }
        self.params = p;
        self.rel = coef(p.release_ms, self.sample_rate);
    }

    pub fn reduction_db(&self) -> f32 {
        -lin_to_db(self.gain).min(0.0)
    }

    pub fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if !self.params.enabled {
            self.gain = 1.0;
            return;
        }
        let ceil = db_to_lin(self.params.ceiling_db);
        // Per-sample attack that reaches any target within the lookahead window.
        let att = coef(
            LOOKAHEAD as f32 / self.sample_rate * 1000.0 / 5.0,
            self.sample_rate,
        );
        for i in 0..left.len() {
            let xl = left[i];
            let xr = right[i];
            let peak = xl.abs().max(xr.abs());
            self.need[self.pos] = if peak > ceil { ceil / peak } else { 1.0 };
            let target = self.need.iter().copied().fold(1.0f32, f32::min);
            let c = if target < self.gain { att } else { self.rel };
            self.gain = target + c * (self.gain - target);
            let dl = self.delay_l[self.pos];
            let dr = self.delay_r[self.pos];
            self.delay_l[self.pos] = xl;
            self.delay_r[self.pos] = xr;
            self.pos = (self.pos + 1) % LOOKAHEAD;
            left[i] = (dl * self.gain).clamp(-ceil, ceil);
            right[i] = (dr * self.gain).clamp(-ceil, ceil);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressor_static_curve() {
        let p = CompressorParams {
            knee_db: 0.0,
            threshold_db: -20.0,
            ratio: 4.0,
            ..Default::default()
        };
        assert_eq!(Compressor::static_reduction(&p, -30.0), 0.0);
        assert!((Compressor::static_reduction(&p, -10.0) - 7.5).abs() < 1e-4);
    }

    #[test]
    fn limiter_never_exceeds_ceiling() {
        let mut lim = Limiter::new(48000.0);
        lim.set_params(LimiterParams {
            enabled: true,
            ceiling_db: -1.0,
            release_ms: 50.0,
        });
        let ceil = db_to_lin(-1.0) + 1e-6;
        let mut l: Vec<f32> = (0..4800)
            .map(|i| if i % 700 < 30 { 1.8 } else { 0.3 })
            .collect();
        let mut r = l.clone();
        lim.process(&mut l, &mut r);
        assert!(l.iter().all(|s| s.abs() <= ceil));
    }

    #[test]
    fn gate_closes_on_silence() {
        let mut g = Gate::new(48000.0);
        g.set_params(GateParams {
            enabled: true,
            hold_ms: 0.0,
            release_ms: 5.0,
            ..Default::default()
        });
        let mut l = vec![0.0001f32; 48000];
        let mut r = l.clone();
        g.process(&mut l, &mut r);
        assert!(l[47999].abs() < 0.0001 * 0.01);
    }
}

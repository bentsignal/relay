//! Clock bridges: connect a pipe running on a foreign clock (a device, a
//! network stream) to the mixer clock, converting sample rate and steering a
//! resampler so the pipe's fill stays just above a safety margin.

use crate::pipe::{PipeReader, PipeShared, PipeWriter};
use relay_dsp::{ENGINE_RATE, Resampler};
use std::time::Duration;

/// Port statistics surfaced in the UI's details view.
#[derive(Clone, Copy, Debug, Default)]
pub struct BridgeStats {
    /// Average buffered audio in the pipe, milliseconds.
    pub buffer_ms: f32,
    pub ratio_ppm: f32,
    pub underruns: u32,
    pub overruns: u32,
}

#[derive(Clone, Copy, Debug)]
pub struct BridgeTuning {
    /// Starting safety margin.
    pub margin: Duration,
    pub min_margin: Duration,
    pub max_margin: Duration,
    /// How much the margin grows after each underrun.
    pub margin_step: Duration,
    /// How long without underruns before the margin shrinks a step.
    pub calm_period: Duration,
}

impl BridgeTuning {
    /// For local hardware devices.
    pub fn device() -> Self {
        Self {
            margin: Duration::from_micros(1500),
            min_margin: Duration::from_micros(1000),
            max_margin: Duration::from_millis(30),
            margin_step: Duration::from_millis(2),
            calm_period: Duration::from_secs(20),
        }
    }

    /// For network streams, which see much more jitter.
    pub fn network() -> Self {
        Self {
            margin: Duration::from_millis(8),
            min_margin: Duration::from_millis(3),
            max_margin: Duration::from_millis(250),
            margin_step: Duration::from_millis(5),
            calm_period: Duration::from_secs(15),
        }
    }
}

struct DriftController {
    rate: f64,
    nominal: f64,
    tuning: BridgeTuning,
    margin_frames: f64,
    /// Mixer-clock time, in seconds, advanced by every read/write.
    now: f64,
    last_update: f64,
    last_underruns: u32,
    calm_since: f64,
    smoothed_fill: f64,
}

/// How often the controller looks at the pipe.
const WINDOW: Duration = Duration::from_millis(250);
/// Time over which excess buffering is drained.
const CORRECTION_TIME: f64 = 1.5;
/// Max steering away from the nominal ratio.
const MAX_ADJUST: f64 = 0.004;
/// Excess beyond which we stop slewing and jump.
const JUMP_EXCESS: Duration = Duration::from_millis(60);

enum Steer {
    Keep,
    Ratio(f64),
    /// Too much buffered: drop this many frames at once.
    Jump(usize, f64),
}

impl DriftController {
    fn new(rate: u32, nominal: f64, tuning: BridgeTuning) -> Self {
        Self {
            rate: rate as f64,
            nominal,
            tuning,
            margin_frames: tuning.margin.as_secs_f64() * rate as f64,
            now: 0.0,
            last_update: 0.0,
            last_underruns: 0,
            calm_since: 0.0,
            smoothed_fill: 0.0,
        }
    }

    fn prime_frames(&self, shared: &PipeShared) -> u32 {
        (self.margin_frames as u32).saturating_add(shared.max_read())
    }

    fn update(&mut self, shared: &PipeShared, mixer_frames: usize) -> Steer {
        self.now += mixer_frames as f64 / ENGINE_RATE as f64;
        let now = self.now;
        if now - self.last_update < WINDOW.as_secs_f64() {
            return Steer::Keep;
        }
        self.last_update = now;

        let underruns = shared.underruns();
        if underruns != self.last_underruns {
            self.last_underruns = underruns;
            self.calm_since = now;
            let max = self.tuning.max_margin.as_secs_f64() * self.rate;
            self.margin_frames =
                (self.margin_frames + self.tuning.margin_step.as_secs_f64() * self.rate).min(max);
            shared.set_prime_frames(self.prime_frames(shared));
        } else if now - self.calm_since > self.tuning.calm_period.as_secs_f64() {
            self.calm_since = now;
            let min = self.tuning.min_margin.as_secs_f64() * self.rate;
            self.margin_frames = (self.margin_frames - 0.0005 * self.rate).max(min);
            shared.set_prime_frames(self.prime_frames(shared));
        }

        let Some(min_fill) = shared.take_min_fill() else {
            return Steer::Keep;
        };
        if !shared.is_primed() {
            return Steer::Keep;
        }
        let min_fill = min_fill as f64;
        self.smoothed_fill = if self.smoothed_fill == 0.0 {
            min_fill
        } else {
            self.smoothed_fill * 0.8 + min_fill * 0.2
        };
        let excess = min_fill - self.margin_frames;
        if excess > JUMP_EXCESS.as_secs_f64() * self.rate {
            self.smoothed_fill = self.margin_frames;
            return Steer::Jump(excess as usize, self.nominal);
        }
        let adjust = (excess / (CORRECTION_TIME * self.rate)).clamp(-MAX_ADJUST, MAX_ADJUST);
        Steer::Ratio(self.nominal * (1.0 + adjust))
    }
}

/// Reads from a pipe on a foreign clock and delivers mixer-rate audio.
pub struct InputBridge {
    reader: PipeReader,
    rs: Resampler,
    ctl: DriftController,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
}

impl InputBridge {
    pub fn new(reader: PipeReader, tuning: BridgeTuning) -> Self {
        let rate = reader.shared().rate;
        let nominal = rate as f64 / ENGINE_RATE as f64;
        let ctl = DriftController::new(rate, nominal, tuning);
        reader
            .shared()
            .set_prime_frames(ctl.margin_frames as u32 + 512);
        Self {
            reader,
            rs: Resampler::new(nominal),
            ctl,
            scratch_l: vec![0.0; 16384],
            scratch_r: vec![0.0; 16384],
        }
    }

    pub fn shared(&self) -> &PipeShared {
        self.reader.shared()
    }

    pub fn stats(&self) -> BridgeStats {
        let s = self.reader.shared();
        BridgeStats {
            buffer_ms: ((self.ctl.smoothed_fill + self.rs.buffered_input()) / self.ctl.rate
                * 1000.0) as f32,
            ratio_ppm: ((self.rs.ratio() / self.rs.nominal_ratio() - 1.0) * 1e6) as f32,
            underruns: s.underruns(),
            overruns: s.overruns(),
        }
    }

    /// Fills `left`/`right` completely; silence where no audio is available.
    /// Returns false if the output is (entirely) silence because no data flowed.
    pub fn read(&mut self, left: &mut [f32], right: &mut [f32]) -> bool {
        let n = left.len();
        match self.ctl.update(self.reader.shared(), n) {
            Steer::Keep => {}
            Steer::Ratio(r) => self.rs.set_ratio(r),
            Steer::Jump(frames, r) => {
                self.reader.skip(frames);
                self.rs.set_ratio(r);
            }
        }
        let need = self
            .rs
            .input_needed(n)
            .min(self.scratch_l.len())
            .min(self.rs.free_space());
        let mut got = 0;
        if need > 0 {
            got = self
                .reader
                .read_planar(&mut self.scratch_l[..need], &mut self.scratch_r[..need]);
            if got == 0 && !self.reader.shared().is_primed() {
                // Not delivering yet; keep the resampler history clean.
                left.fill(0.0);
                right.fill(0.0);
                return false;
            }
            // Underrun frames come back as silence; feed them to keep timing.
            self.rs
                .push(&self.scratch_l[..need], &self.scratch_r[..need]);
        }
        let produced = self.rs.pull(left, right);
        if produced < n {
            left[produced..].fill(0.0);
            right[produced..].fill(0.0);
        }
        got > 0 || need == 0
    }
}

/// Takes mixer-rate audio and writes it into a pipe consumed on a foreign clock.
pub struct OutputBridge {
    writer: PipeWriter,
    rs: Resampler,
    ctl: DriftController,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
}

impl OutputBridge {
    pub fn new(writer: PipeWriter, tuning: BridgeTuning) -> Self {
        let rate = writer.shared().rate;
        let nominal = ENGINE_RATE as f64 / rate as f64;
        let ctl = DriftController::new(rate, nominal, tuning);
        writer
            .shared()
            .set_prime_frames(ctl.margin_frames as u32 + 480);
        Self {
            writer,
            rs: Resampler::new(nominal),
            ctl,
            scratch_l: vec![0.0; 16384],
            scratch_r: vec![0.0; 16384],
        }
    }

    pub fn shared(&self) -> &PipeShared {
        self.writer.shared()
    }

    pub fn stats(&self) -> BridgeStats {
        let s = self.writer.shared();
        BridgeStats {
            buffer_ms: (self.ctl.smoothed_fill / self.ctl.rate * 1000.0) as f32,
            ratio_ppm: ((self.rs.ratio() / self.rs.nominal_ratio() - 1.0) * 1e6) as f32,
            underruns: s.underruns(),
            overruns: s.overruns(),
        }
    }

    pub fn write(&mut self, left: &[f32], right: &[f32]) {
        let mut drop_block = false;
        match self.ctl.update(self.writer.shared(), left.len()) {
            Steer::Keep => {}
            Steer::Ratio(r) => self.rs.set_ratio(r),
            Steer::Jump(_, r) => {
                self.rs.set_ratio(r);
                drop_block = true;
            }
        }
        if drop_block {
            return;
        }
        self.rs.push(left, right);
        loop {
            let n = self.rs.pull(&mut self.scratch_l, &mut self.scratch_r);
            if n == 0 {
                break;
            }
            self.writer
                .write_planar(&self.scratch_l[..n], &self.scratch_r[..n]);
            if n < self.scratch_l.len() {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipe::pipe;

    /// Simulates a 44.1 kHz device running 300 ppm fast feeding the mixer and
    /// checks the bridge keeps buffering bounded with no underruns.
    #[test]
    fn input_bridge_tracks_fast_device() {
        let dev_rate = 44100u32;
        let (mut w, r) = pipe(dev_rate, 44100);
        let mut bridge = InputBridge::new(r, BridgeTuning::device());
        let mut out_l = vec![0f32; 240];
        let mut out_r = vec![0f32; 240];
        let dev_period = 441usize; // 10 ms device callbacks
        let drift = 1.0003;
        let mut dev_frames_owed = 0.0f64;
        let mut t_mixer = 0.0f64;
        let mut phase = 0.0f64;
        // Run 20 simulated seconds.
        for block in 0..4000 {
            t_mixer += 240.0 / 48000.0;
            // Device produces in bursts of its period.
            let dev_should = t_mixer * dev_rate as f64 * drift;
            while dev_frames_owed + (dev_period as f64) <= dev_should {
                let mut buf = Vec::with_capacity(dev_period * 2);
                for _ in 0..dev_period {
                    let s = (phase * 2.0 * std::f64::consts::PI).sin() as f32 * 0.25;
                    phase += 440.0 / dev_rate as f64;
                    buf.push(s);
                    buf.push(s);
                }
                w.write_interleaved(&buf);
                dev_frames_owed += dev_period as f64;
            }
            bridge.read(&mut out_l, &mut out_r);
            let _ = block;
        }
        let stats = bridge.stats();
        assert!(
            stats.buffer_ms < 15.0,
            "buffer grew to {} ms",
            stats.buffer_ms
        );
        assert!(stats.underruns <= 1, "underruns: {}", stats.underruns);
    }
}

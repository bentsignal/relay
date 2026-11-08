//! Variable-ratio windowed-sinc resampler.
//!
//! Used for both sample-rate conversion (e.g. 44.1 kHz devices) and clock-drift
//! correction, where the ratio is nudged continuously by a few hundred ppm.
//! The ratio is "input frames consumed per output frame".

const HALF_TAPS: usize = 16;
const TAPS: usize = HALF_TAPS * 2;
const PHASES: usize = 256;
/// Input history kept in front of the read position.
const CAPACITY: usize = 16384;

pub struct Resampler {
    table: Vec<f32>,
    buf: [Vec<f32>; 2],
    /// Number of valid frames in `buf`.
    len: usize,
    /// Read position, in input frames, relative to `buf[..][0]`.
    pos: f64,
    nominal: f64,
    ratio: f64,
}

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..50 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < 1e-12 * sum {
            break;
        }
    }
    sum
}

impl Resampler {
    /// `nominal_ratio` = input rate / output rate.
    pub fn new(nominal_ratio: f64) -> Self {
        let nominal = nominal_ratio.clamp(0.1, 10.0);
        // Cutoff relative to the input Nyquist; lower it when downsampling.
        let cutoff = 0.94 * (1.0 / nominal).min(1.0);
        let beta = 8.0;
        let i0b = bessel_i0(beta);
        let mut table = vec![0f32; (PHASES + 1) * TAPS];
        for p in 0..=PHASES {
            let frac = p as f64 / PHASES as f64;
            let row = &mut table[p * TAPS..(p + 1) * TAPS];
            let mut sum = 0.0;
            for (k, v) in row.iter_mut().enumerate() {
                let t = k as f64 - (HALF_TAPS as f64 - 1.0) - frac;
                let x = std::f64::consts::PI * cutoff * t;
                let sinc = if x.abs() < 1e-9 { 1.0 } else { x.sin() / x };
                let w_arg = t / HALF_TAPS as f64;
                let w = if w_arg.abs() >= 1.0 {
                    0.0
                } else {
                    bessel_i0(beta * (1.0 - w_arg * w_arg).sqrt()) / i0b
                };
                let h = cutoff * sinc * w;
                *v = h as f32;
                sum += h;
            }
            for v in row.iter_mut() {
                *v = (*v as f64 / sum) as f32;
            }
        }
        let mut r = Self {
            table,
            buf: [vec![0.0; CAPACITY], vec![0.0; CAPACITY]],
            len: 0,
            pos: 0.0,
            nominal,
            ratio: nominal,
        };
        r.reset();
        r
    }

    pub fn reset(&mut self) {
        // Pre-roll so the first output is centred on the first input frame.
        self.len = HALF_TAPS - 1;
        for b in &mut self.buf {
            b[..self.len].fill(0.0);
        }
        self.pos = (HALF_TAPS - 1) as f64;
        self.ratio = self.nominal;
    }

    pub fn nominal_ratio(&self) -> f64 {
        self.nominal
    }

    pub fn ratio(&self) -> f64 {
        self.ratio
    }

    /// Sets the instantaneous ratio. Clamped to +-5% of nominal.
    pub fn set_ratio(&mut self, ratio: f64) {
        self.ratio = ratio.clamp(self.nominal * 0.95, self.nominal * 1.05);
    }

    /// Frames of input buffered beyond the current position (latency of the resampler).
    pub fn buffered_input(&self) -> f64 {
        self.len as f64 - self.pos
    }

    fn compact(&mut self) {
        let keep_from = (self.pos.floor() as usize).saturating_sub(HALF_TAPS - 1);
        if keep_from == 0 {
            return;
        }
        for b in &mut self.buf {
            b.copy_within(keep_from..self.len, 0);
        }
        self.len -= keep_from;
        self.pos -= keep_from as f64;
    }

    /// Space available for `push` without dropping input.
    pub fn free_space(&self) -> usize {
        CAPACITY - self.len
    }

    /// Appends input frames. Excess beyond capacity is discarded.
    pub fn push(&mut self, left: &[f32], right: &[f32]) -> usize {
        if self.len + left.len() > CAPACITY {
            self.compact();
        }
        let n = left.len().min(CAPACITY - self.len);
        self.buf[0][self.len..self.len + n].copy_from_slice(&left[..n]);
        self.buf[1][self.len..self.len + n].copy_from_slice(&right[..n]);
        self.len += n;
        n
    }

    /// Input frames that must still be pushed before `n_out` frames can be pulled.
    pub fn input_needed(&self, n_out: usize) -> usize {
        let last = self.pos + self.ratio * (n_out.max(1) - 1) as f64;
        let need = last.floor() as usize + HALF_TAPS + 1;
        need.saturating_sub(self.len)
    }

    /// Produces as many output frames as fit in `out_l`/`out_r` and buffered input allows.
    pub fn pull(&mut self, out_l: &mut [f32], out_r: &mut [f32]) -> usize {
        let mut produced = 0;
        let n = out_l.len().min(out_r.len());
        while produced < n {
            let base = self.pos.floor();
            let ib = base as usize;
            if ib + HALF_TAPS >= self.len {
                break;
            }
            let frac = self.pos - base;
            let fp = frac * PHASES as f64;
            let p0 = fp.floor() as usize;
            let t = (fp - p0 as f64) as f32;
            let r0 = &self.table[p0 * TAPS..(p0 + 1) * TAPS];
            let r1 = &self.table[(p0 + 1) * TAPS..(p0 + 2) * TAPS];
            let start = ib + 1 - HALF_TAPS;
            let xl = &self.buf[0][start..start + TAPS];
            let xr = &self.buf[1][start..start + TAPS];
            let (mut al, mut ar) = (0f32, 0f32);
            for k in 0..TAPS {
                let h = r0[k] + (r1[k] - r0[k]) * t;
                al += xl[k] * h;
                ar += xr[k] * h;
            }
            out_l[produced] = al;
            out_r[produced] = ar;
            produced += 1;
            self.pos += self.ratio;
        }
        if self.pos as usize > CAPACITY / 2 {
            self.compact();
        }
        produced
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::PI;

    fn tone(freq: f64, rate: f64, n: usize) -> Vec<f32> {
        (0..n)
            .map(|i| (2.0 * PI * freq * i as f64 / rate).sin() as f32 * 0.5)
            .collect()
    }

    fn run(ratio: f64, input: &[f32]) -> Vec<f32> {
        let mut r = Resampler::new(ratio);
        let mut out = Vec::new();
        let mut ol = vec![0f32; 256];
        let mut or = vec![0f32; 256];
        for chunk in input.chunks(240) {
            r.push(chunk, chunk);
            loop {
                let n = r.pull(&mut ol, &mut or);
                out.extend_from_slice(&ol[..n]);
                if n < ol.len() {
                    break;
                }
            }
        }
        out
    }

    #[test]
    fn unity_ratio_is_transparent() {
        let input = tone(1000.0, 48000.0, 48000);
        let out = run(1.0, &input);
        // Output is delayed by the pre-roll; compare after settling.
        let off = 0;
        let err: f32 = (2000..40000)
            .map(|i| (out[i + off] - input[i]).abs())
            .fold(0.0, f32::max);
        assert!(err < 1e-3, "max error {err}");
    }

    #[test]
    fn converts_44k1_to_48k() {
        let input = tone(1000.0, 44100.0, 44100);
        let out = run(44100.0 / 48000.0, &input);
        assert!((out.len() as i64 - 48000).abs() < 40, "{}", out.len());
        // Check frequency by zero crossings over the middle portion.
        let seg = &out[4800..43200];
        let crossings = seg.windows(2).filter(|w| w[0] <= 0.0 && w[1] > 0.0).count();
        assert!((crossings as i64 - 800).abs() <= 2, "{crossings}");
    }

    #[test]
    fn drift_ratio_changes_are_smooth() {
        let input = tone(5000.0, 48000.0, 96000);
        let mut r = Resampler::new(1.0);
        let mut out = Vec::new();
        let mut ol = vec![0f32; 240];
        let mut or = vec![0f32; 240];
        for (i, chunk) in input.chunks(240).enumerate() {
            r.set_ratio(1.0 + 0.0005 * ((i as f64) * 0.1).sin());
            r.push(chunk, chunk);
            let n = r.pull(&mut ol, &mut or);
            out.extend_from_slice(&ol[..n]);
        }
        let peak = out[2000..].iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak < 0.52 && peak > 0.48, "{peak}");
    }
}

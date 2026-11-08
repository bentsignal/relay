//! Peak/RMS metering. The audio thread updates a [`Meter`]; readers take
//! [`MeterReading`]s from shared atomics published by the engine.

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MeterReading {
    pub peak_l: f32,
    pub peak_r: f32,
    pub rms_l: f32,
    pub rms_r: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Meter {
    peak: [f32; 2],
    sum_sq: [f64; 2],
    count: usize,
}

impl Meter {
    pub fn process(&mut self, left: &[f32], right: &[f32]) {
        for (ch, buf) in [left, right].into_iter().enumerate() {
            let mut p = self.peak[ch];
            let mut s = 0f64;
            for &x in buf {
                let a = x.abs();
                if a > p {
                    p = a;
                }
                s += (x * x) as f64;
            }
            self.peak[ch] = p;
            self.sum_sq[ch] += s;
        }
        self.count += left.len();
    }

    /// Returns the reading accumulated since the last call and resets.
    pub fn take(&mut self) -> MeterReading {
        let n = self.count.max(1) as f64;
        let r = MeterReading {
            peak_l: self.peak[0],
            peak_r: self.peak[1],
            rms_l: (self.sum_sq[0] / n).sqrt() as f32,
            rms_r: (self.sum_sq[1] / n).sqrt() as f32,
        };
        *self = Self::default();
        r
    }
}

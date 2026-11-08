/// A gain that ramps linearly to its target over one block to avoid zipper noise.
#[derive(Clone, Debug)]
pub struct SmoothedGain {
    current: f32,
}

impl Default for SmoothedGain {
    fn default() -> Self {
        Self { current: 1.0 }
    }
}

impl SmoothedGain {
    pub fn new(gain: f32) -> Self {
        Self { current: gain }
    }

    pub fn current(&self) -> f32 {
        self.current
    }

    /// Multiplies the block by a ramp from the current gain to `target`.
    pub fn apply(&mut self, target: f32, left: &mut [f32], right: &mut [f32]) {
        let n = left.len();
        if n == 0 {
            return;
        }
        if (target - self.current).abs() < 1e-6 {
            if (target - 1.0).abs() < 1e-6 {
                return;
            }
            for s in left.iter_mut() {
                *s *= target;
            }
            for s in right.iter_mut() {
                *s *= target;
            }
        } else {
            let step = (target - self.current) / n as f32;
            let mut g = self.current;
            for i in 0..n {
                g += step;
                left[i] *= g;
                right[i] *= g;
            }
        }
        self.current = target;
    }
}

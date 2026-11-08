//! Noise suppression for voice inputs (RNNoise via nnnoiseless).

use nnnoiseless::DenoiseState;
use relay_mixer::Processor;

const FRAME: usize = DenoiseState::FRAME_SIZE;

struct Denoise {
    state: Box<DenoiseState<'static>>,
    input: [f32; FRAME],
    in_len: usize,
    processed: [f32; FRAME],
    /// Output ring; starts one frame deep, which is the processor's latency.
    out: Vec<f32>,
    out_read: usize,
    out_len: usize,
}

pub fn create() -> Box<dyn Processor> {
    let mut out = vec![0.0; FRAME * 4];
    out[..FRAME].fill(0.0);
    Box::new(Denoise {
        state: DenoiseState::new(),
        input: [0.0; FRAME],
        in_len: 0,
        processed: [0.0; FRAME],
        out,
        out_read: 0,
        out_len: FRAME,
    })
}

impl Processor for Denoise {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        let cap = self.out.len();
        for i in 0..left.len() {
            self.input[self.in_len] = (left[i] + right[i]) * 0.5 * 32768.0;
            self.in_len += 1;
            if self.in_len == FRAME {
                self.state.process_frame(&mut self.processed, &self.input);
                self.in_len = 0;
                for &s in &self.processed {
                    let w = (self.out_read + self.out_len) % cap;
                    self.out[w] = s / 32768.0;
                    self.out_len += 1;
                }
            }
            let v = if self.out_len > 0 {
                let v = self.out[self.out_read];
                self.out_read = (self.out_read + 1) % cap;
                self.out_len -= 1;
                v
            } else {
                0.0
            };
            left[i] = v;
            right[i] = v;
        }
    }
}

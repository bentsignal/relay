//! A stereo ring indexed by absolute sample time.
//!
//! Both of the plugin's devices run on one shared timeline, so the writer
//! stores frame `t` at slot `t mod N` and the reader asks for the frames of
//! its own sample times. Every slot carries the sample time it was written
//! for, so frames nobody wrote (the writer is idle or far behind) read as
//! silence instead of stale audio. Readers never consume, so several readers
//! can read the same time range.
//!
//! Each slot is a tiny seqlock: the writer invalidates the stamp, writes the
//! samples, then publishes the stamp; the reader keeps a frame only when the
//! stamp matched before and after copying it. All operations are wait-free.

use std::sync::atomic::{AtomicI64, AtomicU32, Ordering, fence};

const INVALID: i64 = i64::MIN;

pub struct TimeRing {
    samples: Box<[AtomicU32]>,
    stamps: Box<[AtomicI64]>,
}

impl TimeRing {
    /// A ring holding `frames` stereo frames.
    pub fn new(frames: usize) -> Self {
        assert!(frames > 0);
        Self {
            samples: (0..frames * 2).map(|_| AtomicU32::new(0)).collect(),
            stamps: (0..frames).map(|_| AtomicI64::new(INVALID)).collect(),
        }
    }

    pub fn frames(&self) -> usize {
        self.stamps.len()
    }

    fn slot(&self, t: i64) -> usize {
        t.rem_euclid(self.stamps.len() as i64) as usize
    }

    /// Stores interleaved stereo `data` for sample times `start..`.
    pub fn write(&self, start: i64, data: &[f32]) {
        for (i, frame) in data.chunks_exact(2).enumerate() {
            let t = start + i as i64;
            let s = self.slot(t);
            self.stamps[s].store(INVALID, Ordering::Relaxed);
            fence(Ordering::Release);
            self.samples[s * 2].store(frame[0].to_bits(), Ordering::Relaxed);
            self.samples[s * 2 + 1].store(frame[1].to_bits(), Ordering::Relaxed);
            self.stamps[s].store(t, Ordering::Release);
        }
    }

    /// Fills interleaved stereo `out` with the frames for sample times
    /// `start..`; frames that weren't written for exactly that time are zero.
    /// Returns how many real frames were found.
    pub fn read(&self, start: i64, out: &mut [f32]) -> usize {
        let mut found = 0;
        for (i, frame) in out.chunks_exact_mut(2).enumerate() {
            let t = start + i as i64;
            let s = self.slot(t);
            if self.stamps[s].load(Ordering::Acquire) == t {
                let l = self.samples[s * 2].load(Ordering::Relaxed);
                let r = self.samples[s * 2 + 1].load(Ordering::Relaxed);
                fence(Ordering::Acquire);
                if self.stamps[s].load(Ordering::Relaxed) == t {
                    frame[0] = f32::from_bits(l);
                    frame[1] = f32::from_bits(r);
                    found += 1;
                    continue;
                }
            }
            frame[0] = 0.0;
            frame[1] = 0.0;
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(start: i64, frames: usize) -> Vec<f32> {
        (0..frames).flat_map(|i| [(start + i as i64) as f32, -((start + i as i64) as f32)]).collect()
    }

    #[test]
    fn reads_what_was_written_for_the_same_times() {
        let ring = TimeRing::new(64);
        ring.write(100, &ramp(100, 16));
        let mut out = vec![9.0; 32];
        assert_eq!(ring.read(100, &mut out), 16);
        assert_eq!(out, ramp(100, 16));
        // Reading doesn't consume.
        assert_eq!(ring.read(104, &mut out[..8]), 4);
        assert_eq!(&out[..8], &ramp(104, 4)[..]);
    }

    #[test]
    fn unwritten_or_overwritten_times_are_silent() {
        let ring = TimeRing::new(64);
        let mut out = vec![9.0; 16];
        assert_eq!(ring.read(0, &mut out), 0);
        assert!(out.iter().all(|&s| s == 0.0));

        ring.write(10, &ramp(10, 8));
        // Partially covered range: frames 6..10 missing, 10..14 present.
        let mut out = vec![9.0; 16];
        assert_eq!(ring.read(6, &mut out), 4);
        assert!(out[..8].iter().all(|&s| s == 0.0));
        assert_eq!(&out[8..], &ramp(10, 4)[..]);

        // A full lap later the same slots hold newer frames: old times are gone.
        ring.write(10 + 64, &ramp(10 + 64, 8));
        assert_eq!(ring.read(10, &mut out), 0);
        assert_eq!(ring.read(10 + 64, &mut out), 8);
    }

    #[test]
    fn negative_times_wrap() {
        let ring = TimeRing::new(8);
        ring.write(-3, &ramp(-3, 6));
        let mut out = vec![0.0; 12];
        assert_eq!(ring.read(-3, &mut out), 6);
        assert_eq!(out, ramp(-3, 6));
    }
}

//! Lock-free stereo audio pipes between device threads and the mixer.
//!
//! A pipe carries interleaved stereo `f32` frames at the producer's sample
//! rate. Each side runs on its own clock; [`crate::bridge`] corrects the drift.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

#[derive(Debug)]
pub struct PipeShared {
    pub rate: u32,
    capacity: u32,
    underruns: AtomicU32,
    overruns: AtomicU32,
    /// Smallest fill observed right after a read since the last `take_min_fill`.
    min_fill: AtomicU32,
    /// Frames that must be buffered before reading starts (or restarts after an underrun).
    prime_frames: AtomicU32,
    /// Largest single read seen, used to size `prime_frames`.
    max_read: AtomicU32,
    /// Set by the producer while it is alive and delivering.
    active: AtomicBool,
    primed: AtomicBool,
}

impl PipeShared {
    pub fn underruns(&self) -> u32 {
        self.underruns.load(Ordering::Relaxed)
    }
    pub fn overruns(&self) -> u32 {
        self.overruns.load(Ordering::Relaxed)
    }
    pub fn capacity(&self) -> u32 {
        self.capacity
    }
    /// Returns the minimum post-read fill since the last call, if any read happened.
    pub fn take_min_fill(&self) -> Option<u32> {
        let v = self.min_fill.swap(u32::MAX, Ordering::Relaxed);
        (v != u32::MAX).then_some(v)
    }
    pub fn max_read(&self) -> u32 {
        self.max_read.load(Ordering::Relaxed)
    }
    pub fn set_prime_frames(&self, frames: u32) {
        self.prime_frames
            .store(frames.min(self.capacity / 2), Ordering::Relaxed);
    }
    pub fn prime_frames(&self) -> u32 {
        self.prime_frames.load(Ordering::Relaxed)
    }
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::Relaxed)
    }
    pub fn set_active(&self, on: bool) {
        self.active.store(on, Ordering::Relaxed);
    }
    pub fn is_primed(&self) -> bool {
        self.primed.load(Ordering::Relaxed)
    }
}

pub struct PipeWriter {
    prod: rtrb::Producer<f32>,
    shared: Arc<PipeShared>,
}

pub struct PipeReader {
    cons: rtrb::Consumer<f32>,
    shared: Arc<PipeShared>,
}

/// Creates a pipe holding up to `capacity_frames` stereo frames at `rate` Hz.
pub fn pipe(rate: u32, capacity_frames: usize) -> (PipeWriter, PipeReader) {
    let (prod, cons) = rtrb::RingBuffer::new(capacity_frames * 2);
    let shared = Arc::new(PipeShared {
        rate,
        capacity: capacity_frames as u32,
        underruns: AtomicU32::new(0),
        overruns: AtomicU32::new(0),
        min_fill: AtomicU32::new(u32::MAX),
        prime_frames: AtomicU32::new((rate / 200).max(64)),
        max_read: AtomicU32::new(0),
        active: AtomicBool::new(true),
        primed: AtomicBool::new(false),
    });
    (
        PipeWriter {
            prod,
            shared: shared.clone(),
        },
        PipeReader { cons, shared },
    )
}

impl PipeWriter {
    pub fn shared(&self) -> &Arc<PipeShared> {
        &self.shared
    }

    pub fn fill_frames(&self) -> usize {
        (self.prod.buffer().capacity() - self.prod.slots()) / 2
    }

    pub fn free_frames(&self) -> usize {
        self.prod.slots() / 2
    }

    /// Writes interleaved stereo frames. Returns frames written; the rest is dropped.
    pub fn write_interleaved(&mut self, data: &[f32]) -> usize {
        let frames = data.len() / 2;
        let n = frames.min(self.free_frames());
        if n < frames {
            self.shared.overruns.fetch_add(1, Ordering::Relaxed);
        }
        if n == 0 {
            return 0;
        }
        if let Ok(mut chunk) = self.prod.write_chunk_uninit(n * 2) {
            let (a, b) = chunk.as_mut_slices();
            let (da, db) = data[..n * 2].split_at(a.len());
            for (d, s) in a.iter_mut().zip(da) {
                d.write(*s);
            }
            for (d, s) in b.iter_mut().zip(db) {
                d.write(*s);
            }
            unsafe { chunk.commit_all() };
        }
        n
    }

    /// Writes planar stereo frames. Returns frames written.
    pub fn write_planar(&mut self, left: &[f32], right: &[f32]) -> usize {
        let frames = left.len().min(right.len());
        let n = frames.min(self.free_frames());
        if n < frames {
            self.shared.overruns.fetch_add(1, Ordering::Relaxed);
        }
        if n == 0 {
            return 0;
        }
        if let Ok(mut chunk) = self.prod.write_chunk_uninit(n * 2) {
            let (a, b) = chunk.as_mut_slices();
            let mut i = 0usize;
            for slot in a.iter_mut().chain(b.iter_mut()) {
                let f = i / 2;
                slot.write(if i % 2 == 0 { left[f] } else { right[f] });
                i += 1;
            }
            unsafe { chunk.commit_all() };
        }
        n
    }
}

impl Drop for PipeWriter {
    fn drop(&mut self) {
        self.shared.set_active(false);
    }
}

impl PipeReader {
    pub fn shared(&self) -> &Arc<PipeShared> {
        &self.shared
    }

    pub fn fill_frames(&self) -> usize {
        self.cons.slots() / 2
    }

    /// Discards up to `frames` buffered frames.
    pub fn skip(&mut self, frames: usize) -> usize {
        let n = frames.min(self.fill_frames());
        if let Ok(chunk) = self.cons.read_chunk(n * 2) {
            chunk.commit_all();
        }
        n
    }

    fn note_read(&self, wanted: usize, fill_after: usize) {
        let s = &self.shared;
        if wanted as u32 > s.max_read.load(Ordering::Relaxed) {
            s.max_read.store(wanted as u32, Ordering::Relaxed);
        }
        s.min_fill.fetch_min(fill_after as u32, Ordering::Relaxed);
    }

    /// Checks the priming gate. Returns false if the reader should output silence.
    fn gate(&mut self, wanted: usize) -> bool {
        let s = &self.shared;
        if s.primed.load(Ordering::Relaxed) {
            return true;
        }
        let threshold = (s.prime_frames.load(Ordering::Relaxed) as usize).max(wanted);
        if self.fill_frames() >= threshold {
            s.primed.store(true, Ordering::Relaxed);
            // Reset drift measurement so priming doesn't look like excess.
            s.min_fill.store(u32::MAX, Ordering::Relaxed);
            true
        } else {
            false
        }
    }

    fn underrun(&self) {
        self.shared.underruns.fetch_add(1, Ordering::Relaxed);
        self.shared.primed.store(false, Ordering::Relaxed);
    }

    /// Reads planar stereo. Always fills the outputs completely (silence where no
    /// data is available) and returns the number of real frames read.
    pub fn read_planar(&mut self, left: &mut [f32], right: &mut [f32]) -> usize {
        let wanted = left.len().min(right.len());
        if !self.gate(wanted) {
            left.fill(0.0);
            right.fill(0.0);
            return 0;
        }
        let avail = self.fill_frames();
        let n = wanted.min(avail);
        if let Ok(chunk) = self.cons.read_chunk(n * 2) {
            let (a, b) = chunk.as_slices();
            let mut i = 0usize;
            for s in a.iter().chain(b.iter()) {
                let f = i / 2;
                if i % 2 == 0 {
                    left[f] = *s;
                } else {
                    right[f] = *s;
                }
                i += 1;
            }
            chunk.commit_all();
        }
        if n < wanted {
            left[n..wanted].fill(0.0);
            right[n..wanted].fill(0.0);
            self.underrun();
        }
        self.note_read(wanted, avail - n);
        n
    }

    /// Reads interleaved stereo frames into `out` (len = frames * 2). Same semantics as `read_planar`.
    pub fn read_interleaved(&mut self, out: &mut [f32]) -> usize {
        let wanted = out.len() / 2;
        if !self.gate(wanted) {
            out.fill(0.0);
            return 0;
        }
        let avail = self.fill_frames();
        let n = wanted.min(avail);
        if let Ok(chunk) = self.cons.read_chunk(n * 2) {
            let (a, b) = chunk.as_slices();
            out[..a.len()].copy_from_slice(a);
            out[a.len()..a.len() + b.len()].copy_from_slice(b);
            chunk.commit_all();
        }
        if n < wanted {
            out[n * 2..].fill(0.0);
            self.underrun();
        }
        self.note_read(wanted, avail - n);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_priming() {
        let (mut w, mut r) = pipe(48000, 1024);
        r.shared().set_prime_frames(100);
        let l = vec![0.5f32; 50];
        let rr = vec![-0.5f32; 50];
        w.write_planar(&l, &rr);
        let mut ol = vec![0f32; 10];
        let mut or = vec![0f32; 10];
        assert_eq!(r.read_planar(&mut ol, &mut or), 0, "not primed yet");
        w.write_planar(&l, &rr);
        assert_eq!(r.read_planar(&mut ol, &mut or), 10);
        assert_eq!(ol[0], 0.5);
        assert_eq!(or[9], -0.5);
        assert_eq!(r.shared().take_min_fill(), Some(90));
        // Drain and underrun.
        let mut big = vec![0f32; 200];
        let mut big2 = vec![0f32; 200];
        assert_eq!(r.read_planar(&mut big, &mut big2), 90);
        assert_eq!(r.shared().underruns(), 1);
        assert!(!r.shared().is_primed());
    }
}

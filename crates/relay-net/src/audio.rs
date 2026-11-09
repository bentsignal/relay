//! Opus audio streams over QUIC datagrams.

use crate::wire::{AudioPacket, audio_datagram};
use parking_lot::Mutex;
use relay_mixer::{PipeReader, PipeWriter};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const RATE: u32 = 48_000;
/// 5 ms frames.
pub const FRAME: usize = 240;
pub const BITRATE: i32 = 192_000;

/// Where an encoder sends its packets.
pub trait DatagramOut: Send + Sync {
    /// Returns false if not connected (the packet is dropped).
    fn send(&self, bytes: bytes::Bytes) -> bool;
}

pub struct Sender {
    stop: Arc<AtomicBool>,
}

impl Drop for Sender {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// Starts encoding audio from `reader` and sending it as stream `stream`.
pub fn spawn_sender(stream: u32, mut reader: PipeReader, out: Arc<dyn DatagramOut>) -> Sender {
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    reader.shared().set_prime_frames(FRAME as u32);
    let _ = std::thread::Builder::new().name(format!("relay-net-send-{stream}")).spawn(move || {
        let _rt = relay_mixer::clock::platform::promote_realtime(FRAME as u32);
        let timer = relay_mixer::clock::platform::Timer::new();
        let mut enc = match opus::Encoder::new(RATE, opus::Channels::Stereo, opus::Application::LowDelay) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("opus encoder: {e}");
                return;
            }
        };
        let _ = enc.set_bitrate(opus::Bitrate::Bits(BITRATE));
        let mut pcm = vec![0f32; FRAME * 2];
        let mut packet = vec![0u8; 1500];
        let mut seq: u32 = 0;
        let mut ts: u64 = 0;
        while !stop2.load(Ordering::Relaxed) {
            while reader.fill_frames() >= FRAME {
                reader.read_interleaved(&mut pcm);
                match enc.encode_float(&pcm, &mut packet) {
                    Ok(n) => {
                        out.send(audio_datagram(stream, seq, ts, &packet[..n]));
                    }
                    Err(e) => tracing::warn!("opus encode: {e}"),
                }
                seq = seq.wrapping_add(1);
                ts += FRAME as u64;
            }
            timer.sleep_until(Instant::now() + Duration::from_millis(2));
        }
    });
    Sender { stop }
}

/// Receive-side statistics for one stream.
#[derive(Clone, Copy, Debug, Default)]
pub struct RecvStats {
    pub received: u64,
    pub lost: u64,
    pub late: u64,
    pub jitter_ms: f32,
}

pub struct Receiver {
    decoder: opus::Decoder,
    next_seq: Option<u32>,
    pub writer: Option<PipeWriter>,
    pcm: Vec<f32>,
    start: Instant,
    last_transit: Option<f64>,
    jitter: f64,
    pub stats: RecvStats,
    pub last_packet: Instant,
}

impl Receiver {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            decoder: opus::Decoder::new(RATE, opus::Channels::Stereo)?,
            next_seq: None,
            writer: None,
            pcm: vec![0f32; 5760 * 2],
            start: Instant::now(),
            last_transit: None,
            jitter: 0.0,
            stats: RecvStats::default(),
            last_packet: Instant::now(),
        })
    }

    fn emit(&mut self, n: usize) {
        if let Some(w) = self.writer.as_mut() {
            w.write_interleaved(&self.pcm[..n * 2]);
        }
    }

    pub fn on_packet(&mut self, p: &AudioPacket) {
        self.last_packet = Instant::now();
        // RFC 3550 interarrival jitter, in samples.
        let arrival = self.start.elapsed().as_secs_f64() * RATE as f64;
        let transit = arrival - p.ts as f64;
        if let Some(last) = self.last_transit {
            let d = (transit - last).abs();
            self.jitter += (d - self.jitter) / 16.0;
        }
        self.last_transit = Some(transit);
        self.stats.jitter_ms = (self.jitter / RATE as f64 * 1000.0) as f32;

        if let Some(expected) = self.next_seq {
            let ahead = p.seq.wrapping_sub(expected);
            if ahead > u32::MAX / 2 {
                // Older than expected: arrived too late to play.
                self.stats.late += 1;
                return;
            }
            if ahead > 0 {
                self.stats.lost += ahead as u64;
                // Conceal up to 10 missing frames; beyond that, just resync.
                for _ in 0..ahead.min(10) {
                    if let Ok(n) = self.decoder.decode_float(&[], &mut self.pcm[..FRAME * 2], false) {
                        self.emit(n);
                    }
                }
            }
        }
        self.next_seq = Some(p.seq.wrapping_add(1));
        self.stats.received += 1;
        match self.decoder.decode_float(p.payload, &mut self.pcm, false) {
            Ok(n) => self.emit(n),
            Err(e) => tracing::debug!("opus decode: {e}"),
        }
    }
}

pub type SharedReceiver = Arc<Mutex<Receiver>>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::wire::parse_audio;

    struct Loop(Arc<Mutex<Vec<bytes::Bytes>>>);
    impl DatagramOut for Loop {
        fn send(&self, b: bytes::Bytes) -> bool {
            self.0.lock().push(b);
            true
        }
    }

    #[test]
    fn encode_decode_roundtrip() {
        let (mut w, r) = relay_mixer::pipe(RATE, RATE as usize);
        let sent = Arc::new(Mutex::new(Vec::new()));
        let tx = spawn_sender(7, r, Arc::new(Loop(sent.clone())));
        let mut buf = Vec::new();
        for i in 0..4800 {
            let s = (i as f32 * 1000.0 / RATE as f32 * std::f32::consts::TAU).sin() * 0.5;
            buf.push(s);
            buf.push(s);
        }
        w.write_interleaved(&buf);
        std::thread::sleep(Duration::from_millis(100));
        drop(tx);
        let packets = sent.lock().clone();
        assert_eq!(packets.len(), 20);
        let (pw, mut pr) = relay_mixer::pipe(RATE, RATE as usize);
        let mut rx = Receiver::new().unwrap();
        rx.writer = Some(pw);
        for (i, p) in packets.iter().enumerate() {
            if i == 10 {
                continue; // simulate loss
            }
            rx.on_packet(&parse_audio(p).unwrap());
        }
        assert_eq!(rx.stats.lost, 1);
        assert_eq!(pr.fill_frames(), 4800);
        let mut out = vec![0f32; 9600];
        pr.shared().set_prime_frames(1);
        pr.read_interleaved(&mut out);
        let peak = out[4000..].iter().fold(0f32, |m, s| m.max(s.abs()));
        assert!(peak > 0.4, "{peak}");
    }
}

//! Video over QUIC: every encoded frame travels on its own unidirectional
//! stream, so a slow frame never holds up the next one, and QUIC retransmits
//! losses on its own. The receiver restores order, skips frames that can't be
//! decoded, and asks the sender for a keyframe when it has to.

use relay_proto::{EncodedVideo, VideoCodec};
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

pub const HEADER: usize = 25;
const MAGIC: u8 = 0x56; // 'V'

pub fn header(f: &EncodedVideo) -> [u8; HEADER] {
    let mut h = [0u8; HEADER];
    h[0] = MAGIC;
    h[1..3].copy_from_slice(&(f.stream as u16).to_be_bytes());
    h[3..7].copy_from_slice(&f.seq.to_be_bytes());
    h[7..15].copy_from_slice(&f.pts_us.to_be_bytes());
    h[15] = u8::from(f.key) | if f.codec == VideoCodec::Hevc { 2 } else { 0 };
    h[16] = 0;
    h[17..19].copy_from_slice(&f.width.to_be_bytes());
    h[19..21].copy_from_slice(&f.height.to_be_bytes());
    h[21..25].copy_from_slice(&(f.data.len() as u32).to_be_bytes());
    h
}

/// Parses a header; returns the frame (without data) and the payload length.
pub fn parse_header(h: &[u8; HEADER]) -> Option<(EncodedVideo, usize)> {
    if h[0] != MAGIC {
        return None;
    }
    let len = u32::from_be_bytes(h[21..25].try_into().ok()?) as usize;
    if len > 32 << 20 {
        return None;
    }
    Some((
        EncodedVideo {
            stream: u16::from_be_bytes([h[1], h[2]]) as u32,
            seq: u32::from_be_bytes(h[3..7].try_into().ok()?),
            pts_us: u64::from_be_bytes(h[7..15].try_into().ok()?),
            key: h[15] & 1 != 0,
            codec: if h[15] & 2 != 0 { VideoCodec::Hevc } else { VideoCodec::H264 },
            width: u16::from_be_bytes([h[17], h[18]]),
            height: u16::from_be_bytes([h[19], h[20]]),
            data: Vec::new(),
        },
        len,
    ))
}

/// Restores frame order and decides which frames are decodable.
pub struct Reorder {
    next: Option<u32>,
    pending: BTreeMap<u32, (EncodedVideo, Instant)>,
    /// After a gap we can only resume at a keyframe.
    need_key: bool,
    pub dropped: u64,
}

/// How long to wait for a missing frame before giving up on it.
const GAP_WAIT: Duration = Duration::from_millis(60);

pub enum Output {
    Frame(EncodedVideo),
    RequestKeyframe,
}

impl Default for Reorder {
    fn default() -> Self {
        Self { next: None, pending: BTreeMap::new(), need_key: true, dropped: 0 }
    }
}

impl Reorder {
    pub fn push(&mut self, f: EncodedVideo, out: &mut Vec<Output>) {
        if let Some(next) = self.next
            && f.seq.wrapping_sub(next) > u32::MAX / 2
        {
            self.dropped += 1; // late duplicate or already skipped
            return;
        }
        self.pending.insert(f.seq, (f, Instant::now()));
        self.flush(out);
    }

    pub fn flush(&mut self, out: &mut Vec<Output>) {
        loop {
            let Some((&seq, _)) = self.pending.iter().next() else { return };
            let expected = self.next.unwrap_or(seq);
            if seq == expected {
                let (f, _) = self.pending.remove(&seq).unwrap();
                self.next = Some(seq.wrapping_add(1));
                if self.need_key && !f.key {
                    self.dropped += 1;
                    continue;
                }
                self.need_key = false;
                out.push(Output::Frame(f));
                continue;
            }
            // A gap: wait a little for the missing frame, then skip ahead.
            let oldest = self.pending.values().map(|(_, t)| *t).min().unwrap();
            if oldest.elapsed() < GAP_WAIT {
                return;
            }
            self.dropped += seq.wrapping_sub(expected) as u64;
            self.next = Some(seq);
            if !self.pending.get(&seq).map(|(f, _)| f.key).unwrap_or(false) {
                if !self.need_key {
                    out.push(Output::RequestKeyframe);
                }
                self.need_key = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(seq: u32, key: bool) -> EncodedVideo {
        EncodedVideo { seq, key, data: vec![1, 2, 3], ..Default::default() }
    }

    #[test]
    fn header_roundtrip() {
        let mut f = frame(42, true);
        f.width = 1920;
        f.height = 1080;
        f.pts_us = 123456789;
        let (g, len) = parse_header(&header(&f)).unwrap();
        assert_eq!((g.seq, g.key, g.width, g.height, g.pts_us, len), (42, true, 1920, 1080, 123456789, 3));
    }

    #[test]
    fn reorders_and_waits_for_keyframe() {
        let mut r = Reorder::default();
        let mut out = Vec::new();
        r.push(frame(1, false), &mut out); // no keyframe yet: dropped
        assert!(out.is_empty());
        r.push(frame(3, false), &mut out);
        r.push(frame(2, true), &mut out);
        let seqs: Vec<u32> =
            out.iter().filter_map(|o| if let Output::Frame(f) = o { Some(f.seq) } else { None }).collect();
        assert_eq!(seqs, vec![2, 3]);
        out.clear();
        r.push(frame(5, false), &mut out); // 4 missing
        std::thread::sleep(GAP_WAIT + Duration::from_millis(10));
        r.flush(&mut out);
        assert!(matches!(out.first(), Some(Output::RequestKeyframe)));
        out.clear();
        r.push(frame(6, true), &mut out);
        assert!(matches!(out.first(), Some(Output::Frame(f)) if f.seq == 6));
    }
}

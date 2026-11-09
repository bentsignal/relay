//! Messages exchanged between Relay devices.

use relay_proto::{Platform, StreamKind};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Bumped when devices can no longer talk to each other.
pub const NET_PROTOCOL: u32 = 1;
pub const ALPN: &[u8] = b"relay/1";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Ctrl {
    Hello {
        id: String,
        name: String,
        platform: Platform,
        version: String,
        protocol: u32,
    },
    /// The receiving user allowed (or had already allowed) this device.
    Accepted,
    Rejected {
        reason: String,
    },
    Ping {
        t: u64,
    },
    Pong {
        t: u64,
    },
    StreamStart {
        stream: u32,
        kind: StreamKind,
        name: String,
        codec: String,
        rate: u32,
        channels: u16,
    },
    StreamStop {
        stream: u32,
    },
    /// Receiver feedback about a stream it is getting.
    Report {
        stream: u32,
        loss_pct: f32,
        jitter_ms: f32,
    },
    /// Video: ask the sender for a keyframe after loss.
    Keyframe {
        stream: u32,
    },
    Name {
        name: String,
    },
}

pub async fn write_ctrl(w: &mut quinn::SendStream, msg: &Ctrl) -> anyhow::Result<()> {
    let body = serde_json::to_vec(msg)?;
    w.write_u32(body.len() as u32).await?;
    w.write_all(&body).await?;
    Ok(())
}

pub async fn read_ctrl(r: &mut quinn::RecvStream) -> anyhow::Result<Ctrl> {
    let len = r.read_u32().await?;
    anyhow::ensure!(len < 1 << 20, "control message too large");
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body).await?;
    Ok(serde_json::from_slice(&body)?)
}

pub const DG_AUDIO: u8 = 1;
pub const DG_VIDEO: u8 = 2;

/// Audio datagram: kind, stream (u16), seq (u32), timestamp in samples (u64), payload.
pub const AUDIO_HEADER: usize = 1 + 2 + 4 + 8;

pub fn audio_datagram(stream: u32, seq: u32, ts: u64, payload: &[u8]) -> bytes::Bytes {
    let mut b = Vec::with_capacity(AUDIO_HEADER + payload.len());
    b.push(DG_AUDIO);
    b.extend_from_slice(&(stream as u16).to_be_bytes());
    b.extend_from_slice(&seq.to_be_bytes());
    b.extend_from_slice(&ts.to_be_bytes());
    b.extend_from_slice(payload);
    b.into()
}

pub struct AudioPacket<'a> {
    pub stream: u32,
    pub seq: u32,
    pub ts: u64,
    pub payload: &'a [u8],
}

pub fn parse_audio(b: &[u8]) -> Option<AudioPacket<'_>> {
    if b.len() < AUDIO_HEADER || b[0] != DG_AUDIO {
        return None;
    }
    Some(AudioPacket {
        stream: u16::from_be_bytes([b[1], b[2]]) as u32,
        seq: u32::from_be_bytes([b[3], b[4], b[5], b[6]]),
        ts: u64::from_be_bytes(b[7..15].try_into().ok()?),
        payload: &b[AUDIO_HEADER..],
    })
}

//! Relay's network layer: automatic discovery on the LAN, paired and
//! encrypted QUIC connections, Opus audio over datagrams, and live link health.

pub mod audio;
pub mod discovery;
pub mod identity;
mod service;
pub mod tls;
pub mod video;
pub mod wire;

pub use service::{DEFAULT_PORT, NetConfig, NetEvent, NetHandle, VideoTx};

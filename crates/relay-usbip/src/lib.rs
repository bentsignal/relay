//! Relay's virtual audio devices on Windows.
//!
//! Each device is a user-mode USB Audio 2.0 device served over USB/IP on
//! loopback and attached through the Microsoft-signed usbip-win2 virtual host
//! controller. Windows' own USB audio driver then exposes a "Relay" speaker
//! and microphone. No custom kernel driver is involved.

pub mod attach;
pub mod protocol;
pub mod server;
pub mod uac2;

use anyhow::{Result, bail};
use relay_mixer::{PipeReader, PipeWriter, pipe};
use std::path::PathBuf;
use std::sync::Arc;

pub use server::DeviceStats;
pub use uac2::Identity;

/// TCP port our USB/IP server listens on (loopback only).
pub const DEFAULT_PORT: u16 = 3250;

/// The engine-side ends of one virtual device.
pub struct Endpoints {
    pub name: String,
    /// Audio apps play to the device.
    pub playback: PipeReader,
    /// Audio the engine sends to the device's microphone.
    pub capture: PipeWriter,
    pub stats: Arc<DeviceStats>,
}

pub struct VirtualAudio {
    server: Arc<server::Server>,
    stats: Vec<Arc<DeviceStats>>,
    client: Option<PathBuf>,
    port: u16,
    attached: Vec<u8>,
}

impl VirtualAudio {
    /// Starts the server for the given devices. Call [`VirtualAudio::attach_all`] to plug them in.
    pub fn start(port: u16, identities: Vec<Identity>) -> Result<(Self, Vec<Endpoints>)> {
        let mut specs = Vec::new();
        let mut ends = Vec::new();
        for id in identities {
            let (play_w, play_r) = pipe(uac2::RATE, uac2::RATE as usize / 2);
            let (cap_w, cap_r) = pipe(uac2::RATE, uac2::RATE as usize / 2);
            ends.push((id.product.clone(), play_r, cap_w));
            specs.push(server::DeviceSpec { identity: id, playback: play_w, capture: cap_r });
        }
        let (server, stats) = server::Server::start(port, specs)?;
        let all_stats = stats.clone();
        let endpoints = ends
            .into_iter()
            .zip(stats)
            .map(|((name, playback, capture), stats)| Endpoints { name, playback, capture, stats })
            .collect();
        Ok((Self { server, stats: all_stats, client: attach::find_client(), port, attached: Vec::new() }, endpoints))
    }

    /// A fresh reader of what apps play to device `index` (replaces any previous reader).
    pub fn connect_playback(&self, index: usize) -> PipeReader {
        let (w, r) = pipe(uac2::RATE, uac2::RATE as usize / 2);
        self.server.replace_playback(index, w);
        r
    }

    /// A fresh writer feeding device `index`'s microphone (replaces any previous writer).
    pub fn connect_capture(&self, index: usize) -> PipeWriter {
        let (w, r) = pipe(uac2::RATE, uac2::RATE as usize / 2);
        self.server.replace_capture(index, r);
        w
    }

    pub fn client_found(&self) -> bool {
        self.client.is_some()
    }

    /// Plugs every device in. Leaves stale attachments from a previous run alone
    /// (they disappear when their TCP session died).
    pub fn attach_all(&mut self) -> Result<()> {
        let Some(client) = self.client.clone() else { bail!("usbip client not found") };
        for stale in attach::our_ports(&client, self.port) {
            let _ = attach::detach(&client, stale);
        }
        use std::sync::atomic::Ordering::Relaxed;
        // A reconnect left over from a previous run may plug a device in by itself.
        std::thread::sleep(std::time::Duration::from_millis(300));
        for (i, busid) in self.server.busids().into_iter().enumerate() {
            let connected = || self.stats.get(i).map(|s| s.connected.load(Relaxed)).unwrap_or(false);
            if connected() {
                tracing::info!(%busid, "virtual device already attached");
                continue;
            }
            match attach::attach(&client, self.port, &busid) {
                Ok(port) => {
                    tracing::info!(%busid, port, "virtual device attached");
                    self.attached.push(port);
                }
                Err(e) => {
                    // The import may have been taken by that leftover reconnect.
                    let t = std::time::Instant::now();
                    while !connected() && t.elapsed() < std::time::Duration::from_secs(3) {
                        std::thread::sleep(std::time::Duration::from_millis(100));
                    }
                    if !connected() {
                        return Err(e);
                    }
                    tracing::info!(%busid, "virtual device attached by an earlier request");
                }
            }
        }
        for p in attach::our_ports(&client, self.port) {
            if !self.attached.contains(&p) {
                self.attached.push(p);
            }
        }
        Ok(())
    }

    pub fn detach_all(&mut self) {
        if let Some(client) = &self.client {
            for p in self.attached.drain(..) {
                let _ = attach::detach(client, p);
            }
        }
    }
}

impl Drop for VirtualAudio {
    fn drop(&mut self) {
        self.detach_all();
        self.server.stop();
    }
}

/// Timer used by the isochronous scheduler.
pub(crate) mod timer {
    pub use relay_mixer::clock::platform::Timer;
}

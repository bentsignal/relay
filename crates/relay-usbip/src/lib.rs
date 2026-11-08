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
            specs.push(server::DeviceSpec {
                identity: id,
                playback: play_w,
                capture: cap_r,
            });
        }
        let (server, stats) = server::Server::start(port, specs)?;
        let endpoints = ends
            .into_iter()
            .zip(stats)
            .map(|((name, playback, capture), stats)| Endpoints {
                name,
                playback,
                capture,
                stats,
            })
            .collect();
        Ok((
            Self {
                server,
                client: attach::find_client(),
                port,
                attached: Vec::new(),
            },
            endpoints,
        ))
    }

    pub fn client_found(&self) -> bool {
        self.client.is_some()
    }

    /// Plugs every device in. Leaves stale attachments from a previous run alone
    /// (they disappear when their TCP session died).
    pub fn attach_all(&mut self) -> Result<()> {
        let Some(client) = self.client.clone() else {
            bail!("usbip client not found")
        };
        for stale in attach::our_ports(&client, self.port) {
            let _ = attach::detach(&client, stale);
        }
        for busid in self.server.busids() {
            let port = attach::attach(&client, self.port, &busid)?;
            tracing::info!(%busid, port, "virtual device attached");
            self.attached.push(port);
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

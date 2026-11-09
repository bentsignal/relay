use relay_proto::{Bus, BusId, Channel, Effects, MixerConfig, PortStatus, Sink, Source, Strip, StripId};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairedPeer {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub addrs: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub mixer: MixerConfig,
    pub next_id: u32,
    pub device_name: String,
    pub network_enabled: bool,
    pub channel: Channel,
    /// Keep Relay's virtual devices as the OS defaults.
    pub relay_default: bool,
    pub peers: Vec<PairedPeer>,
    /// Set once the first-run layout has been created.
    pub initialized: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            mixer: MixerConfig::default(),
            next_id: 1,
            device_name: default_device_name(),
            network_enabled: true,
            channel: Channel::Stable,
            relay_default: true,
            peers: Vec::new(),
            initialized: false,
        }
    }
}

pub fn default_device_name() -> String {
    let host = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).ok();
    #[cfg(target_os = "macos")]
    let host = host.or_else(|| {
        std::process::Command::new("scutil")
            .args(["--get", "ComputerName"])
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty())
    });
    host.unwrap_or_else(|| "This device".into())
}

impl Config {
    pub fn load(path: &Path) -> Self {
        match std::fs::read(path) {
            Ok(bytes) => match serde_json::from_slice::<Config>(&bytes) {
                Ok(c) => c,
                Err(e) => {
                    tracing::error!("config unreadable, starting fresh: {e}");
                    let _ = std::fs::copy(path, path.with_extension("json.bad"));
                    Config::default()
                }
            },
            Err(_) => Config::default(),
        }
    }

    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut clean = self.clone();
        for s in &mut clean.mixer.strips {
            s.status = PortStatus::Idle;
        }
        for b in &mut clean.mixer.buses {
            b.status = PortStatus::Idle;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_vec_pretty(&clean)?)?;
        std::fs::rename(tmp, path)
    }

    pub fn alloc_id(&mut self) -> u32 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    pub fn strip_mut(&mut self, id: StripId) -> Option<&mut Strip> {
        self.mixer.strips.iter_mut().find(|s| s.id == id)
    }

    pub fn bus_mut(&mut self, id: BusId) -> Option<&mut Bus> {
        self.mixer.buses.iter_mut().find(|b| b.id == id)
    }

    pub fn new_strip(&mut self, name: String, source: Source, routes: Vec<BusId>) -> Strip {
        Strip {
            id: self.alloc_id(),
            name,
            source,
            gain_db: 0.0,
            mute: false,
            routes,
            effects: Effects::default(),
            status: PortStatus::Idle,
        }
    }

    pub fn new_bus(&mut self, name: String, sink: Sink) -> Bus {
        Bus {
            id: self.alloc_id(),
            name,
            sink,
            gain_db: 0.0,
            mute: false,
            effects: Effects::default(),
            status: PortStatus::Idle,
        }
    }
}

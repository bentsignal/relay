//! The engine's control side: owns configuration, opens devices, and drives
//! the realtime mixer graph.

use crate::config::Config;
use crate::virt::{self, VirtualBackend};
use relay_audio::DeviceStream;
use relay_mixer::{
    BridgeTuning, BusRt, EffectsRt, GraphHandle, InputBridge, MAX_BUSES, MAX_STRIPS, MixerCmd, MixerThread,
    OutputBridge, PipeShared, Silence, SinkPort, SourcePort, StripRt,
};
use relay_proto::*;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

enum Live {
    None,
    Device(DeviceStream),
    Pipe(Arc<PipeShared>),
}

impl Live {
    fn failed(&self) -> bool {
        matches!(self, Live::Device(d) if d.failed())
    }
}

struct PortRt {
    slot: usize,
    live: Live,
}

pub struct Engine {
    pub cfg: Config,
    graph: GraphHandle,
    _mixer: MixerThread,
    strips: HashMap<StripId, PortRt>,
    buses: HashMap<BusId, PortRt>,
    pub devices: Devices,
    virt: Box<dyn VirtualBackend>,
    started: Instant,
    /// State changed; UIs need a new snapshot.
    pub dirty: bool,
    /// Config changed; persist soon.
    pub save_due: bool,
    pub notices: Vec<(String, bool)>,
    cpu: (u64, u64, Instant, f32),
    defaults_applied: bool,
    pub network: Network,
    pub apps: Vec<AppInfo>,
    pub video: Video,
    pub update: Update,
    updater: crate::updates::Updater,
    pub quit: bool,
    video_mgr: crate::video::VideoMgr,
    firewall_fix: Option<std::process::Child>,
    ticks: u64,
    /// Number of connected UIs; some scans only matter while one is open.
    pub ui_clients: usize,
    firewall_checked: Option<Instant>,
    pub net: Option<relay_net::NetHandle>,
    net_events: Option<crossbeam_channel::Receiver<relay_net::NetEvent>>,
}

fn status_of(live: &Live, missing: &str) -> PortStatus {
    match live {
        Live::None => PortStatus::Missing(missing.into()),
        Live::Device(d) if d.failed() => PortStatus::Error("The device stopped responding".into()),
        _ => PortStatus::Running,
    }
}

impl Engine {
    pub fn new(mut cfg: Config) -> anyhow::Result<Self> {
        let devices = relay_audio::list_devices();
        if !cfg.initialized {
            first_run_layout(&mut cfg, &devices);
        }
        let (graph, g) = relay_mixer::graph();
        let mixer = MixerThread::spawn(g)?;
        // Virtual devices come up after the layout is chosen: Windows makes a
        // newly plugged device the default, which would hide the hardware.
        let virt = virt::create();
        let channel = cfg.channel;
        let mut e = Self {
            cfg,
            graph,
            _mixer: mixer,
            strips: HashMap::new(),
            buses: HashMap::new(),
            devices,
            virt,
            started: Instant::now(),
            dirty: true,
            save_due: true,
            notices: Vec::new(),
            cpu: (0, 0, Instant::now(), 0.0),
            defaults_applied: false,
            network: Network::default(),
            apps: Vec::new(),
            video: Video::default(),
            update: Update::default(),
            updater: crate::updates::Updater::start(channel),
            quit: false,
            video_mgr: crate::video::VideoMgr::default(),
            firewall_fix: None,
            ticks: 0,
            ui_clients: 0,
            firewall_checked: None,
            net: None,
            net_events: None,
        };
        e.update.channel = e.cfg.channel;
        e.update.current = crate::VERSION.to_string();
        e.update.rolled_back_from = relay_update::install::load_state().rolled_back_from;
        if !e.updater.managed {
            e.update.state = UpdateState::Idle;
        }
        e.start_network();
        e.rebuild_all();
        Ok(e)
    }

    fn send(&mut self, cmd: MixerCmd) {
        if self.graph.send(cmd).is_err() {
            tracing::error!("mixer command queue full");
        }
    }

    fn free_slot(used: &HashMap<u32, PortRt>, max: usize) -> Option<usize> {
        (0..max).find(|s| !used.values().any(|p| p.slot == *s))
    }

    fn route_mask(&self, routes: &[BusId]) -> u64 {
        routes.iter().filter_map(|b| self.buses.get(b)).fold(0u64, |m, p| m | (1u64 << p.slot))
    }

    fn open_source(&mut self, src: &Source) -> (Box<dyn SourcePort>, Live) {
        let tuning = BridgeTuning::device();
        let bridged = |reader: relay_mixer::PipeReader, tuning| -> (Box<dyn SourcePort>, Live) {
            let shared = reader.shared().clone();
            (Box::new(InputBridge::new(reader, tuning)), Live::Pipe(shared))
        };
        match src {
            Source::Device { key, .. } | Source::Loopback { key, .. } => {
                let res = if matches!(src, Source::Device { .. }) {
                    relay_audio::open_input(key)
                } else {
                    relay_audio::open_loopback(key)
                };
                match res {
                    Ok((stream, reader)) => (Box::new(InputBridge::new(reader, tuning)), Live::Device(stream)),
                    Err(e) => {
                        tracing::info!("input {key} unavailable: {e:#}");
                        (Box::new(Silence), Live::None)
                    }
                }
            }
            Source::Virtual { index } => match self.virt.playback(*index) {
                Some(r) => bridged(r, tuning),
                None => (Box::new(Silence), Live::None),
            },
            Source::Network { peer, stream } => match self.net.as_ref().map(|n| n.open_source(peer, *stream)) {
                Some(r) => bridged(r, BridgeTuning::network()),
                None => (Box::new(Silence), Live::None),
            },
            Source::App { process } => {
                #[cfg(windows)]
                {
                    match relay_audio::windows::open_app(process) {
                        Ok((stream, reader)) => (Box::new(InputBridge::new(reader, tuning)), Live::Device(stream)),
                        Err(_) => (Box::new(Silence), Live::None),
                    }
                }
                #[cfg(not(windows))]
                {
                    let _ = process;
                    (Box::new(Silence), Live::None)
                }
            }
            Source::None => (Box::new(Silence), Live::None),
        }
    }

    fn open_sink(&mut self, bus: &Bus) -> (Box<dyn SinkPort>, Live) {
        let tuning = BridgeTuning::device();
        match &bus.sink {
            Sink::Device { key, .. } => match relay_audio::open_output(key) {
                Ok((stream, writer)) => (Box::new(OutputBridge::new(writer, tuning)), Live::Device(stream)),
                Err(e) => {
                    tracing::info!("output {key} unavailable: {e:#}");
                    (Box::new(relay_mixer::Discard), Live::None)
                }
            },
            Sink::VirtualMic { index } => match self.virt.mic(*index) {
                Some(w) => {
                    let shared = w.shared().clone();
                    (Box::new(OutputBridge::new(w, tuning)), Live::Pipe(shared))
                }
                None => (Box::new(relay_mixer::Discard), Live::None),
            },
            Sink::Network { peer } => match self.net.as_ref().map(|n| n.open_sink(peer, bus.id, &bus.name)) {
                Some(w) => {
                    let shared = w.shared().clone();
                    // The encoder runs on the mixer clock: no drift to correct.
                    (Box::new(relay_mixer::DirectSink(w)), Live::Pipe(shared))
                }
                None => (Box::new(relay_mixer::Discard), Live::None),
            },
            Sink::None => (Box::new(relay_mixer::Discard), Live::None),
        }
    }

    fn source_missing_text(&self, src: &Source) -> String {
        match src {
            Source::Device { name, .. } | Source::Loopback { name, .. } => format!("{name} isn't connected"),
            Source::Virtual { .. } => "Relay's devices aren't ready".into(),
            Source::Network { .. } => "Not receiving".into(),
            Source::App { process } => format!("{} isn't running", process.trim_end_matches(".exe")),
            Source::None => String::new(),
        }
    }

    fn sink_missing_text(&self, sink: &Sink) -> String {
        match sink {
            Sink::Device { name, .. } => format!("{name} isn't connected"),
            Sink::VirtualMic { .. } => "Relay's devices aren't ready".into(),
            Sink::Network { .. } => "Not connected".into(),
            Sink::None => String::new(),
        }
    }

    fn build_bus(&mut self, id: BusId) {
        let Some(bus) = self.cfg.mixer.buses.iter().find(|b| b.id == id).cloned() else { return };
        let slot = match self.buses.get(&id) {
            Some(p) => p.slot,
            None => match Self::free_slot(&self.buses, MAX_BUSES) {
                Some(s) => s,
                None => return,
            },
        };
        if let (Some(net), Some(PortRt { .. })) = (self.net.as_ref(), self.buses.get(&id)) {
            // Rebuilding: stop any previous network send for this bus.
            for p in self.network.peers.iter() {
                net.close_sink(&p.id, id);
            }
        }
        let (sink, live) = self.open_sink(&bus);
        let status = if matches!(bus.sink, Sink::None) {
            PortStatus::Idle
        } else {
            status_of(&live, &self.sink_missing_text(&bus.sink))
        };
        self.send(MixerCmd::AddBus(BusRt::new(slot, sink, bus.gain_db, bus.mute, EffectsRt::new(&bus.effects, None))));
        self.buses.insert(id, PortRt { slot, live });
        if let Some(b) = self.cfg.bus_mut(id) {
            b.status = status;
        }
    }

    fn build_strip(&mut self, id: StripId) {
        let Some(strip) = self.cfg.mixer.strips.iter().find(|s| s.id == id).cloned() else { return };
        let slot = match self.strips.get(&id) {
            Some(p) => p.slot,
            None => match Self::free_slot(&self.strips, MAX_STRIPS) {
                Some(s) => s,
                None => return,
            },
        };
        let (source, live) = self.open_source(&strip.source);
        let status = match &strip.source {
            Source::None => PortStatus::Idle,
            s => status_of(&live, &self.source_missing_text(s)),
        };
        let mask = self.route_mask(&strip.routes);
        let denoise = strip.effects.denoise.then(crate::denoise::create);
        self.send(MixerCmd::AddStrip(StripRt::new(
            slot,
            source,
            strip.gain_db,
            strip.mute,
            mask,
            EffectsRt::new(&strip.effects, denoise),
        )));
        self.strips.insert(id, PortRt { slot, live });
        if let Some(s) = self.cfg.strip_mut(id) {
            s.status = status;
        }
    }

    fn rebuild_all(&mut self) {
        let bus_ids: Vec<_> = self.cfg.mixer.buses.iter().map(|b| b.id).collect();
        for id in bus_ids {
            self.build_bus(id);
        }
        let strip_ids: Vec<_> = self.cfg.mixer.strips.iter().map(|s| s.id).collect();
        for id in strip_ids {
            self.build_strip(id);
        }
        self.dirty = true;
    }

    fn update_routes(&mut self) {
        let masks: Vec<(usize, u64)> = self
            .cfg
            .mixer
            .strips
            .iter()
            .filter_map(|s| self.strips.get(&s.id).map(|p| (p.slot, self.route_mask(&s.routes))))
            .collect();
        for (slot, mask) in masks {
            self.send(MixerCmd::SetStripRoutes(slot, mask));
        }
    }

    fn changed(&mut self) {
        self.dirty = true;
        self.save_due = true;
    }

    pub fn handle(&mut self, cmd: Command) {
        match cmd {
            Command::AddStrip { source } => {
                let name = source_name(&source);
                let routes = self.cfg.mixer.buses.first().map(|b| vec![b.id]).unwrap_or_default();
                let strip = self.cfg.new_strip(name, source, routes);
                let id = strip.id;
                self.cfg.mixer.strips.push(strip);
                self.build_strip(id);
                self.changed();
            }
            Command::RemoveStrip { id } => {
                if let Some(p) = self.strips.remove(&id) {
                    self.send(MixerCmd::RemoveStrip(p.slot));
                }
                self.cfg.mixer.strips.retain(|s| s.id != id);
                self.changed();
            }
            Command::SetStripSource { id, source } => {
                if let Some(s) = self.cfg.strip_mut(id) {
                    if s.name == source_name(&s.source) || s.name.is_empty() {
                        s.name = source_name(&source);
                    }
                    s.source = source;
                }
                self.build_strip(id);
                self.changed();
            }
            Command::SetStripName { id, name } => {
                if let Some(s) = self.cfg.strip_mut(id) {
                    s.name = name;
                }
                self.changed();
            }
            Command::SetStripGain { id, gain_db } => {
                let gain_db = gain_db.clamp(-60.0, 12.0);
                if let Some(s) = self.cfg.strip_mut(id) {
                    s.gain_db = gain_db;
                }
                if let Some(p) = self.strips.get(&id) {
                    let slot = p.slot;
                    self.send(MixerCmd::SetStripGain(slot, relay_dsp::db_to_gain(gain_db)));
                }
                self.changed();
            }
            Command::SetStripMute { id, mute } => {
                if let Some(s) = self.cfg.strip_mut(id) {
                    s.mute = mute;
                }
                if let Some(p) = self.strips.get(&id) {
                    let slot = p.slot;
                    self.send(MixerCmd::SetStripMute(slot, mute));
                }
                self.changed();
            }
            Command::SetRoute { strip, bus, on } => {
                if let Some(s) = self.cfg.strip_mut(strip) {
                    s.routes.retain(|b| *b != bus);
                    if on {
                        s.routes.push(bus);
                    }
                }
                self.update_routes();
                self.changed();
            }
            Command::SetStripEffects { id, effects } => {
                let denoise_changed =
                    self.cfg.strip_mut(id).map(|s| s.effects.denoise != effects.denoise).unwrap_or(false);
                if let Some(s) = self.cfg.strip_mut(id) {
                    s.effects = effects.clone();
                }
                if let Some(p) = self.strips.get(&id) {
                    let slot = p.slot;
                    if denoise_changed {
                        let d = effects.denoise.then(crate::denoise::create);
                        self.send(MixerCmd::SetStripDenoise(slot, d));
                    }
                    self.send(MixerCmd::SetStripEffects(slot, Box::new(effects)));
                }
                self.changed();
            }
            Command::AddBus { sink } => {
                let name = sink_name(&sink);
                let bus = self.cfg.new_bus(name, sink);
                let id = bus.id;
                self.cfg.mixer.buses.push(bus);
                self.build_bus(id);
                self.changed();
            }
            Command::RemoveBus { id } => {
                if let Some(p) = self.buses.remove(&id) {
                    self.send(MixerCmd::RemoveBus(p.slot));
                }
                if let (Some(net), Some(Sink::Network { peer })) =
                    (self.net.as_ref(), self.cfg.mixer.buses.iter().find(|b| b.id == id).map(|b| b.sink.clone()))
                {
                    net.close_sink(&peer, id);
                }
                self.cfg.mixer.buses.retain(|b| b.id != id);
                for s in &mut self.cfg.mixer.strips {
                    s.routes.retain(|b| *b != id);
                }
                self.update_routes();
                self.changed();
            }
            Command::SetBusSink { id, sink } => {
                if let Some(b) = self.cfg.bus_mut(id) {
                    if b.name == sink_name(&b.sink) || b.name.is_empty() {
                        b.name = sink_name(&sink);
                    }
                    b.sink = sink;
                }
                self.build_bus(id);
                self.changed();
            }
            Command::SetBusName { id, name } => {
                if let Some(b) = self.cfg.bus_mut(id) {
                    b.name = name;
                }
                self.changed();
            }
            Command::SetBusGain { id, gain_db } => {
                let gain_db = gain_db.clamp(-60.0, 12.0);
                if let Some(b) = self.cfg.bus_mut(id) {
                    b.gain_db = gain_db;
                }
                if let Some(p) = self.buses.get(&id) {
                    let slot = p.slot;
                    self.send(MixerCmd::SetBusGain(slot, relay_dsp::db_to_gain(gain_db)));
                }
                self.changed();
            }
            Command::SetBusMute { id, mute } => {
                if let Some(b) = self.cfg.bus_mut(id) {
                    b.mute = mute;
                }
                if let Some(p) = self.buses.get(&id) {
                    let slot = p.slot;
                    self.send(MixerCmd::SetBusMute(slot, mute));
                }
                self.changed();
            }
            Command::SetBusEffects { id, effects } => {
                if let Some(b) = self.cfg.bus_mut(id) {
                    b.effects = effects.clone();
                }
                if let Some(p) = self.buses.get(&id) {
                    let slot = p.slot;
                    self.send(MixerCmd::SetBusEffects(slot, Box::new(effects)));
                }
                self.changed();
            }
            Command::FixFirewall => match crate::firewall::fix() {
                Ok(child) => self.firewall_fix = Some(child),
                Err(e) => self.notices.push((format!("Couldn't start the fix: {e}"), true)),
            },
            Command::InstallVirtualAudio => {
                if let Err(e) = self.virt.install() {
                    self.notices.push((format!("Couldn't start the installer: {e}"), true));
                }
                self.dirty = true;
            }
            Command::MakeRelayDefault { on } => {
                self.cfg.relay_default = on;
                self.defaults_applied = false;
                if !on {
                    self.restore_hardware_defaults();
                }
                self.changed();
            }
            Command::SetChannel { channel } => {
                self.cfg.channel = channel;
                self.update.channel = channel;
                self.updater.ready = None;
                self.updater.check(channel);
                self.changed();
            }
            Command::CheckForUpdates => {
                if self.updater.managed {
                    self.updater.check(self.cfg.channel);
                } else {
                    self.notices.push(("This copy of Relay isn't installed, so it can't update itself.".into(), false));
                }
            }
            Command::ApplyUpdate => {
                if let Some(v) = self.updater.ready.clone() {
                    match crate::updates::apply(&v) {
                        Ok(()) => self.quit = true,
                        Err(e) => self.notices.push((format!("Update failed: {e}"), true)),
                    }
                }
            }
            Command::SetDeviceName { name } => {
                let name = name.trim().to_string();
                if !name.is_empty() {
                    self.cfg.device_name = name.clone();
                    if let Some(n) = &self.net {
                        n.set_name(name);
                    }
                    self.changed();
                }
            }
            Command::SetNetworkEnabled { on } => {
                self.cfg.network_enabled = on;
                if on {
                    self.start_network()
                } else {
                    self.stop_network()
                }
                self.changed();
            }
            Command::Connect { peer } => {
                if let Some(n) = &self.net {
                    n.connect(&peer);
                }
            }
            Command::AcceptPeer { peer } => {
                if let Some(n) = &self.net {
                    n.accept(&peer);
                }
            }
            Command::RejectPeer { peer } => {
                if let Some(n) = &self.net {
                    n.reject(&peer);
                }
            }
            Command::StartVideo { source, peer, with_audio } => {
                let Some(net) = self.net.as_ref() else {
                    self.notices.push(("Network is off".into(), true));
                    return;
                };
                let name =
                    self.video.sources.iter().find(|s| s.key == source).map(|s| s.name.clone()).unwrap_or_default();
                if let Err(e) = self.video_mgr.start(net, &source, &peer, with_audio, &name) {
                    self.notices.push((e, true));
                }
                if with_audio
                    && !self.cfg.mixer.buses.iter().any(|b| matches!(&b.sink, Sink::Network { peer: p } if *p == peer))
                {
                    // Send this device's sound along with the picture.
                    self.handle(Command::AddBus { sink: Sink::Network { peer: peer.clone() } });
                    if let Some(bus) = self.cfg.mixer.buses.last().map(|b| b.id) {
                        let desktop: Vec<StripId> = self
                            .cfg
                            .mixer
                            .strips
                            .iter()
                            .filter(|s| matches!(s.source, Source::Virtual { .. } | Source::Loopback { .. }))
                            .map(|s| s.id)
                            .collect();
                        for strip in desktop {
                            self.handle(Command::SetRoute { strip, bus, on: true });
                        }
                    }
                }
                self.dirty = true;
            }
            Command::WatchVideo { peer, stream } => {
                let peer_name =
                    self.network.peers.iter().find(|p| p.id == peer).map(|p| p.name.clone()).unwrap_or_default();
                let name = self
                    .network
                    .peers
                    .iter()
                    .find(|p| p.id == peer)
                    .and_then(|p| p.incoming.iter().find(|s| s.id == stream))
                    .map(|s| s.name.clone())
                    .unwrap_or_default();
                if let Some(net) = self.net.as_ref() {
                    self.video_mgr.on_incoming(net, &peer, stream, &name, &peer_name);
                }
                self.dirty = true;
            }
            Command::StopVideo { id } => {
                self.video_mgr.stop(id);
                self.dirty = true;
            }
            Command::ForgetPeer { peer } => {
                if let Some(n) = &self.net {
                    n.forget(&peer);
                }
                self.cfg.peers.retain(|p| p.id != peer);
                self.changed();
            }
            // Network, video and update commands are routed by main before reaching here.
            _ => {}
        }
    }

    fn restore_hardware_defaults(&mut self) {
        #[cfg(windows)]
        {
            let out = self.cfg.mixer.buses.iter().find_map(|b| match &b.sink {
                Sink::Device { key, .. } => Some(key.clone()),
                _ => None,
            });
            let inp = self.cfg.mixer.strips.iter().find_map(|s| match &s.source {
                Source::Device { key, .. } => Some(key.clone()),
                _ => None,
            });
            for key in [out, inp].into_iter().flatten() {
                let _ = relay_audio::windows::set_default_endpoint(relay_audio::windows::endpoint_id_from_key(&key));
            }
        }
    }

    fn apply_relay_defaults(&mut self) {
        if self.defaults_applied || !self.cfg.relay_default || self.virt.state() != VirtualAudioState::Ready {
            return;
        }
        #[cfg(windows)]
        {
            let out = self.devices.outputs.iter().find(|d| d.is_relay).map(|d| d.key.clone());
            let inp = self.devices.inputs.iter().find(|d| d.is_relay).map(|d| d.key.clone());
            if let (Some(out), Some(inp)) = (out, inp) {
                for key in [out, inp] {
                    if let Err(e) =
                        relay_audio::windows::set_default_endpoint(relay_audio::windows::endpoint_id_from_key(&key))
                    {
                        tracing::warn!("couldn't set default device: {e:#}");
                    }
                }
                self.defaults_applied = true;
                self.dirty = true;
            }
        }
    }

    /// Periodic housekeeping: device changes, reconnects, virtual devices.
    pub fn tick(&mut self) {
        let before = self.virt.state();
        self.virt.tick();
        let virt_changed = self.virt.state() != before;

        self.ticks += 1;
        // Device scans cost a few ms of COM calls; every other tick is plenty.
        let mut devices_changed = false;
        if self.ticks % 2 == 1 || virt_changed {
            let devices = relay_audio::list_devices();
            devices_changed = devices != self.devices;
            if devices_changed {
                self.devices = devices;
                self.dirty = true;
            }
        }

        // Reopen ports that failed or whose device came back.
        let strips: Vec<(StripId, Source)> = self.cfg.mixer.strips.iter().map(|s| (s.id, s.source.clone())).collect();
        for (id, src) in strips {
            let Some(p) = self.strips.get(&id) else { continue };
            let retry = p.live.failed()
                || (matches!(p.live, Live::None)
                    && !matches!(src, Source::None)
                    && (devices_changed || virt_changed || matches!(src, Source::Network { .. } | Source::App { .. })));
            if retry {
                self.build_strip(id);
                self.dirty = true;
            }
        }
        let buses: Vec<(BusId, Sink)> = self.cfg.mixer.buses.iter().map(|b| (b.id, b.sink.clone())).collect();
        for (id, sink) in buses {
            let Some(p) = self.buses.get(&id) else { continue };
            let retry = p.live.failed()
                || (matches!(p.live, Live::None)
                    && !matches!(sink, Sink::None)
                    && (devices_changed || virt_changed || matches!(sink, Sink::Network { .. })));
            if retry {
                self.build_bus(id);
                self.dirty = true;
            }
        }
        if virt_changed {
            self.defaults_applied = false;
            self.dirty = true;
        }
        self.apply_relay_defaults();
        self.refresh_video();
        let fix_done = match self.firewall_fix.as_mut().map(|c| c.try_wait()) {
            Some(Ok(Some(_))) | Some(Err(_)) => {
                self.firewall_fix = None;
                true
            }
            _ => false,
        };
        if self.net.is_some()
            && (fix_done || self.firewall_checked.map(|t| t.elapsed() > Duration::from_secs(60)).unwrap_or(true))
        {
            self.firewall_checked = Some(Instant::now());
            let blocked = crate::firewall::blocked();
            if blocked != self.network.firewall_blocked {
                self.network.firewall_blocked = blocked;
                self.dirty = true;
            }
        }
        #[cfg(windows)]
        if self.ticks % 2 == 0
            && (self.ui_clients > 0 || self.cfg.mixer.strips.iter().any(|s| matches!(s.source, Source::App { .. })))
        {
            let apps = relay_audio::windows::list_apps();
            if apps != self.apps {
                self.apps = apps;
                self.dirty = true;
            }
        }

        // Engine load.
        let m = self.graph.meters();
        let cycles = m.cycles.load(Ordering::Relaxed);
        let busy = m.busy_ns.load(Ordering::Relaxed);
        let (c0, b0, t0, _) = self.cpu;
        let dt = t0.elapsed().as_secs_f64();
        if dt > 0.5 {
            let pct = ((busy - b0) as f64 / 1e9 / dt * 100.0) as f32;
            if cycles > c0 {
                self.cpu = (cycles, busy, Instant::now(), pct);
            }
        }
        self.graph.collect_garbage();
    }

    pub fn meters(&self) -> Meters {
        let m = self.graph.meters();
        Meters {
            strips: self
                .cfg
                .mixer
                .strips
                .iter()
                .filter_map(|s| self.strips.get(&s.id).map(|p| (s.id, m.strip(p.slot))))
                .collect(),
            buses: self
                .cfg
                .mixer
                .buses
                .iter()
                .filter_map(|b| self.buses.get(&b.id).map(|p| (b.id, m.bus(p.slot))))
                .collect(),
        }
    }

    pub fn start_network(&mut self) {
        if self.net.is_some() || !self.cfg.network_enabled {
            return;
        }
        let port = std::env::var("RELAY_NET_PORT").ok().and_then(|p| p.parse().ok()).unwrap_or(relay_net::DEFAULT_PORT);
        let cfg = relay_net::NetConfig {
            data_dir: crate::paths::data_dir(),
            name: self.cfg.device_name.clone(),
            version: crate::VERSION.to_string(),
            port,
            paired: self.cfg.peers.iter().map(|p| (p.id.clone(), p.name.clone(), p.addrs.clone())).collect(),
        };
        match relay_net::NetHandle::start(cfg) {
            Ok((net, events)) => {
                self.network.this_id = net.id();
                self.network.port = net.port();
                self.network.problem = None;
                self.net = Some(net);
                self.net_events = Some(events);
                self.rebuild_network_ports();
            }
            Err(e) => {
                tracing::error!("network failed to start: {e:#}");
                self.network.problem = Some(format!("Network unavailable: {e}"));
            }
        }
        self.dirty = true;
    }

    pub fn poll_video(&mut self) {
        self.video_mgr.poll(self.net.as_ref());
    }

    /// Refreshes video state for the UI.
    pub fn refresh_video(&mut self) {
        let v = self.video_mgr.state();
        if v != self.video {
            self.video = v;
            self.dirty = true;
        }
    }

    pub fn stop_network(&mut self) {
        self.video_mgr.clear();
        self.net = None;
        self.net_events = None;
        self.network.peers.clear();
        self.network.problem = None;
        self.rebuild_network_ports();
        self.dirty = true;
    }

    fn rebuild_network_ports(&mut self) {
        let buses: Vec<BusId> =
            self.cfg.mixer.buses.iter().filter(|b| matches!(b.sink, Sink::Network { .. })).map(|b| b.id).collect();
        for id in buses {
            self.build_bus(id);
        }
        let strips: Vec<StripId> =
            self.cfg.mixer.strips.iter().filter(|s| matches!(s.source, Source::Network { .. })).map(|s| s.id).collect();
        for id in strips {
            self.build_strip(id);
        }
    }

    pub fn poll_updates(&mut self) {
        while let Ok(crate::updates::UpdateMsg::State(st)) = self.updater.rx.try_recv() {
            if let UpdateState::Ready(v) = &st {
                self.updater.ready = Some(v.clone());
            }
            self.update.state = st;
            self.dirty = true;
        }
    }

    pub fn poll_network(&mut self) {
        let Some(rx) = self.net_events.clone() else { return };
        while let Ok(ev) = rx.try_recv() {
            match ev {
                relay_net::NetEvent::Peers { peers, problem } => {
                    if self.network.peers != peers || self.network.problem != problem {
                        self.network.peers = peers;
                        self.network.problem = problem;
                        self.dirty = true;
                    }
                }
                relay_net::NetEvent::Paired { id, name } => {
                    let addrs = self.cfg.peers.iter().find(|p| p.id == id).map(|p| p.addrs.clone()).unwrap_or_default();
                    self.cfg.peers.retain(|p| p.id != id);
                    self.cfg.peers.push(crate::config::PairedPeer { id, name, addrs });
                    self.changed();
                }
                relay_net::NetEvent::PeerAddrs { id, addrs } => {
                    if let Some(p) = self.cfg.peers.iter_mut().find(|p| p.id == id)
                        && p.addrs != addrs
                    {
                        p.addrs = addrs;
                        self.save_due = true;
                    }
                }
                relay_net::NetEvent::Unpaired { id } => {
                    self.cfg.peers.retain(|p| p.id != id);
                    self.changed();
                }
                relay_net::NetEvent::IncomingStream { peer, stream, kind, name } => {
                    if kind == StreamKind::Video {
                        let peer_name = self
                            .network
                            .peers
                            .iter()
                            .find(|p| p.id == peer)
                            .map(|p| p.name.clone())
                            .unwrap_or_default();
                        if let Some(net) = self.net.as_ref() {
                            self.video_mgr.on_incoming(net, &peer, stream, &name, &peer_name);
                        }
                        self.dirty = true;
                        continue;
                    }
                    let exists = self.cfg.mixer.strips.iter().any(
                        |s| matches!(&s.source, Source::Network { peer: p, stream: st } if *p == peer && *st == stream),
                    );
                    if !exists {
                        let peer_name = self
                            .network
                            .peers
                            .iter()
                            .find(|p| p.id == peer)
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| "Network".into());
                        let label = if self.cfg.mixer.strips.iter().any(|s| s.name == peer_name) {
                            format!("{peer_name} {name}")
                        } else {
                            peer_name
                        };
                        let routes: Vec<BusId> = self
                            .cfg
                            .mixer
                            .buses
                            .iter()
                            .find(|b| matches!(b.sink, Sink::Device { .. }))
                            .map(|b| vec![b.id])
                            .unwrap_or_default();
                        let strip = self.cfg.new_strip(label, Source::Network { peer, stream }, routes);
                        let id = strip.id;
                        self.cfg.mixer.strips.push(strip);
                        self.build_strip(id);
                        self.changed();
                    }
                }
                relay_net::NetEvent::StreamEnded { peer, stream } => {
                    self.video_mgr.on_ended(&peer, stream);
                    self.dirty = true;
                }
            }
        }
    }

    /// Network ports are "running" only while the link to that device is up.
    fn network_status(&self, peer: &str, receiving: Option<u32>) -> PortStatus {
        let Some(p) = self.network.peers.iter().find(|p| p.id == peer) else {
            return PortStatus::Missing(if self.net.is_some() {
                "Device not found".into()
            } else {
                "Network is off".into()
            });
        };
        match &p.state {
            LinkState::Live => match receiving {
                Some(stream) if !p.incoming.iter().any(|s| s.id == stream) => {
                    PortStatus::Missing(format!("{} isn't sending", p.name))
                }
                _ => PortStatus::Running,
            },
            LinkState::Degraded(why) => PortStatus::Error(why.clone()),
            LinkState::Lost(why) => PortStatus::Error(format!("{}: {why}", p.name)),
            _ => PortStatus::Missing(format!("{} isn't connected", p.name)),
        }
    }

    pub fn state(&self) -> EngineState {
        let m = self.graph.meters();
        let mut network = self.network.clone();
        network.this_name = self.cfg.device_name.clone();
        network.enabled = self.cfg.network_enabled;
        let mut mixer = self.cfg.mixer.clone();
        for s in &mut mixer.strips {
            if let Source::Network { peer, stream } = &s.source {
                s.status = self.network_status(peer, Some(*stream));
            }
        }
        for b in &mut mixer.buses {
            if let Sink::Network { peer } = &b.sink {
                b.status = self.network_status(peer, None);
            }
        }
        EngineState {
            version: crate::VERSION.to_string(),
            mixer,
            devices: self.devices.clone(),
            apps: self.apps.clone(),
            virtual_audio: VirtualAudio {
                state: self.virt.state(),
                is_default: self.cfg.relay_default && self.defaults_applied,
            },
            network,
            video: self.video.clone(),
            update: self.update.clone(),
            engine: EngineHealth {
                cpu_pct: self.cpu.3,
                block_ms: relay_mixer::BLOCK as f32 / 48.0,
                overruns: m.late_cycles.load(Ordering::Relaxed),
                uptime_s: self.started.elapsed().as_secs(),
            },
        }
    }
}

pub fn source_name(src: &Source) -> String {
    match src {
        Source::Device { name, .. } => short_device_name(name),
        Source::Loopback { name, .. } => format!("{} (loopback)", short_device_name(name)),
        Source::Virtual { index: 0 } => "Desktop".into(),
        Source::Virtual { index } => format!("Relay {}", index + 1),
        Source::App { process } => process.trim_end_matches(".exe").to_string(),
        Source::Network { .. } => "Network".into(),
        Source::None => "Input".into(),
    }
}

pub fn sink_name(sink: &Sink) -> String {
    match sink {
        Sink::Device { name, .. } => short_device_name(name),
        Sink::VirtualMic { index: 0 } => "Relay Mic".into(),
        Sink::VirtualMic { index } => format!("Relay Mic {}", index + 1),
        Sink::Network { .. } => "Network".into(),
        Sink::None => "Output".into(),
    }
}

/// "Speakers (Realtek(R) Audio)" -> "Speakers"; keeps names short in the mixer.
pub fn short_device_name(name: &str) -> String {
    let base = name.split(" (").next().unwrap_or(name).trim();
    if base.is_empty() { name.to_string() } else { base.to_string() }
}

fn first_run_layout(cfg: &mut Config, devices: &Devices) {
    let pick = |list: &[DeviceInfo]| {
        list.iter().find(|d| d.is_default && !d.is_relay).or_else(|| list.iter().find(|d| !d.is_relay)).cloned()
    };
    let out = pick(&devices.outputs);
    let inp = pick(&devices.inputs);

    let speakers_sink = out.map(|d| Sink::Device { key: d.key, name: d.name }).unwrap_or(Sink::None);
    let speakers = cfg.new_bus(sink_name(&speakers_sink), speakers_sink);
    let mic_bus = cfg.new_bus("Relay Mic".into(), Sink::VirtualMic { index: 0 });
    let desktop = cfg.new_strip("Desktop".into(), Source::Virtual { index: 0 }, vec![speakers.id]);
    let mic_source = inp.map(|d| Source::Device { key: d.key, name: d.name }).unwrap_or(Source::None);
    let mut mic = cfg.new_strip("Mic".into(), mic_source, vec![mic_bus.id]);
    mic.effects.eq_enabled = false;
    cfg.mixer.buses = vec![speakers, mic_bus];
    cfg.mixer.strips = vec![mic, desktop];
    cfg.initialized = true;
}

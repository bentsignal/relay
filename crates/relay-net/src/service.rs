//! The network service: discovery, connections, pairing and stream routing.

use crate::audio::{self, DatagramOut, Receiver, SharedReceiver};
use crate::discovery::{Announce, Discovery, Seen};
use crate::identity::{self, Identity};
use crate::tls;
use crate::wire::{self, Ctrl, NET_PROTOCOL};
use crossbeam_channel::{Receiver as CbReceiver, Sender as CbSender, unbounded};
use parking_lot::Mutex;
use relay_mixer::{PipeReader, PipeShared, PipeWriter, pipe};
use relay_proto::{LinkState, LinkStats, Peer, Platform, RemoteStream, StreamKind};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

pub const DEFAULT_PORT: u16 = 47800;

pub struct NetConfig {
    pub data_dir: PathBuf,
    pub name: String,
    pub version: String,
    pub port: u16,
    /// (id, name, last known addresses) of devices the user already allowed.
    pub paired: Vec<(String, String, Vec<String>)>,
}

#[derive(Debug)]
pub enum NetEvent {
    Peers {
        peers: Vec<Peer>,
        problem: Option<String>,
    },
    Paired {
        id: String,
        name: String,
    },
    /// Where a paired device was last reachable (persist for quick reconnects).
    PeerAddrs {
        id: String,
        addrs: Vec<String>,
    },
    Unpaired {
        id: String,
    },
    IncomingStream {
        peer: String,
        stream: u32,
        kind: StreamKind,
        name: String,
    },
    StreamEnded {
        peer: String,
        stream: u32,
    },
}

struct PeerEntry {
    id: String,
    name: String,
    platform: Platform,
    version: String,
    addrs: Vec<SocketAddr>,
    last_seen: Option<Instant>,
    paired: bool,
    state: LinkState,
    conn: Option<quinn::Connection>,
    /// Device ID of whoever opened `conn`.
    conn_initiator: String,
    ctrl: Option<mpsc::UnboundedSender<Ctrl>>,
    decision: Option<oneshot::Sender<bool>>,
    dialing: bool,
    lost_at: Option<Instant>,
    last_dial: Option<Instant>,
    incoming: Vec<RemoteStream>,
    stats: LinkStats,
    counters: (u64, u64, u64, u64, Instant),
    remote_reports: HashMap<u32, (f32, f32)>,
}

impl PeerEntry {
    fn new(id: String) -> Self {
        Self {
            id,
            name: "Relay".into(),
            platform: Platform::Other,
            version: String::new(),
            addrs: Vec::new(),
            last_seen: None,
            paired: false,
            state: LinkState::Available,
            conn: None,
            conn_initiator: String::new(),
            ctrl: None,
            decision: None,
            dialing: false,
            lost_at: None,
            last_dial: None,
            incoming: Vec::new(),
            stats: LinkStats::default(),
            counters: (0, 0, 0, 0, Instant::now()),
            remote_reports: HashMap::new(),
        }
    }

    fn to_proto(&self) -> Peer {
        Peer {
            id: self.id.clone(),
            name: self.name.clone(),
            platform: self.platform,
            version: self.version.clone(),
            paired: self.paired,
            state: self.state.clone(),
            addresses: self.addrs.iter().map(|a| a.to_string()).collect(),
            stats: self.stats.clone(),
            incoming: self.incoming.clone(),
        }
    }
}

struct SinkEntry {
    name: String,
    _sender: audio::Sender,
}

struct Shared {
    me: Identity,
    name: Mutex<String>,
    version: String,
    port: u16,
    peers: Mutex<HashMap<String, PeerEntry>>,
    sinks: Mutex<HashMap<(String, u32), SinkEntry>>,
    receivers: Mutex<HashMap<(String, u32), SharedReceiver>>,
    source_pipes: Mutex<HashMap<(String, u32), Arc<PipeShared>>>,
    video_sinks: Mutex<HashMap<(String, u32), (String, Arc<std::sync::atomic::AtomicBool>)>>,
    video_subs: Mutex<HashMap<(String, u32), VideoSub>>,
    events: CbSender<NetEvent>,
    endpoint: quinn::Endpoint,
    problem: Mutex<Option<String>>,
}

pub struct NetHandle {
    shared: Arc<Shared>,
    discovery: Discovery,
    rt: tokio::runtime::Runtime,
}

struct PeerOut {
    shared: Arc<Shared>,
    peer: String,
}

impl DatagramOut for PeerOut {
    fn send(&self, bytes: bytes::Bytes) -> bool {
        let conn = self.shared.peers.lock().get(&self.peer).and_then(|p| p.conn.clone());
        match conn {
            Some(c) => c.send_datagram(bytes).is_ok(),
            None => false,
        }
    }
}

fn now_us(epoch: Instant) -> u64 {
    epoch.elapsed().as_micros() as u64
}

impl NetHandle {
    pub fn start(cfg: NetConfig) -> anyhow::Result<(Self, CbReceiver<NetEvent>)> {
        let me = identity::load_or_create(&cfg.data_dir)?;
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("relay-net")
            .enable_all()
            .build()?;
        let server = tls::server_config(&me)?;
        let mut problem = None;
        let endpoint = {
            let _g = rt.enter();
            match quinn::Endpoint::server(server.clone(), SocketAddr::from(([0, 0, 0, 0], cfg.port))) {
                Ok(e) => e,
                Err(e) => {
                    problem = Some(format!("Port {} is busy; using another", cfg.port));
                    tracing::warn!("bind {}: {e}", cfg.port);
                    quinn::Endpoint::server(server, SocketAddr::from(([0, 0, 0, 0], 0)))?
                }
            }
        };
        let port = endpoint.local_addr()?.port();
        let (tx, rx) = unbounded();
        let mut peers = HashMap::new();
        for (id, name, addrs) in cfg.paired {
            let mut p = PeerEntry::new(id.clone());
            p.name = name;
            p.addrs = addrs.iter().filter_map(|a| a.parse().ok()).collect();
            p.paired = true;
            p.state = LinkState::Offline;
            peers.insert(id, p);
        }
        let shared = Arc::new(Shared {
            name: Mutex::new(cfg.name.clone()),
            version: cfg.version.clone(),
            port,
            peers: Mutex::new(peers),
            sinks: Mutex::new(HashMap::new()),
            receivers: Mutex::new(HashMap::new()),
            source_pipes: Mutex::new(HashMap::new()),
            video_sinks: Mutex::new(HashMap::new()),
            video_subs: Mutex::new(HashMap::new()),
            events: tx,
            endpoint,
            problem: Mutex::new(problem),
            me,
        });

        let (seen_tx, seen_rx) = unbounded::<Seen>();
        let discovery = Discovery::start(
            Announce {
                relay: 1,
                id: shared.me.id.clone(),
                name: cfg.name,
                platform: Platform::current(),
                version: cfg.version,
                port,
            },
            seen_tx,
        );
        if !discovery.problems.is_empty() {
            tracing::warn!("discovery problems: {:?}", discovery.problems);
            if discovery.problems.len() >= 2 {
                *shared.problem.lock() = Some("Can't search the network for devices".into());
            }
        }

        rt.spawn(accept_loop(shared.clone()));
        rt.spawn(housekeeping(shared.clone(), seen_rx));
        tracing::info!(id = %shared.me.id, port, "network service started");
        Ok((Self { shared, discovery, rt }, rx))
    }

    pub fn id(&self) -> String {
        self.shared.me.id.clone()
    }

    pub fn port(&self) -> u16 {
        self.shared.port
    }

    pub fn set_name(&self, name: String) {
        *self.shared.name.lock() = name.clone();
        self.discovery.set_name(name.clone());
        for p in self.shared.peers.lock().values() {
            if let Some(c) = &p.ctrl {
                let _ = c.send(Ctrl::Name { name: name.clone() });
            }
        }
    }

    /// Connects to a device by address (fallback when discovery can't see it).
    pub fn connect_addr(&self, addr: SocketAddr) {
        let shared = self.shared.clone();
        self.rt.spawn(async move {
            if let Err(e) = dial_addr(shared, addr).await {
                tracing::info!(%addr, "connect by address failed: {e:#}");
            }
        });
    }

    /// User asked to connect to (and pair with) a device.
    pub fn connect(&self, peer: &str) {
        let shared = self.shared.clone();
        let peer = peer.to_string();
        self.rt.spawn(async move { dial(shared, peer).await });
    }

    pub fn accept(&self, peer: &str) {
        if let Some(tx) = self.shared.peers.lock().get_mut(peer).and_then(|p| p.decision.take()) {
            let _ = tx.send(true);
        }
    }

    pub fn reject(&self, peer: &str) {
        if let Some(tx) = self.shared.peers.lock().get_mut(peer).and_then(|p| p.decision.take()) {
            let _ = tx.send(false);
        }
    }

    pub fn forget(&self, peer: &str) {
        let mut peers = self.shared.peers.lock();
        if let Some(p) = peers.get_mut(peer) {
            p.paired = false;
            if let Some(c) = p.conn.take() {
                c.close(1u32.into(), b"forgotten");
            }
            p.ctrl = None;
            p.state = LinkState::Available;
        }
        drop(peers);
        let _ = self.shared.events.send(NetEvent::Unpaired { id: peer.to_string() });
        emit_peers(&self.shared);
    }

    /// Starts sending a mixer bus to `peer` as `stream`. The returned writer is
    /// fed by the mixer at its own clock.
    pub fn open_sink(&self, peer: &str, stream: u32, name: &str) -> PipeWriter {
        let (w, r) = pipe(audio::RATE, audio::RATE as usize / 4);
        let out = Arc::new(PeerOut { shared: self.shared.clone(), peer: peer.to_string() });
        let sender = audio::spawn_sender(stream, r, out);
        self.shared
            .sinks
            .lock()
            .insert((peer.to_string(), stream), SinkEntry { name: name.to_string(), _sender: sender });
        if let Some(c) = self.shared.peers.lock().get(peer).and_then(|p| p.ctrl.clone()) {
            let _ = c.send(stream_start(stream, name));
        }
        w
    }

    pub fn close_sink(&self, peer: &str, stream: u32) {
        if self.shared.sinks.lock().remove(&(peer.to_string(), stream)).is_some()
            && let Some(c) = self.shared.peers.lock().get(peer).and_then(|p| p.ctrl.clone())
        {
            let _ = c.send(Ctrl::StreamStop { stream });
        }
    }

    /// Receives `stream` from `peer` into a fresh pipe (replacing any previous one).
    pub fn open_source(&self, peer: &str, stream: u32) -> PipeReader {
        let (w, r) = pipe(audio::RATE, audio::RATE as usize / 2);
        let key = (peer.to_string(), stream);
        self.shared.source_pipes.lock().insert(key.clone(), r.shared().clone());
        let mut recv = self.shared.receivers.lock();
        let entry = recv.entry(key).or_insert_with(|| Arc::new(Mutex::new(Receiver::new().expect("opus decoder"))));
        entry.lock().writer = Some(w);
        r
    }

    /// Starts a video stream to `peer`. Frames are handed to the returned sender.
    pub fn open_video_sink(&self, peer: &str, stream: u32, name: &str) -> VideoTx {
        let key = Arc::new(std::sync::atomic::AtomicBool::new(true));
        self.shared.video_sinks.lock().insert((peer.to_string(), stream), (name.to_string(), key.clone()));
        if let Some(c) = self.shared.peers.lock().get(peer).and_then(|p| p.ctrl.clone()) {
            let _ = c.send(video_start(stream, name));
        }
        VideoTx {
            shared: self.shared.clone(),
            peer: peer.to_string(),
            stream,
            rt: self.rt.handle().clone(),
            inflight: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            key,
        }
    }

    /// Receives video `stream` from `peer` (replacing an earlier subscriber).
    pub fn subscribe_video(&self, peer: &str, stream: u32) -> CbReceiver<relay_proto::EncodedVideo> {
        let (tx, rx) = crossbeam_channel::bounded(8);
        let mut subs = self.shared.video_subs.lock();
        let sub = subs.entry((peer.to_string(), stream)).or_default();
        sub.tx = Some(tx);
        sub.reorder = crate::video::Reorder::default();
        drop(subs);
        // Start from a keyframe.
        if let Some(c) = self.shared.peers.lock().get(peer).and_then(|p| p.ctrl.clone()) {
            let _ = c.send(Ctrl::Keyframe { stream });
        }
        rx
    }

    /// Asks `peer` for a fresh keyframe on video `stream`.
    pub fn request_keyframe(&self, peer: &str, stream: u32) {
        if let Some(c) = self.shared.peers.lock().get(peer).and_then(|p| p.ctrl.clone()) {
            let _ = c.send(Ctrl::Keyframe { stream });
        }
    }

    pub fn link_state(&self, peer: &str) -> Option<LinkState> {
        self.shared.peers.lock().get(peer).map(|p| p.state.clone())
    }
}

impl Drop for NetHandle {
    fn drop(&mut self) {
        for p in self.shared.peers.lock().values() {
            if let Some(c) = &p.conn {
                c.close(0u32.into(), b"bye");
            }
        }
        self.shared.endpoint.close(0u32.into(), b"bye");
    }
}

#[derive(Default)]
struct VideoSub {
    reorder: crate::video::Reorder,
    tx: Option<CbSender<relay_proto::EncodedVideo>>,
}

/// Sends encoded video frames to one peer.
pub struct VideoTx {
    shared: Arc<Shared>,
    peer: String,
    stream: u32,
    rt: tokio::runtime::Handle,
    inflight: Arc<std::sync::atomic::AtomicUsize>,
    key: Arc<std::sync::atomic::AtomicBool>,
}

impl VideoTx {
    /// True (once) when the receiver needs a keyframe.
    pub fn take_keyframe_request(&self) -> bool {
        self.key.swap(false, std::sync::atomic::Ordering::Relaxed)
    }

    pub fn connected(&self) -> bool {
        self.shared.peers.lock().get(&self.peer).map(|p| p.conn.is_some()).unwrap_or(false)
    }

    /// Queues a frame. Non-key frames are dropped while the link is backed up,
    /// and a keyframe is requested so the picture recovers cleanly.
    pub fn send(&self, mut frame: relay_proto::EncodedVideo) -> bool {
        use std::sync::atomic::Ordering;
        frame.stream = self.stream;
        let Some(conn) = self.shared.peers.lock().get(&self.peer).and_then(|p| p.conn.clone()) else { return false };
        if self.inflight.load(Ordering::Relaxed) >= 4 && !frame.key {
            self.key.store(true, Ordering::Relaxed);
            return false;
        }
        self.inflight.fetch_add(1, Ordering::Relaxed);
        let inflight = self.inflight.clone();
        self.rt.spawn(async move {
            let res = async {
                let mut s = conn.open_uni().await?;
                let _ = s.set_priority(if frame.key { i32::MAX } else { frame.seq as i32 & 0x3fff_ffff });
                s.write_all(&crate::video::header(&frame)).await?;
                s.write_all(&frame.data).await?;
                s.finish()?;
                anyhow::Ok(())
            }
            .await;
            if let Err(e) = res {
                tracing::debug!("video send: {e:#}");
            }
            inflight.fetch_sub(1, Ordering::Relaxed);
        });
        true
    }
}

impl Drop for VideoTx {
    fn drop(&mut self) {
        self.shared.video_sinks.lock().remove(&(self.peer.clone(), self.stream));
        if let Some(c) = self.shared.peers.lock().get(&self.peer).and_then(|p| p.ctrl.clone()) {
            let _ = c.send(Ctrl::StreamStop { stream: self.stream });
        }
    }
}

fn video_start(stream: u32, name: &str) -> Ctrl {
    Ctrl::StreamStart {
        stream,
        kind: StreamKind::Video,
        name: name.to_string(),
        codec: "h264".into(),
        rate: 0,
        channels: 0,
    }
}

fn deliver_video(shared: &Shared, peer: &str, frame: relay_proto::EncodedVideo, ctrl: &mpsc::UnboundedSender<Ctrl>) {
    let stream = frame.stream;
    let mut out = Vec::new();
    let mut subs = shared.video_subs.lock();
    let sub = subs.entry((peer.to_string(), stream)).or_default();
    sub.reorder.push(frame, &mut out);
    emit_video(sub, stream, out, ctrl);
}

fn emit_video(sub: &mut VideoSub, stream: u32, out: Vec<crate::video::Output>, ctrl: &mpsc::UnboundedSender<Ctrl>) {
    for o in out {
        match o {
            crate::video::Output::Frame(f) => {
                if let Some(tx) = &sub.tx
                    && tx.try_send(f).is_err()
                {
                    // The decoder fell behind: skip to the next keyframe.
                    sub.reorder = crate::video::Reorder::default();
                    let _ = ctrl.send(Ctrl::Keyframe { stream });
                }
            }
            crate::video::Output::RequestKeyframe => {
                let _ = ctrl.send(Ctrl::Keyframe { stream });
            }
        }
    }
}

fn stream_start(stream: u32, name: &str) -> Ctrl {
    Ctrl::StreamStart {
        stream,
        kind: StreamKind::Audio,
        name: name.to_string(),
        codec: "opus".into(),
        rate: audio::RATE,
        channels: 2,
    }
}

fn emit_peers(shared: &Shared) {
    let mut peers: Vec<Peer> = shared
        .peers
        .lock()
        .values()
        .filter(|p| p.paired || p.last_seen.is_some() || p.conn.is_some())
        .map(|p| p.to_proto())
        .collect();
    peers.sort_by(|a, b| b.paired.cmp(&a.paired).then(a.name.cmp(&b.name)));
    let problem = shared.problem.lock().clone();
    let _ = shared.events.send(NetEvent::Peers { peers, problem });
}

fn hello(shared: &Shared) -> Ctrl {
    Ctrl::Hello {
        id: shared.me.id.clone(),
        name: shared.name.lock().clone(),
        platform: Platform::current(),
        version: shared.version.clone(),
        protocol: NET_PROTOCOL,
    }
}

fn set_state(shared: &Shared, peer: &str, state: LinkState) {
    if let Some(p) = shared.peers.lock().get_mut(peer) {
        p.state = state;
    }
    emit_peers(shared);
}

fn apply_hello(shared: &Shared, peer: &str, h: &Ctrl) {
    if let Ctrl::Hello { name, platform, version, .. } = h {
        let mut peers = shared.peers.lock();
        let p = peers.entry(peer.to_string()).or_insert_with(|| PeerEntry::new(peer.to_string()));
        p.name = name.clone();
        p.platform = *platform;
        p.version = version.clone();
    }
}

fn mark_paired(shared: &Shared, peer: &str) {
    let mut newly = None;
    if let Some(p) = shared.peers.lock().get_mut(peer)
        && !p.paired
    {
        p.paired = true;
        newly = Some(p.name.clone());
    }
    if let Some(name) = newly {
        let _ = shared.events.send(NetEvent::Paired { id: peer.to_string(), name });
    }
}

async fn dial(shared: Arc<Shared>, peer: String) {
    let addrs = {
        let mut peers = shared.peers.lock();
        let Some(p) = peers.get_mut(&peer) else { return };
        if p.dialing || p.conn.is_some() {
            return;
        }
        p.dialing = true;
        let seen = p.last_seen.map(|t| t.elapsed() < Duration::from_secs(20)).unwrap_or(false);
        if !matches!(p.state, LinkState::Live | LinkState::Degraded(_)) && (seen || !p.paired) {
            p.state = LinkState::Connecting;
        }
        p.addrs.clone()
    };
    emit_peers(&shared);
    let result = dial_inner(&shared, &peer, &addrs).await;
    if let Some(p) = shared.peers.lock().get_mut(&peer) {
        p.dialing = false;
    }
    if let Err(e) = result {
        tracing::info!(%peer, "dial failed: {e:#}");
        let mut peers = shared.peers.lock();
        if let Some(p) = peers.get_mut(&peer)
            && p.conn.is_none()
            && matches!(p.state, LinkState::Connecting | LinkState::Requesting)
        {
            let seen = p.last_seen.map(|t| t.elapsed() < Duration::from_secs(20)).unwrap_or(false);
            p.state = if !seen && p.paired {
                // Not on the network right now; it was only tried at its last address.
                LinkState::Offline
            } else if p.paired && p.lost_at.is_some() {
                LinkState::Lost(short_err(&e))
            } else {
                LinkState::Refused(short_err(&e))
            };
        }
        drop(peers);
        emit_peers(&shared);
    }
}

fn short_err(e: &anyhow::Error) -> String {
    let s = format!("{e:#}");
    if s.contains("timed out") || s.contains("timeout") {
        "Can't reach it (check its firewall)".into()
    } else if s.contains("declined") {
        "Declined".into()
    } else if s.contains("version") || s.contains("Update") {
        s
    } else {
        "Couldn't connect".into()
    }
}

/// Orders addresses so the likeliest LAN path is tried first: same subnet as
/// one of our interfaces, then private ranges, then everything else.
fn rank_addrs(addrs: &[SocketAddr]) -> Vec<SocketAddr> {
    let ours: Vec<(std::net::Ipv4Addr, std::net::Ipv4Addr)> = if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v) if !v.ip.is_loopback() => Some((v.ip, v.netmask)),
            _ => None,
        })
        .collect();
    let rank = |a: &SocketAddr| -> u8 {
        let std::net::IpAddr::V4(ip) = a.ip() else { return 5 };
        let o = ip.octets();
        let same_subnet = ours.iter().any(|(mine, mask)| {
            let (m, k) = (mine.octets(), mask.octets());
            (0..4).all(|i| m[i] & k[i] == o[i] & k[i])
        });
        if ip.is_loopback() {
            4
        } else if same_subnet && (o[0] == 192 || o[0] == 10) {
            0
        } else if same_subnet {
            1
        } else if ip.is_private() {
            2
        } else {
            3
        }
    };
    let mut v = addrs.to_vec();
    v.sort_by_key(rank);
    v
}

async fn dial_inner(shared: &Arc<Shared>, peer: &str, addrs: &[SocketAddr]) -> anyhow::Result<()> {
    anyhow::ensure!(!addrs.is_empty(), "no address known");
    let addrs = &rank_addrs(addrs);
    let cfg = tls::client_config(&shared.me, Some(peer.to_string()))?;
    let mut last_err = anyhow::anyhow!("no address worked");
    for addr in addrs {
        let attempt = async {
            let connecting = shared.endpoint.connect_with(cfg.clone(), *addr, "relay")?;
            let conn = connecting.await?;
            anyhow::Ok(conn)
        };
        match tokio::time::timeout(Duration::from_secs(3), attempt).await {
            Ok(Ok(conn)) => {
                let (mut send, mut recv) = conn.open_bi().await?;
                wire::write_ctrl(&mut send, &hello(shared)).await?;
                let their = tokio::time::timeout(Duration::from_secs(5), wire::read_ctrl(&mut recv)).await??;
                apply_hello(shared, peer, &their);
                if let Ctrl::Hello { protocol, .. } = &their
                    && *protocol != NET_PROTOCOL
                {
                    let name = shared.peers.lock().get(peer).map(|p| p.name.clone()).unwrap_or_default();
                    anyhow::bail!(
                        "Update Relay on {} to connect",
                        if *protocol < NET_PROTOCOL { name } else { "this device".into() }
                    );
                }
                set_state(shared, peer, LinkState::Requesting);
                match tokio::time::timeout(Duration::from_secs(130), wire::read_ctrl(&mut recv)).await?? {
                    Ctrl::Accepted => {
                        mark_paired(shared, peer);
                        let shared = shared.clone();
                        let me = shared.me.id.clone();
                        tokio::spawn(session(shared, conn, me, peer.to_string(), send, recv));
                        return Ok(());
                    }
                    Ctrl::Rejected { reason } => anyhow::bail!("declined: {reason}"),
                    other => anyhow::bail!("unexpected {other:?}"),
                }
            }
            Ok(Err(e)) => last_err = e,
            Err(_) => last_err = anyhow::anyhow!("timed out reaching {addr}"),
        }
    }
    Err(last_err)
}

/// Connects to an address without knowing which device is there (a fallback
/// for networks where discovery can't see it). Trust still requires the other
/// side to allow the connection.
async fn dial_addr(shared: Arc<Shared>, addr: SocketAddr) -> anyhow::Result<()> {
    let cfg = tls::client_config(&shared.me, None)?;
    let connecting = shared.endpoint.connect_with(cfg, addr, "relay")?;
    let conn = tokio::time::timeout(Duration::from_secs(3), connecting).await??;
    let peer = tls::peer_id(&conn).ok_or_else(|| anyhow::anyhow!("no peer certificate"))?;
    {
        let mut peers = shared.peers.lock();
        let p = peers.entry(peer.clone()).or_insert_with(|| PeerEntry::new(peer.clone()));
        if !p.addrs.contains(&addr) {
            p.addrs.insert(0, addr);
        }
        p.last_seen = Some(Instant::now());
        if p.conn.is_some() {
            conn.close(0u32.into(), b"duplicate");
            return Ok(());
        }
    }
    conn.close(0u32.into(), b"probe");
    dial(shared, peer).await;
    Ok(())
}

async fn accept_loop(shared: Arc<Shared>) {
    while let Some(incoming) = shared.endpoint.accept().await {
        let shared = shared.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_incoming(shared, incoming).await {
                tracing::info!("incoming connection failed: {e:#}");
            }
        });
    }
}

async fn handle_incoming(shared: Arc<Shared>, incoming: quinn::Incoming) -> anyhow::Result<()> {
    let conn = incoming.await?;
    let peer = tls::peer_id(&conn).ok_or_else(|| anyhow::anyhow!("no peer certificate"))?;
    let (mut send, mut recv) = tokio::time::timeout(Duration::from_secs(5), conn.accept_bi()).await??;
    let their = tokio::time::timeout(Duration::from_secs(5), wire::read_ctrl(&mut recv)).await??;
    let Ctrl::Hello { protocol, .. } = &their else { anyhow::bail!("expected hello") };
    let protocol = *protocol;
    {
        let mut peers = shared.peers.lock();
        let p = peers.entry(peer.clone()).or_insert_with(|| PeerEntry::new(peer.clone()));
        let addr = conn.remote_address();
        if !p.addrs.contains(&addr) && addr.port() != 0 {
            // Peers dial from their listening socket, so this is where they can be reached.
            p.addrs.retain(|a| a.ip() != addr.ip());
            p.addrs.insert(0, addr);
        }
        p.last_seen.get_or_insert_with(Instant::now);
    }
    apply_hello(&shared, &peer, &their);
    wire::write_ctrl(&mut send, &hello(&shared)).await?;
    if protocol != NET_PROTOCOL {
        let reason = "Relay versions differ; update both devices".to_string();
        wire::write_ctrl(&mut send, &Ctrl::Rejected { reason: reason.clone() }).await?;
        set_state(&shared, &peer, LinkState::Refused(reason));
        return Ok(());
    }
    let paired = shared.peers.lock().get(&peer).map(|p| p.paired).unwrap_or(false);
    let allowed = if paired {
        true
    } else {
        let (tx, rx) = oneshot::channel();
        {
            let mut peers = shared.peers.lock();
            if let Some(p) = peers.get_mut(&peer) {
                p.decision = Some(tx);
                p.state = LinkState::Incoming;
            }
        }
        emit_peers(&shared);
        let decided = tokio::select! {
            r = tokio::time::timeout(Duration::from_secs(120), rx) => matches!(r, Ok(Ok(true))),
            _ = conn.closed() => false,
        };
        if let Some(p) = shared.peers.lock().get_mut(&peer) {
            p.decision = None;
            if p.state == LinkState::Incoming {
                p.state = LinkState::Available;
            }
        }
        decided
    };
    if !allowed {
        let _ = wire::write_ctrl(&mut send, &Ctrl::Rejected { reason: "Declined".into() }).await;
        let _ = send.finish();
        tokio::time::sleep(Duration::from_millis(200)).await;
        conn.close(2u32.into(), b"declined");
        emit_peers(&shared);
        return Ok(());
    }
    wire::write_ctrl(&mut send, &Ctrl::Accepted).await?;
    mark_paired(&shared, &peer);
    session(shared, conn, peer.clone(), peer, send, recv).await;
    Ok(())
}

/// Runs an established, accepted connection until it closes.
async fn session(
    shared: Arc<Shared>,
    conn: quinn::Connection,
    initiator: String,
    peer: String,
    mut send: quinn::SendStream,
    mut recv: quinn::RecvStream,
) {
    let (ctrl_tx, mut ctrl_rx) = mpsc::unbounded_channel::<Ctrl>();
    // Register, resolving duplicate connections deterministically: keep the one
    // opened by the lower device ID.
    {
        let mut peers = shared.peers.lock();
        let p = peers.entry(peer.clone()).or_insert_with(|| PeerEntry::new(peer.clone()));
        if let Some(existing) = &p.conn {
            let keep_new = initiator < p.conn_initiator;
            if keep_new {
                existing.close(3u32.into(), b"duplicate");
            } else {
                drop(peers);
                conn.close(3u32.into(), b"duplicate");
                return;
            }
        }
        p.conn = Some(conn.clone());
        p.conn_initiator = initiator.clone();
        p.ctrl = Some(ctrl_tx.clone());
        p.state = LinkState::Live;
        p.lost_at = None;
        p.counters = (0, 0, 0, 0, Instant::now());
    }
    tracing::info!(%peer, "connected");
    let addrs: Vec<String> =
        shared.peers.lock().get(&peer).map(|p| p.addrs.iter().map(|a| a.to_string()).collect()).unwrap_or_default();
    let _ = shared.events.send(NetEvent::PeerAddrs { id: peer.clone(), addrs });
    emit_peers(&shared);

    // Announce every stream we already send to this peer.
    let announce: Vec<Ctrl> = shared
        .sinks
        .lock()
        .iter()
        .filter(|((p, _), _)| *p == peer)
        .map(|((_, s), e)| stream_start(*s, &e.name))
        .chain(
            shared
                .video_sinks
                .lock()
                .iter()
                .filter(|((p, _), _)| *p == peer)
                .map(|((_, s), (name, _))| video_start(*s, name)),
        )
        .collect();
    for c in announce {
        let _ = ctrl_tx.send(c);
    }

    let epoch = Instant::now();
    let writer = tokio::spawn(async move {
        while let Some(msg) = ctrl_rx.recv().await {
            if wire::write_ctrl(&mut send, &msg).await.is_err() {
                break;
            }
        }
    });

    let reader = {
        let shared = shared.clone();
        let peer = peer.clone();
        let ctrl_tx = ctrl_tx.clone();
        tokio::spawn(async move {
            while let Ok(msg) = wire::read_ctrl(&mut recv).await {
                match msg {
                    Ctrl::Ping { t } => {
                        let _ = ctrl_tx.send(Ctrl::Pong { t });
                    }
                    Ctrl::Pong { .. } => {}
                    Ctrl::StreamStart { stream, kind, name, .. } => {
                        {
                            let mut peers = shared.peers.lock();
                            if let Some(p) = peers.get_mut(&peer) {
                                p.incoming.retain(|s| s.id != stream);
                                p.incoming.push(RemoteStream { id: stream, kind, name: name.clone() });
                            }
                        }
                        let _ = shared.events.send(NetEvent::IncomingStream { peer: peer.clone(), stream, kind, name });
                        emit_peers(&shared);
                    }
                    Ctrl::StreamStop { stream } => {
                        if let Some(p) = shared.peers.lock().get_mut(&peer) {
                            p.incoming.retain(|s| s.id != stream);
                        }
                        let _ = shared.events.send(NetEvent::StreamEnded { peer: peer.clone(), stream });
                        emit_peers(&shared);
                    }
                    Ctrl::Keyframe { stream } => {
                        if let Some((_, k)) = shared.video_sinks.lock().get(&(peer.clone(), stream)) {
                            k.store(true, std::sync::atomic::Ordering::Relaxed);
                        }
                    }
                    Ctrl::Report { stream, loss_pct, jitter_ms } => {
                        if let Some(p) = shared.peers.lock().get_mut(&peer) {
                            p.remote_reports.insert(stream, (loss_pct, jitter_ms));
                        }
                    }
                    Ctrl::Name { name } => {
                        if let Some(p) = shared.peers.lock().get_mut(&peer) {
                            p.name = name;
                        }
                        emit_peers(&shared);
                    }
                    _ => {}
                }
            }
        })
    };

    let datagrams = {
        let shared = shared.clone();
        let peer = peer.clone();
        let conn = conn.clone();
        tokio::spawn(async move {
            while let Ok(d) = conn.read_datagram().await {
                if let Some(p) = wire::parse_audio(&d) {
                    let key = (peer.clone(), p.stream);
                    let rx = shared.receivers.lock().get(&key).cloned();
                    let rx = match rx {
                        Some(r) => r,
                        None => {
                            let Ok(r) = Receiver::new() else { continue };
                            let r = Arc::new(Mutex::new(r));
                            shared.receivers.lock().insert(key, r.clone());
                            r
                        }
                    };
                    rx.lock().on_packet(&p);
                }
            }
        })
    };

    let uni = {
        let shared = shared.clone();
        let peer = peer.clone();
        let conn = conn.clone();
        let ctrl_tx = ctrl_tx.clone();
        tokio::spawn(async move {
            while let Ok(mut rs) = conn.accept_uni().await {
                let shared = shared.clone();
                let peer = peer.clone();
                let ctrl_tx = ctrl_tx.clone();
                tokio::spawn(async move {
                    let mut h = [0u8; crate::video::HEADER];
                    if rs.read_exact(&mut h).await.is_err() {
                        return;
                    }
                    let Some((mut frame, len)) = crate::video::parse_header(&h) else { return };
                    let mut data = vec![0u8; len];
                    if rs.read_exact(&mut data).await.is_err() {
                        return;
                    }
                    frame.data = data;
                    deliver_video(&shared, &peer, frame, &ctrl_tx);
                });
            }
        })
    };

    let video_gaps = {
        let shared = shared.clone();
        let peer = peer.clone();
        let ctrl_tx = ctrl_tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(30));
            loop {
                iv.tick().await;
                let mut subs = shared.video_subs.lock();
                for ((p, stream), sub) in subs.iter_mut() {
                    if *p != peer {
                        continue;
                    }
                    let mut out = Vec::new();
                    sub.reorder.flush(&mut out);
                    emit_video(sub, *stream, out, &ctrl_tx);
                }
            }
        })
    };

    let pinger = {
        let ctrl_tx = ctrl_tx.clone();
        tokio::spawn(async move {
            let mut iv = tokio::time::interval(Duration::from_millis(500));
            loop {
                iv.tick().await;
                if ctrl_tx.send(Ctrl::Ping { t: now_us(epoch) }).is_err() {
                    break;
                }
            }
        })
    };

    let reason = conn.closed().await;
    pinger.abort();
    uni.abort();
    video_gaps.abort();
    datagrams.abort();
    reader.abort();
    writer.abort();
    tracing::info!(%peer, "disconnected: {reason}");

    let mut peers = shared.peers.lock();
    if let Some(p) = peers.get_mut(&peer)
        && p.conn.as_ref().map(|c| c.stable_id()) == Some(conn.stable_id())
    {
        p.conn = None;
        p.ctrl = None;
        let ended: Vec<u32> = p.incoming.drain(..).map(|s| s.id).collect();
        p.state = match &reason {
            quinn::ConnectionError::ApplicationClosed(c) if c.reason.as_ref() == b"bye" => {
                LinkState::Lost(format!("{} closed Relay", p.name))
            }
            quinn::ConnectionError::ApplicationClosed(c)
                if c.reason.as_ref() == b"forgotten" || c.reason.as_ref() == b"declined" =>
            {
                LinkState::Available
            }
            quinn::ConnectionError::TimedOut => LinkState::Lost("Stopped responding".into()),
            quinn::ConnectionError::LocallyClosed => LinkState::Available,
            _ => LinkState::Lost("Connection dropped".into()),
        };
        if matches!(p.state, LinkState::Lost(_)) {
            p.lost_at = Some(Instant::now());
        }
        drop(peers);
        for s in ended {
            let _ = shared.events.send(NetEvent::StreamEnded { peer: peer.clone(), stream: s });
        }
    } else {
        drop(peers);
    }
    emit_peers(&shared);
}

/// Folds discovery results in, auto-reconnects paired devices, updates stats.
async fn housekeeping(shared: Arc<Shared>, seen: CbReceiver<Seen>) {
    let mut tick = tokio::time::interval(Duration::from_millis(1000));
    loop {
        tick.tick().await;
        let mut changed = false;
        while let Ok(s) = seen.try_recv() {
            let mut peers = shared.peers.lock();
            let p = peers.entry(s.id.clone()).or_insert_with(|| PeerEntry::new(s.id.clone()));
            if p.last_seen.is_none() {
                changed = true;
            }
            p.last_seen = Some(Instant::now());
            if p.conn.is_none() && p.name != s.name {
                p.name = s.name.clone();
                changed = true;
            }
            p.platform = s.platform;
            p.version = s.version.clone();
            if !p.addrs.contains(&s.addr) {
                p.addrs.retain(|a| a.ip() != s.addr.ip());
                p.addrs.insert(0, s.addr);
                changed = true;
            }
            if p.state == LinkState::Offline {
                p.state = LinkState::Available;
                changed = true;
            }
        }

        let mut to_dial = Vec::new();
        {
            let mut peers = shared.peers.lock();
            let me = shared.me.id.clone();
            for p in peers.values_mut() {
                let recent = p.last_seen.map(|t| t.elapsed() < Duration::from_secs(20)).unwrap_or(false);
                if p.conn.is_none() && !recent && p.last_seen.is_some() {
                    if p.paired {
                        if !matches!(p.state, LinkState::Offline | LinkState::Lost(_)) {
                            p.state = LinkState::Offline;
                            changed = true;
                        }
                    } else {
                        p.last_seen = None;
                        changed = true;
                    }
                }
                // Paired devices reconnect on their own. The lower ID dials;
                // the other side steps in if that hasn't happened for a while.
                // Paired devices reconnect on their own, also at their last known
                // address when discovery can't see them (e.g. multicast is
                // filtered). Both ends dialing also gets through firewalls that
                // only admit replies.
                let reachable = recent || !p.addrs.is_empty();
                let due =
                    p.last_dial.map(|t| t.elapsed() > Duration::from_secs(if recent { 2 } else { 5 })).unwrap_or(true);
                if p.paired
                    && p.conn.is_none()
                    && !p.dialing
                    && reachable
                    && due
                    && !matches!(p.state, LinkState::Refused(_))
                {
                    let waited = p.lost_at.map(|t| t.elapsed() > Duration::from_secs(6)).unwrap_or(true);
                    if me < p.id || waited || !recent {
                        p.last_dial = Some(Instant::now());
                        to_dial.push(p.id.clone());
                    }
                }
            }
        }
        for id in to_dial {
            tokio::spawn(dial(shared.clone(), id));
        }

        // Link statistics and health.
        {
            let receivers = shared.receivers.lock();
            let pipes = shared.source_pipes.lock();
            let mut peers = shared.peers.lock();
            for p in peers.values_mut() {
                let Some(conn) = &p.conn else { continue };
                let st = conn.stats();
                let (tx0, rx0, sent0, lost0, t0) = p.counters;
                let dt = t0.elapsed().as_secs_f32().max(0.001);
                let tx = st.udp_tx.bytes;
                let rx = st.udp_rx.bytes;
                let sent = st.path.sent_packets;
                let lost = st.path.lost_packets;
                p.stats.rtt_ms = conn.rtt().as_secs_f32() * 1000.0;
                if t0.elapsed() > Duration::from_millis(500) && tx0 > 0 {
                    p.stats.send_kbps = (tx - tx0) as f32 * 8.0 / 1000.0 / dt;
                    p.stats.recv_kbps = (rx - rx0) as f32 * 8.0 / 1000.0 / dt;
                }
                let sent_d = sent.saturating_sub(sent0).max(1);
                let path_loss = lost.saturating_sub(lost0) as f32 / sent_d as f32 * 100.0;
                p.counters = (tx, rx, sent, lost, Instant::now());
                let mut loss = path_loss;
                let mut jitter = 0f32;
                let mut buffer = 0f32;
                let mut glitches = 0u32;
                for ((peer, stream), r) in receivers.iter() {
                    if *peer != p.id {
                        continue;
                    }
                    let mut r = r.lock();
                    let s = r.stats;
                    let total = (s.received + s.lost).max(1);
                    loss = loss.max(s.lost as f32 / total as f32 * 100.0);
                    jitter = jitter.max(s.jitter_ms);
                    if let Some(c) = &p.ctrl {
                        let _ = c.send(Ctrl::Report {
                            stream: *stream,
                            loss_pct: s.lost as f32 / total as f32 * 100.0,
                            jitter_ms: s.jitter_ms,
                        });
                    }
                    r.stats.received = 0;
                    r.stats.lost = 0;
                    if let Some(ps) = pipes.get(&(peer.clone(), *stream)) {
                        glitches += ps.underruns();
                        buffer = buffer.max(1000.0 * ps.prime_frames() as f32 / audio::RATE as f32);
                    }
                }
                for (l, j) in p.remote_reports.values() {
                    loss = loss.max(*l);
                    jitter = jitter.max(*j);
                }
                p.stats.loss_pct = p.stats.loss_pct * 0.5 + loss * 0.5;
                p.stats.jitter_ms = jitter;
                p.stats.buffer_ms = buffer;
                p.stats.glitches = glitches;
                let new_state = if p.stats.loss_pct > 3.0 {
                    LinkState::Degraded(format!("Losing audio ({:.0}% packet loss)", p.stats.loss_pct))
                } else if p.stats.rtt_ms > 80.0 {
                    LinkState::Degraded(format!("Slow link ({:.0} ms)", p.stats.rtt_ms))
                } else {
                    LinkState::Live
                };
                if new_state != p.state && matches!(p.state, LinkState::Live | LinkState::Degraded(_)) {
                    p.state = new_state;
                }
                changed = true;
            }
        }
        let _ = changed;
        emit_peers(&shared);
    }
}

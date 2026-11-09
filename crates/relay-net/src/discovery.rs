//! Finds other Relay devices: mDNS (`_relay._udp`) plus a UDP broadcast
//! beacon as a fallback for networks that filter multicast.

use crossbeam_channel::Sender;
use parking_lot::Mutex;
use relay_proto::Platform;
use serde::{Deserialize, Serialize};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

pub const SERVICE: &str = "_relay._udp.local.";
pub const BEACON_PORT: u16 = 47801;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Announce {
    pub relay: u32,
    pub id: String,
    pub name: String,
    pub platform: Platform,
    pub version: String,
    pub port: u16,
}

#[derive(Clone, Debug)]
pub struct Seen {
    pub id: String,
    pub name: String,
    pub platform: Platform,
    pub version: String,
    pub addr: SocketAddr,
}

pub struct Discovery {
    mdns: Option<mdns_sd::ServiceDaemon>,
    me: Arc<Mutex<Announce>>,
    stop: Arc<AtomicBool>,
    pub problems: Vec<String>,
}

fn platform_str(p: Platform) -> &'static str {
    match p {
        Platform::Windows => "windows",
        Platform::Macos => "macos",
        Platform::Linux => "linux",
        Platform::Other => "other",
    }
}

fn parse_platform(s: &str) -> Platform {
    match s {
        "windows" => Platform::Windows,
        "macos" => Platform::Macos,
        "linux" => Platform::Linux,
        _ => Platform::Other,
    }
}

fn register(daemon: &mdns_sd::ServiceDaemon, me: &Announce) -> mdns_sd::Result<()> {
    let host = format!("relay-{}.local.", me.id);
    let props = [
        ("id", me.id.as_str()),
        ("name", me.name.as_str()),
        ("platform", platform_str(me.platform)),
        ("version", me.version.as_str()),
    ];
    let info = mdns_sd::ServiceInfo::new(SERVICE, &me.id, &host, "", me.port, &props[..])?.enable_addr_auto();
    daemon.register(info)
}

impl Discovery {
    pub fn start(me: Announce, tx: Sender<Seen>) -> Self {
        let mut problems = Vec::new();
        let stop = Arc::new(AtomicBool::new(false));
        let me = Arc::new(Mutex::new(me));

        let mdns = match mdns_sd::ServiceDaemon::new() {
            Ok(d) => {
                if let Err(e) = register(&d, &me.lock()) {
                    problems.push(format!("mDNS register failed: {e}"));
                }
                match d.browse(SERVICE) {
                    Ok(rx) => {
                        let tx = tx.clone();
                        let my_id = me.lock().id.clone();
                        let stop = stop.clone();
                        let _ = std::thread::Builder::new().name("relay-mdns".into()).spawn(move || {
                            while !stop.load(Ordering::Relaxed) {
                                let Ok(ev) = rx.recv_timeout(Duration::from_millis(500)) else { continue };
                                if let mdns_sd::ServiceEvent::ServiceResolved(info) = ev {
                                    let Some(id) = info.get_property_val_str("id").map(str::to_string) else {
                                        continue;
                                    };
                                    if id == my_id {
                                        continue;
                                    }
                                    let name = info.get_property_val_str("name").unwrap_or("Relay").to_string();
                                    let platform = parse_platform(info.get_property_val_str("platform").unwrap_or(""));
                                    let version = info.get_property_val_str("version").unwrap_or("").to_string();
                                    for ip in info.get_addresses_v4() {
                                        let _ = tx.send(Seen {
                                            id: id.clone(),
                                            name: name.clone(),
                                            platform,
                                            version: version.clone(),
                                            addr: SocketAddr::new(IpAddr::V4(ip), info.get_port()),
                                        });
                                    }
                                }
                            }
                        });
                    }
                    Err(e) => problems.push(format!("mDNS browse failed: {e}")),
                }
                Some(d)
            }
            Err(e) => {
                problems.push(format!("mDNS unavailable: {e}"));
                None
            }
        };

        match beacon_socket() {
            Ok(sock) => {
                let me2 = me.clone();
                let stop2 = stop.clone();
                let _ = std::thread::Builder::new()
                    .name("relay-beacon".into())
                    .spawn(move || beacon_loop(sock, me2, tx, stop2));
            }
            Err(e) => problems.push(format!("Beacon port {BEACON_PORT} unavailable: {e}")),
        }

        Self { mdns, me, stop, problems }
    }

    pub fn set_name(&self, name: String) {
        let mut me = self.me.lock();
        me.name = name;
        if let Some(d) = &self.mdns {
            let _ = d.unregister(&format!("{}.{}", me.id, SERVICE));
            let _ = register(d, &me);
        }
    }
}

impl Drop for Discovery {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(d) = self.mdns.take() {
            let _ = d.shutdown();
        }
    }
}

fn beacon_socket() -> std::io::Result<UdpSocket> {
    use socket2::{Domain, Protocol, Socket, Type};
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    s.set_reuse_address(true)?;
    #[cfg(unix)]
    s.set_reuse_port(true)?;
    s.set_broadcast(true)?;
    s.bind(&SocketAddr::from((Ipv4Addr::UNSPECIFIED, BEACON_PORT)).into())?;
    let s: UdpSocket = s.into();
    s.set_read_timeout(Some(Duration::from_millis(500)))?;
    Ok(s)
}

fn broadcast_targets() -> Vec<SocketAddr> {
    let mut out = vec![SocketAddr::from((Ipv4Addr::BROADCAST, BEACON_PORT))];
    if let Ok(ifaces) = if_addrs::get_if_addrs() {
        for i in ifaces {
            if let if_addrs::IfAddr::V4(v4) = i.addr
                && !v4.ip.is_loopback()
                && let Some(b) = v4.broadcast
            {
                out.push(SocketAddr::from((b, BEACON_PORT)));
            }
        }
    }
    out
}

fn beacon_loop(sock: UdpSocket, me: Arc<Mutex<Announce>>, tx: Sender<Seen>, stop: Arc<AtomicBool>) {
    let mut last_send = Instant::now() - Duration::from_secs(10);
    let mut targets = broadcast_targets();
    let mut buf = [0u8; 2048];
    while !stop.load(Ordering::Relaxed) {
        if last_send.elapsed() >= Duration::from_secs(2) {
            last_send = Instant::now();
            targets = if targets.len() <= 1 { broadcast_targets() } else { targets };
            if let Ok(bytes) = serde_json::to_vec(&*me.lock()) {
                for t in &targets {
                    let _ = sock.send_to(&bytes, t);
                }
            }
        }
        let Ok((n, from)) = sock.recv_from(&mut buf) else { continue };
        let Ok(a) = serde_json::from_slice::<Announce>(&buf[..n]) else { continue };
        if a.relay != 1 || a.id == me.lock().id {
            continue;
        }
        let _ = tx.send(Seen {
            id: a.id,
            name: a.name,
            platform: a.platform,
            version: a.version,
            addr: SocketAddr::new(from.ip(), a.port),
        });
    }
}

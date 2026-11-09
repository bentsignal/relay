//! Debug client: prints the engine state and meters.
//!
//!   relayctl [SECONDS] ['{"cmd":"set_route","strip":3,"bus":2,"on":true}' ...]
//!
//! Extra arguments are engine commands (JSON) sent after connecting.
use relay_proto::{ClientMsg, ServerMsg};
use std::time::{Duration, Instant};

fn main() -> std::io::Result<()> {
    let secs: u64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(5);
    let (client, rx) = relay_ipc::Client::connect("relayctl")?;
    client.send(&ClientMsg::Meters { on: true })?;
    for arg in std::env::args().skip(2) {
        let cmd: relay_proto::Command = serde_json::from_str(&arg).map_err(std::io::Error::other)?;
        client.send(&ClientMsg::Command(cmd))?;
    }
    let start = Instant::now();
    let mut last_meter = Instant::now();
    while start.elapsed() < Duration::from_secs(secs) {
        match rx.recv_timeout(Duration::from_millis(200)) {
            Ok(ServerMsg::State(s)) => {
                println!(
                    "engine {} virtual={:?} default={} cpu={:.1}%",
                    s.version, s.virtual_audio.state, s.virtual_audio.is_default, s.engine.cpu_pct
                );
                for st in &s.mixer.strips {
                    println!("  strip #{} {} {:?} {:?} routes={:?}", st.id, st.name, st.source, st.status, st.routes);
                }
                for b in &s.mixer.buses {
                    println!("  bus #{} {} {:?} {:?}", b.id, b.name, b.sink, b.status);
                }
                println!("  net me={} port={} problem={:?}", s.network.this_id, s.network.port, s.network.problem);
                println!(
                    "  update {:?} channel={:?} rolled_back_from={:?}",
                    s.update.state, s.update.channel, s.update.rolled_back_from
                );
                for p in &s.network.peers {
                    println!(
                        "  peer {} {} {:?} paired={} addrs={:?} rtt={:.1} incoming={:?}",
                        p.id, p.name, p.state, p.paired, p.addresses, p.stats.rtt_ms, p.incoming
                    );
                }
            }
            Ok(ServerMsg::Meters(m)) if last_meter.elapsed() > Duration::from_millis(500) => {
                last_meter = Instant::now();
                let f = |v: &[(u32, relay_proto::Level)]| {
                    v.iter().map(|(id, l)| format!("{id}:{:.2}", l.peak_l.max(l.peak_r))).collect::<Vec<_>>().join(" ")
                };
                println!("  meters strips[{}] buses[{}]", f(&m.strips), f(&m.buses));
            }
            Ok(ServerMsg::Notice { text, .. }) => println!("notice: {text}"),
            Ok(_) => {}
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(_) => {
                println!("engine disconnected");
                break;
            }
        }
    }
    Ok(())
}

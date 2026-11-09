//! Background update checks, staged downloads, and safe switching with a
//! watchdog that rolls back if the new version doesn't come up.

use crossbeam_channel::{Receiver, Sender, unbounded};
use relay_proto::{Channel, UpdateState};
use relay_update::install;
use std::time::{Duration, Instant};

pub enum UpdateMsg {
    State(UpdateState),
}

pub struct Updater {
    tx_cmd: Sender<Channel>,
    pub rx: Receiver<UpdateMsg>,
    pub ready: Option<String>,
    pub managed: bool,
}

const CHECK_EVERY: Duration = Duration::from_secs(30 * 60);

impl Updater {
    pub fn start(channel: Channel) -> Self {
        let managed = install::running_installed() && !install::is_dev_build();
        let (tx_cmd, rx_cmd) = unbounded::<Channel>();
        let (tx, rx) = unbounded();
        if managed {
            std::thread::Builder::new().name("relay-updates".into()).spawn(move || checker(channel, rx_cmd, tx)).ok();
            confirm_after_healthy();
        }
        Self { tx_cmd, rx, ready: None, managed }
    }

    pub fn check(&self, channel: Channel) {
        let _ = self.tx_cmd.send(channel);
    }
}

/// If this version was just applied, mark it good after it has run for a while.
fn confirm_after_healthy() {
    let st = install::load_state();
    if st.pending.as_deref() == Some(crate::VERSION) {
        std::thread::spawn(|| {
            std::thread::sleep(Duration::from_secs(20));
            if let Err(e) = install::confirm() {
                tracing::warn!("couldn't confirm update: {e:#}");
            } else {
                tracing::info!("update confirmed healthy");
                install::prune();
            }
        });
    }
}

fn checker(mut channel: Channel, rx: Receiver<Channel>, tx: Sender<UpdateMsg>) {
    let mut last = Instant::now() - CHECK_EVERY + Duration::from_secs(20);
    loop {
        let wait = CHECK_EVERY.saturating_sub(last.elapsed());
        match rx.recv_timeout(wait) {
            Ok(c) => channel = c,
            Err(crossbeam_channel::RecvTimeoutError::Timeout) => {}
            Err(_) => return,
        }
        last = Instant::now();
        let _ = tx.send(UpdateMsg::State(UpdateState::Checking));
        let state = match check_once(channel, &tx) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!("update check failed: {e:#}");
                UpdateState::Error("Couldn't check for updates".into())
            }
        };
        let _ = tx.send(UpdateMsg::State(state));
    }
}

fn check_once(channel: Channel, tx: &Sender<UpdateMsg>) -> anyhow::Result<UpdateState> {
    let m = relay_update::fetch_manifest(channel.as_str())?;
    if m.version == crate::VERSION {
        return Ok(UpdateState::UpToDate);
    }
    let asset = m.assets.get(relay_update::platform()).ok_or_else(|| anyhow::anyhow!("no build for this platform"))?;
    let dir = install::version_dir(&m.version);
    if !dir.join(".complete").exists() {
        let _ = tx.send(UpdateMsg::State(UpdateState::Downloading(0.0)));
        let zip = relay_update::downloads_dir().join(format!("relay-{}-{}.zip", m.version, relay_update::platform()));
        let mut last_sent = Instant::now();
        relay_update::download(asset, &zip, |p| {
            if last_sent.elapsed() > Duration::from_millis(250) {
                last_sent = Instant::now();
                let _ = tx.send(UpdateMsg::State(UpdateState::Downloading(p)));
            }
        })?;
        relay_update::extract(&zip, &dir)?;
        std::fs::write(dir.join(".complete"), &m.version)?;
        let _ = std::fs::remove_file(&zip);
    }
    Ok(UpdateState::Ready(m.version))
}

/// Switches to `version` and starts the watchdog. The caller then exits.
pub fn apply(version: &str) -> anyhow::Result<()> {
    let old = crate::VERSION.to_string();
    install::activate(version)?;
    // The watchdog runs from the old version's files, which stay on disk.
    let old_dir = install::version_dir(&old);
    let watchdog = if cfg!(windows) {
        old_dir.join("relay-engine.exe")
    } else {
        old_dir.join("Relay.app/Contents/MacOS/relay-engine")
    };
    let exe = if watchdog.exists() { watchdog } else { std::env::current_exe()? };
    spawn_detached(&exe, &["--watchdog", version])?;
    Ok(())
}

pub fn spawn_detached(exe: &std::path::Path, args: &[&str]) -> std::io::Result<()> {
    let mut cmd = std::process::Command::new(exe);
    cmd.args(args)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(0x0000_0008 | 0x0000_0200 | 0x0800_0000);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    cmd.spawn().map(|_| ())
}

fn engine_version() -> Option<String> {
    let (client, rx) = relay_ipc::Client::connect("watchdog").ok()?;
    let _ = client;
    match rx.recv_timeout(Duration::from_secs(3)) {
        Ok(relay_proto::ServerMsg::Hello { engine_version, .. }) => Some(engine_version),
        _ => None,
    }
}

/// `relay-engine --watchdog <version>`: launches the newly activated engine
/// and rolls back if it doesn't come up and stay up.
pub fn watchdog(version: &str) {
    tracing::info!(version, "update watchdog started");
    // Wait for the old engine to release its resources.
    let t = Instant::now();
    while relay_ipc::engine_running() && t.elapsed() < Duration::from_secs(10) {
        std::thread::sleep(Duration::from_millis(200));
    }
    let Some(new_engine) = install::active_exe("relay-engine") else {
        tracing::error!("new engine missing; rolling back");
        roll_back_and_start();
        return;
    };
    if spawn_detached(&new_engine, &[]).is_err() {
        roll_back_and_start();
        return;
    }
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut up_since: Option<Instant> = None;
    while Instant::now() < deadline || up_since.is_some() {
        std::thread::sleep(Duration::from_millis(500));
        match engine_version() {
            Some(v) if v == version => {
                let since = *up_since.get_or_insert_with(Instant::now);
                if since.elapsed() > Duration::from_secs(12) {
                    tracing::info!("new engine healthy");
                    return;
                }
            }
            _ => {
                if up_since.is_some() {
                    break; // it came up and then died
                }
            }
        }
    }
    tracing::error!(version, "new engine didn't stay up; rolling back");
    roll_back_and_start();
}

fn roll_back_and_start() {
    match install::rollback() {
        Ok(Some(prev)) => {
            tracing::info!(prev, "rolled back");
            if let Some(exe) = install::active_exe("relay-engine") {
                let _ = spawn_detached(&exe, &[]);
            }
        }
        Ok(None) => tracing::error!("nothing to roll back to"),
        Err(e) => tracing::error!("rollback failed: {e:#}"),
    }
}

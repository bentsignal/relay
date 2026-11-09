#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

//! Relay's headless engine. Runs at login, owns all audio, network and video
//! work. The Relay app is a remote control that talks to it over local IPC.

mod config;
mod denoise;
mod engine;
mod firewall;
mod ipc;
mod paths;
#[cfg(windows)]
mod tray;
mod updates;
mod video;
mod virt;

use crossbeam_channel::{Receiver, Sender, select, tick, unbounded};
use engine::Engine;
use ipc::{ClientId, ClientTx};
use relay_proto::{ClientMsg, Command, IPC_VERSION, ServerMsg};
use std::time::{Duration, Instant};

pub const VERSION: &str = match option_env!("RELAY_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

pub enum Event {
    Connected(ClientTx),
    Client(ClientId, ClientMsg),
    Gone(ClientId),
    Shutdown,
}

fn init_logging() -> tracing_appender::non_blocking::WorkerGuard {
    let dir = paths::log_dir();
    let _ = std::fs::create_dir_all(&dir);
    let file = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix("engine")
        .filename_suffix("log")
        .max_log_files(7)
        .build(&dir)
        .expect("log file");
    let (nb, guard) = tracing_appender::non_blocking(file);
    use tracing_subscriber::prelude::*;
    let filter = tracing_subscriber::EnvFilter::try_from_env("RELAY_LOG").unwrap_or_else(|_| "info".into());
    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer().with_writer(nb).with_ansi(false))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
    guard
}

fn main() {
    if std::env::args().any(|a| a == "--version") {
        println!("{VERSION}");
        return;
    }
    let _guard = init_logging();
    let args: Vec<String> = std::env::args().collect();
    if let Some(i) = args.iter().position(|a| a == "--watchdog") {
        if let Some(v) = args.get(i + 1) {
            updates::watchdog(v);
        }
        return;
    }
    // Lets the updater's rollback be tested with a build that can't start.
    if VERSION.contains("crashtest") {
        tracing::error!("crash-test build: exiting");
        std::process::exit(3);
    }
    if relay_ipc::engine_running() {
        tracing::info!("an engine is already running");
        return;
    }
    std::panic::set_hook(Box::new(|info| {
        tracing::error!("panic: {info}");
    }));
    tracing::info!(version = VERSION, "Relay engine starting");
    // On macOS the main thread runs AppKit for video viewer windows and the
    // engine runs on a secondary thread; elsewhere this just calls `run`.
    if let Err(e) = relay_video::run_app(run) {
        tracing::error!("engine failed: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> anyhow::Result<()> {
    let cfg = config::Config::load(&paths::config_file());
    let mut engine = Engine::new(cfg)?;
    let (tx, rx) = unbounded::<Event>();
    ipc::spawn(tx.clone())?;
    #[cfg(windows)]
    if std::env::var_os("RELAY_NO_TRAY").is_none() {
        tray::spawn(tx.clone());
    }
    install_ctrlc(tx.clone());
    control_loop(&mut engine, rx, tx);
    if let Err(e) = engine.cfg.save(&paths::config_file()) {
        tracing::error!("saving config failed: {e}");
    }
    tracing::info!("Relay engine stopped");
    Ok(())
}

fn install_ctrlc(_tx: Sender<Event>) {
    // Console Ctrl-C only matters for development runs; the default handler
    // terminates the process and the OS reclaims the virtual devices.
}

fn control_loop(engine: &mut Engine, rx: Receiver<Event>, _tx: Sender<Event>) {
    let mut clients: Vec<ClientTx> = Vec::new();
    let meter_tick = tick(Duration::from_millis(33));
    let house_tick = tick(Duration::from_millis(1500));
    let mut last_state = Instant::now() - Duration::from_secs(1);
    let mut last_save = Instant::now();
    loop {
        select! {
            recv(rx) -> ev => {
                let Ok(ev) = ev else { return };
                match ev {
                    Event::Connected(c) => {
                        tracing::info!(client = c.id, "UI connected");
                        c.send(ServerMsg::Hello { ipc_version: IPC_VERSION, engine_version: VERSION.into() });
                        c.send(ServerMsg::State(Box::new(engine.state())));
                        clients.push(c);
                    }
                    Event::Gone(id) => {
                        clients.retain(|c| c.id != id);
                    }
                    Event::Client(id, msg) => match msg {
                        ClientMsg::Hello { ipc_version, client } => {
                            tracing::info!(id, client, ipc_version, "client hello");
                            if ipc_version != IPC_VERSION
                                && let Some(c) = clients.iter().find(|c| c.id == id)
                            {
                                c.send(ServerMsg::Notice { text: "Relay is updating. Restart the app.".into(), error: true });
                            }
                        }
                        ClientMsg::Meters { on } => {
                            if let Some(c) = clients.iter_mut().find(|c| c.id == id) {
                                c.meters = on;
                            }
                        }
                        ClientMsg::Command(Command::Quit) => return,
                        ClientMsg::Command(cmd) => {
                            engine.handle(cmd);
                            if engine.quit {
                                for c in &clients {
                                    c.send(ServerMsg::Notice { text: "Restarting to update…".into(), error: false });
                                }
                                std::thread::sleep(Duration::from_millis(150));
                                return;
                            }
                        }
                    },
                    Event::Shutdown => return,
                }
            }
            recv(meter_tick) -> _ => {
                if clients.iter().any(|c| c.meters) {
                    let m = engine.meters();
                    clients.retain(|c| !c.meters || c.send(ServerMsg::Meters(m.clone())));
                }
            }
            recv(house_tick) -> _ => {
                engine.tick();
            }
        }
        engine.ui_clients = clients.len();
        engine.poll_network();
        engine.poll_video();
        engine.poll_updates();
        for (text, error) in engine.notices.drain(..) {
            for c in &clients {
                c.send(ServerMsg::Notice { text: text.clone(), error });
            }
        }
        if engine.dirty && last_state.elapsed() >= Duration::from_millis(50) {
            engine.dirty = false;
            last_state = Instant::now();
            let state = engine.state();
            clients.retain(|c| c.send(ServerMsg::State(Box::new(state.clone()))));
        }
        if engine.save_due && last_save.elapsed() >= Duration::from_secs(1) {
            engine.save_due = false;
            last_save = Instant::now();
            if let Err(e) = engine.cfg.save(&paths::config_file()) {
                tracing::error!("saving config failed: {e}");
            }
        }
    }
}

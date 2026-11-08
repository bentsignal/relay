//! Accepts UI connections and shuttles messages to the control loop.

use crate::Event;
use crossbeam_channel::{Sender, TrySendError, bounded};
use relay_proto::{ClientMsg, ServerMsg};
use std::sync::atomic::{AtomicU64, Ordering};

pub type ClientId = u64;

pub struct ClientTx {
    pub id: ClientId,
    pub tx: Sender<ServerMsg>,
    pub meters: bool,
}

impl ClientTx {
    /// Queues a message; drops it if the client isn't keeping up.
    pub fn send(&self, msg: ServerMsg) -> bool {
        !matches!(self.tx.try_send(msg), Err(TrySendError::Disconnected(_)))
    }
}

pub fn spawn(events: Sender<Event>) -> std::io::Result<()> {
    let listener = relay_ipc::Listener::bind()?;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    std::thread::Builder::new().name("ipc-accept".into()).spawn(move || {
        loop {
            let (mut reader, mut writer) = match listener.accept() {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!("ipc accept failed: {e}");
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    continue;
                }
            };
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let (tx, rx) = bounded::<ServerMsg>(256);
            let _ = std::thread::Builder::new().name("ipc-write".into()).spawn(move || {
                while let Ok(msg) = rx.recv() {
                    if relay_ipc::write_msg(&mut writer, &msg).is_err() {
                        break;
                    }
                }
            });
            if events.send(Event::Connected(ClientTx { id, tx, meters: false })).is_err() {
                return;
            }
            let ev = events.clone();
            let _ = std::thread::Builder::new().name("ipc-read".into()).spawn(move || {
                loop {
                    match relay_ipc::read_msg::<ClientMsg>(&mut reader) {
                        Ok(msg) => {
                            if ev.send(Event::Client(id, msg)).is_err() {
                                return;
                            }
                        }
                        Err(_) => {
                            let _ = ev.send(Event::Gone(id));
                            return;
                        }
                    }
                }
            });
        }
    })?;
    Ok(())
}

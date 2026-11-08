//! Local IPC between the engine and UI: length-prefixed JSON messages over a
//! per-user local socket (a named pipe on Windows, a Unix socket elsewhere).

use crossbeam_channel::{Receiver, unbounded};
use interprocess::local_socket::{GenericNamespaced, ListenerOptions, Stream, prelude::*};
use parking_lot::Mutex;
use relay_proto::{ClientMsg, IPC_VERSION, ServerMsg};
use serde::{Serialize, de::DeserializeOwned};
use std::io::{self, BufReader, Read, Write};
use std::sync::Arc;

/// The engine's socket name, unique per OS user.
pub fn socket_name() -> String {
    let user = std::env::var("USERNAME").or_else(|_| std::env::var("USER")).unwrap_or_else(|_| "user".into());
    let user: String = user.chars().filter(|c| c.is_ascii_alphanumeric()).collect();
    format!("relay-engine-{user}.sock")
}

const MAX_MSG: u32 = 16 * 1024 * 1024;

pub fn write_msg<T: Serialize>(w: &mut impl Write, msg: &T) -> io::Result<()> {
    let body = serde_json::to_vec(msg).map_err(io::Error::other)?;
    let mut frame = Vec::with_capacity(body.len() + 4);
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    w.write_all(&frame)?;
    w.flush()
}

pub fn read_msg<T: DeserializeOwned>(r: &mut impl Read) -> io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len > MAX_MSG {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "message too large"));
    }
    let mut body = vec![0u8; len as usize];
    r.read_exact(&mut body)?;
    serde_json::from_slice(&body).map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

/// A shared handle to one end of a local socket; reads and writes may happen
/// from different threads.
#[derive(Clone)]
pub struct Conn(Arc<Stream>);

impl Read for Conn {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl Write for Conn {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&*self.0).flush()
    }
}

/// Server side: accepts UI connections.
pub struct Listener {
    inner: interprocess::local_socket::Listener,
}

impl Listener {
    pub fn bind() -> io::Result<Self> {
        let name = socket_name();
        let inner = ListenerOptions::new().name(name.to_ns_name::<GenericNamespaced>()?).create_sync()?;
        Ok(Self { inner })
    }

    /// Blocks for the next connection and returns a reader and a writer for it.
    pub fn accept(&self) -> io::Result<(BufReader<Conn>, Conn)> {
        let conn = Conn(Arc::new(self.inner.accept()?));
        Ok((BufReader::new(conn.clone()), conn))
    }
}

/// True if an engine is listening.
pub fn engine_running() -> bool {
    socket_name().to_ns_name::<GenericNamespaced>().ok().and_then(|n| Stream::connect(n).ok()).is_some()
}

/// UI side connection.
#[derive(Clone)]
pub struct Client {
    send: Arc<Mutex<Conn>>,
}

impl Client {
    /// Connects and performs the hello exchange. Messages from the engine
    /// arrive on the returned channel, which closes when the engine goes away.
    pub fn connect(client_name: &str) -> io::Result<(Self, Receiver<ServerMsg>)> {
        let conn = Conn(Arc::new(Stream::connect(socket_name().to_ns_name::<GenericNamespaced>()?)?));
        let (r, mut w) = (conn.clone(), conn);
        write_msg(&mut w, &ClientMsg::Hello { ipc_version: IPC_VERSION, client: client_name.into() })?;
        let (tx, rx) = unbounded();
        std::thread::Builder::new().name("relay-ipc-recv".into()).spawn(move || {
            let mut r = BufReader::new(r);
            while let Ok(msg) = read_msg::<ServerMsg>(&mut r) {
                if tx.send(msg).is_err() {
                    break;
                }
            }
        })?;
        Ok((Self { send: Arc::new(Mutex::new(w)) }, rx))
    }

    pub fn send(&self, msg: &ClientMsg) -> io::Result<()> {
        write_msg(&mut *self.send.lock(), msg)
    }
}

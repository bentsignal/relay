//! USB/IP server hosting Relay's virtual audio devices on loopback.
//!
//! One reader thread per connection parses URBs. Control transfers are
//! answered immediately. Isochronous URBs are given a slot on a per-endpoint
//! timeline (1 ms per packet) and completed by a scheduler thread when their
//! slot ends, which is what paces Windows' audio engine.
//!
//! Playback (OUT) audio is handed to the mixer as soon as it arrives rather
//! than at completion, which removes the host's URB queue from the latency.

use crate::protocol::*;
use crate::uac2::{self, ControlResult, Descriptors, Identity, Setup, UsbState};
use parking_lot::{Condvar, Mutex};
use relay_mixer::{PipeReader, PipeWriter};
use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Live counters for one device, shown in the UI's details view.
#[derive(Default, Debug)]
pub struct DeviceStats {
    pub connected: AtomicBool,
    pub playback_active: AtomicBool,
    pub capture_active: AtomicBool,
    pub out_packets: AtomicU64,
    pub in_packets: AtomicU64,
    pub sessions: AtomicU32,
}

struct Device {
    busid: String,
    number: u32,
    desc: Descriptors,
    usb: Mutex<UsbState>,
    /// Apps -> Relay.
    playback: Mutex<PipeWriter>,
    /// Relay -> apps (microphone).
    capture: Mutex<PipeReader>,
    stats: Arc<DeviceStats>,
    /// Exactly one import may be active at a time.
    claimed: AtomicBool,
    flush_capture: AtomicBool,
}

pub struct DeviceSpec {
    pub identity: Identity,
    pub playback: PipeWriter,
    pub capture: PipeReader,
}

pub struct Server {
    addr: SocketAddr,
    devices: Vec<Arc<Device>>,
    stop: Arc<AtomicBool>,
}

impl Server {
    /// Binds the loopback listener and starts serving. Devices are numbered from bus 1-1.
    pub fn start(port: u16, specs: Vec<DeviceSpec>) -> std::io::Result<(Arc<Self>, Vec<Arc<DeviceStats>>)> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))?;
        let addr = listener.local_addr()?;
        let mut stats = Vec::new();
        let devices: Vec<Arc<Device>> = specs
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                let st = Arc::new(DeviceStats::default());
                stats.push(st.clone());
                Arc::new(Device {
                    busid: format!("1-{}", i + 1),
                    number: i as u32 + 1,
                    desc: Descriptors::new(&s.identity),
                    usb: Mutex::new(UsbState::default()),
                    playback: Mutex::new(s.playback),
                    capture: Mutex::new(s.capture),
                    stats: st,
                    claimed: AtomicBool::new(false),
                    flush_capture: AtomicBool::new(true),
                })
            })
            .collect();
        let server = Arc::new(Self { addr, devices, stop: Arc::new(AtomicBool::new(false)) });
        let s2 = server.clone();
        std::thread::Builder::new().name("usbip-accept".into()).spawn(move || s2.accept_loop(listener))?;
        Ok((server, stats))
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// Routes this device's playback audio into a new pipe writer.
    pub fn replace_playback(&self, index: usize, w: PipeWriter) {
        if let Some(d) = self.devices.get(index) {
            *d.playback.lock() = w;
        }
    }

    /// Feeds this device's microphone from a new pipe reader.
    pub fn replace_capture(&self, index: usize, r: PipeReader) {
        if let Some(d) = self.devices.get(index) {
            *d.capture.lock() = r;
            d.flush_capture.store(true, Ordering::Relaxed);
        }
    }

    pub fn busids(&self) -> Vec<String> {
        self.devices.iter().map(|d| d.busid.clone()).collect()
    }

    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
        // Unblock accept().
        let _ = TcpStream::connect_timeout(&self.addr, Duration::from_millis(200));
    }

    fn accept_loop(self: Arc<Self>, listener: TcpListener) {
        for conn in listener.incoming() {
            if self.stop.load(Ordering::Relaxed) {
                break;
            }
            let Ok(conn) = conn else { continue };
            // Loopback only, enforced by the bind address; double-check.
            if !conn.peer_addr().map(|a| a.ip().is_loopback()).unwrap_or(false) {
                continue;
            }
            let _ = conn.set_nodelay(true);
            let me = self.clone();
            let _ = std::thread::Builder::new().name("usbip-conn".into()).spawn(move || {
                if let Err(e) = me.handle(conn) {
                    tracing::debug!("usbip connection ended: {e}");
                }
            });
        }
    }

    fn write_device(w: &mut impl Write, d: &Device) -> std::io::Result<()> {
        let dd = &d.desc.device;
        w.write_all(&fixed_str::<256>(&format!("/sys/devices/platform/relay/{}", d.busid)))?;
        w.write_all(&fixed_str::<32>(&d.busid))?;
        w.write_all(&1u32.to_be_bytes())?;
        w.write_all(&d.number.to_be_bytes())?;
        w.write_all(&SPEED_FULL.to_be_bytes())?;
        // idVendor, idProduct, bcdDevice (descriptor stores little-endian).
        for off in [8, 10, 12] {
            w.write_all(&u16::from_le_bytes([dd[off], dd[off + 1]]).to_be_bytes())?;
        }
        w.write_all(&[dd[4], dd[5], dd[6], 1, dd[17], 3])
    }

    fn handle(self: Arc<Self>, conn: TcpStream) -> std::io::Result<()> {
        let reader = conn.try_clone()?;
        reader.set_read_timeout(Some(Duration::from_secs(10)))?;
        let mut r = BufReader::with_capacity(64 * 1024, reader);
        let mut w = conn.try_clone()?;
        let _version = read_u16(&mut r)?;
        let code = read_u16(&mut r)?;
        let _status = read_u32(&mut r)?;
        match code {
            OP_REQ_DEVLIST => {
                let mut out = Vec::new();
                op_header(&mut out, OP_REP_DEVLIST, 0)?;
                out.extend_from_slice(&(self.devices.len() as u32).to_be_bytes());
                for d in &self.devices {
                    Self::write_device(&mut out, d)?;
                    for (class, sub) in [(1u8, 1u8), (1, 2), (1, 2)] {
                        out.extend_from_slice(&[class, sub, 0x20, 0]);
                    }
                }
                w.write_all(&out)
            }
            OP_REQ_IMPORT => {
                let mut busid = [0u8; 32];
                r.read_exact(&mut busid)?;
                let busid = String::from_utf8_lossy(&busid).trim_end_matches('\0').to_string();
                let Some(dev) = self.devices.iter().find(|d| d.busid == busid).cloned() else {
                    return op_header(&mut w, OP_REP_IMPORT, 1);
                };
                if dev.claimed.swap(true, Ordering::AcqRel) {
                    tracing::warn!(%busid, "rejected duplicate import");
                    return op_header(&mut w, OP_REP_IMPORT, 1);
                }
                let mut out = Vec::new();
                op_header(&mut out, OP_REP_IMPORT, 0)?;
                Self::write_device(&mut out, &dev)?;
                w.write_all(&out)?;
                conn.set_read_timeout(None)?;
                r.get_ref().set_read_timeout(None)?;
                tracing::info!(%busid, "virtual device imported");
                dev.stats.connected.store(true, Ordering::Relaxed);
                dev.stats.sessions.fetch_add(1, Ordering::Relaxed);
                let res = Session::run(dev.clone(), r, w, self.stop.clone());
                dev.stats.connected.store(false, Ordering::Relaxed);
                dev.stats.playback_active.store(false, Ordering::Relaxed);
                dev.stats.capture_active.store(false, Ordering::Relaxed);
                *dev.usb.lock() = UsbState::default();
                dev.claimed.store(false, Ordering::Release);
                match &res {
                    Ok(()) => tracing::info!(%busid, "virtual device session ended"),
                    Err(e) => tracing::info!(%busid, "virtual device session ended: {e}"),
                }
                res
            }
            _ => Ok(()),
        }
    }
}

struct Pending {
    seq: u32,
    ep: u32,
    deadline: Instant,
    start_frame: u32,
    iso: Vec<IsoDesc>,
    /// For OUT: total length to acknowledge.
    out_len: u32,
}

#[derive(Default)]
struct Queues {
    pending: VecDeque<Pending>,
    next_slot: [Option<Instant>; 3],
    closed: bool,
}

struct Session {
    dev: Arc<Device>,
    writer: Mutex<TcpStream>,
    queues: Mutex<Queues>,
    wake: Condvar,
}

impl Session {
    fn run(dev: Arc<Device>, mut r: BufReader<TcpStream>, w: TcpStream, stop: Arc<AtomicBool>) -> std::io::Result<()> {
        let sess = Arc::new(Session {
            dev,
            writer: Mutex::new(w.try_clone()?),
            queues: Mutex::new(Queues::default()),
            wake: Condvar::new(),
        });
        let s2 = sess.clone();
        let sched = std::thread::Builder::new().name("usbip-iso".into()).spawn(move || s2.scheduler())?;
        let result = sess.reader(&mut r, &stop);
        sess.queues.lock().closed = true;
        sess.wake.notify_all();
        let _ = w.shutdown(std::net::Shutdown::Both);
        let _ = sched.join();
        result
    }

    fn send(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.writer.lock().write_all(bytes)
    }

    fn reader(&self, r: &mut BufReader<TcpStream>, stop: &AtomicBool) -> std::io::Result<()> {
        loop {
            if stop.load(Ordering::Relaxed) {
                return Ok(());
            }
            let h = Header::read(r)?;
            match h.command {
                CMD_SUBMIT => {
                    let sub = Submit::read_body(h, r)?;
                    self.submit(sub)?;
                }
                CMD_UNLINK => {
                    let target = read_u32(r)?;
                    let mut pad = [0u8; 24];
                    r.read_exact(&mut pad)?;
                    let found = {
                        let mut q = self.queues.lock();
                        let before = q.pending.len();
                        q.pending.retain(|p| p.seq != target);
                        q.pending.len() != before
                    };
                    self.send(&ret_unlink(h.seq, if found { ST_CONNRESET } else { ST_OK }))?;
                }
                other => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unknown command {other}"),
                    ));
                }
            }
        }
    }

    fn submit(&self, sub: Submit) -> std::io::Result<()> {
        let h = sub.header;
        if h.ep == 0 {
            let setup = Setup::parse(&sub.setup);
            let result = {
                let mut usb = self.dev.usb.lock();
                let prev_capture = usb.capture_alt;
                let r = uac2::control(&self.dev.desc, &mut usb, setup, &sub.data);
                self.dev.stats.playback_active.store(usb.playback_alt == 1, Ordering::Relaxed);
                self.dev.stats.capture_active.store(usb.capture_alt == 1, Ordering::Relaxed);
                if usb.capture_alt == 1 && prev_capture != 1 {
                    self.dev.flush_capture.store(true, Ordering::Relaxed);
                }
                r
            };
            let reply = match result {
                ControlResult::Data(d) => ret_submit(h.seq, ST_OK, d.len() as u32, 0, None, &d),
                ControlResult::Ack => {
                    let actual = if h.direction == DIR_OUT { sub.data.len() as u32 } else { 0 };
                    ret_submit(h.seq, ST_OK, actual, 0, None, &[])
                }
                ControlResult::Stall => ret_submit(h.seq, ST_PIPE, 0, 0, None, &[]),
            };
            return self.send(&reply);
        }

        let valid = (h.ep == uac2::EP_OUT && h.direction == DIR_OUT) || (h.ep == uac2::EP_IN && h.direction == DIR_IN);
        if !valid || !sub.is_iso() {
            let iso = sub
                .is_iso()
                .then(|| sub.iso.iter().map(|d| IsoDesc { actual: 0, status: ST_PIPE, ..*d }).collect::<Vec<_>>());
            return self.send(&ret_submit(h.seq, ST_PIPE, 0, sub.start_frame, iso.as_deref(), &[]));
        }

        let mut iso = sub.iso;
        let mut out_len = 0;
        if h.direction == DIR_OUT {
            // Deliver playback audio now; acknowledge on the timeline.
            let active = self.dev.usb.lock().playback_alt == 1;
            let mut pcm = Vec::with_capacity(sub.data.len() / uac2::SUBSLOT);
            for d in iso.iter_mut() {
                let start = d.offset as usize;
                let end = (start + d.length as usize).min(sub.data.len());
                if start < end && active {
                    for s in sub.data[start..end].chunks_exact(2) {
                        pcm.push(i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0);
                    }
                }
                d.actual = d.length;
                d.status = ST_OK;
                out_len += d.length;
            }
            if !pcm.is_empty() {
                self.dev.playback.lock().write_interleaved(&pcm);
                self.dev.stats.out_packets.fetch_add(iso.len() as u64, Ordering::Relaxed);
            }
        }

        let now = Instant::now();
        let mut q = self.queues.lock();
        let slot = &mut q.next_slot[h.ep as usize];
        let base = match *slot {
            Some(t) if now.saturating_duration_since(t) < Duration::from_millis(250) => {
                t.max(now - Duration::from_millis(20))
            }
            _ => now,
        };
        let deadline = base + Duration::from_millis(iso.len() as u64);
        *slot = Some(deadline);
        q.pending.push_back(Pending { seq: h.seq, ep: h.ep, deadline, start_frame: sub.start_frame, iso, out_len });
        drop(q);
        self.wake.notify_one();
        Ok(())
    }

    fn complete(&self, p: Pending) -> std::io::Result<()> {
        if p.ep == uac2::EP_OUT {
            return self.send(&ret_submit(p.seq, ST_OK, p.out_len, p.start_frame, Some(&p.iso), &[]));
        }
        // Capture: fill each packet with 48 frames of the newest mic audio.
        let mut iso = p.iso;
        let frames = uac2::FRAMES_PER_MS;
        let per_packet = frames * uac2::FRAME_BYTES;
        let active = self.dev.usb.lock().capture_alt == 1;
        let mut floats = vec![0f32; frames * 2 * iso.len()];
        if active {
            let mut cap = self.dev.capture.lock();
            if self.dev.flush_capture.swap(false, Ordering::Relaxed) {
                // Discard audio queued while nobody was listening; keep ~10 ms.
                let fill = cap.fill_frames();
                cap.skip(fill.saturating_sub(480));
            }
            cap.read_interleaved(&mut floats);
        }
        let mut data = Vec::with_capacity(per_packet * iso.len());
        for (i, d) in iso.iter_mut().enumerate() {
            let n = per_packet.min(d.length as usize);
            for s in &floats[i * frames * 2..i * frames * 2 + n / 2] {
                data.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
            }
            d.actual = n as u32;
            d.status = ST_OK;
        }
        self.dev.stats.in_packets.fetch_add(iso.len() as u64, Ordering::Relaxed);
        self.send(&ret_submit(p.seq, ST_OK, data.len() as u32, p.start_frame, Some(&iso), &data))
    }

    fn scheduler(&self) {
        let timer = crate::timer::Timer::new();
        loop {
            let due = {
                let mut q = self.queues.lock();
                loop {
                    if q.closed {
                        return;
                    }
                    if !q.pending.is_empty() {
                        break;
                    }
                    self.wake.wait_for(&mut q, Duration::from_millis(100));
                }
                // Earliest deadline across endpoints (each endpoint's queue is ordered).
                let now = Instant::now();
                let mut due = Vec::new();
                let mut i = 0;
                while i < q.pending.len() {
                    if q.pending[i].deadline <= now {
                        due.push(q.pending.remove(i).unwrap());
                    } else {
                        i += 1;
                    }
                }
                if due.is_empty() {
                    let next = q.pending.iter().map(|p| p.deadline).min().unwrap();
                    drop(q);
                    timer.sleep_until(next.min(now + Duration::from_millis(2)));
                    continue;
                }
                due
            };
            for p in due {
                if self.complete(p).is_err() {
                    return;
                }
            }
        }
    }
}

//! Sending and receiving video between devices.

use relay_proto::{PortStatus, Video, VideoReceive, VideoSend, VideoSource};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

struct Sending {
    id: u32,
    peer: String,
    source: String,
    with_audio: bool,
    tx: Arc<relay_net::VideoTx>,
    capture: Option<Box<dyn relay_video::Capture>>,
    error: Option<String>,
}

struct Receiving {
    peer: String,
    stream: u32,
    name: String,
    stop: Arc<AtomicBool>,
    need_key: Arc<AtomicBool>,
    open: Arc<AtomicBool>,
}

pub struct VideoMgr {
    sending: Vec<Sending>,
    receiving: Vec<Receiving>,
    next_id: u32,
    sources: Vec<VideoSource>,
    sources_at: Option<Instant>,
}

impl Default for VideoMgr {
    fn default() -> Self {
        Self { sending: Vec::new(), receiving: Vec::new(), next_id: 1000, sources: Vec::new(), sources_at: None }
    }
}

impl VideoMgr {
    /// Starts sending `source` to `peer`. Returns an error message for the UI.
    pub fn start(
        &mut self,
        net: &relay_net::NetHandle,
        source: &str,
        peer: &str,
        with_audio: bool,
        name: &str,
    ) -> Result<u32, String> {
        if self.sending.iter().any(|s| s.peer == peer && s.source == source) {
            return Err("Already sending that".into());
        }
        let id = self.next_id;
        self.next_id += 1;
        let tx = Arc::new(net.open_video_sink(peer, id, name));
        let tx2 = tx.clone();
        let capture = relay_video::start(
            source,
            relay_video::Settings::default(),
            Box::new(move |f| {
                tx2.send(f);
            }),
        );
        let (capture, error) = match capture {
            Ok(c) => (Some(c), None),
            Err(e) => {
                tracing::warn!("video capture failed: {e:#}");
                (None, Some(format!("{e}")))
            }
        };
        let failed = error.clone();
        self.sending.push(Sending {
            id,
            peer: peer.to_string(),
            source: source.to_string(),
            with_audio,
            tx,
            capture,
            error,
        });
        match failed {
            Some(e) => Err(format!("Couldn't capture: {e}")),
            None => Ok(id),
        }
    }

    pub fn stop(&mut self, id: u32) {
        self.sending.retain(|s| s.id != id);
    }

    pub fn wants_audio(&self, peer: &str) -> bool {
        self.sending.iter().any(|s| s.peer == peer && s.with_audio)
    }

    /// A device started sending us video: show it.
    pub fn on_incoming(&mut self, net: &relay_net::NetHandle, peer: &str, stream: u32, name: &str, peer_name: &str) {
        if self.receiving.iter().any(|r| r.peer == peer && r.stream == stream && r.open.load(Ordering::Relaxed)) {
            return;
        }
        self.receiving.retain(|r| !(r.peer == peer && r.stream == stream));
        let rx = net.subscribe_video(peer, stream);
        let stop = Arc::new(AtomicBool::new(false));
        let need_key = Arc::new(AtomicBool::new(false));
        let open = Arc::new(AtomicBool::new(true));
        let title = if name.is_empty() { peer_name.to_string() } else { format!("{peer_name} · {name}") };
        let (s2, k2, o2) = (stop.clone(), need_key.clone(), open.clone());
        let _ = std::thread::Builder::new().name("relay-video-recv".into()).spawn(move || {
            #[cfg(windows)]
            {
                let viewer = match relay_video::windows::viewer::open(&title) {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!("viewer failed: {e:#}");
                        o2.store(false, Ordering::Relaxed);
                        return;
                    }
                };
                while !s2.load(Ordering::Relaxed) {
                    match rx.recv_timeout(Duration::from_millis(250)) {
                        Ok(f) => {
                            if !viewer.push(f) {
                                break;
                            }
                        }
                        Err(crossbeam_channel::RecvTimeoutError::Timeout) => {
                            if !viewer.is_open() {
                                break;
                            }
                        }
                        Err(_) => break,
                    }
                    if viewer.take_keyframe_request() {
                        k2.store(true, Ordering::Relaxed);
                    }
                }
            }
            #[cfg(not(windows))]
            {
                let _ = (rx, s2, k2, title);
                tracing::warn!("no video viewer on this platform yet");
            }
            o2.store(false, Ordering::Relaxed);
        });
        self.receiving.push(Receiving { peer: peer.to_string(), stream, name: name.to_string(), stop, need_key, open });
    }

    pub fn on_ended(&mut self, peer: &str, stream: u32) {
        self.receiving.retain(|r| {
            let keep = !(r.peer == peer && r.stream == stream);
            if !keep {
                r.stop.store(true, Ordering::Relaxed);
            }
            keep
        });
    }

    pub fn clear(&mut self) {
        self.sending.clear();
        for r in self.receiving.drain(..) {
            r.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Keyframe requests in both directions, and closed viewers.
    pub fn poll(&mut self, net: Option<&relay_net::NetHandle>) {
        for s in &self.sending {
            if s.tx.take_keyframe_request()
                && let Some(c) = &s.capture
            {
                c.request_keyframe();
            }
        }
        if let Some(net) = net {
            for r in &self.receiving {
                if r.need_key.swap(false, Ordering::Relaxed) {
                    net.request_keyframe(&r.peer, r.stream);
                }
            }
        }
    }

    pub fn state(&mut self) -> Video {
        if self.sources_at.map(|t| t.elapsed() > Duration::from_secs(5)).unwrap_or(true) {
            self.sources = relay_video::sources();
            self.sources_at = Some(Instant::now());
        }
        Video {
            sources: self.sources.clone(),
            sending: self
                .sending
                .iter()
                .map(|s| {
                    let st = s.capture.as_ref().map(|c| c.stats()).unwrap_or_default();
                    VideoSend {
                        id: s.id,
                        source: s.source.clone(),
                        peer: s.peer.clone(),
                        with_audio: s.with_audio,
                        status: match (&s.error, s.tx.connected()) {
                            (Some(e), _) => PortStatus::Error(e.clone()),
                            (None, false) => PortStatus::Missing("Not connected".into()),
                            (None, true) => PortStatus::Running,
                        },
                        width: st.width,
                        height: st.height,
                        fps: st.fps,
                        kbps: st.kbps,
                        encoder: if st.encoder_hw { "Hardware H.264".into() } else { "H.264".into() },
                    }
                })
                .collect(),
            receiving: self
                .receiving
                .iter()
                .filter(|r| r.open.load(Ordering::Relaxed))
                .map(|r| VideoReceive {
                    peer: r.peer.clone(),
                    stream: r.stream,
                    name: r.name.clone(),
                    width: 0,
                    height: 0,
                    fps: 0.0,
                    latency_ms: 0.0,
                    as_camera: false,
                    decoder: "Hardware H.264".into(),
                })
                .collect(),
            camera_supported: false,
        }
    }
}

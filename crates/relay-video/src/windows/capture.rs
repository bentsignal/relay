//! Screen capture with DXGI Desktop Duplication, converted and encoded on the GPU.

use super::convert::{Converter, texture};
use super::encode::{EncodedOut, Encoder};
use super::{Gpu, create_device, outputs};
use crate::{Capture, CaptureStats, FrameSink, Settings};
use anyhow::{Context, Result};
use parking_lot::Mutex;
use relay_proto::{EncodedVideo, VideoCodec};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::core::Interface;

struct Shared {
    stop: AtomicBool,
    key: AtomicBool,
    stats: Mutex<CaptureStats>,
}

struct ScreenCapture {
    shared: Arc<Shared>,
}

impl Capture for ScreenCapture {
    fn request_keyframe(&self) {
        self.shared.key.store(true, Ordering::Relaxed);
    }
    fn stats(&self) -> CaptureStats {
        *self.shared.stats.lock()
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

pub fn epoch() -> Instant {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    *EPOCH.get_or_init(Instant::now)
}

pub fn start_screen(ai: usize, oi: usize, settings: Settings, sink: FrameSink) -> Result<Box<dyn Capture>> {
    let shared = Arc::new(Shared {
        stop: AtomicBool::new(false),
        key: AtomicBool::new(true),
        stats: Mutex::new(CaptureStats::default()),
    });
    let s2 = shared.clone();
    // Fail fast on obvious setup errors, then keep running on a thread.
    let (tx, rx) = crossbeam_channel::bounded::<Result<()>>(1);
    std::thread::Builder::new().name("relay-video-capture".into()).spawn(move || {
        let mut sink = sink;
        let mut first = Some(tx);
        while !s2.stop.load(Ordering::Relaxed) {
            match run(ai, oi, settings, &mut sink, &s2, &mut first) {
                Ok(()) => break,
                Err(e) => {
                    tracing::warn!("screen capture restarting: {e:#}");
                    if let Some(tx) = first.take() {
                        let _ = tx.send(Err(e));
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                }
            }
        }
    })?;
    match rx.recv_timeout(Duration::from_secs(10)) {
        Ok(Ok(())) => Ok(Box::new(ScreenCapture { shared })),
        Ok(Err(e)) => Err(e),
        Err(_) => Ok(Box::new(ScreenCapture { shared })),
    }
}

fn fit(src: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let scale = (max.0 as f32 / src.0 as f32).min(max.1 as f32 / src.1 as f32).min(1.0);
    let w = ((src.0 as f32 * scale) as u32) & !1;
    let h = ((src.1 as f32 * scale) as u32) & !1;
    (w.max(2), h.max(2))
}

fn run(
    ai: usize,
    oi: usize,
    settings: Settings,
    sink: &mut FrameSink,
    shared: &Shared,
    first: &mut Option<crossbeam_channel::Sender<Result<()>>>,
) -> Result<()> {
    let (adapter, output, ..) =
        outputs().into_iter().find(|o| o.5 == ai && o.6 == oi).context("that screen is no longer connected")?;
    let gpu: Gpu = create_device(Some(&adapter))?;
    let output1: IDXGIOutput1 = output.cast()?;
    let dup = unsafe { output1.DuplicateOutput(&gpu.device) }.context("screen capture unavailable")?;
    let desc = unsafe { dup.GetDesc() };
    let src = (desc.ModeDesc.Width, desc.ModeDesc.Height);
    let dst = fit(src, (settings.max_width, settings.max_height));
    let latest =
        texture(&gpu, src.0, src.1, DXGI_FORMAT_B8G8R8A8_UNORM, D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE)?;
    let nv12: Vec<ID3D11Texture2D> = (0..4)
        .map(|_| texture(&gpu, dst.0, dst.1, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET))
        .collect::<Result<_>>()?;
    let converter = Converter::new(&gpu, src, dst, settings.fps, true)?;
    let mut encoder = Encoder::new(&gpu, dst.0, dst.1, settings.fps, settings.bitrate_kbps)?;
    if let Some(tx) = first.take() {
        let _ = tx.send(Ok(()));
    }
    tracing::info!(src = ?src, dst = ?dst, encoder = %encoder.name, "screen capture started");

    let period = Duration::from_secs_f64(1.0 / settings.fps as f64);
    let start = Instant::now();
    let mut next = start;
    let mut have_frame = false;
    let mut ring = 0usize;
    let mut seq = 0u32;
    let mut outs: Vec<EncodedOut> = Vec::new();
    let mut window = (Instant::now(), 0u32, 0usize);
    let (mut dbg_loops, mut dbg_encodes) = (0u32, 0u32);
    let mut pending_time = 0i64;

    let mut pending: Option<usize> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        next += period;
        let now = Instant::now();
        if next < now {
            next = now; // fell behind; don't burst
        }
        // Newest desktop image, if it changed.
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource = None;
        match unsafe { dup.AcquireNextFrame(0, &mut info, &mut resource) } {
            Ok(()) => {
                if let Some(res) = resource {
                    let tex: ID3D11Texture2D = res.cast()?;
                    unsafe { gpu.context.CopyResource(&latest, &tex) };
                    have_frame = true;
                }
                unsafe {
                    let _ = dup.ReleaseFrame();
                }
            }
            Err(e) if e.code() == DXGI_ERROR_WAIT_TIMEOUT => {}
            Err(e) if e.code() == DXGI_ERROR_ACCESS_LOST => {
                anyhow::bail!("desktop changed (mode switch or secure desktop)");
            }
            Err(e) => return Err(e.into()),
        }
        // One frame per tick, converted now so its timestamp is the capture time.
        if have_frame {
            converter.convert(&latest, 0, &nv12[ring])?;
            pending = Some(ring);
            pending_time = epoch().elapsed().as_micros() as i64 * 10;
            ring = (ring + 1) % nv12.len();
        }
        dbg_loops += 1;
        // Until the next tick, feed the encoder whenever it asks and collect output.
        loop {
            encoder.poll(&mut outs)?;
            if encoder.wants_input()
                && let Some(slot) = pending.take()
            {
                let key = shared.key.swap(false, Ordering::Relaxed);
                encoder.encode(&nv12[slot], pending_time, key)?;
                dbg_encodes += 1;
            }
            for o in outs.drain(..) {
                window.2 += o.data.len();
                window.1 += 1;
                sink(EncodedVideo {
                    stream: 0,
                    seq,
                    pts_us: (o.time_100ns / 10) as u64,
                    key: o.key,
                    codec: VideoCodec::H264,
                    width: dst.0 as u16,
                    height: dst.1 as u16,
                    data: o.data,
                });
                seq = seq.wrapping_add(1);
            }
            let now = Instant::now();
            if now >= next {
                break;
            }
            std::thread::sleep((next - now).min(Duration::from_micros(700)));
        }
        if window.0.elapsed() >= Duration::from_secs(1) {
            tracing::debug!(loops = dbg_loops, encodes = dbg_encodes, "capture loop");
            dbg_loops = 0;
            dbg_encodes = 0;
            let dt = window.0.elapsed().as_secs_f32();
            *shared.stats.lock() = CaptureStats {
                width: dst.0,
                height: dst.1,
                fps: window.1 as f32 / dt,
                kbps: window.2 as f32 * 8.0 / 1000.0 / dt,
                encoder_hw: true,
            };
            window = (Instant::now(), 0, 0);
        }
    }
    Ok(())
}

//! Screen capture with ScreenCaptureKit and camera capture with
//! AVCaptureSession, both feeding a VideoToolbox H.264 encoder.

use super::encode::{Encoded, Encoder};
use super::ffi::*;
use crate::{Capture, CaptureStats, FrameSink, Settings};
use anyhow::{Context, Result, anyhow, bail};
use crossbeam_channel::bounded;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{AnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceInput, AVCaptureOutput,
    AVCaptureSession, AVCaptureSessionPreset1280x720, AVCaptureVideoDataOutput,
    AVCaptureVideoDataOutputSampleBufferDelegate, AVMediaTypeVideo,
};
use objc2_core_media::{CMSampleBuffer, CMTime as OCMTime, CMTimeFlags};
use objc2_foundation::{NSArray, NSDictionary, NSError, NSNumber, NSString};
use objc2_screen_capture_kit::{
    SCContentFilter, SCFrameStatus, SCShareableContent, SCStream, SCStreamConfiguration, SCStreamDelegate,
    SCStreamFrameInfoStatus, SCStreamOutput, SCStreamOutputType,
};
use parking_lot::Mutex;
use relay_proto::{EncodedVideo, VideoCodec};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, Ordering};
use std::sync::{Arc, Weak};
use std::time::{Duration, Instant};

pub const SCREEN_PERMISSION: &str = "Allow Relay to record the screen in System Settings › Privacy & Security › \
     Screen & System Audio Recording, then try again";
pub const CAMERA_PERMISSION: &str =
    "Allow Relay to use the camera in System Settings › Privacy & Security › Camera, then try again";

/// Monotonic microseconds since the first video timestamp in this process.
pub fn now_us() -> i64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_micros() as i64
}

/// Shared by the capture callback, the encoder callback and the handle.
struct Pipeline {
    settings: Settings,
    sink: Mutex<FrameSink>,
    encoder: Mutex<Option<Encoder>>,
    key: AtomicBool,
    stopped: AtomicBool,
    seq: AtomicU32,
    last_pts: AtomicI64,
    /// Most recent frame, re-encoded when the screen is static (ScreenCaptureKit
    /// only delivers frames when something changes) or a keyframe is requested.
    last: Mutex<Option<(Cf, Instant)>>,
    keep_last: bool,
    window: Mutex<(Instant, u32, usize)>,
    stats: Mutex<CaptureStats>,
    me: Weak<Pipeline>,
}

impl Pipeline {
    fn new(settings: Settings, sink: FrameSink, keep_last: bool) -> Arc<Self> {
        Arc::new_cyclic(|me| Pipeline {
            settings,
            sink: Mutex::new(sink),
            encoder: Mutex::new(None),
            key: AtomicBool::new(true),
            stopped: AtomicBool::new(false),
            seq: AtomicU32::new(0),
            last_pts: AtomicI64::new(-1),
            last: Mutex::new(None),
            keep_last,
            window: Mutex::new((Instant::now(), 0, 0)),
            stats: Mutex::new(CaptureStats::default()),
            me: me.clone(),
        })
    }

    /// Encodes one captured frame.
    fn on_pixels(&self, pb: CVPixelBufferRef) {
        if self.stopped.load(Ordering::Relaxed) || pb.is_null() {
            return;
        }
        let (w, h) = unsafe { (CVPixelBufferGetWidth(pb) as u32, CVPixelBufferGetHeight(pb) as u32) };
        let mut enc = self.encoder.lock();
        if enc.as_ref().map(|e| (e.width, e.height) != (w, h)).unwrap_or(true) {
            *enc = None;
            let me = self.me.clone();
            let s = self.settings;
            match Encoder::new(
                w,
                h,
                s.fps,
                s.bitrate_kbps,
                Box::new(move |e| {
                    if let Some(p) = me.upgrade() {
                        p.deliver(e);
                    }
                }),
            ) {
                Ok(e) => {
                    self.stats.lock().encoder_hw = e.hw;
                    *enc = Some(e);
                    self.key.store(true, Ordering::Relaxed);
                }
                Err(e) => {
                    tracing::warn!("{e:#}");
                    return;
                }
            }
        }
        let Some(encoder) = enc.as_mut() else { return };
        // Strictly increasing timestamps, even for repeated frames.
        let pts = now_us().max(self.last_pts.load(Ordering::Relaxed) + 1);
        self.last_pts.store(pts, Ordering::Relaxed);
        let key = self.key.swap(false, Ordering::Relaxed);
        if let Err(e) = encoder.encode(pb, pts, key) {
            tracing::warn!("{e:#}");
            *enc = None; // recreated with the next frame
        }
        drop(enc);
        if self.keep_last {
            *self.last.lock() = Cf::retained(pb as CFTypeRef).map(|c| (c, Instant::now()));
        }
    }

    fn deliver(&self, e: Encoded) {
        if self.stopped.load(Ordering::Relaxed) {
            return;
        }
        let len = e.data.len();
        let frame = EncodedVideo {
            stream: 0,
            seq: self.seq.fetch_add(1, Ordering::Relaxed),
            pts_us: e.pts_us.max(0) as u64,
            key: e.key,
            codec: VideoCodec::H264,
            width: e.width as u16,
            height: e.height as u16,
            data: e.data,
        };
        (self.sink.lock())(frame);
        let mut w = self.window.lock();
        w.1 += 1;
        w.2 += len;
        let dt = w.0.elapsed().as_secs_f32();
        if dt >= 1.0 {
            let mut st = self.stats.lock();
            st.width = e.width;
            st.height = e.height;
            st.fps = w.1 as f32 / dt;
            st.kbps = w.2 as f32 * 8.0 / 1000.0 / dt;
            *w = (Instant::now(), 0, 0);
        }
    }

    /// Re-encodes the last frame when a keyframe is wanted or nothing changed
    /// for a while, so viewers that join late or lose packets recover.
    fn spawn_repeater(self: &Arc<Self>) {
        let me = Arc::downgrade(self);
        let _ = std::thread::Builder::new().name("relay-video-repeat".into()).spawn(move || {
            loop {
                std::thread::sleep(Duration::from_millis(20));
                let Some(p) = me.upgrade() else { return };
                if p.stopped.load(Ordering::Relaxed) {
                    return;
                }
                let due = {
                    let last = p.last.lock();
                    match &*last {
                        Some((pb, at)) => {
                            let age = at.elapsed();
                            let want = (p.key.load(Ordering::Relaxed) && age > Duration::from_millis(30))
                                || age > Duration::from_millis(500);
                            want.then(|| Cf::retained(pb.0))
                        }
                        None => None,
                    }
                };
                if let Some(Some(pb)) = due {
                    p.on_pixels(pb.0 as CVPixelBufferRef);
                }
            }
        });
    }

    fn shutdown(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let enc = self.encoder.lock().take();
        drop(enc);
        *self.last.lock() = None;
    }
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "RelayVideoCaptureOutput"]
    #[ivars = Arc<Pipeline>]
    struct Output;

    unsafe impl NSObjectProtocol for Output {}

    unsafe impl SCStreamOutput for Output {
        #[unsafe(method(stream:didOutputSampleBuffer:ofType:))]
        fn stream_output(&self, _stream: &SCStream, sb: &CMSampleBuffer, kind: SCStreamOutputType) {
            if kind != SCStreamOutputType::Screen {
                return;
            }
            let sb = sb as *const CMSampleBuffer as CMSampleBufferRef;
            unsafe {
                if CMSampleBufferIsValid(sb) == 0 || !frame_complete(sb) {
                    return;
                }
                self.ivars().on_pixels(CMSampleBufferGetImageBuffer(sb));
            }
        }
    }

    unsafe impl SCStreamDelegate for Output {
        #[unsafe(method(stream:didStopWithError:))]
        fn stream_stopped(&self, _stream: &SCStream, error: &NSError) {
            tracing::warn!("screen capture stopped: {}", error.localizedDescription());
            self.ivars().stats.lock().fps = 0.0;
        }
    }

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for Output {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn camera_output(&self, _out: &AVCaptureOutput, sb: &CMSampleBuffer, _conn: &AVCaptureConnection) {
            let sb = sb as *const CMSampleBuffer as CMSampleBufferRef;
            unsafe { self.ivars().on_pixels(CMSampleBufferGetImageBuffer(sb)) };
        }
    }
);

impl Output {
    fn new(p: Arc<Pipeline>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(p);
        unsafe { msg_send![super(this), init] }
    }
}

/// ScreenCaptureKit marks frames where nothing changed as idle (no pixels).
unsafe fn frame_complete(sb: CMSampleBufferRef) -> bool {
    unsafe {
        let arr = CMSampleBufferGetSampleAttachmentsArray(sb, 0);
        if arr.is_null() || CFArrayGetCount(arr) == 0 {
            return false;
        }
        let dict = CFArrayGetValueAtIndex(arr, 0);
        let key = SCStreamFrameInfoStatus as *const NSString as *const std::ffi::c_void;
        let v = CFDictionaryGetValue(dict, key);
        if v.is_null() {
            return false;
        }
        let mut status = 0i32;
        CFNumberGetValue(v, kCFNumberSInt32Type, &mut status as *mut i32 as *mut std::ffi::c_void);
        status as isize == SCFrameStatus::Complete.0
    }
}

fn octime(value: i64, timescale: i32) -> OCMTime {
    OCMTime { value, timescale, flags: CMTimeFlags::Valid, epoch: 0 }
}

fn fit(src: (u32, u32), max: (u32, u32)) -> (u32, u32) {
    let scale = (max.0 as f32 / src.0 as f32).min(max.1 as f32 / src.1 as f32).min(1.0);
    let w = ((src.0 as f32 * scale) as u32) & !1;
    let h = ((src.1 as f32 * scale) as u32) & !1;
    (w.max(2), h.max(2))
}

fn ns_error(e: &NSError) -> anyhow::Error {
    anyhow!("{} (code {})", e.localizedDescription(), e.code())
}

struct ScreenCapture {
    pipeline: Arc<Pipeline>,
    stream: Retained<SCStream>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
}

// SCStream may be stopped from any thread.
unsafe impl Send for ScreenCapture {}

impl Capture for ScreenCapture {
    fn request_keyframe(&self) {
        self.pipeline.key.store(true, Ordering::Relaxed);
    }
    fn stats(&self) -> CaptureStats {
        *self.pipeline.stats.lock()
    }
}

impl Drop for ScreenCapture {
    fn drop(&mut self) {
        self.pipeline.stopped.store(true, Ordering::Relaxed);
        let (tx, rx) = bounded::<()>(1);
        let block = block2::RcBlock::new(move |_e: *mut NSError| {
            let _ = tx.try_send(());
        });
        unsafe { self.stream.stopCaptureWithCompletionHandler(Some(&block)) };
        let _ = rx.recv_timeout(Duration::from_secs(2));
        self.pipeline.shutdown();
    }
}

fn shareable_content() -> Result<Retained<SCShareableContent>> {
    let (tx, rx) = bounded::<Result<Retained<SCShareableContent>>>(1);
    let block = block2::RcBlock::new(move |content: *mut SCShareableContent, err: *mut NSError| {
        let r = match (unsafe { Retained::retain(content) }, unsafe { err.as_ref() }) {
            (Some(c), _) => Ok(c),
            (None, Some(e)) if e.code() == objc2_screen_capture_kit::SCStreamErrorCode::UserDeclined.0 => {
                Err(anyhow!(SCREEN_PERMISSION))
            }
            (None, Some(e)) => Err(ns_error(e)),
            (None, None) => Err(anyhow!("no screens available")),
        };
        let _ = tx.try_send(r);
    });
    unsafe { SCShareableContent::getShareableContentWithCompletionHandler(&block) };
    rx.recv_timeout(Duration::from_secs(8)).map_err(|_| anyhow!("screen capture didn't respond"))?
}

pub fn start_screen(display: u32, settings: Settings, sink: FrameSink) -> Result<Box<dyn Capture>> {
    if !unsafe { CGPreflightScreenCaptureAccess() } {
        // Shows the system prompt (once) and adds Relay to the list in Settings.
        unsafe { CGRequestScreenCaptureAccess() };
        bail!(SCREEN_PERMISSION);
    }
    let content = shareable_content()?;
    let displays = unsafe { content.displays() };
    let sc_display =
        displays.iter().find(|d| unsafe { d.displayID() } == display).context("that screen is no longer connected")?;
    let native = display_pixels(display)
        .unwrap_or_else(|| unsafe { (sc_display.width() as u32 * 2, sc_display.height() as u32 * 2) });
    let dst = fit(native, (settings.max_width, settings.max_height));
    let fps = settings.fps.clamp(1, 120);

    let pipeline = Pipeline::new(settings, sink, true);
    let output = Output::new(pipeline.clone());
    let queue = DispatchQueue::new("relay.video.screen", None);
    let stream = unsafe {
        let filter =
            SCContentFilter::initWithDisplay_excludingWindows(SCContentFilter::alloc(), &sc_display, &NSArray::new());
        let cfg = SCStreamConfiguration::new();
        cfg.setWidth(dst.0 as usize);
        cfg.setHeight(dst.1 as usize);
        cfg.setMinimumFrameInterval(octime(1, fps as i32));
        cfg.setPixelFormat(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        cfg.setShowsCursor(true);
        cfg.setQueueDepth(5);
        cfg.setCapturesAudio(false);
        cfg.setColorMatrix(&*(kCGDisplayStreamYCbCrMatrix_ITU_R_709_2 as *const objc2_core_foundation::CFString));
        let stream = SCStream::initWithFilter_configuration_delegate(
            SCStream::alloc(),
            &filter,
            &cfg,
            Some(ProtocolObject::from_ref(&*output)),
        );
        stream
            .addStreamOutput_type_sampleHandlerQueue_error(
                ProtocolObject::from_ref(&*output),
                SCStreamOutputType::Screen,
                Some(&queue),
            )
            .map_err(|e| ns_error(&e))?;
        stream
    };
    let (tx, rx) = bounded::<Result<()>>(1);
    let block = block2::RcBlock::new(move |err: *mut NSError| {
        let r = match unsafe { err.as_ref() } {
            None => Ok(()),
            Some(e) if e.code() == objc2_screen_capture_kit::SCStreamErrorCode::UserDeclined.0 => {
                Err(anyhow!(SCREEN_PERMISSION))
            }
            Some(e) => Err(ns_error(e)),
        };
        let _ = tx.try_send(r);
    });
    unsafe { stream.startCaptureWithCompletionHandler(Some(&block)) };
    let started =
        rx.recv_timeout(Duration::from_secs(10)).unwrap_or_else(|_| Err(anyhow!("screen capture didn't start")));
    let cap = ScreenCapture { pipeline: pipeline.clone(), stream, _output: output, _queue: queue };
    started?; // dropping `cap` stops the stream
    pipeline.stats.lock().width = dst.0;
    pipeline.stats.lock().height = dst.1;
    pipeline.spawn_repeater();
    let display_id = display;
    tracing::info!(display_id, native = ?native, dst = ?dst, fps, "screen capture started");
    Ok(Box::new(cap))
}

struct CameraCapture {
    pipeline: Arc<Pipeline>,
    session: Retained<AVCaptureSession>,
    _output: Retained<Output>,
    _queue: DispatchRetained<DispatchQueue>,
}

unsafe impl Send for CameraCapture {}

impl Capture for CameraCapture {
    fn request_keyframe(&self) {
        self.pipeline.key.store(true, Ordering::Relaxed);
    }
    fn stats(&self) -> CaptureStats {
        *self.pipeline.stats.lock()
    }
}

impl Drop for CameraCapture {
    fn drop(&mut self) {
        self.pipeline.stopped.store(true, Ordering::Relaxed);
        unsafe { self.session.stopRunning() };
        self.pipeline.shutdown();
    }
}

pub fn camera_authorized() -> Result<()> {
    let media = unsafe { AVMediaTypeVideo }.context("AVFoundation unavailable")?;
    let status = unsafe { AVCaptureDevice::authorizationStatusForMediaType(media) };
    if status != AVAuthorizationStatus::Authorized {
        tracing::info!(status = status.0, "camera not authorized (0 = not asked yet, 1 = restricted, 2 = denied)");
    }
    match status {
        AVAuthorizationStatus::Authorized => Ok(()),
        AVAuthorizationStatus::NotDetermined => {
            // Shows the system prompt; the user starts the camera again after answering.
            let block = block2::RcBlock::new(|_ok: Bool| {});
            unsafe { AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block) };
            bail!(CAMERA_PERMISSION)
        }
        _ => bail!(CAMERA_PERMISSION),
    }
}

pub fn start_camera(unique_id: &str, settings: Settings, sink: FrameSink) -> Result<Box<dyn Capture>> {
    camera_authorized()?;
    let device = unsafe { AVCaptureDevice::deviceWithUniqueID(&NSString::from_str(unique_id)) }
        .context("that camera is no longer connected")?;
    let input = unsafe { AVCaptureDeviceInput::deviceInputWithDevice_error(&device) }.map_err(|e| ns_error(&e))?;
    let settings = Settings { fps: settings.fps.min(30), ..settings };
    let pipeline = Pipeline::new(settings, sink, false);
    let output = Output::new(pipeline.clone());
    let queue = DispatchQueue::new("relay.video.camera", None);
    let session = unsafe {
        let session = AVCaptureSession::new();
        session.beginConfiguration();
        if session.canSetSessionPreset(AVCaptureSessionPreset1280x720) {
            session.setSessionPreset(AVCaptureSessionPreset1280x720);
        }
        if !session.canAddInput(&input) {
            session.commitConfiguration();
            bail!("the camera is in use or unavailable");
        }
        session.addInput(&input);
        let data_out = AVCaptureVideoDataOutput::new();
        let format = NSNumber::new_u32(kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange);
        let key = &*(kCVPixelBufferPixelFormatTypeKey as *const NSString);
        let settings_dict = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[&*format as &AnyObject]);
        data_out.setVideoSettings(Some(&settings_dict));
        data_out.setAlwaysDiscardsLateVideoFrames(true);
        data_out.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*output)), Some(&queue));
        if !session.canAddOutput(&data_out) {
            session.commitConfiguration();
            bail!("couldn't read frames from the camera");
        }
        session.addOutput(&data_out);
        session.commitConfiguration();
        set_camera_fps(&device, 30.0);
        session.startRunning();
        session
    };
    if !unsafe { session.isRunning() } {
        bail!("the camera didn't start");
    }
    tracing::info!(camera = unique_id, "camera capture started");
    Ok(Box::new(CameraCapture { pipeline, session, _output: output, _queue: queue }))
}

unsafe fn set_camera_fps(device: &AVCaptureDevice, fps: f64) {
    unsafe {
        let ranges = device.activeFormat().videoSupportedFrameRateRanges();
        if !ranges.iter().any(|r| r.minFrameRate() <= fps && fps <= r.maxFrameRate()) {
            return;
        }
        if device.lockForConfiguration().is_ok() {
            let d = octime(1, fps as i32);
            device.setActiveVideoMinFrameDuration(d);
            device.setActiveVideoMaxFrameDuration(d);
            device.unlockForConfiguration();
        }
    }
}

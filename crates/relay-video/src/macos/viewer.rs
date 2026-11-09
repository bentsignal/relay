//! An NSWindow showing incoming H.264 through AVSampleBufferDisplayLayer,
//! which decodes in hardware and presents each frame as soon as it arrives.
//! Double-click toggles full screen; Esc leaves it; the close button closes.
//!
//! Windows live on the main thread (see `app.rs`); frames are enqueued from
//! the caller's thread straight into the layer's renderer.

use super::app;
use super::ffi::*;
use crate::h264::{self, NAL_AUD, NAL_IDR, NAL_PPS, NAL_SPS};
use anyhow::{Result, anyhow, bail};
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{NSObject, NSObjectProtocol, ProtocolObject};
use objc2::{DefinedClass, MainThreadMarker, MainThreadOnly, define_class, msg_send};
use objc2_app_kit::{
    NSBackingStoreType, NSColor, NSEvent, NSScreen, NSView, NSWindow, NSWindowCollectionBehavior, NSWindowDelegate,
    NSWindowStyleMask,
};
use objc2_av_foundation::{
    AVLayerVideoGravityResizeAspect, AVQueuedSampleBufferRendering, AVQueuedSampleBufferRenderingStatus,
    AVSampleBufferDisplayLayer, AVSampleBufferVideoRenderer,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSNotification, NSPoint, NSRect, NSSize, NSString};
use parking_lot::Mutex;
use relay_proto::EncodedVideo;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

/// Handle to an open viewer window.
pub struct Viewer {
    shared: Arc<Shared>,
    id: usize,
}

impl Viewer {
    /// Returns false once the user has closed the window.
    pub fn push(&self, f: EncodedVideo) -> bool {
        if self.shared.closed.load(Ordering::Relaxed) {
            return false;
        }
        self.shared.feed(&f, self.id);
        true
    }

    pub fn is_open(&self) -> bool {
        !self.shared.closed.load(Ordering::Relaxed)
    }

    /// True (once) after the viewer had to drop frames and needs a keyframe.
    pub fn take_keyframe_request(&self) -> bool {
        self.shared.need_key.swap(false, Ordering::Relaxed)
    }

    /// Whether this stream also feeds a system webcam (Windows only for now).
    pub fn is_camera(&self) -> bool {
        false
    }
}

impl Drop for Viewer {
    fn drop(&mut self) {
        let id = self.id;
        DispatchQueue::main().exec_async(move || close_window(id));
    }
}

struct Renderer(Retained<AVSampleBufferVideoRenderer>);
// AVSampleBufferVideoRenderer accepts sample buffers from any thread.
unsafe impl Send for Renderer {}
unsafe impl Sync for Renderer {}

struct Shared {
    closed: AtomicBool,
    need_key: AtomicBool,
    renderer: Renderer,
    dec: Mutex<Decode>,
}

struct Decode {
    format: Option<Cf>,
    sps: Vec<u8>,
    pps: Vec<u8>,
    size: (i32, i32),
    waiting_key: bool,
    last_request: Option<Instant>,
}

impl Shared {
    fn request_key(&self, st: &mut Decode) {
        if st.last_request.map(|t| t.elapsed() > Duration::from_millis(500)).unwrap_or(true) {
            st.last_request = Some(Instant::now());
            self.need_key.store(true, Ordering::Relaxed);
        }
    }

    fn feed(&self, f: &EncodedVideo, id: usize) {
        let nals = h264::split_annexb(&f.data);
        let mut st = self.dec.lock();
        let sps = nals.iter().find(|n| h264::nal_type(n) == NAL_SPS);
        let pps = nals.iter().find(|n| h264::nal_type(n) == NAL_PPS);
        if let (Some(&sps), Some(&pps)) = (sps, pps)
            && (st.format.is_none() || sps != st.sps.as_slice() || pps != st.pps.as_slice())
        {
            match format_from_params(sps, pps) {
                Some(fmt) => {
                    let d = unsafe { CMVideoFormatDescriptionGetDimensions(fmt.0) };
                    if (d.width, d.height) != st.size {
                        st.size = (d.width, d.height);
                        DispatchQueue::main().exec_async(move || fit_window(id, d.width, d.height));
                    }
                    st.format = Some(fmt);
                    st.sps = sps.to_vec();
                    st.pps = pps.to_vec();
                }
                None => tracing::debug!("bad SPS/PPS"),
            }
        }
        let is_key = f.key || nals.iter().any(|n| h264::nal_type(n) == NAL_IDR);
        if st.format.is_none() || (st.waiting_key && !is_key) {
            self.request_key(&mut st);
            return;
        }
        let r = &self.renderer.0;
        unsafe {
            if r.status() == AVQueuedSampleBufferRenderingStatus::Failed || r.requiresFlushToResumeDecoding() {
                tracing::debug!("video renderer needs a flush: {:?}", r.error().map(|e| e.localizedDescription()));
                r.flush();
                st.waiting_key = true;
                if !is_key {
                    self.request_key(&mut st);
                    return;
                }
            }
            if !r.isReadyForMoreMediaData() {
                // Decoder is behind: drop until the next keyframe.
                st.waiting_key = true;
                self.request_key(&mut st);
                return;
            }
        }
        st.waiting_key = false;
        let avcc =
            h264::to_avcc(nals.iter().copied().filter(|n| !matches!(h264::nal_type(n), NAL_SPS | NAL_PPS | NAL_AUD)));
        let fmt = st.format.as_ref().map(|f| f.0).unwrap_or(ptr::null());
        match unsafe { sample_buffer(&avcc, fmt, f.pts_us as i64, is_key) } {
            Some(sb) => unsafe { r.enqueueSampleBuffer(&*(sb.0 as *const CMSampleBuffer)) },
            None => {
                st.waiting_key = true;
                self.request_key(&mut st);
            }
        }
    }
}

fn format_from_params(sps: &[u8], pps: &[u8]) -> Option<Cf> {
    let ptrs = [sps.as_ptr(), pps.as_ptr()];
    let sizes = [sps.len(), pps.len()];
    let mut fmt: CMFormatDescriptionRef = ptr::null();
    let st = unsafe {
        CMVideoFormatDescriptionCreateFromH264ParameterSets(
            kCFAllocatorDefault,
            2,
            ptrs.as_ptr(),
            sizes.as_ptr(),
            4,
            &mut fmt,
        )
    };
    if st != 0 { None } else { Cf::owned(fmt) }
}

/// One-sample CMSampleBuffer (AVCC data) marked for immediate display.
unsafe fn sample_buffer(avcc: &[u8], fmt: CMFormatDescriptionRef, pts_us: i64, key: bool) -> Option<Cf> {
    unsafe {
        if avcc.is_empty() || fmt.is_null() {
            return None;
        }
        let len = avcc.len();
        let mut block: CMBlockBufferRef = ptr::null_mut();
        if CMBlockBufferCreateWithMemoryBlock(
            kCFAllocatorDefault,
            ptr::null_mut(),
            len,
            kCFAllocatorDefault,
            ptr::null(),
            0,
            len,
            kCMBlockBufferAssureMemoryNowFlag,
            &mut block,
        ) != 0
        {
            return None;
        }
        let block = Cf::owned(block as CFTypeRef)?;
        if CMBlockBufferReplaceDataBytes(avcc.as_ptr() as *const _, block.0 as CMBlockBufferRef, 0, len) != 0 {
            return None;
        }
        let timing = CMSampleTimingInfo {
            duration: CMTime::INVALID,
            presentation_time_stamp: CMTime::new(pts_us, 1_000_000),
            decode_time_stamp: CMTime::INVALID,
        };
        let mut sb: CMSampleBufferRef = ptr::null_mut();
        if CMSampleBufferCreateReady(
            kCFAllocatorDefault,
            block.0 as CMBlockBufferRef,
            fmt,
            1,
            1,
            &timing,
            1,
            &len,
            &mut sb,
        ) != 0
        {
            return None;
        }
        let sb = Cf::owned(sb as CFTypeRef)?;
        let arr = CMSampleBufferGetSampleAttachmentsArray(sb.0 as CMSampleBufferRef, 1);
        if !arr.is_null() && CFArrayGetCount(arr) > 0 {
            let d = CFArrayGetValueAtIndex(arr, 0) as CFMutableDictionaryRef;
            CFDictionarySetValue(d, kCMSampleAttachmentKey_DisplayImmediately, kCFBooleanTrue);
            if !key {
                CFDictionarySetValue(d, kCMSampleAttachmentKey_NotSync, kCFBooleanTrue);
            }
        }
        Some(sb)
    }
}

// ------------------------------------------------------------- main thread

struct Window {
    window: Retained<NSWindow>,
    _delegate: Retained<WindowDelegate>,
    sized: bool,
}

thread_local! {
    static WINDOWS: RefCell<HashMap<usize, Window>> = RefCell::new(HashMap::new());
}
static NEXT_ID: AtomicUsize = AtomicUsize::new(1);

define_class!(
    #[unsafe(super(NSView))]
    #[thread_kind = MainThreadOnly]
    #[name = "RelayVideoView"]
    struct VideoView;

    impl VideoView {
        #[unsafe(method(acceptsFirstResponder))]
        fn accepts_first_responder(&self) -> bool {
            true
        }

        #[unsafe(method(mouseDown:))]
        fn mouse_down(&self, event: &NSEvent) {
            if event.clickCount() == 2
                && let Some(w) = self.window()
            {
                w.toggleFullScreen(None);
            }
        }

        #[unsafe(method(keyDown:))]
        fn key_down(&self, event: &NSEvent) {
            const ESC: u16 = 53;
            if event.keyCode() == ESC {
                if let Some(w) = self.window()
                    && w.styleMask().contains(NSWindowStyleMask::FullScreen)
                {
                    w.toggleFullScreen(None);
                }
            } else {
                unsafe { msg_send![super(self), keyDown: event] }
            }
        }
    }
);

define_class!(
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[name = "RelayViewerWindowDelegate"]
    #[ivars = (Arc<Shared>, usize)]
    struct WindowDelegate;

    unsafe impl NSObjectProtocol for WindowDelegate {}

    unsafe impl NSWindowDelegate for WindowDelegate {
        #[unsafe(method(windowWillClose:))]
        fn window_will_close(&self, _n: &NSNotification) {
            let (shared, id) = self.ivars();
            shared.closed.store(true, Ordering::Relaxed);
            let id = *id;
            // Not while AppKit is still calling into this delegate.
            DispatchQueue::main().exec_async(move || forget_window(id));
        }
    }
);

impl WindowDelegate {
    fn new(mtm: MainThreadMarker, shared: Arc<Shared>, id: usize) -> Retained<Self> {
        let this = Self::alloc(mtm).set_ivars((shared, id));
        unsafe { msg_send![super(this), init] }
    }
}

fn forget_window(id: usize) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    if WINDOWS.with_borrow_mut(|w| w.remove(&id)).is_some() {
        app::window_closed(mtm);
    }
}

fn close_window(id: usize) {
    let w = WINDOWS.with_borrow(|w| w.get(&id).map(|w| w.window.clone()));
    if let Some(w) = w {
        w.close(); // -> windowWillClose -> forget_window
    }
}

/// Sizes the window to the video's aspect ratio the first time it is known.
fn fit_window(id: usize, w: i32, h: i32) {
    let Some(mtm) = MainThreadMarker::new() else { return };
    let window = WINDOWS.with_borrow_mut(|ws| {
        let win = ws.get_mut(&id)?;
        if win.sized {
            return None;
        }
        win.sized = true;
        Some(win.window.clone())
    });
    let Some(window) = window else { return };
    if w <= 0 || h <= 0 || window.styleMask().contains(NSWindowStyleMask::FullScreen) {
        return;
    }
    let screen = window.screen().or_else(|| NSScreen::mainScreen(mtm));
    let avail = screen.map(|s| s.visibleFrame().size).unwrap_or(NSSize::new(1440.0, 900.0));
    let scale = (avail.width * 0.8 / w as f64).min(avail.height * 0.8 / h as f64).min(1.0);
    window.setContentSize(NSSize::new((w as f64 * scale).round(), (h as f64 * scale).round()));
    window.center();
}

fn create(mtm: MainThreadMarker, title: &str) -> Result<Viewer> {
    let layer = unsafe { AVSampleBufferDisplayLayer::new() };
    let black = NSColor::blackColor();
    unsafe {
        if let Some(g) = AVLayerVideoGravityResizeAspect {
            layer.setVideoGravity(g);
        }
        layer.setBackgroundColor(Some(&black.CGColor()));
    }
    let renderer = unsafe { layer.sampleBufferRenderer() };
    let shared = Arc::new(Shared {
        closed: AtomicBool::new(false),
        need_key: AtomicBool::new(false),
        renderer: Renderer(renderer),
        dec: Mutex::new(Decode {
            format: None,
            sps: Vec::new(),
            pps: Vec::new(),
            size: (0, 0),
            waiting_key: true,
            last_request: None,
        }),
    });
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let rect = NSRect::new(NSPoint::new(0.0, 0.0), NSSize::new(1280.0, 720.0));
    let style = NSWindowStyleMask::Titled
        | NSWindowStyleMask::Closable
        | NSWindowStyleMask::Miniaturizable
        | NSWindowStyleMask::Resizable;
    let window = unsafe {
        NSWindow::initWithContentRect_styleMask_backing_defer(
            NSWindow::alloc(mtm),
            rect,
            style,
            NSBackingStoreType::Buffered,
            false,
        )
    };
    unsafe { window.setReleasedWhenClosed(false) };
    window.setTitle(&NSString::from_str(title));
    window.setBackgroundColor(Some(&black));
    window.setCollectionBehavior(NSWindowCollectionBehavior::FullScreenPrimary);
    let view: Retained<VideoView> = unsafe { msg_send![VideoView::alloc(mtm), initWithFrame: rect] };
    // Layer-hosting view: set the layer before turning layers on.
    view.setLayer(Some(&layer));
    view.setWantsLayer(true);
    window.setContentView(Some(&view));
    let delegate = WindowDelegate::new(mtm, shared.clone(), id);
    window.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));
    window.center();
    window.makeKeyAndOrderFront(None);
    window.makeFirstResponder(Some(&view));
    WINDOWS.with_borrow_mut(|w| w.insert(id, Window { window, _delegate: delegate, sized: false }));
    app::window_opened(mtm);
    Ok(Viewer { shared, id })
}

pub fn open(title: &str) -> Result<Viewer> {
    if let Some(mtm) = MainThreadMarker::new() {
        return create(mtm, title);
    }
    if !app::loop_running() {
        bail!("video viewer windows need the main-thread event loop (relay_video::run_app)");
    }
    let (tx, rx) = crossbeam_channel::bounded::<Result<Viewer>>(1);
    let title = title.to_string();
    DispatchQueue::main().exec_async(move || {
        let r = match MainThreadMarker::new() {
            Some(mtm) => create(mtm, &title),
            None => Err(anyhow!("not on the main thread")),
        };
        if let Err(Ok(v)) = tx.send(r).map_err(|e| e.into_inner()) {
            drop(v); // caller gave up waiting; closes the window
        }
    });
    rx.recv_timeout(Duration::from_secs(5)).map_err(|_| anyhow!("the viewer window didn't open"))?
}

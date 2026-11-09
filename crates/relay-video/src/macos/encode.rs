//! VideoToolbox H.264 encoder in real-time, low-latency mode. Output is
//! converted from AVCC to Annex B, with SPS/PPS in-band on every keyframe.

use super::ffi::*;
use crate::h264;
use anyhow::{Result, bail};
use std::ffi::c_void;
use std::ptr;

pub struct Encoded {
    pub data: Vec<u8>,
    pub key: bool,
    pub pts_us: i64,
    pub width: u32,
    pub height: u32,
}

/// Called on VideoToolbox's thread for every encoded frame.
pub type OnEncoded = Box<dyn Fn(Encoded) + Send + Sync>;

pub struct Encoder {
    session: VTCompressionSessionRef,
    // Boxed so its address is stable for the callback's refcon.
    _cb: Box<Callback>,
    pub width: u32,
    pub height: u32,
    pub hw: bool,
}

struct Callback {
    on: OnEncoded,
    width: u32,
    height: u32,
}

// VTCompressionSession is thread-safe; we serialize encode calls anyway.
unsafe impl Send for Encoder {}

unsafe fn set(session: VTCompressionSessionRef, key: CFStringRef, value: CFTypeRef, what: &str) {
    let st = unsafe { VTSessionSetProperty(session as CFTypeRef, key, value) };
    if st != 0 {
        tracing::debug!(status = st, "encoder property {what} not supported");
    }
}

impl Encoder {
    pub fn new(width: u32, height: u32, fps: u32, kbps: u32, on: OnEncoded) -> Result<Self> {
        let cb = Box::new(Callback { on, width, height });
        let mut session: VTCompressionSessionRef = ptr::null_mut();
        unsafe {
            // Hardware first (the low-latency rate controller runs on Apple's
            // media engine); fall back to whatever VideoToolbox offers.
            let mut hw = true;
            let mut st = -1;
            for require_hw in [true, false] {
                let spec = dict(&[
                    (kVTVideoEncoderSpecification_EnableLowLatencyRateControl, bool_ref(true)),
                    (kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder, bool_ref(require_hw)),
                ]);
                st = VTCompressionSessionCreate(
                    kCFAllocatorDefault,
                    width as i32,
                    height as i32,
                    kCMVideoCodecType_H264,
                    spec.0,
                    ptr::null(),
                    kCFAllocatorDefault,
                    output_callback,
                    &*cb as *const Callback as *mut c_void,
                    &mut session,
                );
                if st == 0 && !session.is_null() {
                    hw = require_hw;
                    break;
                }
                tracing::info!(status = st, require_hw, "H.264 encoder unavailable");
            }
            if st != 0 || session.is_null() {
                bail!("couldn't create the H.264 encoder (VideoToolbox error {st})");
            }
            set(session, kVTCompressionPropertyKey_RealTime, bool_ref(true), "RealTime");
            set(session, kVTCompressionPropertyKey_AllowFrameReordering, bool_ref(false), "AllowFrameReordering");
            set(
                session,
                kVTCompressionPropertyKey_ProfileLevel,
                kVTProfileLevel_H264_ConstrainedHigh_AutoLevel,
                "Profile",
            );
            let bps = Cf::i32((kbps.saturating_mul(1000)).min(i32::MAX as u32) as i32);
            set(session, kVTCompressionPropertyKey_AverageBitRate, bps.0, "AverageBitRate");
            let fps_n = Cf::f64(fps as f64);
            set(session, kVTCompressionPropertyKey_ExpectedFrameRate, fps_n.0, "ExpectedFrameRate");
            let gop = Cf::i32((fps * 4) as i32);
            set(session, kVTCompressionPropertyKey_MaxKeyFrameInterval, gop.0, "MaxKeyFrameInterval");
            let gop_s = Cf::f64(4.0);
            set(session, kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration, gop_s.0, "MaxKeyFrameIntervalDuration");
            let st = VTCompressionSessionPrepareToEncodeFrames(session);
            if st != 0 {
                VTCompressionSessionInvalidate(session);
                CFRelease(session as CFTypeRef);
                bail!("the H.264 encoder failed to start (VideoToolbox error {st})");
            }
            tracing::info!(width, height, fps, kbps, hw, "video encoder ready");
            Ok(Self { session, _cb: cb, width, height, hw })
        }
    }

    /// Encodes one frame. Output arrives through the callback, normally
    /// before this returns or shortly after.
    pub fn encode(&mut self, pixels: CVPixelBufferRef, pts_us: i64, force_key: bool) -> Result<()> {
        let props = force_key.then(|| dict(&[(unsafe { kVTEncodeFrameOptionKey_ForceKeyFrame }, bool_ref(true))]));
        let st = unsafe {
            VTCompressionSessionEncodeFrame(
                self.session,
                pixels,
                CMTime::new(pts_us, 1_000_000),
                CMTime::INVALID,
                props.as_ref().map(|p| p.0).unwrap_or(ptr::null()),
                ptr::null_mut(),
                ptr::null_mut(),
            )
        };
        if st != 0 {
            bail!("encode failed (VideoToolbox error {st})");
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            VTCompressionSessionCompleteFrames(self.session, CMTime::INVALID);
            VTCompressionSessionInvalidate(self.session);
            CFRelease(self.session as CFTypeRef);
        }
    }
}

/// A CFDictionary with CFType keys and values (+1, released on drop).
pub fn dict(pairs: &[(CFStringRef, CFTypeRef)]) -> Cf {
    #[link(name = "CoreFoundation", kind = "framework")]
    unsafe extern "C" {
        static kCFTypeDictionaryKeyCallBacks: c_void;
        static kCFTypeDictionaryValueCallBacks: c_void;
        fn CFDictionaryCreate(
            alloc: CFAllocatorRef,
            keys: *const *const c_void,
            values: *const *const c_void,
            n: isize,
            kcb: *const c_void,
            vcb: *const c_void,
        ) -> CFDictionaryRef;
    }
    let keys: Vec<*const c_void> = pairs.iter().map(|p| p.0).collect();
    let values: Vec<*const c_void> = pairs.iter().map(|p| p.1).collect();
    unsafe {
        Cf(CFDictionaryCreate(
            kCFAllocatorDefault,
            keys.as_ptr(),
            values.as_ptr(),
            pairs.len() as isize,
            &kCFTypeDictionaryKeyCallBacks,
            &kCFTypeDictionaryValueCallBacks,
        ))
    }
}

/// True unless the sample is marked NotSync.
pub unsafe fn is_sync(sbuf: CMSampleBufferRef) -> bool {
    unsafe {
        let arr = CMSampleBufferGetSampleAttachmentsArray(sbuf, 0);
        if arr.is_null() || CFArrayGetCount(arr) == 0 {
            return true;
        }
        let d = CFArrayGetValueAtIndex(arr, 0);
        let v = CFDictionaryGetValue(d, kCMSampleAttachmentKey_NotSync);
        v.is_null() || CFBooleanGetValue(v) == 0
    }
}

/// Annex B access unit from an encoded sample buffer; SPS/PPS prepended on
/// keyframes.
pub unsafe fn sample_to_annexb(sbuf: CMSampleBufferRef, key: bool) -> Option<Vec<u8>> {
    unsafe {
        let block = CMSampleBufferGetDataBuffer(sbuf);
        if block.is_null() {
            return None;
        }
        let len = CMBlockBufferGetDataLength(block);
        let mut avcc = vec![0u8; len];
        if CMBlockBufferCopyDataBytes(block, 0, len, avcc.as_mut_ptr() as *mut c_void) != 0 {
            return None;
        }
        let fmt = CMSampleBufferGetFormatDescription(sbuf);
        let mut params: Vec<&[u8]> = Vec::new();
        let mut nal_len = 4i32;
        if !fmt.is_null() {
            let mut count = 0usize;
            let (mut p, mut sz) = (ptr::null(), 0usize);
            if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(fmt, 0, &mut p, &mut sz, &mut count, &mut nal_len)
                == 0
                && key
            {
                for i in 0..count {
                    if CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
                        fmt,
                        i,
                        &mut p,
                        &mut sz,
                        ptr::null_mut(),
                        ptr::null_mut(),
                    ) == 0
                    {
                        params.push(std::slice::from_raw_parts(p, sz));
                    }
                }
            }
        }
        h264::avcc_to_annexb(&avcc, nal_len as usize, &params)
    }
}

unsafe extern "C" fn output_callback(
    refcon: *mut c_void,
    _frame_refcon: *mut c_void,
    status: OSStatus,
    flags: u32,
    sbuf: CMSampleBufferRef,
) {
    if status != 0 || sbuf.is_null() || flags & kVTEncodeInfo_FrameDropped != 0 {
        if status != 0 {
            tracing::debug!(status, "encoder dropped a frame");
        }
        return;
    }
    let cb = unsafe { &*(refcon as *const Callback) };
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe {
        if CMSampleBufferDataIsReady(sbuf) == 0 {
            return;
        }
        let key = is_sync(sbuf);
        let Some(data) = sample_to_annexb(sbuf, key) else { return };
        let pts_us = CMSampleBufferGetPresentationTimeStamp(sbuf).as_micros();
        (cb.on)(Encoded { data, key, pts_us, width: cb.width, height: cb.height });
    }));
    if r.is_err() {
        tracing::error!("panic in video encoder callback");
    }
}

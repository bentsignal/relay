//! Minimal raw bindings to the C frameworks (CoreFoundation, CoreMedia,
//! CoreVideo, VideoToolbox, CoreGraphics). Objective-C APIs go through the
//! objc2 crates; these C calls are small and stable, so declaring them here
//! keeps the signatures explicit.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::c_void;

pub type OSStatus = i32;
pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFDictionaryRef = *const c_void;
pub type CFMutableDictionaryRef = *mut c_void;
pub type CFArrayRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type CMSampleBufferRef = *mut c_void;
pub type CMBlockBufferRef = *mut c_void;
pub type CMFormatDescriptionRef = *const c_void;
pub type CVPixelBufferRef = *mut c_void;
pub type VTCompressionSessionRef = *mut c_void;
pub type CGDirectDisplayID = u32;
pub type CGDisplayModeRef = *const c_void;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct CMTime {
    pub value: i64,
    pub timescale: i32,
    pub flags: u32,
    pub epoch: i64,
}

impl CMTime {
    pub const INVALID: CMTime = CMTime { value: 0, timescale: 0, flags: 0, epoch: 0 };
    pub fn new(value: i64, timescale: i32) -> Self {
        CMTime { value, timescale, flags: 1, epoch: 0 }
    }
    pub fn is_valid(&self) -> bool {
        self.flags & 1 != 0 && self.timescale > 0
    }
    pub fn as_micros(&self) -> i64 {
        if !self.is_valid() {
            return 0;
        }
        (self.value as i128 * 1_000_000 / self.timescale as i128) as i64
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CMSampleTimingInfo {
    pub duration: CMTime,
    pub presentation_time_stamp: CMTime,
    pub decode_time_stamp: CMTime,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct CMVideoDimensions {
    pub width: i32,
    pub height: i32,
}

pub type VTCompressionOutputCallback = unsafe extern "C" fn(
    output_ref_con: *mut c_void,
    source_frame_ref_con: *mut c_void,
    status: OSStatus,
    info_flags: u32,
    sample_buffer: CMSampleBufferRef,
);

pub const kCMVideoCodecType_H264: u32 = u32::from_be_bytes(*b"avc1");
pub const kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange: u32 = u32::from_be_bytes(*b"420v");
pub const kVTEncodeInfo_FrameDropped: u32 = 1 << 1;
pub const kCMBlockBufferAssureMemoryNowFlag: u32 = 1;
pub const kCFNumberSInt32Type: isize = 3;
pub const kCFNumberFloat64Type: isize = 6;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub static kCFAllocatorDefault: CFAllocatorRef;
    pub static kCFBooleanTrue: CFTypeRef;
    pub static kCFBooleanFalse: CFTypeRef;
    pub fn CFRetain(cf: CFTypeRef) -> CFTypeRef;
    pub fn CFRelease(cf: CFTypeRef);
    pub fn CFArrayGetCount(a: CFArrayRef) -> isize;
    pub fn CFArrayGetValueAtIndex(a: CFArrayRef, i: isize) -> *const c_void;
    pub fn CFDictionaryGetValue(d: CFDictionaryRef, key: *const c_void) -> *const c_void;
    pub fn CFDictionarySetValue(d: CFMutableDictionaryRef, key: *const c_void, value: *const c_void);
    pub fn CFBooleanGetValue(b: CFTypeRef) -> u8;
    pub fn CFNumberCreate(alloc: CFAllocatorRef, ty: isize, value: *const c_void) -> CFTypeRef;
    pub fn CFNumberGetValue(n: CFTypeRef, ty: isize, value: *mut c_void) -> u8;
}

#[link(name = "CoreMedia", kind = "framework")]
unsafe extern "C" {
    pub static kCMSampleAttachmentKey_NotSync: CFStringRef;
    pub static kCMSampleAttachmentKey_DisplayImmediately: CFStringRef;
    pub fn CMSampleBufferIsValid(sbuf: CMSampleBufferRef) -> u8;
    pub fn CMSampleBufferDataIsReady(sbuf: CMSampleBufferRef) -> u8;
    pub fn CMSampleBufferGetImageBuffer(sbuf: CMSampleBufferRef) -> CVPixelBufferRef;
    pub fn CMSampleBufferGetDataBuffer(sbuf: CMSampleBufferRef) -> CMBlockBufferRef;
    pub fn CMSampleBufferGetFormatDescription(sbuf: CMSampleBufferRef) -> CMFormatDescriptionRef;
    pub fn CMSampleBufferGetPresentationTimeStamp(sbuf: CMSampleBufferRef) -> CMTime;
    pub fn CMSampleBufferGetSampleAttachmentsArray(sbuf: CMSampleBufferRef, create: u8) -> CFArrayRef;
    pub fn CMSampleBufferCreateReady(
        alloc: CFAllocatorRef,
        data_buffer: CMBlockBufferRef,
        format: CMFormatDescriptionRef,
        num_samples: isize,
        num_timing: isize,
        timing: *const CMSampleTimingInfo,
        num_sizes: isize,
        sizes: *const usize,
        out: *mut CMSampleBufferRef,
    ) -> OSStatus;
    pub fn CMBlockBufferGetDataLength(b: CMBlockBufferRef) -> usize;
    pub fn CMBlockBufferCopyDataBytes(b: CMBlockBufferRef, offset: usize, len: usize, dst: *mut c_void) -> OSStatus;
    pub fn CMBlockBufferCreateWithMemoryBlock(
        alloc: CFAllocatorRef,
        memory_block: *mut c_void,
        block_length: usize,
        block_allocator: CFAllocatorRef,
        custom_block_source: *const c_void,
        offset_to_data: usize,
        data_length: usize,
        flags: u32,
        out: *mut CMBlockBufferRef,
    ) -> OSStatus;
    pub fn CMBlockBufferReplaceDataBytes(
        src: *const c_void,
        dst: CMBlockBufferRef,
        offset: usize,
        len: usize,
    ) -> OSStatus;
    pub fn CMVideoFormatDescriptionGetH264ParameterSetAtIndex(
        desc: CMFormatDescriptionRef,
        index: usize,
        ptr_out: *mut *const u8,
        size_out: *mut usize,
        count_out: *mut usize,
        nal_header_len_out: *mut i32,
    ) -> OSStatus;
    pub fn CMVideoFormatDescriptionCreateFromH264ParameterSets(
        alloc: CFAllocatorRef,
        count: usize,
        pointers: *const *const u8,
        sizes: *const usize,
        nal_header_len: i32,
        out: *mut CMFormatDescriptionRef,
    ) -> OSStatus;
    pub fn CMVideoFormatDescriptionGetDimensions(desc: CMFormatDescriptionRef) -> CMVideoDimensions;
}

#[link(name = "CoreVideo", kind = "framework")]
unsafe extern "C" {
    pub static kCVPixelBufferPixelFormatTypeKey: CFStringRef;
    pub fn CVPixelBufferGetWidth(pb: CVPixelBufferRef) -> usize;
    pub fn CVPixelBufferGetHeight(pb: CVPixelBufferRef) -> usize;
}

#[link(name = "VideoToolbox", kind = "framework")]
unsafe extern "C" {
    pub static kVTVideoEncoderSpecification_EnableLowLatencyRateControl: CFStringRef;
    pub static kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder: CFStringRef;
    pub static kVTCompressionPropertyKey_RealTime: CFStringRef;
    pub static kVTCompressionPropertyKey_AllowFrameReordering: CFStringRef;
    pub static kVTCompressionPropertyKey_AverageBitRate: CFStringRef;
    pub static kVTCompressionPropertyKey_DataRateLimits: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxKeyFrameInterval: CFStringRef;
    pub static kVTCompressionPropertyKey_MaxKeyFrameIntervalDuration: CFStringRef;
    pub static kVTCompressionPropertyKey_ExpectedFrameRate: CFStringRef;
    pub static kVTCompressionPropertyKey_ProfileLevel: CFStringRef;
    pub static kVTProfileLevel_H264_ConstrainedHigh_AutoLevel: CFStringRef;
    pub static kVTProfileLevel_H264_High_AutoLevel: CFStringRef;
    pub static kVTEncodeFrameOptionKey_ForceKeyFrame: CFStringRef;
    pub fn VTCompressionSessionCreate(
        alloc: CFAllocatorRef,
        width: i32,
        height: i32,
        codec: u32,
        encoder_specification: CFDictionaryRef,
        source_image_buffer_attributes: CFDictionaryRef,
        compressed_data_allocator: CFAllocatorRef,
        output_callback: VTCompressionOutputCallback,
        output_ref_con: *mut c_void,
        out: *mut VTCompressionSessionRef,
    ) -> OSStatus;
    pub fn VTSessionSetProperty(session: CFTypeRef, key: CFStringRef, value: CFTypeRef) -> OSStatus;
    pub fn VTSessionCopyProperty(
        session: CFTypeRef,
        key: CFStringRef,
        alloc: CFAllocatorRef,
        value_out: *mut CFTypeRef,
    ) -> OSStatus;
    pub fn VTCompressionSessionPrepareToEncodeFrames(session: VTCompressionSessionRef) -> OSStatus;
    pub fn VTCompressionSessionEncodeFrame(
        session: VTCompressionSessionRef,
        image_buffer: CVPixelBufferRef,
        pts: CMTime,
        duration: CMTime,
        frame_properties: CFDictionaryRef,
        source_frame_ref_con: *mut c_void,
        info_flags_out: *mut u32,
    ) -> OSStatus;
    pub fn VTCompressionSessionCompleteFrames(session: VTCompressionSessionRef, until: CMTime) -> OSStatus;
    pub fn VTCompressionSessionInvalidate(session: VTCompressionSessionRef);
}

#[link(name = "CoreGraphics", kind = "framework")]
unsafe extern "C" {
    pub static kCGDisplayStreamYCbCrMatrix_ITU_R_709_2: CFStringRef;
    pub fn CGPreflightScreenCaptureAccess() -> bool;
    pub fn CGRequestScreenCaptureAccess() -> bool;
    pub fn CGGetActiveDisplayList(max: u32, displays: *mut CGDirectDisplayID, count: *mut u32) -> i32;
    pub fn CGMainDisplayID() -> CGDirectDisplayID;
    pub fn CGDisplayCopyDisplayMode(display: CGDirectDisplayID) -> CGDisplayModeRef;
    pub fn CGDisplayModeGetPixelWidth(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeGetPixelHeight(mode: CGDisplayModeRef) -> usize;
    pub fn CGDisplayModeRelease(mode: CGDisplayModeRef);
    pub fn CGDisplayIsBuiltin(display: CGDirectDisplayID) -> u32;
}

/// An owned CoreFoundation reference, released on drop.
pub struct Cf(pub CFTypeRef);

impl Cf {
    /// Takes ownership of a +1 reference. Returns `None` for null.
    pub fn owned(p: CFTypeRef) -> Option<Self> {
        (!p.is_null()).then_some(Cf(p))
    }
    /// Retains a +0 reference.
    pub fn retained(p: CFTypeRef) -> Option<Self> {
        (!p.is_null()).then(|| Cf(unsafe { CFRetain(p) }))
    }
    pub fn i32(v: i32) -> Self {
        Cf(unsafe { CFNumberCreate(kCFAllocatorDefault, kCFNumberSInt32Type, &v as *const i32 as *const c_void) })
    }
    pub fn f64(v: f64) -> Self {
        Cf(unsafe { CFNumberCreate(kCFAllocatorDefault, kCFNumberFloat64Type, &v as *const f64 as *const c_void) })
    }
}

impl Drop for Cf {
    fn drop(&mut self) {
        unsafe { CFRelease(self.0) }
    }
}

// CoreFoundation objects are thread-safe to retain/release and to read.
unsafe impl Send for Cf {}
unsafe impl Sync for Cf {}

pub fn bool_ref(b: bool) -> CFTypeRef {
    unsafe { if b { kCFBooleanTrue } else { kCFBooleanFalse } }
}

/// Native pixel size of a display's current mode.
pub fn display_pixels(id: CGDirectDisplayID) -> Option<(u32, u32)> {
    unsafe {
        let mode = CGDisplayCopyDisplayMode(id);
        if mode.is_null() {
            return None;
        }
        let size = (CGDisplayModeGetPixelWidth(mode) as u32, CGDisplayModeGetPixelHeight(mode) as u32);
        CGDisplayModeRelease(mode);
        Some(size)
    }
}

pub fn active_displays() -> Vec<CGDirectDisplayID> {
    let mut ids = [0u32; 16];
    let mut n = 0u32;
    if unsafe { CGGetActiveDisplayList(ids.len() as u32, ids.as_mut_ptr(), &mut n) } != 0 {
        return Vec::new();
    }
    ids[..n as usize].to_vec()
}

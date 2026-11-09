//! CoreAudio pieces cpal doesn't cover: finding devices (including hidden
//! ones) by UID, duplex IOProcs, and the system default devices.

use anyhow::{Result, bail};
use objc2_core_audio::{
    AudioDeviceCreateIOProcID, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID, AudioDeviceStart, AudioDeviceStop,
    AudioObjectGetPropertyData, AudioObjectID, AudioObjectPropertyAddress, AudioObjectSetPropertyData,
    kAudioDevicePropertyBufferFrameSize, kAudioDevicePropertyDeviceUID, kAudioHardwarePropertyDefaultInputDevice,
    kAudioHardwarePropertyDefaultOutputDevice, kAudioHardwarePropertyTranslateUIDToDevice,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyScopeGlobal, kAudioObjectSystemObject, kAudioObjectUnknown,
};
use objc2_core_audio_types::{AudioBufferList, AudioTimeStamp};
use objc2_core_foundation::{CFRetained, CFString};
use std::ffi::c_void;
use std::ptr::NonNull;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

const SYSTEM: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;

fn global(selector: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: kAudioObjectPropertyScopeGlobal,
        mElement: kAudioObjectPropertyElementMain,
    }
}

fn check(status: i32, what: &str) -> Result<()> {
    if status != 0 {
        let code = status as u32;
        let fourcc = code.to_be_bytes();
        if fourcc.iter().all(|b| b.is_ascii_graphic() || *b == b' ') {
            bail!("{what} failed ('{}')", String::from_utf8_lossy(&fourcc));
        }
        bail!("{what} failed ({status})");
    }
    Ok(())
}

/// The UID part of a cpal device key ("coreaudio:<uid>").
pub fn uid_from_key(key: &str) -> &str {
    key.strip_prefix("coreaudio:").unwrap_or(key)
}

/// Finds a device by UID. Works for hidden devices too.
pub fn device_by_uid(uid: &str) -> Option<AudioObjectID> {
    let cf = CFString::from_str(uid);
    let qualifier: *const CFString = &*cf;
    let mut id: AudioObjectID = kAudioObjectUnknown;
    let mut size = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM,
            NonNull::from(&global(kAudioHardwarePropertyTranslateUIDToDevice)),
            size_of::<*const CFString>() as u32,
            (&qualifier as *const *const CFString).cast(),
            NonNull::from(&mut size),
            NonNull::from(&mut id).cast(),
        )
    };
    (status == 0 && id != kAudioObjectUnknown).then_some(id)
}

pub fn device_uid(id: AudioObjectID) -> Option<String> {
    let mut uid: *mut CFString = std::ptr::null_mut();
    let mut size = size_of::<*mut CFString>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            id,
            NonNull::from(&global(kAudioDevicePropertyDeviceUID)),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut uid).cast(),
        )
    };
    if status != 0 {
        return None;
    }
    // Returned under the create rule.
    NonNull::new(uid).map(|p| unsafe { CFRetained::from_raw(p) }.to_string())
}

fn default_selectors(input: bool) -> &'static [u32] {
    if input { &[kAudioHardwarePropertyDefaultInputDevice] } else { &[kAudioHardwarePropertyDefaultOutputDevice] }
}

/// UID of the current default input or output device.
pub fn default_device_uid(input: bool) -> Option<String> {
    let mut id: AudioObjectID = kAudioObjectUnknown;
    let mut size = size_of::<AudioObjectID>() as u32;
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM,
            NonNull::from(&global(default_selectors(input)[0])),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::from(&mut id).cast(),
        )
    };
    (status == 0 && id != kAudioObjectUnknown).then(|| device_uid(id)).flatten()
}

/// Makes the device with `uid` the default input or output. The alert-sound
/// device is left alone.
pub fn set_default_device(uid: &str, input: bool) -> Result<()> {
    let Some(id) = device_by_uid(uid) else { bail!("device {uid} not found") };
    for &sel in default_selectors(input) {
        let status = unsafe {
            AudioObjectSetPropertyData(
                SYSTEM,
                NonNull::from(&global(sel)),
                0,
                std::ptr::null(),
                size_of::<AudioObjectID>() as u32,
                NonNull::from(&id).cast(),
            )
        };
        check(status, "setting the default device")?;
    }
    Ok(())
}

/// Interleaved device buffers handed to a [`DuplexStream`] callback.
pub struct IoBuffers<'a> {
    pub input: &'a [f32],
    pub input_channels: usize,
    pub output: &'a mut [f32],
    pub output_channels: usize,
}

type Callback = Box<dyn FnMut(IoBuffers<'_>) + Send>;

struct Context {
    callback: Callback,
    cycles: Arc<AtomicU64>,
}

/// An IOProc running on a CoreAudio device. Dropping it stops IO.
pub struct DuplexStream {
    device: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    context: *mut Context,
    cycles: Arc<AtomicU64>,
}

// The context is only touched by the IO thread while running, and freed after IO stops.
unsafe impl Send for DuplexStream {}

impl DuplexStream {
    /// Starts IO on `device`. The callback runs on the device's realtime thread
    /// and sees the first buffer of each direction (empty if there is none).
    pub fn open(
        device: AudioObjectID,
        buffer_frames: Option<u32>,
        callback: impl FnMut(IoBuffers<'_>) + Send + 'static,
    ) -> Result<Self> {
        if let Some(frames) = buffer_frames {
            // Best effort: the device keeps its own size if it refuses.
            unsafe {
                AudioObjectSetPropertyData(
                    device,
                    NonNull::from(&global(kAudioDevicePropertyBufferFrameSize)),
                    0,
                    std::ptr::null(),
                    4,
                    NonNull::from(&frames).cast(),
                );
            }
        }
        let cycles = Arc::new(AtomicU64::new(0));
        let context = Box::into_raw(Box::new(Context { callback: Box::new(callback), cycles: cycles.clone() }));
        let mut proc_id: AudioDeviceIOProcID = None;
        let status =
            unsafe { AudioDeviceCreateIOProcID(device, Some(io_proc), context.cast(), NonNull::from(&mut proc_id)) };
        if let Err(e) = check(status, "creating the IO callback") {
            drop(unsafe { Box::from_raw(context) });
            return Err(e);
        }
        let status = unsafe { AudioDeviceStart(device, proc_id) };
        if let Err(e) = check(status, "starting the device") {
            unsafe {
                AudioDeviceDestroyIOProcID(device, proc_id);
                drop(Box::from_raw(context));
            }
            return Err(e);
        }
        Ok(Self { device, proc_id, context, cycles })
    }

    /// IO cycles completed so far; stops advancing when the device dies.
    pub fn cycles(&self) -> u64 {
        self.cycles.load(Ordering::Relaxed)
    }
}

impl Drop for DuplexStream {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.device, self.proc_id);
            AudioDeviceDestroyIOProcID(self.device, self.proc_id);
            drop(Box::from_raw(self.context));
        }
    }
}

unsafe fn first_buffer<'a>(list: NonNull<AudioBufferList>) -> Option<(&'a mut [f32], usize)> {
    let list = unsafe { list.as_ref() };
    if list.mNumberBuffers == 0 {
        return None;
    }
    let b = &list.mBuffers[0];
    if b.mData.is_null() || b.mNumberChannels == 0 {
        return None;
    }
    let len = b.mDataByteSize as usize / size_of::<f32>();
    Some((unsafe { std::slice::from_raw_parts_mut(b.mData as *mut f32, len) }, b.mNumberChannels as usize))
}

unsafe extern "C-unwind" fn io_proc(
    _device: AudioObjectID,
    _now: NonNull<AudioTimeStamp>,
    input: NonNull<AudioBufferList>,
    _input_time: NonNull<AudioTimeStamp>,
    output: NonNull<AudioBufferList>,
    _output_time: NonNull<AudioTimeStamp>,
    client: *mut c_void,
) -> i32 {
    let Some(ctx) = (unsafe { (client as *mut Context).as_mut() }) else { return 0 };
    let (input, input_channels) = unsafe { first_buffer(input) }.map(|(b, c)| (&*b, c)).unwrap_or((&[], 0));
    let (output, output_channels) = unsafe { first_buffer(output) }.unwrap_or((&mut [], 0));
    let bufs = IoBuffers { input, input_channels, output, output_channels };
    // Never unwind into CoreAudio.
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| (ctx.callback)(bufs)));
    ctx.cycles.fetch_add(1, Ordering::Relaxed);
    0
}

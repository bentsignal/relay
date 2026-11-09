//! The AudioServerPlugIn driver.
//!
//! Two devices share one host-clock timeline:
//!
//! * **Relay** (visible): apps play to its output and record its input.
//! * **Relay Engine** (hidden): the Relay engine records what apps played to
//!   "Relay" from its input and feeds the "Relay" microphone through its output.
//!
//! Audio crosses between the devices through [`TimeRing`]s indexed by sample
//! time. "Relay" is only published while the engine is doing IO on "Relay
//! Engine", so when the engine quits the device disappears and macOS falls
//! back to the built-in hardware.

#![allow(non_upper_case_globals)]

use crate::ffi::*;
use crate::ring::TimeRing;
use std::ffi::c_void;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

pub const SAMPLE_RATE: f64 = 48_000.0;
pub const CHANNELS: u32 = 2;
/// Frames between zero timestamps.
const ZERO_TS_PERIOD: u32 = 16_384;
/// Frames held by each loop ring (~680 ms).
const RING_FRAMES: usize = 32_768;
/// How long engine IO may go quiet before "Relay" is withdrawn.
const ENGINE_TIMEOUT: Duration = Duration::from_secs(1);
const MONITOR_INTERVAL: Duration = Duration::from_millis(100);
const CLOCK_DOMAIN: u32 = fourcc(b"rely");

const MANUFACTURER: &str = "Relay";
const MODEL_UID: &str = "relay.model.virtual";

pub const PLUGIN: AudioObjectID = kAudioObjectPlugInObject;
pub const MAIN: AudioObjectID = 2;
pub const MAIN_IN: AudioObjectID = 3;
pub const MAIN_OUT: AudioObjectID = 4;
pub const ENGINE: AudioObjectID = 5;
pub const ENGINE_IN: AudioObjectID = 6;
pub const ENGINE_OUT: AudioObjectID = 7;
/// Output volume and mute of "Relay", so the keyboard volume keys work.
pub const VOLUME: AudioObjectID = 8;
pub const MUTE: AudioObjectID = 9;
const MIN_DB: f32 = -64.0;

/// Volume curve: gain = scalar², i.e. dB = 40·log10(scalar), floored at `MIN_DB`.
pub fn scalar_to_db(scalar: f32) -> f32 {
    let s = scalar.clamp(0.0, 1.0);
    if s <= 0.0 { MIN_DB } else { (40.0 * s.log10()).max(MIN_DB) }
}

pub fn db_to_scalar(db: f32) -> f32 {
    let db = db.clamp(MIN_DB, 0.0);
    if db <= MIN_DB { 0.0 } else { 10f32.powf(db / 40.0) }
}

pub struct Device {
    pub id: AudioObjectID,
    pub input: AudioObjectID,
    pub output: AudioObjectID,
    pub name: &'static str,
    pub uid: &'static str,
    pub hidden: bool,
    pub input_terminal: u32,
    pub output_terminal: u32,
}

pub const MAIN_DEVICE: Device = Device {
    id: MAIN,
    input: MAIN_IN,
    output: MAIN_OUT,
    name: "Relay",
    uid: "relay.device.main",
    hidden: false,
    input_terminal: kAudioStreamTerminalTypeMicrophone,
    output_terminal: kAudioStreamTerminalTypeSpeaker,
};

pub const ENGINE_DEVICE: Device = Device {
    id: ENGINE,
    input: ENGINE_IN,
    output: ENGINE_OUT,
    name: "Relay Engine",
    uid: "relay.device.engine",
    hidden: true,
    input_terminal: kAudioStreamTerminalTypeLine,
    output_terminal: kAudioStreamTerminalTypeLine,
};

fn device(id: AudioObjectID) -> Option<&'static Device> {
    match id {
        MAIN => Some(&MAIN_DEVICE),
        ENGINE => Some(&ENGINE_DEVICE),
        _ => None,
    }
}

/// The stream's device and whether it is the input stream.
fn stream(id: AudioObjectID) -> Option<(&'static Device, bool)> {
    [&MAIN_DEVICE, &ENGINE_DEVICE].into_iter().find_map(|d| {
        if d.input == id {
            Some((d, true))
        } else if d.output == id {
            Some((d, false))
        } else {
            None
        }
    })
}

pub fn format() -> AudioStreamBasicDescription {
    AudioStreamBasicDescription {
        mSampleRate: SAMPLE_RATE,
        mFormatID: kAudioFormatLinearPCM,
        mFormatFlags: kAudioFormatFlagsNativeFloatPacked,
        mBytesPerPacket: 4 * CHANNELS,
        mFramesPerPacket: 1,
        mBytesPerFrame: 4 * CHANNELS,
        mChannelsPerFrame: CHANNELS,
        mBitsPerChannel: 32,
        mReserved: 0,
    }
}

fn format_ok(f: &AudioStreamBasicDescription) -> bool {
    let ours = format();
    f.mSampleRate == ours.mSampleRate
        && f.mFormatID == ours.mFormatID
        && f.mChannelsPerFrame == ours.mChannelsPerFrame
        && f.mBitsPerChannel == ours.mBitsPerChannel
        && f.mFormatFlags & kAudioFormatFlagIsFloat != 0
}

// ---------------------------------------------------------------- State

pub struct State {
    host: AtomicPtr<AudioServerPlugInHostInterface>,
    refs: AtomicU32,
    /// "Relay" output -> "Relay Engine" input.
    to_engine: TimeRing,
    /// "Relay Engine" output -> "Relay" input.
    to_main: TimeRing,
    anchor: u64,
    ticks_per_frame: f64,
    ticks_per_sec: f64,
    main_io: AtomicU32,
    engine_io: AtomicU32,
    engine_last_io: AtomicU64,
    published: AtomicBool,
    monitor: AtomicBool,
    /// Volume scalar (f32 bits) of "Relay"'s output.
    volume: AtomicU32,
    mute: AtomicBool,
}

impl State {
    fn new() -> Self {
        let mut tb = MachTimebaseInfo { numer: 0, denom: 0 };
        let ok = unsafe { mach_timebase_info(&mut tb) } == 0 && tb.numer != 0;
        let ticks_per_sec = if ok { 1e9 * tb.denom as f64 / tb.numer as f64 } else { 1e9 };
        Self {
            host: AtomicPtr::new(std::ptr::null_mut()),
            refs: AtomicU32::new(0),
            to_engine: TimeRing::new(RING_FRAMES),
            to_main: TimeRing::new(RING_FRAMES),
            anchor: unsafe { mach_absolute_time() },
            ticks_per_frame: ticks_per_sec / SAMPLE_RATE,
            ticks_per_sec,
            main_io: AtomicU32::new(0),
            engine_io: AtomicU32::new(0),
            engine_last_io: AtomicU64::new(0),
            published: AtomicBool::new(false),
            monitor: AtomicBool::new(false),
            volume: AtomicU32::new(1f32.to_bits()),
            mute: AtomicBool::new(false),
        }
    }

    fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::Relaxed))
    }

    /// Linear gain applied to what apps play to "Relay".
    fn output_gain(&self) -> f32 {
        if self.mute.load(Ordering::Relaxed) {
            return 0.0;
        }
        let s = self.volume();
        if s <= 0.0 { 0.0 } else { s * s }
    }

    fn notify(&self, obj: AudioObjectID, selectors: &[u32]) {
        let host = self.host.load(Ordering::Acquire);
        if host.is_null() {
            return;
        }
        let addrs: Vec<_> = selectors.iter().map(|&s| address(s, kAudioObjectPropertyScopeGlobal)).collect();
        unsafe { ((*host).PropertiesChanged)(host, obj, addrs.len() as u32, addrs.as_ptr()) };
    }

    fn published(&self) -> bool {
        self.published.load(Ordering::Acquire)
    }

    fn devices(&self) -> Vec<AudioObjectID> {
        if self.published() { vec![MAIN, ENGINE] } else { vec![ENGINE] }
    }

    fn io_count(&self, dev: AudioObjectID) -> Option<&AtomicU32> {
        match dev {
            MAIN => Some(&self.main_io),
            ENGINE => Some(&self.engine_io),
            _ => None,
        }
    }

    /// The latest zero timestamp at or before `now`, shared by both devices.
    fn zero_timestamp(&self, now: u64) -> (f64, u64) {
        let period_ticks = self.ticks_per_frame * ZERO_TS_PERIOD as f64;
        let n = (now.saturating_sub(self.anchor) as f64 / period_ticks).floor();
        (n * ZERO_TS_PERIOD as f64, self.anchor + (n * period_ticks) as u64)
    }

    fn engine_alive(&self, now: u64) -> bool {
        if self.engine_io.load(Ordering::Acquire) == 0 {
            return false;
        }
        let last = self.engine_last_io.load(Ordering::Relaxed);
        (now.saturating_sub(last) as f64) < ENGINE_TIMEOUT.as_secs_f64() * self.ticks_per_sec
    }

    /// Publishes or withdraws "Relay" to match the engine. Returns true on a change.
    fn sync_published(&self) -> bool {
        let want = self.engine_alive(unsafe { mach_absolute_time() });
        if self.published.swap(want, Ordering::AcqRel) == want {
            return false;
        }
        log(if want { "engine running: publishing Relay" } else { "engine stopped: withdrawing Relay" });
        self.notify(PLUGIN, &[kAudioPlugInPropertyDeviceList, kAudioObjectPropertyOwnedObjects]);
        true
    }

    fn start_monitor(&'static self) {
        if self.monitor.swap(true, Ordering::AcqRel) {
            return;
        }
        let spawned = std::thread::Builder::new().name("relay-hal-monitor".into()).spawn(move || {
            loop {
                std::thread::sleep(MONITOR_INTERVAL);
                self.sync_published();
            }
        });
        if spawned.is_err() {
            self.monitor.store(false, Ordering::Release);
        }
    }
}

fn address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress { mSelector: selector, mScope: scope, mElement: kAudioObjectPropertyElementMain }
}

static STATE: OnceLock<State> = OnceLock::new();

fn state() -> &'static State {
    STATE.get_or_init(State::new)
}

fn log(msg: &str) {
    if let Ok(c) = std::ffi::CString::new(format!("RelayAudio: {msg}")) {
        unsafe { libc::syslog(libc::LOG_NOTICE, c"%s".as_ptr(), c.as_ptr()) };
    }
}

// ---------------------------------------------------------------- Properties

pub enum Value {
    U32(u32),
    F32(f32),
    F64(f64),
    Str(&'static str),
    U32s(Vec<u32>),
    Format(AudioStreamBasicDescription),
    Formats(Vec<AudioStreamRangedDescription>),
    Ranges(Vec<AudioValueRange>),
}

impl Value {
    /// Size of the whole value; arrays may be truncated to fit the caller's buffer.
    fn size(&self) -> usize {
        use std::mem::size_of;
        match self {
            Value::U32(_) | Value::F32(_) => 4,
            Value::F64(_) => 8,
            Value::Str(_) => size_of::<CFStringRef>(),
            Value::U32s(v) => v.len() * 4,
            Value::Format(_) => size_of::<AudioStreamBasicDescription>(),
            Value::Formats(v) => v.len() * size_of::<AudioStreamRangedDescription>(),
            Value::Ranges(v) => v.len() * size_of::<AudioValueRange>(),
        }
    }

    /// Writes into `out` (capacity `cap` bytes); returns bytes written.
    unsafe fn write(&self, out: *mut c_void, cap: usize) -> Result<usize, OSStatus> {
        unsafe fn array<T: Copy>(v: &[T], out: *mut c_void, cap: usize) -> usize {
            let n = v.len().min(cap / std::mem::size_of::<T>());
            unsafe { std::ptr::copy_nonoverlapping(v.as_ptr(), out as *mut T, n) };
            n * std::mem::size_of::<T>()
        }
        unsafe fn scalar<T: Copy>(v: T, out: *mut c_void, cap: usize) -> Result<usize, OSStatus> {
            if cap < std::mem::size_of::<T>() {
                return Err(kAudioHardwareBadPropertySizeError);
            }
            unsafe { (out as *mut T).write_unaligned(v) };
            Ok(std::mem::size_of::<T>())
        }
        unsafe {
            match self {
                Value::U32(v) => scalar(*v, out, cap),
                Value::F32(v) => scalar(*v, out, cap),
                Value::F64(v) => scalar(*v, out, cap),
                Value::Format(v) => scalar(*v, out, cap),
                Value::Str(s) => {
                    if cap < std::mem::size_of::<CFStringRef>() {
                        return Err(kAudioHardwareBadPropertySizeError);
                    }
                    let cf = CFStringCreateWithBytes(
                        std::ptr::null(),
                        s.as_ptr(),
                        s.len() as isize,
                        kCFStringEncodingUTF8,
                        0,
                    );
                    if cf.is_null() {
                        return Err(kAudioHardwareUnspecifiedError);
                    }
                    scalar(cf, out, cap)
                }
                Value::U32s(v) => Ok(array(v, out, cap)),
                Value::Formats(v) => Ok(array(v, out, cap)),
                Value::Ranges(v) => Ok(array(v, out, cap)),
            }
        }
    }
}

/// Resolves a property. `qualifier` is only consulted by properties that take one.
pub fn property(
    st: &State,
    obj: AudioObjectID,
    addr: &AudioObjectPropertyAddress,
    qualifier: Option<&[u8]>,
) -> Result<Value, OSStatus> {
    use Value::*;
    let unknown = Err(kAudioHardwareUnknownPropertyError);
    let sel = addr.mSelector;
    if obj == PLUGIN {
        return match sel {
            kAudioObjectPropertyBaseClass => Ok(U32(kAudioObjectClassID)),
            kAudioObjectPropertyClass => Ok(U32(kAudioPlugInClassID)),
            kAudioObjectPropertyOwner => Ok(U32(kAudioObjectUnknown)),
            kAudioObjectPropertyManufacturer => Ok(Str(MANUFACTURER)),
            kAudioObjectPropertyOwnedObjects | kAudioPlugInPropertyDeviceList => Ok(U32s(st.devices())),
            kAudioPlugInPropertyTranslateUIDToDevice => {
                let uid = qualifier.and_then(cfstring_qualifier);
                let id = [&MAIN_DEVICE, &ENGINE_DEVICE]
                    .into_iter()
                    .find(|d| uid.as_deref() == Some(d.uid) && (d.id != MAIN || st.published()))
                    .map_or(kAudioObjectUnknown, |d| d.id);
                Ok(U32(id))
            }
            kAudioPlugInPropertyResourceBundle => Ok(Str("")),
            _ => unknown,
        };
    }
    if let Some(d) = device(obj) {
        let scope = addr.mScope;
        let streams = || -> Vec<u32> {
            match scope {
                kAudioObjectPropertyScopeInput => vec![d.input],
                kAudioObjectPropertyScopeOutput => vec![d.output],
                _ => vec![d.input, d.output],
            }
        };
        let main = d.id == MAIN;
        let controls = || -> Vec<u32> {
            match (main, scope) {
                (true, kAudioObjectPropertyScopeGlobal | kAudioObjectPropertyScopeOutput) => vec![VOLUME, MUTE],
                _ => Vec::new(),
            }
        };
        return match sel {
            kAudioObjectPropertyBaseClass => Ok(U32(kAudioObjectClassID)),
            kAudioObjectPropertyClass => Ok(U32(kAudioDeviceClassID)),
            kAudioObjectPropertyOwner => Ok(U32(PLUGIN)),
            kAudioObjectPropertyName => Ok(Str(d.name)),
            kAudioObjectPropertyManufacturer => Ok(Str(MANUFACTURER)),
            kAudioDevicePropertyStreams => Ok(U32s(streams())),
            kAudioObjectPropertyOwnedObjects => Ok(U32s([streams(), controls()].concat())),
            kAudioObjectPropertyControlList => Ok(U32s(if main { vec![VOLUME, MUTE] } else { Vec::new() })),
            kAudioDevicePropertyDeviceUID => Ok(Str(d.uid)),
            kAudioDevicePropertyModelUID => Ok(Str(MODEL_UID)),
            kAudioDevicePropertyTransportType => Ok(U32(kAudioDeviceTransportTypeVirtual)),
            kAudioDevicePropertyRelatedDevices => Ok(U32s(vec![d.id])),
            kAudioDevicePropertyClockDomain => Ok(U32(CLOCK_DOMAIN)),
            kAudioDevicePropertyDeviceIsAlive => Ok(U32((!main || st.published()) as u32)),
            kAudioDevicePropertyDeviceIsRunning => {
                Ok(U32((st.io_count(d.id).map_or(0, |c| c.load(Ordering::Relaxed)) > 0) as u32))
            }
            kAudioDevicePropertyDeviceCanBeDefaultDevice | kAudioDevicePropertyDeviceCanBeDefaultSystemDevice => {
                Ok(U32(main as u32))
            }
            kAudioDevicePropertyLatency | kAudioDevicePropertySafetyOffset => Ok(U32(0)),
            kAudioDevicePropertyNominalSampleRate => Ok(F64(SAMPLE_RATE)),
            kAudioDevicePropertyAvailableNominalSampleRates => {
                Ok(Ranges(vec![AudioValueRange { mMinimum: SAMPLE_RATE, mMaximum: SAMPLE_RATE }]))
            }
            kAudioDevicePropertyIsHidden => Ok(U32(d.hidden as u32)),
            kAudioDevicePropertyPreferredChannelsForStereo => Ok(U32s(vec![1, 2])),
            kAudioDevicePropertyZeroTimeStampPeriod => Ok(U32(ZERO_TS_PERIOD)),
            kAudioDevicePropertyClockIsStable => Ok(U32(1)),
            _ => unknown,
        };
    }
    if let Some((d, input)) = stream(obj) {
        return match sel {
            kAudioObjectPropertyBaseClass => Ok(U32(kAudioObjectClassID)),
            kAudioObjectPropertyClass => Ok(U32(kAudioStreamClassID)),
            kAudioObjectPropertyOwner => Ok(U32(d.id)),
            kAudioObjectPropertyOwnedObjects => Ok(U32s(Vec::new())),
            kAudioStreamPropertyIsActive => Ok(U32(1)),
            kAudioStreamPropertyDirection => Ok(U32(input as u32)),
            kAudioStreamPropertyTerminalType => Ok(U32(if input { d.input_terminal } else { d.output_terminal })),
            kAudioStreamPropertyStartingChannel => Ok(U32(1)),
            kAudioStreamPropertyLatency => Ok(U32(0)),
            kAudioStreamPropertyVirtualFormat | kAudioStreamPropertyPhysicalFormat => Ok(Format(format())),
            kAudioStreamPropertyAvailableVirtualFormats | kAudioStreamPropertyAvailablePhysicalFormats => {
                Ok(Formats(vec![AudioStreamRangedDescription {
                    mFormat: format(),
                    mSampleRateRange: AudioValueRange { mMinimum: SAMPLE_RATE, mMaximum: SAMPLE_RATE },
                }]))
            }
            _ => unknown,
        };
    }
    if obj == VOLUME || obj == MUTE {
        let common = match sel {
            kAudioObjectPropertyBaseClass => {
                Some(U32(if obj == VOLUME { kAudioLevelControlClassID } else { kAudioBooleanControlClassID }))
            }
            kAudioObjectPropertyClass => {
                Some(U32(if obj == VOLUME { kAudioVolumeControlClassID } else { kAudioMuteControlClassID }))
            }
            kAudioObjectPropertyOwner => Some(U32(MAIN)),
            kAudioObjectPropertyOwnedObjects => Some(U32s(Vec::new())),
            kAudioControlPropertyScope => Some(U32(kAudioObjectPropertyScopeOutput)),
            kAudioControlPropertyElement => Some(U32(kAudioObjectPropertyElementMain)),
            _ => None,
        };
        if let Some(v) = common {
            return Ok(v);
        }
        return match (obj, sel) {
            (VOLUME, kAudioLevelControlPropertyScalarValue) => Ok(F32(st.volume())),
            (VOLUME, kAudioLevelControlPropertyDecibelValue) => Ok(F32(scalar_to_db(st.volume()))),
            (VOLUME, kAudioLevelControlPropertyDecibelRange) => {
                Ok(Ranges(vec![AudioValueRange { mMinimum: MIN_DB as f64, mMaximum: 0.0 }]))
            }
            // The value to convert arrives in the output buffer; see get_property_data.
            (VOLUME, kAudioLevelControlPropertyConvertScalarToDecibels)
            | (VOLUME, kAudioLevelControlPropertyConvertDecibelsToScalar) => Ok(F32(0.0)),
            (MUTE, kAudioBooleanControlPropertyValue) => Ok(U32(st.mute.load(Ordering::Relaxed) as u32)),
            _ => unknown,
        };
    }
    Err(kAudioHardwareBadObjectError)
}

fn settable(obj: AudioObjectID, sel: u32) -> bool {
    matches!(
        (obj, sel),
        (VOLUME, kAudioLevelControlPropertyScalarValue | kAudioLevelControlPropertyDecibelValue)
            | (MUTE, kAudioBooleanControlPropertyValue)
    ) || (device(obj).is_some() && sel == kAudioDevicePropertyNominalSampleRate)
        || (stream(obj).is_some()
            && matches!(sel, kAudioStreamPropertyVirtualFormat | kAudioStreamPropertyPhysicalFormat))
}

/// Reads a CFStringRef qualifier into a Rust string.
fn cfstring_qualifier(q: &[u8]) -> Option<String> {
    if q.len() < std::mem::size_of::<CFStringRef>() {
        return None;
    }
    let s = unsafe { (q.as_ptr() as *const CFStringRef).read_unaligned() };
    if s.is_null() || unsafe { CFGetTypeID(s) != CFStringGetTypeID() } {
        return None;
    }
    let mut buf = [0u8; 256];
    if unsafe { CFStringGetCString(s, buf.as_mut_ptr(), buf.len() as isize, kCFStringEncodingUTF8) } == 0 {
        return None;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8(buf[..len].to_vec()).ok()
}

// ---------------------------------------------------------------- Vtable

#[repr(transparent)]
struct InterfacePtr(*const AudioServerPlugInDriverInterface);
unsafe impl Sync for InterfacePtr {}

static INTERFACE: AudioServerPlugInDriverInterface = AudioServerPlugInDriverInterface {
    _reserved: std::ptr::null_mut(),
    QueryInterface: query_interface,
    AddRef: add_ref,
    Release: release,
    Initialize: initialize,
    CreateDevice: create_device,
    DestroyDevice: destroy_device,
    AddDeviceClient: add_device_client,
    RemoveDeviceClient: remove_device_client,
    PerformDeviceConfigurationChange: perform_config_change,
    AbortDeviceConfigurationChange: abort_config_change,
    HasProperty: has_property,
    IsPropertySettable: is_property_settable,
    GetPropertyDataSize: get_property_data_size,
    GetPropertyData: get_property_data,
    SetPropertyData: set_property_data,
    StartIO: start_io,
    StopIO: stop_io,
    GetZeroTimeStamp: get_zero_time_stamp,
    WillDoIOOperation: will_do_io_operation,
    BeginIOOperation: begin_io_operation,
    DoIOOperation: do_io_operation,
    EndIOOperation: end_io_operation,
};

unsafe impl Sync for AudioServerPlugInDriverInterface {}

/// The driver reference handed to the host: a pointer to a pointer to the vtable.
static DRIVER: InterfacePtr = InterfacePtr(&INTERFACE);

fn driver_ref() -> DriverRef {
    &DRIVER as *const InterfacePtr as DriverRef
}

fn is_ours(d: *mut c_void) -> bool {
    d == driver_ref()
}

fn uuid_eq(a: CFUUIDBytes, b: CFUUIDBytes) -> bool {
    a == b
}

/// CFPlugIn factory named in Info.plist.
///
/// # Safety
/// Called by CoreFoundation with a valid CFUUIDRef.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn RelayAudio_Create(_alloc: CFAllocatorRef, type_uuid: CFUUIDRef) -> *mut c_void {
    if type_uuid.is_null() || !uuid_eq(unsafe { CFUUIDGetUUIDBytes(type_uuid) }, kAudioServerPlugInTypeUUID) {
        return std::ptr::null_mut();
    }
    state().refs.fetch_add(1, Ordering::AcqRel);
    driver_ref()
}

unsafe extern "C" fn query_interface(d: *mut c_void, uuid: CFUUIDBytes, out: *mut *mut c_void) -> HRESULT {
    if !is_ours(d) || out.is_null() {
        return kAudioHardwareBadObjectError;
    }
    if uuid_eq(uuid, IUnknownUUID) || uuid_eq(uuid, kAudioServerPlugInDriverInterfaceUUID) {
        state().refs.fetch_add(1, Ordering::AcqRel);
        unsafe { *out = driver_ref() };
        S_OK
    } else {
        unsafe { *out = std::ptr::null_mut() };
        E_NOINTERFACE
    }
}

unsafe extern "C" fn add_ref(d: *mut c_void) -> u32 {
    if !is_ours(d) {
        return 0;
    }
    state().refs.fetch_add(1, Ordering::AcqRel).saturating_add(1)
}

unsafe extern "C" fn release(d: *mut c_void) -> u32 {
    if !is_ours(d) {
        return 0;
    }
    // The driver is a static singleton; nothing to free at zero.
    let refs = &state().refs;
    let mut cur = refs.load(Ordering::Acquire);
    loop {
        let next = cur.saturating_sub(1);
        match refs.compare_exchange(cur, next, Ordering::AcqRel, Ordering::Acquire) {
            Ok(_) => return next,
            Err(v) => cur = v,
        }
    }
}

unsafe extern "C" fn initialize(d: DriverRef, host: HostRef) -> OSStatus {
    if !is_ours(d) {
        return kAudioHardwareBadObjectError;
    }
    let st = state();
    st.host.store(host as *mut _, Ordering::Release);
    st.start_monitor();
    log("initialized");
    kAudioHardwareNoError
}

unsafe extern "C" fn create_device(
    _: DriverRef,
    _: CFTypeRef,
    _: *const AudioServerPlugInClientInfo,
    _: *mut AudioObjectID,
) -> OSStatus {
    kAudioHardwareUnsupportedOperationError
}

unsafe extern "C" fn destroy_device(_: DriverRef, _: AudioObjectID) -> OSStatus {
    kAudioHardwareUnsupportedOperationError
}

unsafe extern "C" fn add_device_client(
    d: DriverRef,
    dev: AudioObjectID,
    _: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    if !is_ours(d) {
        return kAudioHardwareBadObjectError;
    }
    if device(dev).is_none() {
        return kAudioHardwareBadDeviceError;
    }
    kAudioHardwareNoError
}

unsafe extern "C" fn remove_device_client(
    d: DriverRef,
    dev: AudioObjectID,
    _: *const AudioServerPlugInClientInfo,
) -> OSStatus {
    unsafe { add_device_client(d, dev, std::ptr::null()) }
}

unsafe extern "C" fn perform_config_change(d: DriverRef, dev: AudioObjectID, _: u64, _: *mut c_void) -> OSStatus {
    unsafe { add_device_client(d, dev, std::ptr::null()) }
}

unsafe extern "C" fn abort_config_change(d: DriverRef, dev: AudioObjectID, _: u64, _: *mut c_void) -> OSStatus {
    unsafe { add_device_client(d, dev, std::ptr::null()) }
}

unsafe extern "C" fn has_property(
    d: DriverRef,
    obj: AudioObjectID,
    _pid: i32,
    addr: *const AudioObjectPropertyAddress,
) -> Boolean {
    if !is_ours(d) || addr.is_null() {
        return 0;
    }
    property(state(), obj, unsafe { &*addr }, None).is_ok() as Boolean
}

unsafe extern "C" fn is_property_settable(
    d: DriverRef,
    obj: AudioObjectID,
    _pid: i32,
    addr: *const AudioObjectPropertyAddress,
    out: *mut Boolean,
) -> OSStatus {
    if !is_ours(d) || addr.is_null() || out.is_null() {
        return kAudioHardwareBadObjectError;
    }
    let addr = unsafe { &*addr };
    if let Err(e) = property(state(), obj, addr, None) {
        return e;
    }
    unsafe { *out = settable(obj, addr.mSelector) as Boolean };
    kAudioHardwareNoError
}

unsafe fn qualifier<'a>(size: u32, data: *const c_void) -> Option<&'a [u8]> {
    (!data.is_null() && size > 0).then(|| unsafe { std::slice::from_raw_parts(data as *const u8, size as usize) })
}

unsafe extern "C" fn get_property_data_size(
    d: DriverRef,
    obj: AudioObjectID,
    _pid: i32,
    addr: *const AudioObjectPropertyAddress,
    qual_size: u32,
    qual: *const c_void,
    out_size: *mut u32,
) -> OSStatus {
    if !is_ours(d) || addr.is_null() || out_size.is_null() {
        return kAudioHardwareBadObjectError;
    }
    match property(state(), obj, unsafe { &*addr }, unsafe { qualifier(qual_size, qual) }) {
        Ok(v) => {
            unsafe { *out_size = v.size() as u32 };
            kAudioHardwareNoError
        }
        Err(e) => e,
    }
}

unsafe extern "C" fn get_property_data(
    d: DriverRef,
    obj: AudioObjectID,
    _pid: i32,
    addr: *const AudioObjectPropertyAddress,
    qual_size: u32,
    qual: *const c_void,
    in_size: u32,
    out_size: *mut u32,
    out: *mut c_void,
) -> OSStatus {
    if !is_ours(d) || addr.is_null() || out_size.is_null() || out.is_null() {
        return kAudioHardwareBadObjectError;
    }
    let addr = unsafe { &*addr };
    if obj == VOLUME
        && matches!(
            addr.mSelector,
            kAudioLevelControlPropertyConvertScalarToDecibels | kAudioLevelControlPropertyConvertDecibelsToScalar
        )
    {
        if in_size < 4 {
            return kAudioHardwareBadPropertySizeError;
        }
        let v = unsafe { (out as *const f32).read_unaligned() };
        let converted = if addr.mSelector == kAudioLevelControlPropertyConvertScalarToDecibels {
            scalar_to_db(v)
        } else {
            db_to_scalar(v)
        };
        unsafe {
            (out as *mut f32).write_unaligned(converted);
            *out_size = 4;
        }
        return kAudioHardwareNoError;
    }
    match property(state(), obj, addr, unsafe { qualifier(qual_size, qual) }) {
        Ok(v) => match unsafe { v.write(out, in_size as usize) } {
            Ok(n) => {
                unsafe { *out_size = n as u32 };
                kAudioHardwareNoError
            }
            Err(e) => e,
        },
        Err(e) => e,
    }
}

unsafe extern "C" fn set_property_data(
    d: DriverRef,
    obj: AudioObjectID,
    _pid: i32,
    addr: *const AudioObjectPropertyAddress,
    _qual_size: u32,
    _qual: *const c_void,
    in_size: u32,
    data: *const c_void,
) -> OSStatus {
    if !is_ours(d) || addr.is_null() {
        return kAudioHardwareBadObjectError;
    }
    let addr = unsafe { &*addr };
    if let Err(e) = property(state(), obj, addr, None) {
        return e;
    }
    if !settable(obj, addr.mSelector) {
        return kAudioHardwareUnsupportedOperationError;
    }
    if data.is_null() {
        return kAudioHardwareBadPropertySizeError;
    }
    let st = state();
    match addr.mSelector {
        kAudioLevelControlPropertyScalarValue | kAudioLevelControlPropertyDecibelValue => {
            if in_size < 4 {
                return kAudioHardwareBadPropertySizeError;
            }
            let v = unsafe { (data as *const f32).read_unaligned() };
            let scalar = if addr.mSelector == kAudioLevelControlPropertyScalarValue {
                v.clamp(0.0, 1.0)
            } else {
                db_to_scalar(v)
            };
            if !scalar.is_finite() {
                return kAudioHardwareIllegalOperationError;
            }
            if st.volume.swap(scalar.to_bits(), Ordering::Relaxed) != scalar.to_bits() {
                st.notify(VOLUME, &[kAudioLevelControlPropertyScalarValue, kAudioLevelControlPropertyDecibelValue]);
            }
            kAudioHardwareNoError
        }
        kAudioBooleanControlPropertyValue => {
            if in_size < 4 {
                return kAudioHardwareBadPropertySizeError;
            }
            let on = unsafe { (data as *const u32).read_unaligned() } != 0;
            if st.mute.swap(on, Ordering::Relaxed) != on {
                st.notify(MUTE, &[kAudioBooleanControlPropertyValue]);
            }
            kAudioHardwareNoError
        }
        kAudioDevicePropertyNominalSampleRate => {
            if (in_size as usize) < 8 {
                return kAudioHardwareBadPropertySizeError;
            }
            let rate = unsafe { (data as *const f64).read_unaligned() };
            if rate == SAMPLE_RATE { kAudioHardwareNoError } else { kAudioHardwareIllegalOperationError }
        }
        _ => {
            if (in_size as usize) < std::mem::size_of::<AudioStreamBasicDescription>() {
                return kAudioHardwareBadPropertySizeError;
            }
            let f = unsafe { (data as *const AudioStreamBasicDescription).read_unaligned() };
            if format_ok(&f) { kAudioHardwareNoError } else { kAudioDeviceUnsupportedFormatError }
        }
    }
}

// ---------------------------------------------------------------- IO

unsafe extern "C" fn start_io(d: DriverRef, dev: AudioObjectID, _client: u32) -> OSStatus {
    if !is_ours(d) {
        return kAudioHardwareBadObjectError;
    }
    let st = state();
    let Some(count) = st.io_count(dev) else { return kAudioHardwareBadDeviceError };
    if dev == ENGINE {
        st.engine_last_io.store(unsafe { mach_absolute_time() }, Ordering::Relaxed);
    }
    count.fetch_add(1, Ordering::AcqRel);
    kAudioHardwareNoError
}

unsafe extern "C" fn stop_io(d: DriverRef, dev: AudioObjectID, _client: u32) -> OSStatus {
    if !is_ours(d) {
        return kAudioHardwareBadObjectError;
    }
    let Some(count) = state().io_count(dev) else { return kAudioHardwareBadDeviceError };
    let _ = count.fetch_update(Ordering::AcqRel, Ordering::Acquire, |c| Some(c.saturating_sub(1)));
    kAudioHardwareNoError
}

unsafe extern "C" fn get_zero_time_stamp(
    d: DriverRef,
    dev: AudioObjectID,
    _client: u32,
    sample_time: *mut f64,
    host_time: *mut u64,
    seed: *mut u64,
) -> OSStatus {
    if !is_ours(d) || sample_time.is_null() || host_time.is_null() || seed.is_null() {
        return kAudioHardwareBadObjectError;
    }
    if device(dev).is_none() {
        return kAudioHardwareBadDeviceError;
    }
    let (s, h) = state().zero_timestamp(unsafe { mach_absolute_time() });
    unsafe {
        *sample_time = s;
        *host_time = h;
        *seed = 1;
    }
    kAudioHardwareNoError
}

unsafe extern "C" fn will_do_io_operation(
    d: DriverRef,
    dev: AudioObjectID,
    _client: u32,
    op: u32,
    will_do: *mut Boolean,
    in_place: *mut Boolean,
) -> OSStatus {
    if !is_ours(d) || will_do.is_null() || in_place.is_null() {
        return kAudioHardwareBadObjectError;
    }
    if device(dev).is_none() {
        return kAudioHardwareBadDeviceError;
    }
    let ours = matches!(op, kAudioServerPlugInIOOperationReadInput | kAudioServerPlugInIOOperationWriteMix);
    unsafe {
        *will_do = ours as Boolean;
        *in_place = 1;
    }
    kAudioHardwareNoError
}

unsafe extern "C" fn begin_io_operation(
    d: DriverRef,
    dev: AudioObjectID,
    _client: u32,
    _op: u32,
    _frames: u32,
    _info: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    if !is_ours(d) {
        return kAudioHardwareBadObjectError;
    }
    if device(dev).is_none() {
        return kAudioHardwareBadDeviceError;
    }
    kAudioHardwareNoError
}

unsafe extern "C" fn end_io_operation(
    d: DriverRef,
    dev: AudioObjectID,
    client: u32,
    op: u32,
    frames: u32,
    info: *const AudioServerPlugInIOCycleInfo,
) -> OSStatus {
    unsafe { begin_io_operation(d, dev, client, op, frames, info) }
}

unsafe extern "C" fn do_io_operation(
    d: DriverRef,
    dev: AudioObjectID,
    _stream: AudioObjectID,
    _client: u32,
    op: u32,
    frames: u32,
    info: *const AudioServerPlugInIOCycleInfo,
    main: *mut c_void,
    _secondary: *mut c_void,
) -> OSStatus {
    if !is_ours(d) || info.is_null() {
        return kAudioHardwareBadObjectError;
    }
    let st = state();
    let (from, to) = match dev {
        MAIN => (&st.to_main, &st.to_engine),
        ENGINE => {
            st.engine_last_io.store(unsafe { mach_absolute_time() }, Ordering::Relaxed);
            (&st.to_engine, &st.to_main)
        }
        _ => return kAudioHardwareBadDeviceError,
    };
    if main.is_null() || frames == 0 {
        return kAudioHardwareNoError;
    }
    let len = frames as usize * CHANNELS as usize;
    let info = unsafe { &*info };
    match op {
        kAudioServerPlugInIOOperationReadInput => {
            let buf = unsafe { std::slice::from_raw_parts_mut(main as *mut f32, len) };
            from.read(info.mInputTime.mSampleTime.round() as i64, buf);
        }
        kAudioServerPlugInIOOperationWriteMix => {
            let buf = unsafe { std::slice::from_raw_parts(main as *const f32, len) };
            let gain = if dev == MAIN { st.output_gain() } else { 1.0 };
            to.write_scaled(info.mOutputTime.mSampleTime.round() as i64, buf, gain);
        }
        _ => {}
    }
    kAudioHardwareNoError
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serializes tests that touch the global driver state.
    static GLOBAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn get(st: &State, obj: AudioObjectID, sel: u32) -> Value {
        property(st, obj, &address(sel, kAudioObjectPropertyScopeGlobal), None).unwrap_or_else(|e| {
            panic!("object {obj} property {:?}: error {e}", String::from_utf8_lossy(&sel.to_be_bytes()))
        })
    }

    fn ids(v: Value) -> Vec<u32> {
        match v {
            Value::U32s(v) => v,
            _ => panic!("not a list"),
        }
    }

    #[test]
    fn main_device_follows_engine_io() {
        let st = State::new();
        assert_eq!(ids(get(&st, PLUGIN, kAudioPlugInPropertyDeviceList)), vec![ENGINE]);
        st.engine_io.store(1, Ordering::Release);
        st.engine_last_io.store(unsafe { mach_absolute_time() }, Ordering::Relaxed);
        assert!(st.sync_published());
        assert_eq!(ids(get(&st, PLUGIN, kAudioObjectPropertyOwnedObjects)), vec![MAIN, ENGINE]);
        assert!(!st.sync_published(), "no change, no notification");
        // IO went quiet for longer than the timeout.
        st.engine_last_io.store(0, Ordering::Relaxed);
        let late = st.anchor.max(1) + (2.0 * st.ticks_per_sec) as u64;
        assert!(!st.engine_alive(late));
        st.engine_io.store(0, Ordering::Release);
        assert!(st.sync_published());
        assert_eq!(ids(get(&st, PLUGIN, kAudioPlugInPropertyDeviceList)), vec![ENGINE]);
    }

    #[test]
    fn every_object_answers_the_basics() {
        let st = State::new();
        for obj in [PLUGIN, MAIN, MAIN_IN, MAIN_OUT, ENGINE, ENGINE_IN, ENGINE_OUT] {
            for sel in [kAudioObjectPropertyBaseClass, kAudioObjectPropertyClass, kAudioObjectPropertyOwner] {
                get(&st, obj, sel);
            }
        }
        for dev in [MAIN, ENGINE] {
            for sel in [
                kAudioObjectPropertyName,
                kAudioDevicePropertyDeviceUID,
                kAudioDevicePropertyStreams,
                kAudioDevicePropertyNominalSampleRate,
                kAudioDevicePropertyZeroTimeStampPeriod,
                kAudioDevicePropertyIsHidden,
            ] {
                get(&st, dev, sel);
            }
        }
        assert!(matches!(get(&st, ENGINE, kAudioDevicePropertyIsHidden), Value::U32(1)));
        assert!(matches!(get(&st, MAIN, kAudioDevicePropertyIsHidden), Value::U32(0)));
        assert!(matches!(get(&st, ENGINE_IN, kAudioStreamPropertyDirection), Value::U32(1)));
        assert!(matches!(get(&st, ENGINE_OUT, kAudioStreamPropertyDirection), Value::U32(0)));
        assert_eq!(
            property(&st, 99, &address(kAudioObjectPropertyClass, 0), None).err(),
            Some(kAudioHardwareBadObjectError)
        );
    }

    #[test]
    fn volume_curve_round_trips() {
        assert_eq!(scalar_to_db(1.0), 0.0);
        assert_eq!(scalar_to_db(0.0), MIN_DB);
        assert!((scalar_to_db(0.5) - -12.04).abs() < 0.01);
        for s in [0.05f32, 0.3, 0.5, 0.9, 1.0] {
            assert!((db_to_scalar(scalar_to_db(s)) - s).abs() < 1e-4);
        }
        assert_eq!(db_to_scalar(MIN_DB), 0.0);
    }

    #[test]
    fn volume_and_mute_scale_relay_output() {
        let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = driver_ref();
        let st = state();
        let set = |obj, sel, v: u32| unsafe {
            set_property_data(
                d,
                obj,
                0,
                &address(sel, kAudioObjectPropertyScopeGlobal),
                0,
                std::ptr::null(),
                4,
                (&v as *const u32).cast(),
            )
        };
        assert_eq!(set(VOLUME, kAudioLevelControlPropertyScalarValue, 0.5f32.to_bits()), 0);
        assert_eq!(st.output_gain(), 0.25);
        assert_eq!(set(MUTE, kAudioBooleanControlPropertyValue, 1), 0);
        assert_eq!(st.output_gain(), 0.0);
        assert_eq!(set(MUTE, kAudioBooleanControlPropertyValue, 0), 0);
        assert_eq!(set(VOLUME, kAudioLevelControlPropertyDecibelValue, 0f32.to_bits()), 0);
        assert_eq!(st.output_gain(), 1.0);
        let mut v = 0.5f32;
        let mut size = 0u32;
        let addr = address(kAudioLevelControlPropertyConvertScalarToDecibels, kAudioObjectPropertyScopeGlobal);
        unsafe {
            get_property_data(d, VOLUME, 0, &addr, 0, std::ptr::null(), 4, &mut size, (&mut v as *mut f32).cast())
        };
        assert!((v - scalar_to_db(0.5)).abs() < 1e-6);
        assert!(ids(get(st, MAIN, kAudioObjectPropertyControlList)).contains(&VOLUME));
        assert!(ids(get(st, ENGINE, kAudioObjectPropertyControlList)).is_empty());
    }

    #[test]
    fn stream_scopes() {
        let st = State::new();
        let scoped = |scope| ids(property(&st, MAIN, &address(kAudioDevicePropertyStreams, scope), None).unwrap());
        assert_eq!(scoped(kAudioObjectPropertyScopeInput), vec![MAIN_IN]);
        assert_eq!(scoped(kAudioObjectPropertyScopeOutput), vec![MAIN_OUT]);
        assert_eq!(scoped(kAudioObjectPropertyScopeGlobal), vec![MAIN_IN, MAIN_OUT]);
    }

    #[test]
    fn zero_timestamps_step_by_the_period() {
        let st = State::new();
        let period_ticks = (st.ticks_per_frame * ZERO_TS_PERIOD as f64) as u64;
        assert_eq!(st.zero_timestamp(st.anchor), (0.0, st.anchor));
        assert_eq!(st.zero_timestamp(st.anchor + period_ticks - 1).0, 0.0);
        let (s, h) = st.zero_timestamp(st.anchor + period_ticks * 3 + 5);
        assert_eq!(s, 3.0 * ZERO_TS_PERIOD as f64);
        assert!(h.abs_diff(st.anchor + period_ticks * 3) <= 1);
    }

    #[test]
    fn io_loops_between_devices() {
        let _g = GLOBAL.lock().unwrap_or_else(|e| e.into_inner());
        let d = driver_ref();
        let frames = 64u32;
        let mut info: AudioServerPlugInIOCycleInfo = unsafe { std::mem::zeroed() };
        info.mOutputTime.mSampleTime = 1000.0;
        let mut play: Vec<f32> = (0..frames * 2).map(|i| i as f32).collect();
        unsafe {
            do_io_operation(
                d,
                MAIN,
                MAIN_OUT,
                1,
                kAudioServerPlugInIOOperationWriteMix,
                frames,
                &info,
                play.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            );
        }
        info.mInputTime.mSampleTime = 1000.0;
        let mut rec = vec![0f32; (frames * 2) as usize];
        unsafe {
            do_io_operation(
                d,
                ENGINE,
                ENGINE_IN,
                2,
                kAudioServerPlugInIOOperationReadInput,
                frames,
                &info,
                rec.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            );
        }
        assert_eq!(rec, play);
        // Nothing was written to the microphone loop.
        unsafe {
            do_io_operation(
                d,
                MAIN,
                MAIN_IN,
                1,
                kAudioServerPlugInIOOperationReadInput,
                frames,
                &info,
                rec.as_mut_ptr().cast(),
                std::ptr::null_mut(),
            );
        }
        assert!(rec.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn query_interface_accepts_driver_and_iunknown() {
        let d = driver_ref();
        let mut out = std::ptr::null_mut();
        assert_eq!(unsafe { query_interface(d, kAudioServerPlugInDriverInterfaceUUID, &mut out) }, S_OK);
        assert_eq!(out, d);
        assert_eq!(unsafe { query_interface(d, IUnknownUUID, &mut out) }, S_OK);
        assert_eq!(unsafe { query_interface(d, kAudioServerPlugInTypeUUID, &mut out) }, E_NOINTERFACE);
        assert!(out.is_null());
    }
}

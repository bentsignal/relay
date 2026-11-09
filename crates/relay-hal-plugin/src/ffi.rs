//! Hand-written CoreAudio AudioServerPlugIn and CoreFoundation definitions.
//!
//! The plugin runs inside the audio server and must not link the client side
//! of CoreAudio.framework, so nothing here comes from a bindings crate. The
//! tests check every constant against `objc2-core-audio`.

#![allow(non_upper_case_globals, non_snake_case, dead_code)]

use std::ffi::c_void;

pub type OSStatus = i32;
pub type AudioObjectID = u32;
pub type Boolean = u8;
pub type CFTypeRef = *const c_void;
pub type CFStringRef = *const c_void;
pub type CFUUIDRef = *const c_void;
pub type CFAllocatorRef = *const c_void;
pub type HRESULT = i32;

pub const fn fourcc(s: &[u8; 4]) -> u32 {
    ((s[0] as u32) << 24) | ((s[1] as u32) << 16) | ((s[2] as u32) << 8) | (s[3] as u32)
}

// ---------------------------------------------------------------- Structs

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CFUUIDBytes(pub [u8; 16]);

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct AudioObjectPropertyAddress {
    pub mSelector: u32,
    pub mScope: u32,
    pub mElement: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioStreamBasicDescription {
    pub mSampleRate: f64,
    pub mFormatID: u32,
    pub mFormatFlags: u32,
    pub mBytesPerPacket: u32,
    pub mFramesPerPacket: u32,
    pub mBytesPerFrame: u32,
    pub mChannelsPerFrame: u32,
    pub mBitsPerChannel: u32,
    pub mReserved: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioValueRange {
    pub mMinimum: f64,
    pub mMaximum: f64,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct AudioStreamRangedDescription {
    pub mFormat: AudioStreamBasicDescription,
    pub mSampleRateRange: AudioValueRange,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct SMPTETime {
    pub mSubframes: i16,
    pub mSubframeDivisor: i16,
    pub mCounter: u32,
    pub mType: u32,
    pub mFlags: u32,
    pub mHours: i16,
    pub mMinutes: i16,
    pub mSeconds: i16,
    pub mFrames: i16,
}

#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct AudioTimeStamp {
    pub mSampleTime: f64,
    pub mHostTime: u64,
    pub mRateScalar: f64,
    pub mWordClockTime: u64,
    pub mSMPTETime: SMPTETime,
    pub mFlags: u32,
    pub mReserved: u32,
}

#[repr(C)]
pub struct AudioServerPlugInIOCycleInfo {
    pub mIOCycleCounter: u64,
    pub mNominalIOBufferFrameSize: u32,
    pub mCurrentTime: AudioTimeStamp,
    pub mInputTime: AudioTimeStamp,
    pub mOutputTime: AudioTimeStamp,
    pub mMainHostTicksPerFrame: f64,
    pub mDeviceHostTicksPerFrame: f64,
}

#[repr(C)]
pub struct AudioServerPlugInClientInfo {
    pub mClientID: u32,
    pub mProcessID: i32,
    pub mIsNativeEndian: Boolean,
    pub mBundleID: CFStringRef,
}

pub type HostRef = *const AudioServerPlugInHostInterface;
pub type DriverRef = *mut c_void;

#[repr(C)]
pub struct AudioServerPlugInHostInterface {
    pub PropertiesChanged: unsafe extern "C" fn(HostRef, AudioObjectID, u32, *const AudioObjectPropertyAddress) -> OSStatus,
    pub CopyFromStorage: unsafe extern "C" fn(HostRef, CFStringRef, *mut CFTypeRef) -> OSStatus,
    pub WriteToStorage: unsafe extern "C" fn(HostRef, CFStringRef, CFTypeRef) -> OSStatus,
    pub DeleteFromStorage: unsafe extern "C" fn(HostRef, CFStringRef) -> OSStatus,
    pub RequestDeviceConfigurationChange: unsafe extern "C" fn(HostRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
}

#[repr(C)]
pub struct AudioServerPlugInDriverInterface {
    pub _reserved: *mut c_void,
    pub QueryInterface: unsafe extern "C" fn(*mut c_void, CFUUIDBytes, *mut *mut c_void) -> HRESULT,
    pub AddRef: unsafe extern "C" fn(*mut c_void) -> u32,
    pub Release: unsafe extern "C" fn(*mut c_void) -> u32,
    pub Initialize: unsafe extern "C" fn(DriverRef, HostRef) -> OSStatus,
    pub CreateDevice:
        unsafe extern "C" fn(DriverRef, CFTypeRef, *const AudioServerPlugInClientInfo, *mut AudioObjectID) -> OSStatus,
    pub DestroyDevice: unsafe extern "C" fn(DriverRef, AudioObjectID) -> OSStatus,
    pub AddDeviceClient: unsafe extern "C" fn(DriverRef, AudioObjectID, *const AudioServerPlugInClientInfo) -> OSStatus,
    pub RemoveDeviceClient:
        unsafe extern "C" fn(DriverRef, AudioObjectID, *const AudioServerPlugInClientInfo) -> OSStatus,
    pub PerformDeviceConfigurationChange: unsafe extern "C" fn(DriverRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    pub AbortDeviceConfigurationChange: unsafe extern "C" fn(DriverRef, AudioObjectID, u64, *mut c_void) -> OSStatus,
    pub HasProperty: unsafe extern "C" fn(DriverRef, AudioObjectID, i32, *const AudioObjectPropertyAddress) -> Boolean,
    pub IsPropertySettable:
        unsafe extern "C" fn(DriverRef, AudioObjectID, i32, *const AudioObjectPropertyAddress, *mut Boolean) -> OSStatus,
    pub GetPropertyDataSize: unsafe extern "C" fn(
        DriverRef,
        AudioObjectID,
        i32,
        *const AudioObjectPropertyAddress,
        u32,
        *const c_void,
        *mut u32,
    ) -> OSStatus,
    pub GetPropertyData: unsafe extern "C" fn(
        DriverRef,
        AudioObjectID,
        i32,
        *const AudioObjectPropertyAddress,
        u32,
        *const c_void,
        u32,
        *mut u32,
        *mut c_void,
    ) -> OSStatus,
    pub SetPropertyData: unsafe extern "C" fn(
        DriverRef,
        AudioObjectID,
        i32,
        *const AudioObjectPropertyAddress,
        u32,
        *const c_void,
        u32,
        *const c_void,
    ) -> OSStatus,
    pub StartIO: unsafe extern "C" fn(DriverRef, AudioObjectID, u32) -> OSStatus,
    pub StopIO: unsafe extern "C" fn(DriverRef, AudioObjectID, u32) -> OSStatus,
    pub GetZeroTimeStamp: unsafe extern "C" fn(DriverRef, AudioObjectID, u32, *mut f64, *mut u64, *mut u64) -> OSStatus,
    pub WillDoIOOperation:
        unsafe extern "C" fn(DriverRef, AudioObjectID, u32, u32, *mut Boolean, *mut Boolean) -> OSStatus,
    pub BeginIOOperation:
        unsafe extern "C" fn(DriverRef, AudioObjectID, u32, u32, u32, *const AudioServerPlugInIOCycleInfo) -> OSStatus,
    pub DoIOOperation: unsafe extern "C" fn(
        DriverRef,
        AudioObjectID,
        AudioObjectID,
        u32,
        u32,
        u32,
        *const AudioServerPlugInIOCycleInfo,
        *mut c_void,
        *mut c_void,
    ) -> OSStatus,
    pub EndIOOperation:
        unsafe extern "C" fn(DriverRef, AudioObjectID, u32, u32, u32, *const AudioServerPlugInIOCycleInfo) -> OSStatus,
}

// ---------------------------------------------------------------- UUIDs

pub const kAudioServerPlugInTypeUUID: CFUUIDBytes = CFUUIDBytes([
    0x44, 0x3A, 0xBA, 0xB8, 0xE7, 0xB3, 0x49, 0x1A, 0xB9, 0x85, 0xBE, 0xB9, 0x18, 0x70, 0x30, 0xDB,
]);
pub const kAudioServerPlugInDriverInterfaceUUID: CFUUIDBytes = CFUUIDBytes([
    0xEE, 0xA5, 0x77, 0x3D, 0xCC, 0x43, 0x49, 0xF1, 0x8E, 0x00, 0x8F, 0x96, 0xE7, 0xD2, 0x3B, 0x17,
]);
pub const IUnknownUUID: CFUUIDBytes =
    CFUUIDBytes([0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x46]);

pub const S_OK: HRESULT = 0;
pub const E_NOINTERFACE: HRESULT = 0x80000004u32 as i32;

// ---------------------------------------------------------------- Errors

pub const kAudioHardwareNoError: OSStatus = 0;
pub const kAudioHardwareUnspecifiedError: OSStatus = fourcc(b"what") as i32;
pub const kAudioHardwareUnknownPropertyError: OSStatus = fourcc(b"who?") as i32;
pub const kAudioHardwareBadPropertySizeError: OSStatus = fourcc(b"!siz") as i32;
pub const kAudioHardwareIllegalOperationError: OSStatus = fourcc(b"nope") as i32;
pub const kAudioHardwareBadObjectError: OSStatus = fourcc(b"!obj") as i32;
pub const kAudioHardwareBadDeviceError: OSStatus = fourcc(b"!dev") as i32;
pub const kAudioHardwareUnsupportedOperationError: OSStatus = fourcc(b"unop") as i32;
pub const kAudioDeviceUnsupportedFormatError: OSStatus = fourcc(b"!dat") as i32;

// ---------------------------------------------------------------- Objects

pub const kAudioObjectUnknown: AudioObjectID = 0;
pub const kAudioObjectPlugInObject: AudioObjectID = 1;

pub const kAudioObjectClassID: u32 = fourcc(b"aobj");
pub const kAudioPlugInClassID: u32 = fourcc(b"aplg");
pub const kAudioDeviceClassID: u32 = fourcc(b"adev");
pub const kAudioStreamClassID: u32 = fourcc(b"astr");

pub const kAudioObjectPropertyScopeGlobal: u32 = fourcc(b"glob");
pub const kAudioObjectPropertyScopeInput: u32 = fourcc(b"inpt");
pub const kAudioObjectPropertyScopeOutput: u32 = fourcc(b"outp");
pub const kAudioObjectPropertyElementMain: u32 = 0;

pub const kAudioObjectPropertyBaseClass: u32 = fourcc(b"bcls");
pub const kAudioObjectPropertyClass: u32 = fourcc(b"clas");
pub const kAudioObjectPropertyOwner: u32 = fourcc(b"stdv");
pub const kAudioObjectPropertyName: u32 = fourcc(b"lnam");
pub const kAudioObjectPropertyManufacturer: u32 = fourcc(b"lmak");
pub const kAudioObjectPropertyOwnedObjects: u32 = fourcc(b"ownd");
pub const kAudioObjectPropertyControlList: u32 = fourcc(b"ctrl");

pub const kAudioPlugInPropertyDeviceList: u32 = fourcc(b"dev#");
pub const kAudioPlugInPropertyTranslateUIDToDevice: u32 = fourcc(b"uidd");
pub const kAudioPlugInPropertyResourceBundle: u32 = fourcc(b"rsrc");

pub const kAudioDevicePropertyDeviceUID: u32 = fourcc(b"uid ");
pub const kAudioDevicePropertyModelUID: u32 = fourcc(b"muid");
pub const kAudioDevicePropertyTransportType: u32 = fourcc(b"tran");
pub const kAudioDevicePropertyRelatedDevices: u32 = fourcc(b"akin");
pub const kAudioDevicePropertyClockDomain: u32 = fourcc(b"clkd");
pub const kAudioDevicePropertyDeviceIsAlive: u32 = fourcc(b"livn");
pub const kAudioDevicePropertyDeviceIsRunning: u32 = fourcc(b"goin");
pub const kAudioDevicePropertyDeviceCanBeDefaultDevice: u32 = fourcc(b"dflt");
pub const kAudioDevicePropertyDeviceCanBeDefaultSystemDevice: u32 = fourcc(b"sflt");
pub const kAudioDevicePropertyLatency: u32 = fourcc(b"ltnc");
pub const kAudioDevicePropertyStreams: u32 = fourcc(b"stm#");
pub const kAudioDevicePropertySafetyOffset: u32 = fourcc(b"saft");
pub const kAudioDevicePropertyNominalSampleRate: u32 = fourcc(b"nsrt");
pub const kAudioDevicePropertyAvailableNominalSampleRates: u32 = fourcc(b"nsr#");
pub const kAudioDevicePropertyIsHidden: u32 = fourcc(b"hidn");
pub const kAudioDevicePropertyPreferredChannelsForStereo: u32 = fourcc(b"dch2");
pub const kAudioDevicePropertyZeroTimeStampPeriod: u32 = fourcc(b"ring");
pub const kAudioDevicePropertyClockIsStable: u32 = fourcc(b"cstb");
pub const kAudioDeviceTransportTypeVirtual: u32 = fourcc(b"virt");

pub const kAudioStreamPropertyIsActive: u32 = fourcc(b"sact");
pub const kAudioStreamPropertyDirection: u32 = fourcc(b"sdir");
pub const kAudioStreamPropertyTerminalType: u32 = fourcc(b"term");
pub const kAudioStreamPropertyStartingChannel: u32 = fourcc(b"schn");
pub const kAudioStreamPropertyLatency: u32 = fourcc(b"ltnc");
pub const kAudioStreamPropertyVirtualFormat: u32 = fourcc(b"sfmt");
pub const kAudioStreamPropertyAvailableVirtualFormats: u32 = fourcc(b"sfma");
pub const kAudioStreamPropertyPhysicalFormat: u32 = fourcc(b"pft ");
pub const kAudioStreamPropertyAvailablePhysicalFormats: u32 = fourcc(b"pfta");
pub const kAudioStreamTerminalTypeLine: u32 = fourcc(b"line");
pub const kAudioStreamTerminalTypeMicrophone: u32 = fourcc(b"micr");
pub const kAudioStreamTerminalTypeSpeaker: u32 = fourcc(b"spkr");

pub const kAudioFormatLinearPCM: u32 = fourcc(b"lpcm");
pub const kAudioFormatFlagIsFloat: u32 = 1 << 0;
pub const kAudioFormatFlagIsPacked: u32 = 1 << 3;
pub const kAudioFormatFlagsNativeFloatPacked: u32 = kAudioFormatFlagIsFloat | kAudioFormatFlagIsPacked;

pub const kAudioServerPlugInIOOperationReadInput: u32 = fourcc(b"read");
pub const kAudioServerPlugInIOOperationWriteMix: u32 = fourcc(b"rite");

// ---------------------------------------------------------------- CoreFoundation

pub const kCFStringEncodingUTF8: u32 = 0x0800_0100;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    pub fn CFUUIDCreateFromUUIDBytes(alloc: CFAllocatorRef, bytes: CFUUIDBytes) -> CFUUIDRef;
    pub fn CFUUIDGetUUIDBytes(uuid: CFUUIDRef) -> CFUUIDBytes;
    pub fn CFStringCreateWithBytes(
        alloc: CFAllocatorRef,
        bytes: *const u8,
        len: isize,
        encoding: u32,
        external: Boolean,
    ) -> CFStringRef;
    pub fn CFStringGetCString(s: CFStringRef, buf: *mut u8, size: isize, encoding: u32) -> Boolean;
    pub fn CFGetTypeID(cf: CFTypeRef) -> usize;
    pub fn CFStringGetTypeID() -> usize;
    pub fn CFRelease(cf: CFTypeRef);
}

// mach timing lives in libSystem.
#[repr(C)]
pub struct MachTimebaseInfo {
    pub numer: u32,
    pub denom: u32,
}

unsafe extern "C" {
    pub fn mach_absolute_time() -> u64;
    pub fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;
    use objc2_core_audio as ca;
    use objc2_core_audio_types as cat;

    #[test]
    fn constants_match_the_sdk() {
        let pairs: &[(u32, u32)] = &[
            (kAudioHardwareUnspecifiedError as u32, ca::kAudioHardwareUnspecifiedError as u32),
            (kAudioHardwareUnknownPropertyError as u32, ca::kAudioHardwareUnknownPropertyError as u32),
            (kAudioHardwareBadPropertySizeError as u32, ca::kAudioHardwareBadPropertySizeError as u32),
            (kAudioHardwareIllegalOperationError as u32, ca::kAudioHardwareIllegalOperationError as u32),
            (kAudioHardwareBadObjectError as u32, ca::kAudioHardwareBadObjectError as u32),
            (kAudioHardwareBadDeviceError as u32, ca::kAudioHardwareBadDeviceError as u32),
            (kAudioHardwareUnsupportedOperationError as u32, ca::kAudioHardwareUnsupportedOperationError as u32),
            (kAudioDeviceUnsupportedFormatError as u32, ca::kAudioDeviceUnsupportedFormatError as u32),
            (kAudioObjectPlugInObject, ca::kAudioObjectPlugInObject),
            (kAudioObjectClassID, ca::kAudioObjectClassID),
            (kAudioPlugInClassID, ca::kAudioPlugInClassID),
            (kAudioDeviceClassID, ca::kAudioDeviceClassID),
            (kAudioStreamClassID, ca::kAudioStreamClassID),
            (kAudioObjectPropertyScopeGlobal, ca::kAudioObjectPropertyScopeGlobal),
            (kAudioObjectPropertyScopeInput, ca::kAudioObjectPropertyScopeInput),
            (kAudioObjectPropertyScopeOutput, ca::kAudioObjectPropertyScopeOutput),
            (kAudioObjectPropertyElementMain, ca::kAudioObjectPropertyElementMain),
            (kAudioObjectPropertyBaseClass, ca::kAudioObjectPropertyBaseClass),
            (kAudioObjectPropertyClass, ca::kAudioObjectPropertyClass),
            (kAudioObjectPropertyOwner, ca::kAudioObjectPropertyOwner),
            (kAudioObjectPropertyName, ca::kAudioObjectPropertyName),
            (kAudioObjectPropertyManufacturer, ca::kAudioObjectPropertyManufacturer),
            (kAudioObjectPropertyOwnedObjects, ca::kAudioObjectPropertyOwnedObjects),
            (kAudioObjectPropertyControlList, ca::kAudioObjectPropertyControlList),
            (kAudioPlugInPropertyDeviceList, ca::kAudioPlugInPropertyDeviceList),
            (kAudioPlugInPropertyTranslateUIDToDevice, ca::kAudioPlugInPropertyTranslateUIDToDevice),
            (kAudioPlugInPropertyResourceBundle, ca::kAudioPlugInPropertyResourceBundle),
            (kAudioDevicePropertyDeviceUID, ca::kAudioDevicePropertyDeviceUID),
            (kAudioDevicePropertyModelUID, ca::kAudioDevicePropertyModelUID),
            (kAudioDevicePropertyTransportType, ca::kAudioDevicePropertyTransportType),
            (kAudioDevicePropertyRelatedDevices, ca::kAudioDevicePropertyRelatedDevices),
            (kAudioDevicePropertyClockDomain, ca::kAudioDevicePropertyClockDomain),
            (kAudioDevicePropertyDeviceIsAlive, ca::kAudioDevicePropertyDeviceIsAlive),
            (kAudioDevicePropertyDeviceIsRunning, ca::kAudioDevicePropertyDeviceIsRunning),
            (kAudioDevicePropertyDeviceCanBeDefaultDevice, ca::kAudioDevicePropertyDeviceCanBeDefaultDevice),
            (kAudioDevicePropertyDeviceCanBeDefaultSystemDevice, ca::kAudioDevicePropertyDeviceCanBeDefaultSystemDevice),
            (kAudioDevicePropertyLatency, ca::kAudioDevicePropertyLatency),
            (kAudioDevicePropertyStreams, ca::kAudioDevicePropertyStreams),
            (kAudioDevicePropertySafetyOffset, ca::kAudioDevicePropertySafetyOffset),
            (kAudioDevicePropertyNominalSampleRate, ca::kAudioDevicePropertyNominalSampleRate),
            (kAudioDevicePropertyAvailableNominalSampleRates, ca::kAudioDevicePropertyAvailableNominalSampleRates),
            (kAudioDevicePropertyIsHidden, ca::kAudioDevicePropertyIsHidden),
            (kAudioDevicePropertyPreferredChannelsForStereo, ca::kAudioDevicePropertyPreferredChannelsForStereo),
            (kAudioDevicePropertyZeroTimeStampPeriod, ca::kAudioDevicePropertyZeroTimeStampPeriod),
            (kAudioDevicePropertyClockIsStable, ca::kAudioDevicePropertyClockIsStable),
            (kAudioDeviceTransportTypeVirtual, ca::kAudioDeviceTransportTypeVirtual),
            (kAudioStreamPropertyIsActive, ca::kAudioStreamPropertyIsActive),
            (kAudioStreamPropertyDirection, ca::kAudioStreamPropertyDirection),
            (kAudioStreamPropertyTerminalType, ca::kAudioStreamPropertyTerminalType),
            (kAudioStreamPropertyStartingChannel, ca::kAudioStreamPropertyStartingChannel),
            (kAudioStreamPropertyLatency, ca::kAudioStreamPropertyLatency),
            (kAudioStreamPropertyVirtualFormat, ca::kAudioStreamPropertyVirtualFormat),
            (kAudioStreamPropertyAvailableVirtualFormats, ca::kAudioStreamPropertyAvailableVirtualFormats),
            (kAudioStreamPropertyPhysicalFormat, ca::kAudioStreamPropertyPhysicalFormat),
            (kAudioStreamPropertyAvailablePhysicalFormats, ca::kAudioStreamPropertyAvailablePhysicalFormats),
            (kAudioStreamTerminalTypeLine, ca::kAudioStreamTerminalTypeLine),
            (kAudioStreamTerminalTypeMicrophone, ca::kAudioStreamTerminalTypeMicrophone),
            (kAudioStreamTerminalTypeSpeaker, ca::kAudioStreamTerminalTypeSpeaker),
            (kAudioFormatLinearPCM, cat::kAudioFormatLinearPCM),
            (kAudioFormatFlagIsFloat, cat::kAudioFormatFlagIsFloat),
            (kAudioFormatFlagIsPacked, cat::kAudioFormatFlagIsPacked),
            (kAudioFormatFlagsNativeFloatPacked, cat::kAudioFormatFlagsNativeFloatPacked),
            (kAudioServerPlugInIOOperationReadInput, ca::AudioServerPlugInIOOperation::ReadInput.0),
            (kAudioServerPlugInIOOperationWriteMix, ca::AudioServerPlugInIOOperation::WriteMix.0),
        ];
        for (i, (ours, sdk)) in pairs.iter().enumerate() {
            assert_eq!(ours, sdk, "constant #{i}: {ours:#x} != {sdk:#x}");
        }
    }

    #[test]
    fn struct_layouts_match_the_sdk() {
        use std::mem::size_of;
        assert_eq!(size_of::<AudioStreamBasicDescription>(), size_of::<cat::AudioStreamBasicDescription>());
        assert_eq!(size_of::<AudioTimeStamp>(), size_of::<cat::AudioTimeStamp>());
        assert_eq!(size_of::<AudioValueRange>(), size_of::<cat::AudioValueRange>());
        assert_eq!(size_of::<AudioStreamRangedDescription>(), size_of::<ca::AudioStreamRangedDescription>());
        assert_eq!(size_of::<AudioObjectPropertyAddress>(), size_of::<ca::AudioObjectPropertyAddress>());
        assert_eq!(size_of::<AudioServerPlugInClientInfo>(), size_of::<ca::AudioServerPlugInClientInfo>());
        assert_eq!(size_of::<AudioTimeStamp>(), 64);
        assert_eq!(std::mem::offset_of!(AudioServerPlugInIOCycleInfo, mInputTime), 16 + 64);
        assert_eq!(std::mem::offset_of!(AudioServerPlugInIOCycleInfo, mOutputTime), 16 + 128);
        assert_eq!(size_of::<AudioServerPlugInDriverInterface>(), 23 * size_of::<usize>());
    }
}

//! Windows-specific audio helpers: default endpoint control.

use anyhow::{Result, anyhow};
use std::ffi::c_void;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, ERole, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eCommunications,
    eConsole, eMultimedia, eRender,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
};
use windows::core::{GUID, HRESULT, IUnknown, Interface, PCWSTR};

/// cpal's WASAPI device keys look like `wasapi:<endpoint id>`.
pub fn endpoint_id_from_key(key: &str) -> &str {
    key.strip_prefix("wasapi:").unwrap_or(key)
}

pub fn key_from_endpoint_id(id: &str) -> String {
    format!("wasapi:{id}")
}

fn com() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

fn enumerator() -> Result<IMMDeviceEnumerator> {
    com();
    Ok(unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)? })
}

/// Endpoint ID of the current default device (console role).
pub fn default_endpoint(capture: bool) -> Option<String> {
    let e = enumerator().ok()?;
    let flow = if capture { eCapture } else { eRender };
    let dev = unsafe { e.GetDefaultAudioEndpoint(flow, eConsole) }.ok()?;
    let id = unsafe { dev.GetId() }.ok()?;
    let s = unsafe { id.to_string() }.ok();
    unsafe { windows::Win32::System::Com::CoTaskMemFree(Some(id.0 as *const c_void)) };
    s
}

/// True if the endpoint exists and is active.
pub fn endpoint_active(id: &str) -> bool {
    let Ok(e) = enumerator() else { return false };
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    let Ok(dev) = (unsafe { e.GetDevice(PCWSTR(wide.as_ptr())) }) else {
        return false;
    };
    unsafe { dev.GetState() }
        .map(|s| s == DEVICE_STATE_ACTIVE)
        .unwrap_or(false)
}

// The undocumented-but-stable IPolicyConfig interface used by the Sound
// control panel (and tools like EarTrumpet/SoundSwitch) to set defaults.
const CLSID_POLICY_CONFIG: GUID = GUID::from_u128(0x870af99c_171d_4f9e_af0d_e63df40c2bc9);
const IID_POLICY_CONFIG: GUID = GUID::from_u128(0xf8679f50_850a_41cf_9c72_430f290290c8);

#[repr(C)]
struct PolicyConfigVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    _unused: [usize; 10],
    set_default_endpoint: unsafe extern "system" fn(*mut c_void, PCWSTR, ERole) -> HRESULT,
}

/// Makes `endpoint_id` the default for every role (console, multimedia, communications).
pub fn set_default_endpoint(endpoint_id: &str) -> Result<()> {
    com();
    let unk: IUnknown = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)? };
    let mut ptr: *mut c_void = std::ptr::null_mut();
    unsafe { unk.query(&IID_POLICY_CONFIG, &mut ptr) }.ok()?;
    if ptr.is_null() {
        return Err(anyhow!("IPolicyConfig unavailable"));
    }
    let wide: Vec<u16> = endpoint_id.encode_utf16().chain(Some(0)).collect();
    let result = unsafe {
        let vtbl = &**(ptr as *mut *const PolicyConfigVtbl);
        let mut r = Ok(());
        for role in [eConsole, eMultimedia, eCommunications] {
            let hr = (vtbl.set_default_endpoint)(ptr, PCWSTR(wide.as_ptr()), role);
            if hr.is_err() {
                r = Err(anyhow!("SetDefaultEndpoint failed: {hr:?}"));
            }
        }
        (vtbl.release)(ptr);
        r
    };
    let _ = IUnknown::IID;
    result
}

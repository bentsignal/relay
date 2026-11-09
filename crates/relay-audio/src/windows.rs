//! Windows-specific audio helpers: default endpoint control.

use anyhow::{Result, anyhow};
use std::ffi::c_void;
use windows::Win32::Media::Audio::{
    DEVICE_STATE_ACTIVE, ERole, IMMDeviceEnumerator, MMDeviceEnumerator, eCapture, eCommunications, eConsole,
    eMultimedia, eRender,
};
use windows::Win32::System::Com::{CLSCTX_ALL, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx};
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
    unsafe { dev.GetState() }.map(|s| s == DEVICE_STATE_ACTIVE).unwrap_or(false)
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

// ---------------------------------------------------------------- apps

use crate::DeviceStream;
use relay_mixer::{PipeReader, pipe};
use relay_proto::AppInfo;
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS,
};

struct Proc {
    pid: u32,
    parent: u32,
    exe: String,
}

fn processes() -> Vec<Proc> {
    let mut out = Vec::new();
    unsafe {
        let Ok(snap) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else { return out };
        let mut e = PROCESSENTRY32W { dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32, ..Default::default() };
        if Process32FirstW(snap, &mut e).is_ok() {
            loop {
                let len = e.szExeFile.iter().position(|c| *c == 0).unwrap_or(e.szExeFile.len());
                out.push(Proc {
                    pid: e.th32ProcessID,
                    parent: e.th32ParentProcessID,
                    exe: String::from_utf16_lossy(&e.szExeFile[..len]),
                });
                if Process32NextW(snap, &mut e).is_err() {
                    break;
                }
            }
        }
        let _ = CloseHandle(HANDLE(snap.0));
    }
    out
}

/// The top-most process of an app (e.g. the main chrome.exe, not its helpers).
fn root_pid(exe: &str, procs: &[Proc]) -> Option<u32> {
    let by_pid: HashMap<u32, &Proc> = procs.iter().map(|p| (p.pid, p)).collect();
    procs
        .iter()
        .filter(|p| p.exe.eq_ignore_ascii_case(exe))
        .find(|p| by_pid.get(&p.parent).map(|parent| !parent.exe.eq_ignore_ascii_case(exe)).unwrap_or(true))
        .map(|p| p.pid)
}

fn pretty(exe: &str) -> String {
    let base = exe.trim_end_matches(".exe").trim_end_matches(".EXE");
    let mut c = base.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => exe.to_string(),
    }
}

/// Apps that currently have an audio session on any playback device.
pub fn list_apps() -> Vec<AppInfo> {
    let _ = wasapi::initialize_mta();
    let procs = processes();
    let names: HashMap<u32, &str> = procs.iter().map(|p| (p.pid, p.exe.as_str())).collect();
    let me = std::process::id();
    let mut seen = std::collections::BTreeMap::new();
    let Ok(enumerator) = wasapi::DeviceEnumerator::new() else { return Vec::new() };
    let Ok(devices) = enumerator.get_device_collection(wasapi::Direction::Render) else { return Vec::new() };
    for dev in &devices {
        let Ok(dev) = dev else { continue };
        let Ok(manager) = dev.get_iaudiosessionmanager() else { continue };
        let Ok(sessions) = manager.get_audiosessionenumerator() else { continue };
        for i in 0..sessions.get_count().unwrap_or(0) {
            let Ok(ctl) = sessions.get_session(i) else { continue };
            if ctl.get_state().map(|s| s == wasapi::SessionState::Expired).unwrap_or(true) {
                continue;
            }
            let Ok(pid) = ctl.get_process_id() else { continue };
            if pid == 0 || pid == me {
                continue;
            }
            if let Some(exe) = names.get(&pid) {
                let key = exe.to_ascii_lowercase();
                seen.entry(key).or_insert(AppInfo { pid, process: exe.to_string(), name: pretty(exe) });
            }
        }
    }
    seen.into_values().collect()
}

/// Captures one app's audio (and its child processes) with WASAPI process loopback.
pub fn open_app(exe: &str) -> anyhow::Result<(DeviceStream, PipeReader)> {
    let pid = root_pid(exe, &processes()).ok_or_else(|| anyhow!("{exe} isn't running"))?;
    let rate = 48000;
    let (mut writer, reader) = pipe(rate, rate as usize / 2);
    let shared = writer.shared().clone();
    let stop = Arc::new(AtomicBool::new(false));
    let failed = Arc::new(AtomicBool::new(false));
    let (stop2, failed2) = (stop.clone(), failed.clone());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<anyhow::Result<()>>();
    let name = exe.to_string();
    std::thread::Builder::new().name(format!("relay-app-{exe}")).spawn(move || {
        let mut run = || -> anyhow::Result<()> {
            let _ = wasapi::initialize_mta();
            let format = wasapi::WaveFormat::new(32, 32, wasapi::SampleType::Float, rate as usize, 2, None);
            let mut client =
                wasapi::AudioClient::new_application_loopback_client(pid, true).map_err(|e| anyhow!("{e}"))?;
            let mode = wasapi::StreamMode::EventsShared { autoconvert: true, buffer_duration_hns: 0 };
            client.initialize_client(&format, wasapi::Direction::Capture, mode, None).map_err(|e| anyhow!("{e}"))?;
            let event = client.set_get_eventhandle().map_err(|e| anyhow!("{e}"))?;
            let capture = client.get_audiocaptureclient().map_err(|e| anyhow!("{e}"))?;
            client.start_stream().map_err(|e| anyhow!("{e}"))?;
            let _ = ready_tx.send(Ok(()));
            let mut bytes = std::collections::VecDeque::new();
            let mut floats: Vec<f32> = Vec::with_capacity(8192);
            while !stop2.load(Ordering::Relaxed) {
                if event.wait_for_event(200).is_err() {
                    continue; // silence: the app isn't playing right now
                }
                capture.read_from_device_to_deque(&mut bytes).map_err(|e| anyhow!("{e}"))?;
                floats.clear();
                while bytes.len() >= 4 {
                    let b = [
                        bytes.pop_front().unwrap(),
                        bytes.pop_front().unwrap(),
                        bytes.pop_front().unwrap(),
                        bytes.pop_front().unwrap(),
                    ];
                    floats.push(f32::from_le_bytes(b));
                }
                writer.write_interleaved(&floats);
            }
            let _ = client.stop_stream();
            Ok(())
        };
        if let Err(e) = run() {
            tracing::info!("app capture for {name} ended: {e:#}");
            failed2.store(true, Ordering::Relaxed);
            let _ = ready_tx.send(Err(e));
        }
    })?;
    ready_rx.recv_timeout(std::time::Duration::from_secs(5)).map_err(|_| anyhow!("app capture didn't start"))??;
    Ok((DeviceStream::from_thread(stop, exe.to_string(), rate, shared, failed), reader))
}

// ---------------------------------------------------------------- endpoint volume

use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;

fn endpoint_volume_iface(id: &str) -> anyhow::Result<IAudioEndpointVolume> {
    let e = enumerator()?;
    let wide: Vec<u16> = id.encode_utf16().chain(Some(0)).collect();
    let dev = unsafe { e.GetDevice(PCWSTR(wide.as_ptr()))? };
    Ok(unsafe { dev.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None)? })
}

/// (volume 0..1, muted) of an endpoint.
pub fn endpoint_volume(id: &str) -> anyhow::Result<(f32, bool)> {
    let v = endpoint_volume_iface(id)?;
    unsafe { Ok((v.GetMasterVolumeLevelScalar()?, v.GetMute()?.as_bool())) }
}

/// Sets an endpoint to full volume, unmuted. Relay's own faders control level,
/// so its virtual microphone should pass audio through unchanged.
pub fn set_endpoint_unity(id: &str) -> anyhow::Result<()> {
    let v = endpoint_volume_iface(id)?;
    unsafe {
        v.SetMasterVolumeLevelScalar(1.0, std::ptr::null())?;
        v.SetMute(false, std::ptr::null())?;
    }
    Ok(())
}

// ---------------------------------------------------------------- enhancements

/// PKEY_AudioEndpoint_Disable_SysFx: 1 turns off all system audio effects
/// ("Audio enhancements" in Settings), including Windows' automatic echo
/// cancellation, which would otherwise erase audio Relay routes into its mic
/// whenever the same audio is also playing.
const PKEY_DISABLE_SYSFX: windows::Win32::Foundation::PROPERTYKEY =
    windows::Win32::Foundation::PROPERTYKEY { fmtid: GUID::from_u128(0x1da5d803_d492_4edd_8c23_e0c0ffee7f0e), pid: 5 };

#[repr(C)]
struct PolicyConfigFullVtbl {
    query_interface: usize,
    add_ref: usize,
    release: unsafe extern "system" fn(*mut c_void) -> u32,
    _before: [usize; 9],
    set_property_value: unsafe extern "system" fn(
        *mut c_void,
        PCWSTR,
        i32,
        *const windows::Win32::Foundation::PROPERTYKEY,
        *const windows::Win32::System::Com::StructuredStorage::PROPVARIANT,
    ) -> HRESULT,
}

/// Turns off Windows audio effects for an endpoint (no admin rights needed:
/// the audio service applies it, like the Settings toggle).
pub fn disable_enhancements(endpoint_id: &str) -> Result<()> {
    com();
    let unk: IUnknown = unsafe { CoCreateInstance(&CLSID_POLICY_CONFIG, None, CLSCTX_ALL)? };
    let mut ptr: *mut c_void = std::ptr::null_mut();
    unsafe { unk.query(&IID_POLICY_CONFIG, &mut ptr) }.ok()?;
    if ptr.is_null() {
        return Err(anyhow!("IPolicyConfig unavailable"));
    }
    let wide: Vec<u16> = endpoint_id.encode_utf16().chain(Some(0)).collect();
    let value = windows::Win32::System::Com::StructuredStorage::PROPVARIANT::from(1u32);
    let hr = unsafe {
        let vtbl = &**(ptr as *mut *const PolicyConfigFullVtbl);
        let hr = (vtbl.set_property_value)(ptr, PCWSTR(wide.as_ptr()), 0, &PKEY_DISABLE_SYSFX, &value);
        (vtbl.release)(ptr);
        hr
    };
    hr.ok().map_err(|e| anyhow!("couldn't turn off enhancements: {e}"))
}

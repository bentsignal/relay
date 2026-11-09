//! "Relay Camera": incoming video exposed as a Windows 11 webcam.
//!
//! The media source (`native/windows/virtual-camera`, `relay-camera.dll`) runs
//! inside the Windows Camera Frame Server service, not in our process. It
//! creates a shared-memory mapping (`Global\RelayCameraFrames`) while an app is
//! streaming from the camera, and says which size it wants. [`CameraFeed`]
//! scales decoded frames to that size on the GPU, reads them back and publishes
//! them there. When nothing arrives for a second the camera shows black.
//!
//! Setup is two steps:
//! 1. The DLL's COM class is registered machine-wide (admin, once):
//!    `scripts/windows/install-camera.ps1`, run by the one-time installer.
//! 2. The virtual camera device is created for the current user with
//!    `MFCreateVirtualCamera` ([`register`], no admin needed). It persists.
//!
//! The shared-memory layout must match `native/windows/virtual-camera/src/RelayCamera.h`.

use super::Gpu;
use super::convert::{self, Converter, Spaces};
use anyhow::{Context, Result, bail};
use std::mem::offset_of;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::{CloseHandle, HANDLE, RECT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT_NV12;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
use windows::Win32::System::Memory::*;
use windows::Win32::System::Registry::*;
use windows::Win32::System::SystemInformation::GetTickCount64;
use windows::core::{HSTRING, PCWSTR, PWSTR};

/// COM class of the media source. Fixed forever.
pub const CLSID: &str = "{E7AE42DD-B97D-4F1A-B794-0BD645A3D031}";
/// Name passed to `MFCreateVirtualCamera` (apps list it under this name).
pub const FRIENDLY_NAME: &str = "Relay Camera";

/// Frame Server (session 0) creates the global name; a media source loaded in
/// a user's own process (tests) can only create the session-local one.
const MAPPING_NAMES: [&str; 2] = ["Global\\RelayCameraFrames", "Local\\RelayCameraFrames"];
const MAGIC: u32 = 0x4D41_4352; // "RCAM"
const VERSION: u32 = 1;
const SLOT_COUNT: u32 = 3;
const FOURCC_NV12: u32 = 0x3231_564E;
/// The media source stopped asking for frames this long ago: nobody is watching.
const READER_TIMEOUT_MS: u64 = 2000;

#[repr(C)]
struct Slot {
    seq: AtomicU32,
    width: AtomicU32,
    height: AtomicU32,
    pitch: AtomicU32,
    format: AtomicU32,
    reserved0: AtomicU32,
    frame: AtomicU64,
    write_tick: AtomicU64,
    timestamp_100ns: AtomicI64,
    reserved: [u8; 16],
}

#[repr(C)]
struct Header {
    magic: AtomicU32,
    version: AtomicU32,
    header_size: AtomicU32,
    slot_count: AtomicU32,
    slot_size: AtomicU32,
    max_width: AtomicU32,
    max_height: AtomicU32,
    reserved0: AtomicU32,
    active: AtomicU32,
    want_width: AtomicU32,
    want_height: AtomicU32,
    want_fps: AtomicU32,
    read_tick: AtomicU64,
    reserved1: AtomicU64,
    frame_counter: AtomicU64,
    latest_slot: AtomicU32,
    writer_pid: AtomicU32,
    reserved2: [u8; 48],
    slots: [Slot; SLOT_COUNT as usize],
}

const _: () = {
    assert!(size_of::<Slot>() == 64);
    assert!(offset_of!(Header, active) == 32);
    assert!(offset_of!(Header, read_tick) == 48);
    assert!(offset_of!(Header, frame_counter) == 64);
    assert!(offset_of!(Header, slots) == 128);
};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn com_init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    super::encode::startup();
}

/// Windows 11 (build 22000) or later has the virtual camera API.
pub fn supported() -> bool {
    use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
    unsafe {
        LoadLibraryW(windows::core::w!("mfsensorgroup.dll"))
            .ok()
            .and_then(|m| GetProcAddress(m, windows::core::s!("MFCreateVirtualCamera")))
            .is_some()
    }
}

/// Where the COM registration says the media source DLL lives, if it's
/// registered and the file is there.
pub fn installed_dll() -> Option<PathBuf> {
    let key = wide(&format!("SOFTWARE\\Classes\\CLSID\\{CLSID}\\InprocServer32"));
    let mut buf = vec![0u16; 1024];
    let mut len = (buf.len() * 2) as u32;
    let r = unsafe {
        RegGetValueW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(key.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buf.as_mut_ptr().cast()),
            Some(&mut len),
        )
    };
    if r.is_err() {
        return None;
    }
    let n = (len as usize / 2).saturating_sub(1);
    let path = PathBuf::from(String::from_utf16_lossy(&buf[..n.min(buf.len())]).trim_end_matches('\0'));
    path.is_file().then_some(path)
}

/// The COM class is registered (the admin step is done).
pub fn com_registered() -> bool {
    installed_dll().is_some()
}

/// The installed media source is byte-for-byte `packaged` (no reinstall needed).
pub fn install_matches(packaged: &Path) -> bool {
    match (installed_dll(), std::fs::read(packaged)) {
        (Some(inst), Ok(pkg)) => std::fs::read(inst).map(|i| i == pkg).unwrap_or(false),
        _ => false,
    }
}

/// Friendly names of the video capture devices Media Foundation can see.
pub fn video_devices() -> Result<Vec<String>> {
    com_init();
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1)?;
        let attrs = attrs.context("no attributes")?;
        attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut list, &mut count)?;
        let mut names = Vec::new();
        for i in 0..count as usize {
            let item = std::ptr::read(list.add(i));
            if let Some(a) = item {
                let mut p = PWSTR::null();
                let mut n = 0u32;
                if a.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut p, &mut n).is_ok() {
                    names.push(p.to_string().unwrap_or_default());
                    CoTaskMemFree(Some(p.0 as *const _));
                }
            }
        }
        CoTaskMemFree(Some(list as *const _));
        Ok(names)
    }
}

/// "Relay Camera" is enumerable as a video capture device.
pub fn device_present() -> bool {
    video_devices().map(|v| v.iter().any(|n| n.starts_with(FRIENDLY_NAME))).unwrap_or(false)
}

/// The camera is set up and visible to apps.
pub fn is_registered() -> bool {
    com_registered() && device_present()
}

fn virtual_camera() -> Result<IMFVirtualCamera> {
    com_init();
    let name = HSTRING::from(FRIENDLY_NAME);
    let clsid = HSTRING::from(CLSID);
    unsafe {
        MFCreateVirtualCamera(
            MFVirtualCameraType_SoftwareCameraSource,
            MFVirtualCameraLifetime_System,
            MFVirtualCameraAccess_CurrentUser,
            &name,
            &clsid,
            None,
        )
        .context("MFCreateVirtualCamera failed (is camera access allowed in Privacy settings?)")
    }
}

/// Creates (or re-opens) the per-user "Relay Camera" device and enables it.
/// Doesn't need admin, but the media source must be registered first.
pub fn register() -> Result<()> {
    if !com_registered() {
        bail!("the Relay Camera media source isn't installed");
    }
    create_device()
}

/// `MFCreateVirtualCamera` + `Start`, without checking the COM registration.
pub fn create_device() -> Result<()> {
    let cam = virtual_camera()?;
    unsafe { cam.Start(None).context("starting the virtual camera failed")? };
    // System lifetime: the camera stays after we let go of it.
    Ok(())
}

/// Removes the per-user camera device.
pub fn unregister() -> Result<()> {
    let cam = virtual_camera()?;
    unsafe { cam.Remove().context("removing the virtual camera failed") }
}

/// Registers the camera device if the media source is installed but the
/// device is missing. Returns whether the camera is usable.
pub fn ensure_registered() -> Result<bool> {
    if !com_registered() {
        return Ok(false);
    }
    if device_present() {
        return Ok(true);
    }
    register()?;
    // Device interfaces show up asynchronously.
    for _ in 0..20 {
        if device_present() {
            return Ok(true);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Ok(device_present())
}

struct Mapping {
    handle: HANDLE,
    view: MEMORY_MAPPED_VIEW_ADDRESS,
    size: usize,
    slot_size: usize,
    header_size: usize,
}

unsafe impl Send for Mapping {}

impl Mapping {
    fn open() -> Option<Mapping> {
        MAPPING_NAMES.iter().find_map(|n| Self::open_named(n))
    }

    fn open_named(name: &str) -> Option<Mapping> {
        let name = wide(name);
        unsafe {
            let handle = OpenFileMappingW((FILE_MAP_READ | FILE_MAP_WRITE).0, false, PCWSTR(name.as_ptr())).ok()?;
            let view = MapViewOfFile(handle, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, 0);
            if view.Value.is_null() {
                let _ = CloseHandle(handle);
                return None;
            }
            let mut info = MEMORY_BASIC_INFORMATION::default();
            VirtualQuery(Some(view.Value), &mut info, size_of::<MEMORY_BASIC_INFORMATION>());
            let mut m = Mapping { handle, view, size: info.RegionSize, slot_size: 0, header_size: 0 };
            let h = m.header();
            let ok = m.size >= size_of::<Header>()
                && h.magic.load(Ordering::Acquire) == MAGIC
                && h.version.load(Ordering::Relaxed) == VERSION
                && h.slot_count.load(Ordering::Relaxed) == SLOT_COUNT;
            if !ok {
                tracing::debug!("camera mapping has an unknown layout");
                return None;
            }
            let (slot_size, header_size) =
                (h.slot_size.load(Ordering::Relaxed) as usize, h.header_size.load(Ordering::Relaxed) as usize);
            m.slot_size = slot_size;
            m.header_size = header_size;
            if m.header_size < size_of::<Header>() || m.header_size + m.slot_size * SLOT_COUNT as usize > m.size {
                return None;
            }
            Some(m)
        }
    }

    fn header(&self) -> &Header {
        unsafe { &*(self.view.Value as *const Header) }
    }

    fn slot_data(&self, i: usize) -> *mut u8 {
        unsafe { (self.view.Value as *mut u8).add(self.header_size + i * self.slot_size) }
    }

    /// The size an app is currently reading at, if any.
    fn wanted(&self) -> Option<(u32, u32, u32)> {
        let h = self.header();
        if h.active.load(Ordering::Acquire) == 0 {
            return None;
        }
        let now = unsafe { GetTickCount64() };
        if now.saturating_sub(h.read_tick.load(Ordering::Relaxed)) > READER_TIMEOUT_MS {
            return None;
        }
        let (w, h2, fps) = (
            h.want_width.load(Ordering::Relaxed),
            h.want_height.load(Ordering::Relaxed),
            h.want_fps.load(Ordering::Relaxed),
        );
        let fits = w > 0 && h2 > 0 && w % 2 == 0 && h2 % 2 == 0 && (w as usize * h2 as usize * 3 / 2) <= self.slot_size;
        fits.then_some((w, h2, fps.clamp(1, 120)))
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe {
            let _ = UnmapViewOfFile(self.view);
            let _ = CloseHandle(self.handle);
        }
    }
}

/// Letterboxes `src` into `dst`, centred on even pixel boundaries (NV12).
fn fit_even(src: (u32, u32), dst: (u32, u32)) -> RECT {
    let (sw, sh) = (src.0.max(1) as u64, src.1.max(1) as u64);
    let (dw, dh) = (dst.0 as u64, dst.1 as u64);
    let (w, h) = if sw * dh >= sh * dw {
        (dw, (sh * dw + sw / 2) / sw) // wider than the camera: bars top and bottom
    } else {
        ((sw * dh + sh / 2) / sh, dh)
    };
    // Multiples of 4 keep both margins even (camera sizes are multiples of 4).
    let (w, h) = ((w as i32) & !3, (h as i32) & !3);
    let (x, y) = (((dst.0 as i32 - w) / 2) & !1, ((dst.1 as i32 - h) / 2) & !1);
    RECT { left: x, top: y, right: x + w, bottom: y + h }
}

struct Scaler {
    conv: Converter,
    src: (u32, u32),
    dst: (u32, u32),
}

struct Target {
    size: (u32, u32),
    gpu_tex: ID3D11Texture2D,
    staging: ID3D11Texture2D,
}

/// Publishes decoded frames to the Relay Camera. Cheap when no app is using
/// the camera: one atomic load per frame, and a mapping lookup once a second.
pub struct CameraFeed {
    map: Option<Mapping>,
    last_open: Option<Instant>,
    /// Scaler and the (source, camera) sizes it was made for.
    converter: Option<Scaler>,
    target: Option<Target>,
    frames: u64,
    last_push: Option<Instant>,
}

impl Default for CameraFeed {
    fn default() -> Self {
        Self::new()
    }
}

impl CameraFeed {
    pub fn new() -> Self {
        Self { map: None, last_open: None, converter: None, target: None, frames: 0, last_push: None }
    }

    /// The (width, height, fps) an app is reading the camera at, if one is.
    pub fn wanted(&mut self) -> Option<(u32, u32, u32)> {
        if self.map.is_none() && self.last_open.map(|t| t.elapsed() > Duration::from_secs(1)).unwrap_or(true) {
            self.last_open = Some(Instant::now());
            self.map = Mapping::open();
        }
        let w = self.map.as_ref()?.wanted();
        if w.is_none() && self.target.is_some() {
            // Release GPU resources while nobody watches.
            tracing::info!("Relay Camera: no app is reading it any more");
            self.converter = None;
            self.target = None;
        }
        w
    }

    /// True if a frame was pushed less than `max_age` ago.
    pub fn pushed_within(&self, max_age: Duration) -> bool {
        self.last_push.map(|t| t.elapsed() < max_age).unwrap_or(false)
    }

    /// Scales `texture` (array slice `slice`, `width` x `height` NV12) to the
    /// camera's size and publishes it. Returns false when no app is reading
    /// the camera (nothing was done) or the frame was skipped to hold the
    /// camera's frame rate.
    pub(crate) fn push(
        &mut self,
        gpu: &Gpu,
        texture: &ID3D11Texture2D,
        slice: u32,
        width: u32,
        height: u32,
        timestamp_100ns: i64,
    ) -> Result<bool> {
        let Some((cw, ch, fps)) = self.wanted() else { return Ok(false) };
        // Hold roughly the camera's rate; the camera repeats frames itself.
        let period = Duration::from_micros(1_000_000 / fps as u64);
        if let Some(t) = self.last_push
            && t.elapsed() + Duration::from_millis(4) < period
        {
            return Ok(false);
        }
        let src = (width, height);
        let dst = (cw, ch);
        if self.converter.as_ref().map(|c| c.src != src || c.dst != dst).unwrap_or(true) {
            let c = Converter::with_spaces(gpu, src, dst, fps, Spaces::YuvToYuv).context("camera scaler")?;
            c.set_dest(fit_even(src, dst));
            self.converter = Some(Scaler { conv: c, src, dst });
        }
        if self.target.as_ref().map(|t| t.size != dst).unwrap_or(true) {
            let gpu_tex = convert::texture(gpu, cw, ch, DXGI_FORMAT_NV12, D3D11_BIND_RENDER_TARGET)?;
            let desc = D3D11_TEXTURE2D_DESC {
                Width: cw,
                Height: ch,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_NV12,
                SampleDesc: windows::Win32::Graphics::Dxgi::Common::DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging = None;
            unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut staging))? };
            tracing::info!("Relay Camera: an app is reading it at {cw}x{ch} {fps} fps; feeding {width}x{height}");
            self.target = Some(Target { size: dst, gpu_tex, staging: staging.context("staging texture")? });
        }
        let (Some(Scaler { conv, .. }), Some(target), Some(map)) = (&self.converter, &self.target, &self.map) else {
            return Ok(false);
        };
        conv.convert(texture, slice, &target.gpu_tex)?;
        unsafe {
            gpu.context.CopyResource(&target.staging, &target.gpu_tex);
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            gpu.context.Map(&target.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped))?;
            let src_pitch = mapped.RowPitch as usize;
            let base = mapped.pData as *const u8;
            let h = map.header();
            let latest = h.latest_slot.load(Ordering::Relaxed);
            let index = (if h.frame_counter.load(Ordering::Relaxed) == 0 { 0 } else { latest + 1 }) % SLOT_COUNT;
            let slot = &h.slots[index as usize];
            let out = map.slot_data(index as usize);
            let (w, hh) = (cw as usize, ch as usize);
            slot.seq.fetch_add(1, Ordering::AcqRel); // odd: writing
            for y in 0..hh {
                std::ptr::copy_nonoverlapping(base.add(y * src_pitch), out.add(y * w), w);
            }
            let uv_src = base.add(src_pitch * hh);
            let uv_dst = out.add(w * hh);
            for y in 0..hh / 2 {
                std::ptr::copy_nonoverlapping(uv_src.add(y * src_pitch), uv_dst.add(y * w), w);
            }
            gpu.context.Unmap(&target.staging, 0);
            self.frames += 1;
            slot.width.store(cw, Ordering::Relaxed);
            slot.height.store(ch, Ordering::Relaxed);
            slot.pitch.store(cw, Ordering::Relaxed);
            slot.format.store(FOURCC_NV12, Ordering::Relaxed);
            slot.frame.store(self.frames, Ordering::Relaxed);
            slot.write_tick.store(GetTickCount64(), Ordering::Relaxed);
            slot.timestamp_100ns.store(timestamp_100ns, Ordering::Relaxed);
            slot.seq.fetch_add(1, Ordering::Release); // even: done
            h.latest_slot.store(index, Ordering::Release);
            h.writer_pid.store(std::process::id(), Ordering::Relaxed);
            h.frame_counter.fetch_add(1, Ordering::Release);
        }
        self.last_push = Some(Instant::now());
        Ok(true)
    }
}

/// Which viewer feeds the camera: the most recently started stream wins, and
/// when it ends any other stream takes over.
pub(crate) mod owner {
    use std::sync::atomic::{AtomicU64, Ordering};
    static OWNER: AtomicU64 = AtomicU64::new(0);
    static NEXT: AtomicU64 = AtomicU64::new(1);

    pub fn new_id() -> u64 {
        NEXT.fetch_add(1, Ordering::Relaxed)
    }
    /// A new stream started: it takes the camera.
    pub fn claim(id: u64) {
        OWNER.store(id, Ordering::Relaxed);
    }
    /// Takes the camera if nobody has it; returns whether `id` has it.
    pub fn check(id: u64) -> bool {
        let _ = OWNER.compare_exchange(0, id, Ordering::Relaxed, Ordering::Relaxed);
        OWNER.load(Ordering::Relaxed) == id
    }
    pub fn release(id: u64) {
        let _ = OWNER.compare_exchange(id, 0, Ordering::Relaxed, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letterbox_is_even_and_centred() {
        let r = fit_even((1280, 1024), (1920, 1080));
        assert_eq!((r.top, r.bottom), (0, 1080));
        assert_eq!(r.left % 2, 0);
        assert_eq!(r.right % 2, 0);
        assert_eq!(r.left, 1920 - r.right);
        let r = fit_even((1920, 1080), (1280, 720));
        assert_eq!((r.left, r.top, r.right, r.bottom), (0, 0, 1280, 720));
    }

    #[test]
    fn newest_stream_owns_the_camera() {
        let (a, b) = (owner::new_id(), owner::new_id());
        owner::claim(a);
        assert!(owner::check(a));
        owner::claim(b);
        assert!(!owner::check(a));
        owner::release(b);
        assert!(owner::check(a));
        owner::release(a);
    }
}

//! Windows video: capture, encode, decode and present.

pub mod camera;
mod capture;
mod convert;
pub mod decode;
mod encode;
pub mod viewer;

use crate::{Capture, FrameSink, Settings};
use relay_proto::{VideoSource, VideoSourceKind};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_CREATE_DEVICE_VIDEO_SUPPORT, D3D11_SDK_VERSION, D3D11CreateDevice,
    ID3D11Device, ID3D11DeviceContext, ID3D11Multithread,
};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter1, IDXGIFactory1, IDXGIOutput};
use windows::core::Interface;

pub(crate) struct Gpu {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
}

pub(crate) fn create_device(adapter: Option<&IDXGIAdapter1>) -> anyhow::Result<Gpu> {
    let mut device = None;
    let mut context = None;
    unsafe {
        D3D11CreateDevice(
            adapter.map(|a| a.cast::<windows::Win32::Graphics::Dxgi::IDXGIAdapter>()).transpose()?.as_ref(),
            if adapter.is_some() {
                D3D_DRIVER_TYPE_UNKNOWN
            } else {
                windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE
            },
            windows::Win32::Foundation::HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT | D3D11_CREATE_DEVICE_VIDEO_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
    }
    let device: ID3D11Device = device.ok_or_else(|| anyhow::anyhow!("no D3D11 device"))?;
    let context: ID3D11DeviceContext = context.ok_or_else(|| anyhow::anyhow!("no D3D11 context"))?;
    // Media Foundation uses the device from its own threads.
    if let Ok(mt) = device.cast::<ID3D11Multithread>() {
        unsafe {
            let _ = mt.SetMultithreadProtected(true);
        }
    }
    Ok(Gpu { device, context })
}

pub(crate) fn outputs() -> Vec<(IDXGIAdapter1, IDXGIOutput, String, u32, u32, usize, usize)> {
    let mut out = Vec::new();
    let Ok(factory) = (unsafe { CreateDXGIFactory1::<IDXGIFactory1>() }) else { return out };
    let mut ai = 0;
    while let Ok(adapter) = unsafe { factory.EnumAdapters1(ai) } {
        let mut oi = 0;
        while let Ok(output) = unsafe { adapter.EnumOutputs(oi) } {
            if let Ok(desc) = unsafe { output.GetDesc() }
                && desc.AttachedToDesktop.as_bool()
            {
                let r = desc.DesktopCoordinates;
                let (w, h) = ((r.right - r.left) as u32, (r.bottom - r.top) as u32);
                let name = String::from_utf16_lossy(&desc.DeviceName).trim_end_matches('\0').to_string();
                out.push((adapter.clone(), output, name, w, h, ai as usize, oi as usize));
            }
            oi += 1;
        }
        ai += 1;
    }
    out
}

pub fn sources() -> Vec<VideoSource> {
    let outs = outputs();
    let many = outs.len() > 1;
    outs.iter()
        .enumerate()
        .map(|(i, (_, _, _, w, h, ai, oi))| VideoSource {
            key: format!("screen:{ai}:{oi}"),
            name: if many { format!("Screen {} ({w}×{h})", i + 1) } else { format!("Screen ({w}×{h})") },
            kind: VideoSourceKind::Screen,
        })
        .collect()
}

pub fn start(source: &str, settings: Settings, sink: FrameSink) -> anyhow::Result<Box<dyn Capture>> {
    let mut parts = source.split(':');
    match parts.next() {
        Some("screen") => {
            let ai: usize = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            let oi: usize = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            capture::start_screen(ai, oi, settings, sink)
        }
        _ => anyhow::bail!("unknown video source {source}"),
    }
}

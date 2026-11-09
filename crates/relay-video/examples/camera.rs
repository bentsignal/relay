//! Relay Camera checks (Windows).
//!
//! cargo run -p relay-video --example camera -- status
//! cargo run -p relay-video --example camera -- register | unregister
//! cargo run -p relay-video --example camera -- dshow
//!     lists DirectShow video capture devices (what OBS's "Video Capture
//!     Device" and other DirectShow apps see).
//! cargo run -p relay-video --example camera -- grab [out.bmp] [seconds] [height]
//!     opens "Relay Camera" like an app would, reads frames for a few seconds,
//!     prints the formats, frame rate and picture statistics, and saves the
//!     last frame. Run the `loopback` example at the same time to feed it.
//! cargo run -p relay-video --example camera -- grab-inproc [out.bmp] [seconds]
//!     the same, but loads the media source into this process through its COM
//!     registration (HKCU is enough) instead of going through Frame Server.

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    use relay_video::windows::camera;
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str).unwrap_or("status") {
        "status" => {
            println!("api supported:   {}", camera::supported());
            println!("com registered:  {:?}", camera::installed_dll());
            println!("video devices:   {:?}", camera::video_devices()?);
            println!("camera present:  {}", camera::device_present());
        }
        "register" => {
            camera::register()?;
            println!("registered; present: {}", camera::ensure_registered()?);
        }
        "create-device" => {
            camera::create_device()?;
            println!("created; present: {}", camera::device_present());
        }
        "dshow" => {
            for name in dshow::devices()? {
                println!("{name}");
            }
        }
        "unregister" => {
            camera::unregister()?;
            println!("removed");
        }
        cmd @ ("grab" | "grab-inproc") => {
            let out = args.get(2).cloned().unwrap_or_else(|| "relay-camera.bmp".into());
            let secs: f64 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(3.0);
            let height: Option<u32> = args.get(4).and_then(|s| s.parse().ok());
            grab::run(&out, secs, cmd == "grab-inproc", height)?;
        }
        other => anyhow::bail!("unknown command {other}"),
    }
    Ok(())
}

#[cfg(not(windows))]
fn main() {
    eprintln!("the Relay Camera is Windows-only");
}

#[cfg(windows)]
mod dshow {
    use windows::Win32::Media::DirectShow::ICreateDevEnum;
    use windows::Win32::Media::MediaFoundation::{CLSID_SystemDeviceEnum, CLSID_VideoInputDeviceCategory};
    use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
    use windows::Win32::System::Com::*;
    use windows::Win32::System::Variant::VARIANT;

    pub fn devices() -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            let de: ICreateDevEnum = CoCreateInstance(&CLSID_SystemDeviceEnum, None, CLSCTX_INPROC_SERVER)?;
            let mut em = None;
            de.CreateClassEnumerator(&CLSID_VideoInputDeviceCategory, &mut em, 0)?;
            let Some(em) = em else { return Ok(out) };
            loop {
                let mut m = [None];
                if em.Next(&mut m, None).is_err() {
                    break;
                }
                let Some(moniker) = m[0].take() else { break };
                let bag: IPropertyBag = moniker.BindToStorage(None, None)?;
                let mut v = VARIANT::default();
                if bag.Read(windows::core::w!("FriendlyName"), &mut v, None).is_ok() {
                    out.push(v.to_string());
                }
            }
        }
        Ok(out)
    }
}

#[cfg(windows)]
mod grab {
    use anyhow::{Context, Result, bail};
    use std::time::Instant;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{COINIT_MULTITHREADED, CoInitializeEx, CoTaskMemFree};
    use windows::core::PWSTR;

    fn guid_name(g: &windows::core::GUID) -> String {
        if *g == MFVideoFormat_NV12 {
            "NV12".into()
        } else if *g == MFVideoFormat_YUY2 {
            "YUY2".into()
        } else if *g == MFVideoFormat_RGB32 {
            "RGB32".into()
        } else {
            format!("{g:?}")
        }
    }

    pub fn run(out: &str, secs: f64, inproc: bool, height: Option<u32>) -> Result<()> {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            MFStartup(MF_VERSION, MFSTARTUP_FULL)?;
            let mut attrs = None;
            MFCreateAttributes(&mut attrs, 1)?;
            let attrs = attrs.context("attributes")?;
            attrs.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
            let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
            let mut count = 0u32;
            MFEnumDeviceSources(&attrs, &mut list, &mut count)?;
            let mut found = None;
            for i in 0..count as usize {
                if let Some(a) = std::ptr::read(list.add(i)) {
                    let mut p = PWSTR::null();
                    let mut n = 0;
                    a.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, &mut p, &mut n)?;
                    let name = p.to_string()?;
                    CoTaskMemFree(Some(p.0 as *const _));
                    let mut p = PWSTR::null();
                    if a.GetAllocatedString(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK, &mut p, &mut n)
                        .is_ok()
                    {
                        println!("device: {name}  ({})", p.to_string()?);
                        CoTaskMemFree(Some(p.0 as *const _));
                    }
                    if name.starts_with(relay_video::windows::camera::FRIENDLY_NAME) && found.is_none() {
                        found = Some(a);
                    }
                }
            }
            CoTaskMemFree(Some(list as *const _));
            if inproc {
                let clsid =
                    windows::core::GUID::try_from(relay_video::windows::camera::CLSID.trim_matches(['{', '}']))?;
                found = Some(windows::Win32::System::Com::CoCreateInstance(
                    &clsid,
                    None,
                    windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
                )?);
                println!("loaded the media source in-process");
            }
            let Some(activate) = found else { bail!("Relay Camera not found") };
            let source: IMFMediaSource = activate.ActivateObject().context("activating Relay Camera")?;
            let reader = MFCreateSourceReaderFromMediaSource(&source, None)?;
            let stream = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            let mut i = 0;
            let mut chosen = None;
            while let Ok(t) = reader.GetNativeMediaType(stream, i) {
                let sub = t.GetGUID(&MF_MT_SUBTYPE)?;
                let size = t.GetUINT64(&MF_MT_FRAME_SIZE)?;
                let rate = t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0);
                let (w, h) = ((size >> 32) as u32, size as u32);
                println!("format {i}: {} {w}x{h} @ {}/{}", guid_name(&sub), rate >> 32, rate as u32);
                if chosen.is_none() && sub == MFVideoFormat_NV12 && height.is_none_or(|want| want == h) {
                    chosen = Some((t, w, h));
                }
                i += 1;
            }
            let Some((t, w, h)) = chosen else { bail!("no NV12 format") };
            reader.SetCurrentMediaType(stream, None, &t)?;
            println!("reading {w}x{h} NV12 for {secs} s");

            let start = Instant::now();
            let mut frames = 0u32;
            let mut first_ts = None;
            let mut last_ts = 0i64;
            let mut last: Option<IMFSample> = None;
            while start.elapsed().as_secs_f64() < secs {
                let mut flags = 0u32;
                let mut ts = 0i64;
                let mut sample = None;
                reader.ReadSample(stream, 0, None, Some(&mut flags), Some(&mut ts), Some(&mut sample))?;
                if let Some(s) = sample {
                    frames += 1;
                    first_ts.get_or_insert(ts);
                    last_ts = ts;
                    last = Some(s);
                }
            }
            let wall = start.elapsed().as_secs_f64();
            let span = (last_ts - first_ts.unwrap_or(0)) as f64 / 1e7;
            println!("{frames} frames in {wall:.2} s = {:.1} fps (timestamps span {span:.2} s)", frames as f64 / wall);
            let sample = last.context("no frames")?;
            let buf = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut ptr, None, Some(&mut len))?;
            let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buf.Unlock()?;
            let (w, h) = (w as usize, h as usize);
            if data.len() < w * h * 3 / 2 {
                bail!("short frame: {} bytes", data.len());
            }
            let y = &data[..w * h];
            let mean = y.iter().map(|&v| v as u64).sum::<u64>() as f64 / y.len() as f64;
            let lit = y.iter().filter(|&&v| v > 24).count() as f64 / y.len() as f64;
            let mut uniq = std::collections::HashSet::new();
            for v in y.iter().step_by(97) {
                uniq.insert(*v);
            }
            println!(
                "last frame: mean luma {mean:.1}, {:.1}% of pixels above black, {} distinct luma values sampled",
                lit * 100.0,
                uniq.len()
            );
            write_bmp(out, &data, w, h)?;
            println!("saved {out}");
            let _ = source.Shutdown();
        }
        Ok(())
    }

    /// NV12 (BT.709 studio range) to a 24-bit BMP.
    fn write_bmp(path: &str, nv12: &[u8], w: usize, h: usize) -> Result<()> {
        let row = (w * 3 + 3) & !3;
        let mut out = Vec::with_capacity(54 + row * h);
        let size = (54 + row * h) as u32;
        out.extend_from_slice(b"BM");
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&[0; 4]);
        out.extend_from_slice(&54u32.to_le_bytes());
        out.extend_from_slice(&40u32.to_le_bytes());
        out.extend_from_slice(&(w as i32).to_le_bytes());
        out.extend_from_slice(&(h as i32).to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&24u16.to_le_bytes());
        out.extend_from_slice(&[0; 24]);
        let uv = &nv12[w * h..];
        for yy in (0..h).rev() {
            let start = out.len();
            for x in 0..w {
                let y = nv12[yy * w + x] as f32 - 16.0;
                let u = uv[(yy / 2) * w + (x & !1)] as f32 - 128.0;
                let v = uv[(yy / 2) * w + (x & !1) + 1] as f32 - 128.0;
                let c = |f: f32| f.clamp(0.0, 255.0) as u8;
                let r = c(1.164 * y + 1.793 * v);
                let g = c(1.164 * y - 0.213 * u - 0.533 * v);
                let b = c(1.164 * y + 2.112 * u);
                out.extend_from_slice(&[b, g, r]);
            }
            out.resize(start + row, 0);
        }
        std::fs::write(path, out)?;
        Ok(())
    }
}

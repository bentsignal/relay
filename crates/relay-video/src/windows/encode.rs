//! Media Foundation hardware H.264 encoder (asynchronous MFT) fed with NV12
//! textures straight from the GPU.

use super::Gpu;
use anyhow::{Context, Result, bail};
use std::mem::ManuallyDrop;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::System::Variant::VARIANT;
use windows::core::Interface;

pub struct EncodedOut {
    pub data: Vec<u8>,
    pub key: bool,
    pub time_100ns: i64,
}

pub struct Encoder {
    mft: IMFTransform,
    events: IMFMediaEventGenerator,
    codec: Option<ICodecAPI>,
    _manager: IMFDXGIDeviceManager,
    pending_input: u32,
    provides_samples: bool,
    out_size: u32,
    seq_header: Vec<u8>,
    fps: u32,
    pub name: String,
}

pub fn startup() {
    static ONCE: std::sync::Once = std::sync::Once::new();
    ONCE.call_once(|| unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        let _ = MFStartup(MF_VERSION, MFSTARTUP_FULL);
    });
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

fn set_codec(codec: &ICodecAPI, key: &windows::core::GUID, v: u32) {
    unsafe {
        let _ = codec.SetValue(key, &VARIANT::from(v));
    }
}

pub fn device_manager(gpu: &Gpu) -> Result<IMFDXGIDeviceManager> {
    let mut token = 0u32;
    let mut manager = None;
    unsafe {
        MFCreateDXGIDeviceManager(&mut token, &mut manager)?;
        let manager = manager.context("no DXGI device manager")?;
        manager.ResetDevice(&gpu.device, token)?;
        Ok(manager)
    }
}

impl Encoder {
    pub fn new(gpu: &Gpu, width: u32, height: u32, fps: u32, kbps: u32) -> Result<Self> {
        startup();
        let manager = device_manager(gpu)?;
        let input = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_NV12 };
        let output = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: MFVideoFormat_H264 };
        let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        unsafe {
            MFTEnumEx(
                MFT_CATEGORY_VIDEO_ENCODER,
                MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER,
                Some(&input),
                Some(&output),
                &mut activates,
                &mut count,
            )?;
        }
        let list: Vec<IMFActivate> = unsafe {
            let slice = std::slice::from_raw_parts_mut(activates, count as usize);
            let v = slice.iter_mut().filter_map(|a| a.take()).collect();
            CoTaskMemFree(Some(activates as *const _));
            v
        };
        let mut last_err = anyhow::anyhow!("no hardware H.264 encoder");
        for act in list {
            match Self::try_activate(&act, &manager, width, height, fps, kbps) {
                Ok(enc) => return Ok(enc),
                Err(e) => {
                    tracing::info!("encoder candidate failed: {e:#}");
                    last_err = e;
                    unsafe {
                        let _ = act.ShutdownObject();
                    }
                }
            }
        }
        Err(last_err)
    }

    fn try_activate(
        act: &IMFActivate,
        manager: &IMFDXGIDeviceManager,
        width: u32,
        height: u32,
        fps: u32,
        kbps: u32,
    ) -> Result<Self> {
        unsafe {
            let name = {
                let mut buf = [0u16; 256];
                let mut len = 0u32;
                if act.GetString(&MFT_FRIENDLY_NAME_Attribute, &mut buf, Some(&mut len)).is_ok() {
                    String::from_utf16_lossy(&buf[..len as usize])
                } else {
                    "Hardware encoder".into()
                }
            };
            let mft: IMFTransform = act.ActivateObject()?;
            let attrs = mft.GetAttributes()?;
            attrs.SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1)?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;

            let out = MFCreateMediaType()?;
            out.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            out.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            out.SetUINT32(&MF_MT_AVG_BITRATE, kbps * 1000)?;
            out.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            out.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            out.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            out.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)?;
            mft.SetOutputType(0, &out, 0)?;

            let inp = MFCreateMediaType()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_NV12)?;
            inp.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            inp.SetUINT64(&MF_MT_FRAME_RATE, pack(fps, 1))?;
            inp.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &inp, 0)?;

            let codec = mft.cast::<ICodecAPI>().ok();
            if let Some(c) = &codec {
                set_codec(c, &CODECAPI_AVLowLatencyMode, 1);
                set_codec(c, &CODECAPI_AVEncCommonRateControlMode, eAVEncCommonRateControlMode_LowDelayVBR.0 as u32);
                set_codec(c, &CODECAPI_AVEncCommonMeanBitRate, kbps * 1000);
                set_codec(c, &CODECAPI_AVEncMPVGOPSize, fps * 4);
                set_codec(c, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
            }

            let info = mft.GetOutputStreamInfo(0)?;
            let provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
            let seq_header = mft
                .GetOutputCurrentType(0)
                .ok()
                .and_then(|t| {
                    let size = t.GetBlobSize(&MF_MT_MPEG_SEQUENCE_HEADER).ok()?;
                    let mut buf = vec![0u8; size as usize];
                    t.GetBlob(&MF_MT_MPEG_SEQUENCE_HEADER, &mut buf, None).ok()?;
                    Some(buf)
                })
                .unwrap_or_default();

            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            let events: IMFMediaEventGenerator = mft.cast()?;
            tracing::info!(encoder = %name, width, height, fps, kbps, "video encoder ready");
            Ok(Self {
                mft,
                events,
                codec,
                _manager: manager.clone(),
                pending_input: 0,
                provides_samples,
                out_size: info.cbSize.max(1 << 20),
                seq_header,
                fps,
                name,
            })
        }
    }

    /// Polls encoder events. Returns encoded frames that became available.
    pub fn poll(&mut self, out: &mut Vec<EncodedOut>) -> Result<()> {
        loop {
            let ev = match unsafe { self.events.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(ev) => ev,
                Err(e) if e.code() == MF_E_NO_EVENTS_AVAILABLE => return Ok(()),
                Err(e) => return Err(e.into()),
            };
            let kind = unsafe { ev.GetType()? };
            if kind == METransformNeedInput.0 as u32 {
                self.pending_input += 1;
            } else if kind == METransformHaveOutput.0 as u32 {
                self.output(out)?;
            }
        }
    }

    pub fn wants_input(&self) -> bool {
        self.pending_input > 0
    }

    pub fn encode(&mut self, nv12: &ID3D11Texture2D, time_100ns: i64, force_key: bool) -> Result<()> {
        if self.pending_input == 0 {
            bail!("encoder isn't ready for input");
        }
        unsafe {
            let buffer = MFCreateDXGISurfaceBuffer(&ID3D11Texture2D::IID, nv12, 0, false)?;
            if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
                buffer.SetCurrentLength(b2.GetContiguousLength()?)?;
            }
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time_100ns)?;
            sample.SetSampleDuration(10_000_000 / self.fps as i64)?;
            if force_key && let Some(c) = &self.codec {
                set_codec(c, &CODECAPI_AVEncVideoForceKeyFrame, 1);
            }
            self.mft.ProcessInput(0, &sample, 0)?;
        }
        self.pending_input -= 1;
        Ok(())
    }

    fn output(&mut self, out: &mut Vec<EncodedOut>) -> Result<()> {
        unsafe {
            let provided = if self.provides_samples {
                None
            } else {
                let s = MFCreateSample()?;
                s.AddBuffer(&MFCreateMemoryBuffer(self.out_size)?)?;
                Some(s)
            };
            let mut buf = [MFT_OUTPUT_DATA_BUFFER {
                dwStreamID: 0,
                pSample: ManuallyDrop::new(provided),
                dwStatus: 0,
                pEvents: ManuallyDrop::new(None),
            }];
            let mut status = 0u32;
            let r = self.mft.ProcessOutput(0, &mut buf, &mut status);
            let sample = ManuallyDrop::take(&mut buf[0].pSample);
            let _events = ManuallyDrop::take(&mut buf[0].pEvents);
            if let Err(e) = r {
                if e.code() == MF_E_TRANSFORM_STREAM_CHANGE {
                    let t = self.mft.GetOutputAvailableType(0, 0)?;
                    self.mft.SetOutputType(0, &t, 0)?;
                    return Ok(());
                }
                if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT {
                    return Ok(());
                }
                return Err(e.into());
            }
            let Some(sample) = sample else { return Ok(()) };
            let key = sample.GetUINT32(&MFSampleExtension_CleanPoint).unwrap_or(0) != 0;
            let time = sample.GetSampleTime().unwrap_or(0);
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut ptr = std::ptr::null_mut();
            let mut len = 0u32;
            buffer.Lock(&mut ptr, None, Some(&mut len))?;
            let mut data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
            buffer.Unlock()?;
            if key && !starts_with_sps(&data) && !self.seq_header.is_empty() {
                let mut with = self.seq_header.clone();
                with.extend_from_slice(&data);
                data = with;
            }
            out.push(EncodedOut { data, key, time_100ns: time });
        }
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0);
            let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            if let Ok(shutdown) = self.mft.cast::<IMFShutdown>() {
                let _ = shutdown.Shutdown();
            }
        }
    }
}

/// True if the Annex B stream begins with an SPS NAL unit.
fn starts_with_sps(data: &[u8]) -> bool {
    let i = if data.starts_with(&[0, 0, 0, 1]) {
        4
    } else if data.starts_with(&[0, 0, 1]) {
        3
    } else {
        return false;
    };
    data.get(i).map(|b| b & 0x1f == 7).unwrap_or(false)
}

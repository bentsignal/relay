//! Media Foundation H.264 decoder producing NV12 textures (DXVA on the GPU).

use super::Gpu;
use super::encode::{device_manager, startup};
use anyhow::{Context, Result};
use std::mem::ManuallyDrop;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;
use windows::Win32::Media::MediaFoundation::*;
use windows::core::Interface;

pub struct Decoded {
    pub texture: ID3D11Texture2D,
    pub slice: u32,
    pub width: u32,
    pub height: u32,
}

pub struct Decoder {
    mft: IMFTransform,
    _manager: IMFDXGIDeviceManager,
    pub width: u32,
    pub height: u32,
    provides_samples: bool,
}

fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}

impl Decoder {
    pub fn new(gpu: &Gpu, width: u32, height: u32) -> Result<Self> {
        startup();
        let manager = device_manager(gpu)?;
        unsafe {
            let mft: IMFTransform = windows::Win32::System::Com::CoCreateInstance(
                &CLSID_MSH264DecoderMFT,
                None,
                windows::Win32::System::Com::CLSCTX_INPROC_SERVER,
            )
            .context("H.264 decoder unavailable")?;
            let attrs = mft.GetAttributes()?;
            let _ = attrs.SetUINT32(&MF_LOW_LATENCY, 1);
            if attrs.GetUINT32(&MF_SA_D3D11_AWARE).unwrap_or(0) != 0 {
                mft.ProcessMessage(MFT_MESSAGE_SET_D3D_MANAGER, manager.as_raw() as usize)?;
            }
            let inp = MFCreateMediaType()?;
            inp.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            inp.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)?;
            inp.SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))?;
            inp.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            mft.SetInputType(0, &inp, 0)?;
            let mut dec = Self { mft, _manager: manager, width, height, provides_samples: true };
            dec.pick_output()?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
            Ok(dec)
        }
    }

    fn pick_output(&mut self) -> Result<()> {
        unsafe {
            let mut i = 0;
            loop {
                let t = self.mft.GetOutputAvailableType(0, i).context("no NV12 output")?;
                if t.GetGUID(&MF_MT_SUBTYPE)? == MFVideoFormat_NV12 {
                    self.mft.SetOutputType(0, &t, 0)?;
                    if let Ok(size) = t.GetUINT64(&MF_MT_FRAME_SIZE) {
                        self.width = (size >> 32) as u32;
                        self.height = size as u32;
                    }
                    break;
                }
                i += 1;
            }
            let info = self.mft.GetOutputStreamInfo(0)?;
            self.provides_samples = info.dwFlags
                & ((MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0) as u32)
                != 0;
        }
        Ok(())
    }

    /// Feeds one access unit and returns any decoded pictures.
    pub fn decode(&mut self, data: &[u8], time_100ns: i64, out: &mut Vec<Decoded>) -> Result<()> {
        unsafe {
            let buffer = MFCreateMemoryBuffer(data.len() as u32)?;
            let mut ptr = std::ptr::null_mut();
            buffer.Lock(&mut ptr, None, None)?;
            std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
            buffer.Unlock()?;
            buffer.SetCurrentLength(data.len() as u32)?;
            let sample = MFCreateSample()?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(time_100ns)?;
            match self.mft.ProcessInput(0, &sample, 0) {
                Ok(()) => {}
                Err(e) if e.code() == MF_E_NOTACCEPTING => {
                    self.drain(out)?;
                    self.mft.ProcessInput(0, &sample, 0)?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        self.drain(out)
    }

    fn drain(&mut self, out: &mut Vec<Decoded>) -> Result<()> {
        loop {
            unsafe {
                let mut buf = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: ManuallyDrop::new(None),
                    dwStatus: 0,
                    pEvents: ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let r = self.mft.ProcessOutput(0, &mut buf, &mut status);
                let sample = ManuallyDrop::take(&mut buf[0].pSample);
                let _ = ManuallyDrop::take(&mut buf[0].pEvents);
                match r {
                    Ok(()) => {}
                    Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(()),
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        self.pick_output()?;
                        continue;
                    }
                    Err(e) => return Err(e.into()),
                }
                let Some(sample) = sample else { continue };
                let buffer = sample.GetBufferByIndex(0)?;
                let dxgi: IMFDXGIBuffer = buffer.cast().context("decoder didn't produce GPU frames")?;
                let mut tex: Option<ID3D11Texture2D> = None;
                dxgi.GetResource(&ID3D11Texture2D::IID, &mut tex as *mut _ as *mut *mut std::ffi::c_void)?;
                let slice = dxgi.GetSubresourceIndex()?;
                if let Some(texture) = tex {
                    out.push(Decoded { texture, slice, width: self.width, height: self.height });
                }
            }
        }
    }
}

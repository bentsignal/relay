//! GPU format conversion and scaling with the D3D11 video processor
//! (BGRA -> NV12 for encoding, NV12 -> BGRA for display, NV12 -> NV12 scaling
//! for the virtual camera).

use super::Gpu;
use anyhow::{Context, Result};
use windows::Win32::Foundation::{RECT, TRUE};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::core::Interface;

pub struct Converter {
    vdev: ID3D11VideoDevice,
    vctx: ID3D11VideoContext,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    pub src: (u32, u32),
    pub dst: (u32, u32),
}

/// Colour spaces the converter works between.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Spaces {
    /// Full-range sRGB in, BT.709 studio-range YCbCr out (capture -> encoder).
    RgbToYuv,
    /// BT.709 YCbCr in, full-range RGB out (decoder -> display).
    YuvToRgb,
    /// BT.709 YCbCr in and out (decoder -> virtual camera).
    YuvToYuv,
}

impl Converter {
    pub fn new(gpu: &Gpu, src: (u32, u32), dst: (u32, u32), fps: u32, to_yuv: bool) -> Result<Self> {
        Self::with_spaces(gpu, src, dst, fps, if to_yuv { Spaces::RgbToYuv } else { Spaces::YuvToRgb })
    }

    pub fn with_spaces(gpu: &Gpu, src: (u32, u32), dst: (u32, u32), fps: u32, spaces: Spaces) -> Result<Self> {
        let vdev: ID3D11VideoDevice = gpu.device.cast().context("no video device")?;
        let vctx: ID3D11VideoContext = gpu.context.cast().context("no video context")?;
        let desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
            InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
            InputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
            InputWidth: src.0,
            InputHeight: src.1,
            OutputFrameRate: DXGI_RATIONAL { Numerator: fps, Denominator: 1 },
            OutputWidth: dst.0,
            OutputHeight: dst.1,
            Usage: D3D11_VIDEO_USAGE_OPTIMAL_SPEED,
        };
        let enumerator = unsafe { vdev.CreateVideoProcessorEnumerator(&desc)? };
        let processor = unsafe { vdev.CreateVideoProcessor(&enumerator, 0)? };
        unsafe {
            let src_rect = RECT { left: 0, top: 0, right: src.0 as i32, bottom: src.1 as i32 };
            let dst_rect = RECT { left: 0, top: 0, right: dst.0 as i32, bottom: dst.1 as i32 };
            vctx.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&src_rect));
            vctx.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dst_rect));
            vctx.VideoProcessorSetOutputTargetRect(&processor, true, Some(&dst_rect));
            vctx.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 { RGBA: D3D11_VIDEO_COLOR_RGBA { R: 0.0, G: 0.0, B: 0.0, A: 1.0 } },
            };
            vctx.VideoProcessorSetOutputBackgroundColor(&processor, false, &black);
            if let Ok(v1) = vctx.cast::<ID3D11VideoContext1>() {
                let (yuv, rgb) = (DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709);
                let (input, output) = match spaces {
                    Spaces::RgbToYuv => (rgb, yuv),
                    Spaces::YuvToRgb => (yuv, rgb),
                    Spaces::YuvToYuv => (yuv, yuv),
                };
                v1.VideoProcessorSetStreamColorSpace1(&processor, 0, input);
                v1.VideoProcessorSetOutputColorSpace1(&processor, output);
            }
        }
        Ok(Self { vdev, vctx, enumerator, processor, src, dst })
    }

    /// Draws into `rect` of the output (the rest is black).
    pub fn set_dest(&self, rect: RECT) {
        unsafe {
            self.vctx.VideoProcessorSetStreamDestRect(&self.processor, 0, true, Some(&rect));
        }
    }

    /// Converts `input` (array slice `slice`) into `output`.
    pub fn convert(&self, input: &ID3D11Texture2D, slice: u32, output: &ID3D11Texture2D) -> Result<()> {
        unsafe {
            let in_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: slice },
                },
            };
            let mut in_view = None;
            self.vdev.CreateVideoProcessorInputView(input, &self.enumerator, &in_desc, Some(&mut in_view))?;
            let out_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 } },
            };
            let mut out_view = None;
            self.vdev.CreateVideoProcessorOutputView(output, &self.enumerator, &out_desc, Some(&mut out_view))?;
            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: TRUE,
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(in_view),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };
            let streams = [stream];
            let r = self.vctx.VideoProcessorBlt(&self.processor, out_view.as_ref().unwrap(), 0, &streams);
            // Release the input view we put in the ManuallyDrop.
            let [mut s] = streams;
            std::mem::ManuallyDrop::drop(&mut s.pInputSurface);
            std::mem::ManuallyDrop::drop(&mut s.pInputSurfaceRight);
            r?;
        }
        Ok(())
    }
}

pub fn texture(gpu: &Gpu, w: u32, h: u32, format: DXGI_FORMAT, bind: D3D11_BIND_FLAG) -> Result<ID3D11Texture2D> {
    let desc = D3D11_TEXTURE2D_DESC {
        Width: w,
        Height: h,
        MipLevels: 1,
        ArraySize: 1,
        Format: format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: bind.0 as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let mut tex = None;
    unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex))? };
    tex.context("texture creation failed")
}

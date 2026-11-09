//! macOS video: ScreenCaptureKit / AVFoundation capture, VideoToolbox encode,
//! and an AVSampleBufferDisplayLayer viewer.

mod app;
mod capture;
mod encode;
mod ffi;
pub mod viewer;

pub use app::run_app;
pub use capture::{CAMERA_PERMISSION, SCREEN_PERMISSION};

use crate::{Capture, FrameSink, Settings};
use objc2_av_foundation::{
    AVCaptureDeviceDiscoverySession, AVCaptureDevicePosition, AVCaptureDeviceTypeBuiltInWideAngleCamera,
    AVCaptureDeviceTypeExternal, AVMediaTypeVideo,
};
use objc2_foundation::NSArray;
use relay_proto::{VideoSource, VideoSourceKind};

pub fn sources() -> Vec<VideoSource> {
    // CoreGraphics lists displays without asking for screen-recording
    // permission (ScreenCaptureKit would prompt), so the list is always shown.
    let displays: Vec<(u32, (u32, u32))> =
        ffi::active_displays().into_iter().filter_map(|id| ffi::display_pixels(id).map(|px| (id, px))).collect();
    let many = displays.len() > 1;
    let mut out: Vec<VideoSource> = displays
        .iter()
        .enumerate()
        .map(|(i, (id, (w, h)))| VideoSource {
            key: format!("screen:{id}"),
            name: if many { format!("Screen {} ({w}×{h})", i + 1) } else { format!("Screen ({w}×{h})") },
            kind: VideoSourceKind::Screen,
        })
        .collect();
    out.extend(cameras());
    out
}

fn cameras() -> Vec<VideoSource> {
    unsafe {
        let Some(media) = AVMediaTypeVideo else { return Vec::new() };
        let types = NSArray::from_slice(&[AVCaptureDeviceTypeBuiltInWideAngleCamera, AVCaptureDeviceTypeExternal]);
        let session = AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &types,
            Some(media),
            AVCaptureDevicePosition::Unspecified,
        );
        session
            .devices()
            .iter()
            .map(|d| VideoSource {
                key: format!("camera:{}", d.uniqueID()),
                name: d.localizedName().to_string(),
                kind: VideoSourceKind::Camera,
            })
            .collect()
    }
}

pub fn start(source: &str, settings: Settings, sink: FrameSink) -> anyhow::Result<Box<dyn Capture>> {
    match source.split_once(':') {
        Some(("screen", id)) => {
            let id = match id.parse() {
                Ok(id) => id,
                Err(_) => unsafe { ffi::CGMainDisplayID() },
            };
            capture::start_screen(id, settings, sink)
        }
        Some(("camera", id)) => capture::start_camera(id, settings, sink),
        _ => anyhow::bail!("unknown video source {source}"),
    }
}

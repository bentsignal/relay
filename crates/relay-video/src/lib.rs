//! Video for Relay: capture and hardware-encode a screen or camera, and decode
//! and present incoming video.
//!
//! Windows: DXGI Desktop Duplication -> D3D11 video processor (BGRA->NV12) ->
//! Media Foundation hardware H.264 encoder; Media Foundation decoder -> D3D11
//! swap chain for viewing.
//! macOS: ScreenCaptureKit / AVFoundation -> VideoToolbox; decode and display
//! through AVSampleBufferDisplayLayer.

use relay_proto::{EncodedVideo, VideoSource};

#[cfg(windows)]
pub mod windows;

#[derive(Clone, Copy, Debug)]
pub struct Settings {
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub max_width: u32,
    pub max_height: u32,
}

impl Default for Settings {
    fn default() -> Self {
        Self { fps: 60, bitrate_kbps: 20_000, max_width: 1920, max_height: 1080 }
    }
}

/// Receives encoded frames from a capture session.
pub type FrameSink = Box<dyn FnMut(EncodedVideo) + Send>;

/// A running capture + encode pipeline.
pub trait Capture: Send {
    /// Asks the encoder for an IDR frame as soon as possible.
    fn request_keyframe(&self);
    fn stats(&self) -> CaptureStats;
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CaptureStats {
    pub width: u32,
    pub height: u32,
    pub fps: f32,
    pub kbps: f32,
    pub encoder_hw: bool,
}

/// Screens, windows and cameras that can be sent.
pub fn sources() -> Vec<VideoSource> {
    #[cfg(windows)]
    {
        windows::sources()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Starts capturing `source` and delivering encoded frames to `sink`.
pub fn start(source: &str, settings: Settings, sink: FrameSink) -> anyhow::Result<Box<dyn Capture>> {
    #[cfg(windows)]
    {
        windows::start(source, settings, sink)
    }
    #[cfg(not(windows))]
    {
        let _ = (source, settings, sink);
        anyhow::bail!("video capture isn't available on this platform yet")
    }
}

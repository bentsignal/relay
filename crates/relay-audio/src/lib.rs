//! Hardware audio I/O. Device streams are thin: they convert the device's
//! format to interleaved stereo `f32` and move it through a pipe. All timing
//! correction happens in the mixer's clock bridges.

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use relay_mixer::{PipeReader, PipeShared, PipeWriter, pipe};
use relay_proto::{DeviceInfo, Devices};
use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

#[cfg(windows)]
pub mod windows;

/// Name fragment used by every Relay virtual device.
pub const RELAY_DEVICE_TAG: &str = "Relay";

pub fn is_relay_device(name: &str) -> bool {
    // Matches "Relay", "Speakers (Relay)", "Relay Mic", "Microphone (Relay Mic)"...
    name.split(|c: char| !c.is_alphanumeric()).any(|w| w == RELAY_DEVICE_TAG)
}

fn host() -> cpal::Host {
    cpal::default_host()
}

fn info(d: &cpal::Device, default_id: &Option<String>, input: bool) -> Option<DeviceInfo> {
    let key = d.id().ok()?.to_string();
    let name = d.description().ok().map(|d| d.name().to_string()).unwrap_or_else(|| key.clone());
    let cfg = if input { d.default_input_config().ok()? } else { d.default_output_config().ok()? };
    Some(DeviceInfo {
        is_default: default_id.as_deref() == Some(key.as_str()),
        is_relay: is_relay_device(&name),
        key,
        name,
        sample_rate: cfg.sample_rate(),
        channels: cfg.channels(),
    })
}

/// Enumerates capture and playback devices.
pub fn list_devices() -> Devices {
    let host = host();
    let def_in = host.default_input_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let def_out = host.default_output_device().and_then(|d| d.id().ok()).map(|i| i.to_string());
    let mut devices = Devices::default();
    if let Ok(list) = host.input_devices() {
        devices.inputs = list.filter_map(|d| info(&d, &def_in, true)).collect();
    }
    if let Ok(list) = host.output_devices() {
        devices.outputs = list.filter_map(|d| info(&d, &def_out, false)).collect();
    }
    devices
}

fn find(key: &str) -> Result<cpal::Device> {
    let id = cpal::DeviceId::from_str(key).map_err(|e| anyhow!("bad device key {key}: {e}"))?;
    host().device_by_id(&id).ok_or_else(|| anyhow!("device not found"))
}

/// A running device stream. Dropping it stops the stream.
pub enum StreamInner {
    Cpal(cpal::Stream),
    /// A capture thread we own; dropping sets the flag and the thread exits.
    Thread(Arc<AtomicBool>),
}

impl Drop for StreamInner {
    fn drop(&mut self) {
        if let StreamInner::Thread(stop) = self {
            stop.store(true, Ordering::Relaxed);
        }
    }
}

pub struct DeviceStream {
    _stream: StreamInner,
    pub name: String,
    pub rate: u32,
    pub channels: u16,
    pub pipe: Arc<PipeShared>,
    failed: Arc<AtomicBool>,
}

impl DeviceStream {
    pub fn from_thread(
        stop: Arc<AtomicBool>,
        name: String,
        rate: u32,
        pipe: Arc<PipeShared>,
        failed: Arc<AtomicBool>,
    ) -> Self {
        Self { _stream: StreamInner::Thread(stop), name, rate, channels: 2, pipe, failed }
    }

    /// True once the backend has reported the device gone or broken.
    pub fn failed(&self) -> bool {
        self.failed.load(Ordering::Relaxed)
    }
}

fn error_cb(failed: Arc<AtomicBool>, name: String) -> impl FnMut(cpal::Error) + Send + 'static {
    move |e| {
        use cpal::ErrorKind::*;
        match e.kind() {
            Xrun => {}
            _ => {
                tracing::warn!(device = %name, "stream error: {e}");
                failed.store(true, Ordering::Relaxed);
            }
        }
    }
}

/// Converts any supported sample format to f32, interleaved stereo.
fn to_stereo(data: &cpal::Data, channels: usize, out: &mut Vec<f32>) {
    out.clear();
    let frames = data.len() / channels.max(1);
    macro_rules! conv {
        ($t:ty, $f:expr) => {{
            let s = data.as_slice::<$t>().unwrap();
            for f in 0..frames {
                let l = $f(s[f * channels]);
                let r = if channels > 1 { $f(s[f * channels + 1]) } else { l };
                out.push(l);
                out.push(r);
            }
        }};
    }
    match data.sample_format() {
        SampleFormat::F32 => conv!(f32, |x: f32| x),
        SampleFormat::I16 => conv!(i16, |x: i16| x as f32 / 32768.0),
        SampleFormat::I32 => conv!(i32, |x: i32| x as f32 / 2147483648.0),
        SampleFormat::U16 => conv!(u16, |x: u16| (x as f32 - 32768.0) / 32768.0),
        SampleFormat::F64 => conv!(f64, |x: f64| x as f32),
        _ => out.resize(frames * 2, 0.0),
    }
}

fn from_stereo(src: &[f32], data: &mut cpal::Data, channels: usize) {
    let frames = data.len() / channels.max(1);
    macro_rules! conv {
        ($t:ty, $f:expr) => {{
            let d = data.as_slice_mut::<$t>().unwrap();
            for f in 0..frames {
                let l = src[f * 2];
                let r = src[f * 2 + 1];
                if channels == 1 {
                    d[f] = $f((l + r) * 0.5);
                } else {
                    d[f * channels] = $f(l);
                    d[f * channels + 1] = $f(r);
                    for c in 2..channels {
                        d[f * channels + c] = $f(0.0);
                    }
                }
            }
        }};
    }
    let clamp = |x: f32| x.clamp(-1.0, 1.0);
    match data.sample_format() {
        SampleFormat::F32 => conv!(f32, |x: f32| x),
        SampleFormat::I16 => conv!(i16, |x: f32| (clamp(x) * 32767.0) as i16),
        SampleFormat::I32 => conv!(i32, |x: f32| (clamp(x) as f64 * 2147483647.0) as i32),
        SampleFormat::U16 => conv!(u16, |x: f32| (clamp(x) * 32767.0 + 32768.0) as u16),
        SampleFormat::F64 => conv!(f64, |x: f32| x as f64),
        _ => {}
    }
}

const PIPE_SECONDS: f32 = 0.5;

fn open_capture(device: cpal::Device, loopback: bool) -> Result<(DeviceStream, PipeReader)> {
    let name = device.description().map(|d| d.name().to_string()).unwrap_or_default();
    let supported = if loopback { device.default_output_config() } else { device.default_input_config() }
        .context("no usable format")?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let (mut writer, reader) = pipe(rate, (rate as f32 * PIPE_SECONDS) as usize);
    let shared = writer.shared().clone();
    let failed = Arc::new(AtomicBool::new(false));
    let mut scratch: Vec<f32> = Vec::with_capacity(32768);
    let stream = device.build_input_stream_raw(
        config.clone(),
        format,
        move |data: &cpal::Data, _: &cpal::InputCallbackInfo| {
            to_stereo(data, channels, &mut scratch);
            writer.write_interleaved(&scratch);
        },
        error_cb(failed.clone(), name.clone()),
        None,
    )?;
    stream.play()?;
    Ok((
        DeviceStream {
            _stream: StreamInner::Cpal(stream),
            name,
            rate,
            channels: config.channels,
            pipe: shared,
            failed,
        },
        reader,
    ))
}

/// Opens a capture device (microphone, line in...).
pub fn open_input(key: &str) -> Result<(DeviceStream, PipeReader)> {
    open_capture(find(key)?, false)
}

/// Captures everything playing on an output device.
pub fn open_loopback(key: &str) -> Result<(DeviceStream, PipeReader)> {
    open_capture(find(key)?, true)
}

/// Opens a playback device.
pub fn open_output(key: &str) -> Result<(DeviceStream, PipeWriter)> {
    let device = find(key)?;
    let name = device.description().map(|d| d.name().to_string()).unwrap_or_default();
    let supported = device.default_output_config().context("no usable format")?;
    let format = supported.sample_format();
    let config: StreamConfig = supported.config();
    let channels = config.channels as usize;
    let rate = config.sample_rate;
    let (writer, mut reader) = pipe(rate, (rate as f32 * PIPE_SECONDS) as usize);
    let shared = reader.shared().clone();
    let failed = Arc::new(AtomicBool::new(false));
    let mut scratch: Vec<f32> = vec![0.0; 32768];
    let stream = device.build_output_stream_raw(
        config.clone(),
        format,
        move |data: &mut cpal::Data, _: &cpal::OutputCallbackInfo| {
            let frames = data.len() / channels.max(1);
            let n = (frames * 2).min(scratch.len());
            reader.read_interleaved(&mut scratch[..n]);
            from_stereo(&scratch[..n], data, channels);
        },
        error_cb(failed.clone(), name.clone()),
        None,
    )?;
    stream.play()?;
    Ok((
        DeviceStream {
            _stream: StreamInner::Cpal(stream),
            name,
            rate,
            channels: config.channels,
            pipe: shared,
            failed,
        },
        writer,
    ))
}

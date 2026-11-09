//! Relay's CoreAudio AudioServerPlugIn ("RelayAudio.driver").
//!
//! Provides the visible "Relay" speaker/microphone and the hidden "Relay
//! Engine" device the engine uses to reach it. Build the bundle with
//! `scripts/macos/build-hal-plugin.sh`.

pub mod ring;

#[cfg(target_os = "macos")]
mod driver;
#[cfg(target_os = "macos")]
mod ffi;

#[cfg(target_os = "macos")]
pub use driver::{ENGINE_DEVICE, MAIN_DEVICE, RelayAudio_Create};

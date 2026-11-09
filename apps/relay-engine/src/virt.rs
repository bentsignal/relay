//! Platform virtual audio devices ("Relay" speaker + microphone).

use relay_mixer::{PipeReader, PipeWriter};
use relay_proto::VirtualAudioState;

pub trait VirtualBackend: Send {
    fn state(&self) -> VirtualAudioState;
    /// Audio apps play to virtual device `index`.
    fn playback(&mut self, index: u8) -> Option<PipeReader>;
    /// Feeds virtual microphone `index`.
    fn mic(&mut self, index: u8) -> Option<PipeWriter>;
    /// Starts the one-time platform install (may show an admin prompt).
    fn install(&mut self) -> anyhow::Result<()>;
    /// Called every couple of seconds.
    fn tick(&mut self) {}
}

pub fn create() -> Box<dyn VirtualBackend> {
    if std::env::var_os("RELAY_NO_VIRTUAL").is_some() {
        return Box::new(Unsupported);
    }
    #[cfg(windows)]
    {
        Box::new(windows::WinVirtual::new())
    }
    #[cfg(not(windows))]
    {
        Box::new(Unsupported)
    }
}

#[allow(dead_code)]
pub struct Unsupported;

impl VirtualBackend for Unsupported {
    fn state(&self) -> VirtualAudioState {
        VirtualAudioState::Unsupported
    }
    fn playback(&mut self, _: u8) -> Option<PipeReader> {
        None
    }
    fn mic(&mut self, _: u8) -> Option<PipeWriter> {
        None
    }
    fn install(&mut self) -> anyhow::Result<()> {
        anyhow::bail!("virtual devices are not supported on this platform yet")
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use relay_usbip::{DEFAULT_PORT, Identity, VirtualAudio, attach};
    use std::process::Command;

    pub struct WinVirtual {
        va: Option<VirtualAudio>,
        state: VirtualAudioState,
        installing: Option<std::process::Child>,
    }

    impl WinVirtual {
        pub fn new() -> Self {
            let mut v = Self { va: None, state: VirtualAudioState::Unknown, installing: None };
            v.start();
            v
        }

        fn start(&mut self) {
            if !attach::driver_installed() {
                self.state = VirtualAudioState::NeedsInstall;
                return;
            }
            if !attach::driver_running() {
                self.state = VirtualAudioState::Error("The Relay audio driver isn't running. Restart Windows.".into());
                return;
            }
            let id = Identity { product: "Relay".into(), serial: "RELAY-0001".into(), product_id: 0x0001 };
            match VirtualAudio::start(DEFAULT_PORT, vec![id]) {
                Ok((mut va, _initial)) => {
                    if !va.client_found() {
                        self.state =
                            VirtualAudioState::Error("Relay's USB/IP client is missing. Reinstall Relay.".into());
                        return;
                    }
                    match va.attach_all() {
                        Ok(()) => self.state = VirtualAudioState::Ready,
                        Err(e) => {
                            self.state = VirtualAudioState::Error(format!("Couldn't plug in Relay's devices: {e}"))
                        }
                    }
                    self.va = Some(va);
                }
                Err(e) => {
                    self.state = VirtualAudioState::Error(format!("Couldn't start Relay's devices: {e}"));
                }
            }
        }
    }

    impl VirtualBackend for WinVirtual {
        fn state(&self) -> VirtualAudioState {
            self.state.clone()
        }

        fn playback(&mut self, index: u8) -> Option<PipeReader> {
            self.va.as_ref().map(|va| va.connect_playback(index as usize))
        }

        fn mic(&mut self, index: u8) -> Option<PipeWriter> {
            self.va.as_ref().map(|va| va.connect_capture(index as usize))
        }

        fn install(&mut self) -> anyhow::Result<()> {
            let exe = std::env::current_exe()?;
            let dir = exe.parent().unwrap_or(std::path::Path::new("."));
            let script = [dir.join("usbip").join("install-driver.ps1"), dir.join("install-driver.ps1")]
                .into_iter()
                .find(|p| p.is_file())
                .ok_or_else(|| anyhow::anyhow!("driver installer missing"))?;
            // Shows one Windows administrator prompt.
            let child = Command::new("powershell.exe")
                .args([
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-Command",
                    &format!(
                        "$p = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList '-NoProfile','-ExecutionPolicy','Bypass','-File','\"{}\"'; exit $p.ExitCode",
                        script.display()
                    ),
                ])
                .spawn()?;
            self.installing = Some(child);
            self.state = VirtualAudioState::Installing;
            Ok(())
        }

        fn tick(&mut self) {
            if let Some(child) = self.installing.as_mut() {
                match child.try_wait() {
                    Ok(Some(status)) => {
                        self.installing = None;
                        if status.success() && attach::driver_installed() {
                            self.start();
                        } else {
                            self.state = VirtualAudioState::Error("The audio driver wasn't installed.".into());
                        }
                    }
                    Ok(None) => {}
                    Err(e) => {
                        self.installing = None;
                        self.state = VirtualAudioState::Error(e.to_string());
                    }
                }
            }
        }
    }
}

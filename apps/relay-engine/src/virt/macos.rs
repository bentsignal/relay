//! macOS: the RelayAudio.driver HAL plugin provides a visible "Relay" device
//! and a hidden "Relay Engine" device that loop into each other. The engine
//! runs IO on "Relay Engine"; while it does, the plugin shows "Relay" to apps,
//! and "Relay" disappears when the engine stops.

use super::*;
use parking_lot::Mutex;
use relay_audio::macos::{DuplexStream, IoBuffers, device_by_uid};
use relay_mixer::pipe;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub const DRIVER_NAME: &str = "RelayAudio.driver";
pub const HAL_DIR: &str = "/Library/Audio/Plug-Ins/HAL";
/// UID of the visible "Relay" device.
pub const MAIN_UID: &str = "relay.device.main";
/// UID of the hidden device the engine drives.
pub const ENGINE_UID: &str = "relay.device.engine";
const RATE: u32 = 48_000;
const BUFFER_FRAMES: u32 = 256;
/// How long to wait for the audio server to load a freshly installed driver.
const STARTUP_GRACE: Duration = Duration::from_secs(15);

fn installed_path() -> PathBuf {
    Path::new(HAL_DIR).join(DRIVER_NAME)
}

fn installed() -> bool {
    installed_path().join("Contents/MacOS/RelayAudio").is_file()
}

/// The driver bundle shipped with Relay: next to the engine binary, or in the
/// app bundle's Resources.
fn bundled_driver() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    [dir.join(DRIVER_NAME), dir.join("../Resources").join(DRIVER_NAME)]
        .into_iter()
        .find(|p| p.join("Contents/MacOS/RelayAudio").is_file())
        .and_then(|p| p.canonicalize().ok())
}

/// The AppleScript that copies the driver into place and restarts the audio
/// server, behind one administrator prompt.
fn install_script(src: &Path) -> String {
    let q = |p: &Path| format!("'{}'", p.display().to_string().replace('\'', r"'\''"));
    let dst = installed_path();
    let shell = format!(
        "mkdir -p {hal} && rm -rf {dst} && cp -R {src} {hal}/ && xattr -cr {dst}; \
         launchctl kickstart -k system/com.apple.audio.coreaudiod || killall coreaudiod",
        hal = q(Path::new(HAL_DIR)),
        dst = q(&dst),
        src = q(src),
    );
    let escaped = shell.replace('\\', r"\\").replace('"', r#"\""#);
    format!("do shell script \"{escaped}\" with administrator privileges")
}

/// Pipes the IO thread uses; swapped by the control thread.
#[derive(Default)]
struct Ports {
    /// What apps played to "Relay", for the mixer.
    playback: Mutex<Option<PipeWriter>>,
    /// What the mixer sends to the "Relay" microphone.
    mic: Mutex<Option<PipeReader>>,
}

/// One IO cycle of "Relay Engine". Realtime: never blocks; if the control
/// thread is swapping a pipe, this cycle is skipped.
fn process(ports: &Ports, io: IoBuffers<'_>) {
    if io.input_channels == 2
        && let Some(mut w) = ports.playback.try_lock()
        && let Some(w) = w.as_mut()
    {
        w.write_interleaved(io.input);
    }
    let mut filled = false;
    if io.output_channels == 2
        && let Some(mut r) = ports.mic.try_lock()
        && let Some(r) = r.as_mut()
    {
        r.read_interleaved(io.output);
        filled = true;
    }
    if !filled {
        io.output.fill(0.0);
    }
}

pub struct MacVirtual {
    ports: Arc<Ports>,
    stream: Option<DuplexStream>,
    last_cycles: u64,
    state: VirtualAudioState,
    installing: Option<Child>,
    starting_since: Option<Instant>,
}

impl MacVirtual {
    pub fn new() -> Self {
        let mut v = Self {
            ports: Arc::new(Ports::default()),
            stream: None,
            last_cycles: 0,
            state: VirtualAudioState::Unknown,
            installing: None,
            starting_since: None,
        };
        v.start();
        v
    }

    fn start(&mut self) {
        self.stream = None;
        if !installed() {
            self.state = VirtualAudioState::NeedsInstall;
            return;
        }
        let Some(device) = device_by_uid(ENGINE_UID) else {
            self.state = VirtualAudioState::Error("Relay's audio driver isn't loaded. Restart your Mac.".into());
            return;
        };
        let ports = self.ports.clone();
        match DuplexStream::open(device, Some(BUFFER_FRAMES), move |io| process(&ports, io)) {
            Ok(stream) => {
                tracing::info!(device, "virtual devices running");
                self.last_cycles = stream.cycles();
                self.stream = Some(stream);
                self.state = VirtualAudioState::Ready;
                self.starting_since = None;
            }
            Err(e) => {
                tracing::warn!("couldn't start Relay Engine IO: {e:#}");
                self.state = VirtualAudioState::Error(format!("Couldn't start Relay's devices: {e}"));
            }
        }
    }
}

impl VirtualBackend for MacVirtual {
    fn state(&self) -> VirtualAudioState {
        self.state.clone()
    }

    fn playback(&mut self, _index: u8) -> Option<PipeReader> {
        if !installed() {
            return None;
        }
        let (w, r) = pipe(RATE, RATE as usize / 2);
        let old = self.ports.playback.lock().replace(w);
        drop(old);
        Some(r)
    }

    fn mic(&mut self, _index: u8) -> Option<PipeWriter> {
        if !installed() {
            return None;
        }
        let (w, r) = pipe(RATE, RATE as usize / 2);
        let old = self.ports.mic.lock().replace(r);
        drop(old);
        Some(w)
    }

    fn install(&mut self) -> anyhow::Result<()> {
        if self.installing.is_some() {
            return Ok(());
        }
        let src = bundled_driver().ok_or_else(|| anyhow::anyhow!("{DRIVER_NAME} is missing from Relay"))?;
        // The audio server restarts; let go of the device first.
        self.stream = None;
        let child = Command::new("osascript").args(["-e", &install_script(&src)]).spawn()?;
        self.installing = Some(child);
        self.state = VirtualAudioState::Installing;
        Ok(())
    }

    fn tick(&mut self) {
        if let Some(child) = self.installing.as_mut() {
            match child.try_wait() {
                Ok(None) => return,
                Ok(Some(status)) if status.success() && installed() => {
                    self.installing = None;
                    self.state = VirtualAudioState::Starting;
                    self.starting_since = Some(Instant::now());
                }
                Ok(Some(_)) => {
                    self.installing = None;
                    self.state = VirtualAudioState::Error("The audio driver wasn't installed.".into());
                    return;
                }
                Err(e) => {
                    self.installing = None;
                    self.state = VirtualAudioState::Error(e.to_string());
                    return;
                }
            }
        }
        if let Some(stream) = &self.stream {
            // IO stops advancing when the audio server restarts or drops the device.
            let cycles = stream.cycles();
            if cycles != self.last_cycles {
                self.last_cycles = cycles;
                return;
            }
            tracing::warn!("Relay Engine IO stalled; reopening");
        } else if !installed() {
            if !matches!(self.state, VirtualAudioState::Error(_)) {
                self.state = VirtualAudioState::NeedsInstall;
            }
            return;
        }
        let waiting = self.starting_since.is_some_and(|t| t.elapsed() < STARTUP_GRACE);
        self.start();
        if waiting && self.state != VirtualAudioState::Ready {
            // The audio server is still loading the driver.
            self.state = VirtualAudioState::Starting;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_script_quotes_paths() {
        let cmd = install_script(Path::new("/Applications/Relay's \"App\".app/Contents/Resources/RelayAudio.driver"));
        assert!(cmd.starts_with("do shell script \""));
        assert!(cmd.ends_with("\" with administrator privileges"));
        assert!(
            cmd.contains(r#"'/Applications/Relay'\\''s \"App\".app/Contents/Resources/RelayAudio.driver'"#),
            "{cmd}"
        );
        assert!(cmd.contains("launchctl kickstart -k system/com.apple.audio.coreaudiod || killall coreaudiod"));
    }

    #[test]
    fn io_moves_audio_through_the_pipes() {
        let ports = Ports::default();
        let (w, mut from_apps) = pipe(RATE, 4096);
        *ports.playback.lock() = Some(w);
        let (mut to_mic, r) = pipe(RATE, 4096);
        r.shared().set_prime_frames(0);
        *ports.mic.lock() = Some(r);
        let input: Vec<f32> = (0..512).map(|i| i as f32).collect();
        to_mic.write_interleaved(&[0.25; 512]);
        let mut output = vec![9.0; 512];
        process(&ports, IoBuffers { input: &input, input_channels: 2, output: &mut output, output_channels: 2 });
        assert!(output.iter().all(|&s| s == 0.25));
        from_apps.shared().set_prime_frames(0);
        let mut got = vec![0.0; 512];
        assert_eq!(from_apps.read_interleaved(&mut got), 256);
        assert_eq!(got, input);
        // No mic pipe: silence.
        *ports.mic.lock() = None;
        process(&ports, IoBuffers { input: &input, input_channels: 2, output: &mut output, output_channels: 2 });
        assert!(output.iter().all(|&s| s == 0.0));
    }

    #[test]
    fn run_script_through_osascript_parser() {
        // Compiles (doesn't run) the AppleScript to catch quoting mistakes.
        let script = install_script(Path::new("/tmp/it's \"here\"/RelayAudio.driver"));
        let dir = std::env::temp_dir().join(format!("relay-osa-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let out = dir.join("t.scpt");
        let st = Command::new("osacompile").arg("-o").arg(&out).arg("-e").arg(&script).status().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(st.success());
    }
}

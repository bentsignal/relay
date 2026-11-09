# Relay

Free, open source audio and video routing for Windows and macOS. A modern replacement for VoiceMeeter that also sends audio and video between your devices on the same network.

- **Virtual devices.** Apps play to *Relay* and record from Relay's mic (*Line (Relay)* on Windows, so Windows doesn't apply voice processing to it). Relay mixes everything behind them.
- **Mixer.** Inputs (mics, apps, devices, other computers) feed outputs (speakers, headphones, Relay Mic, other computers), each with EQ, gate, compressor, limiter and noise suppression.
- **Your devices, connected.** Other computers running Relay show up on their own. Click Connect, allow it once on the other device, and they reconnect by themselves from then on. Connection health is always visible.
- **Video.** Send a screen or camera to another device. It opens in a low-latency viewer, and on Windows it's also a webcam (*Relay Camera*) for OBS, Discord and others.
- **Updates.** Relay updates itself. Pick *Stable* or *Dev* in Settings; a bad update rolls back automatically.

## Install

Download the latest build from [Releases](https://github.com/bentsignal/relay/releases/latest).

**Windows 11:** unzip `relay-windows-x64.zip` and run `relay.exe`. Relay installs itself for your user, adds itself to the Start menu and starts with Windows. The first time, it asks once for administrator rights to install its audio driver. Windows SmartScreen may warn because the build isn't signed by a paid certificate: choose *More info → Run anyway*.

**macOS 14.2+ (Apple Silicon):** unzip `relay-macos-arm64.zip` and move *Relay* to Applications. The first time, open it from *System Settings → Privacy & Security → Open Anyway*. Relay asks once for your password to install its audio device.

Dev builds of every change are on the [Releases page](https://github.com/bentsignal/relay/releases) as pre-releases. Once installed, switch channels in Settings.

## How it works

| Part | What it does |
| --- | --- |
| `apps/relay-engine` | Headless engine. Starts at login and owns all audio, network and video work, so closing the window never cuts your sound. |
| `apps/relay-app` | The window (GPUI). A remote control for the engine over local IPC. |
| `crates/relay-mixer` | Realtime mixer. Every device or stream joins through a clock bridge that converts sample rate and corrects drift, steering a resampler to keep latency low without glitches. |
| `crates/relay-dsp` | EQ, dynamics, metering and the resampler. |
| `crates/relay-usbip` | Windows virtual audio. A USB Audio 2.0 device served from user mode over USB/IP and plugged into the Microsoft-signed [usbip-win2](https://github.com/vadimgrn/usbip-win2) controller. No custom kernel driver. |
| `crates/relay-hal-plugin` | macOS virtual audio (Core Audio AudioServerPlugIn). |
| `crates/relay-net` | Discovery (mDNS plus a broadcast beacon), encrypted QUIC connections with pairing, Opus audio over datagrams, video frames over streams, link health. |
| `crates/relay-video` | Screen and camera capture, hardware H.264 encode and decode, the viewer. |
| `crates/relay-update` | Signed update channels, staged installs, rollback. |

## Build

```sh
cargo build --workspace
cargo run -p relay-app        # starts the engine too
```

Windows needs CMake (for libopus) and Python with `binary-refinery` to package (`scripts/windows/package.ps1`). macOS packaging is `scripts/macos/package.sh`.

## License

MIT. See [THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) for included components.

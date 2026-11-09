# Engineering notes

Things that aren't obvious from the code.

## Windows virtual audio

- Relay's devices are a USB Audio 2.0 device served by the engine over USB/IP on `127.0.0.1:3250` and plugged into the Microsoft-signed usbip-win2 controller (installed once by `scripts/windows/install-driver.ps1`). When the engine exits or crashes, the device disappears and Windows falls back to the hardware.
- **Voice processing.** Windows 11 attaches *Voice Clarity* (noise suppression) to microphones that have no effects of their own. It erases steady sounds, so music routed into Relay's mic was silenced within a second. The capture side therefore enumerates as a **line input** (USB terminal type `0x0603`, "Line (Relay)"), which Windows leaves alone. Disabling enhancements (`PKEY_AudioEndpoint_Disable_SysFx`) did not remove it; RAW-mode capture showed the signal was intact.
- **Stale reattach loops.** If an engine is killed, usbip-win2 keeps retrying the import in the background. Each retry that reaches a newer engine is rejected, but it also resets the device Windows is streaming to (the stream stops every few seconds). The engine runs `usbip attach --stop-all` before attaching. The per-device `--stop` form returns "The parameter is incorrect".
- The engine sets Relay's endpoints to full volume with effects off. Relay's faders control the level.
- New USB audio devices become the Windows default when they appear. The first-run layout is chosen before the virtual devices attach, so it picks the real hardware.

## Network

- Windows often puts the LAN in the **Public** firewall profile. The firewall rule covers every profile; pairing still needs an explicit Allow.
- Without the rule, inbound traffic is blocked. Paired devices remember their last addresses and both ends keep dialing, which gets through firewalls that admit replies to outbound traffic. First discovery still needs the rule (the engine shows "Windows Firewall is blocking Relay" with a Fix button).
- QUIC (quinn) uses one UDP socket for listening and dialing, so the source address of an incoming connection is also where that device listens.

## Measured (this dev PC, release builds)

- App → Speakers (Relay) → engine → Line (Relay) → app: 48–71 ms round trip, 58 ms median, including both test app buffers.
- Each network hop (Opus 5 ms frames, jitter buffer, drift bridge) adds about 12–13 ms on loopback.
- Screen capture: 2560×1440 → 1080p60 H.264 on the NVIDIA encoder at 60 fps, decoded and shown in the viewer at 60 fps.
- Idle engine: ~33 MB RAM; mixer thread ~3% of one core with 4 inputs and 4 outputs.

## Updates

- Tested end to end with a local manifest server (`RELAY_UPDATE_BASE`), a sandboxed install root (`RELAY_INSTALL_ROOT`) and a build that exits on start (`RELAY_VERSION` containing `crashtest`): install, update, automatic rollback, and the rolled-back version not being offered again.
- Test installs with `RELAY_INSTALL_ROOT` don't touch login items or the Start menu.

## Environment switches (development)

`RELAY_DATA_DIR`, `RELAY_IPC_NAME`, `RELAY_NET_PORT`, `RELAY_NO_VIRTUAL`, `RELAY_NO_TRAY`, `RELAY_INSTALL_ROOT`, `RELAY_UPDATE_BASE`, `RELAY_DEBUG_PIPES`, `RELAY_LOG`. Together they let two engines run side by side on one machine for network tests.

## CI

- A dev build whose workflow file differs from `main`'s current one can't publish: GitHub won't let the workflow token create a tag on that commit ("HTTP 403: Resource not accessible by integration"). This only happens when `.github/workflows` changed while the build was running; the next build publishes normally.
- Pending runs on `main` replace each other, so a burst of pushes produces one build of the latest commit.

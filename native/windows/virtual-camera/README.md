# Relay Camera (Windows)

A Media Foundation virtual camera media source (`relay-camera.dll`, x64, static
CRT) that shows Relay's incoming video to other apps as **"Relay Camera"**
(Windows lists it as "Relay Camera (Windows Virtual Camera)"; DirectShow apps
such as OBS see it too). Windows 11 (build 22000) or later.

- COM class (CLSID): `{E7AE42DD-B97D-4F1A-B794-0BD645A3D031}`. Fixed: it is what
  the engine passes to `MFCreateVirtualCamera` and what HKLM registration names.
- Formats: NV12 1920x1080 and 1280x720 at 30 fps.

## How frames get there

The media source runs inside the Windows Camera Frame Server service
(LocalService, session 0), not in Relay. While an app streams from the camera
it creates the shared-memory mapping `Global\RelayCameraFrames` (readable and
writable by the interactive user), sets `active` and the wanted size in its
header, and answers frame requests at 30 fps from the newest frame there. The
Relay engine (`crates/relay-video/src/windows/camera.rs`) scales decoded video
to that size on the GPU and copies it into one of three NV12 slots. Without a
frame newer than one second the camera sends black, so apps never stall. The
layout is in `src/RelayCamera.h` and must match `camera.rs`.

## Build

```powershell
scripts\windows\build-camera.ps1   # -> target\camera\relay-camera.dll
```

Needs Visual Studio 2022 (or Build Tools) with the C++ workload, a Windows SDK
with `mfvirtualcamera.h` and C++/WinRT, and CMake. WIL is downloaded at a
pinned release (or set `-DRELAY_WIL_DIR=` to a local checkout).
`scripts\windows\package.ps1` builds it and ships it as `camera\relay-camera.dll`.

## Install

1. Once, as admin: `scripts\windows\install-camera.ps1` (run by the one-time
   `install-driver.ps1`) copies the DLL to `%ProgramFiles%\Relay\Camera\` and
   registers its COM class in HKLM with `regsvr32`. Frame Server can't read the
   per-user app folder or HKCU registrations.
2. The engine then creates the per-user camera with `MFCreateVirtualCamera`
   (system lifetime, current user, no admin) if it's missing.

Remove: `install-camera.ps1 -Uninstall` (admin), and remove the device from
Settings > Bluetooth & devices > Cameras, or with
`cargo run -p relay-video --example camera -- unregister`.

## Debugging

`cargo run -p relay-video --example camera -- status | dshow | grab out.bmp`
checks registration, lists devices and reads frames like an app. `grab-inproc`
loads the DLL in-process through a COM registration (HKCU is enough) instead of
Frame Server. With `RELAY_CAMERA_LOG=<file>` set, an in-process media source
appends its debug output to that file.

## License

Derived from Microsoft's Windows-Camera VirtualCamera sample
(<https://github.com/microsoft/Windows-Camera/tree/master/Samples/VirtualCamera>),
MIT licensed; see `LICENSE-Windows-Camera-sample.txt`. Relay's changes are MIT.

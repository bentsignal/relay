#!/usr/bin/env bash
# Builds Relay.app for Apple Silicon and writes dist/relay-macos-arm64.zip.
# Env: RELAY_VERSION (optional), MACOS_SIGN_IDENTITY (default: ad-hoc "-").
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
cd "$root"
version="${RELAY_VERSION:-$(grep -m1 '^version' Cargo.toml | cut -d'"' -f2)}"
identity="${MACOS_SIGN_IDENTITY:--}"
if [[ "${SKIP_BUILD:-}" != "1" ]]; then
  cargo build --release --locked -p relay-engine -p relay-app
fi
dist="$root/dist"
app="$dist/stage-macos/Relay.app"
rm -rf "$dist/stage-macos"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/relay target/release/relay-engine "$app/Contents/MacOS/"
if [[ -f scripts/macos/build-hal-plugin.sh ]]; then
  bash scripts/macos/build-hal-plugin.sh "$app/Contents/Resources"
fi
iconset="$dist/relay.iconset"
rm -rf "$iconset" && mkdir -p "$iconset"
for size in 16 32 128 256 512; do
  sips -z $size $size assets/relay.png --out "$iconset/icon_${size}x${size}.png" >/dev/null
  sips -z $((size * 2)) $((size * 2)) assets/relay.png --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/relay.icns"
cp LICENSE THIRD-PARTY-NOTICES.md "$app/Contents/Resources/"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Relay</string>
  <key>CFBundleDisplayName</key><string>Relay</string>
  <key>CFBundleIdentifier</key><string>dev.relay.app</string>
  <key>CFBundleExecutable</key><string>relay</string>
  <key>CFBundleIconFile</key><string>relay</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>${version%%-*}</string>
  <key>CFBundleVersion</key><string>${version}</string>
  <key>LSMinimumSystemVersion</key><string>14.2</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>NSMicrophoneUsageDescription</key><string>Relay routes your microphone.</string>
  <key>NSCameraUsageDescription</key><string>Relay can send your camera to another device.</string>
  <key>NSAudioCaptureUsageDescription</key><string>Relay can capture audio from apps.</string>
  <key>NSScreenCaptureUsageDescription</key><string>Relay can send your screen to another device.</string>
  <key>NSLocalNetworkUsageDescription</key><string>Relay finds and connects to your other devices.</string>
  <key>NSBonjourServices</key><array><string>_relay._udp</string></array>
</dict>
</plist>
PLIST
# Sign inside-out with a stable identity so macOS privacy permissions survive updates.
for bin in "$app/Contents/MacOS/relay-engine" "$app/Contents/MacOS/relay"; do
  codesign --force --timestamp=none --sign "$identity" "$bin"
done
if [[ -d "$app/Contents/Resources/RelayAudio.driver" ]]; then
  codesign --force --timestamp=none --sign "$identity" "$app/Contents/Resources/RelayAudio.driver"
fi
codesign --force --timestamp=none --sign "$identity" "$app"
mkdir -p "$dist"
rm -f "$dist/relay-macos-arm64.zip"
(cd "$dist/stage-macos" && ditto -c -k --keepParent Relay.app "$dist/relay-macos-arm64.zip")
echo "wrote $dist/relay-macos-arm64.zip"

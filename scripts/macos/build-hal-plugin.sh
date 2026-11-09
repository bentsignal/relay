#!/usr/bin/env bash
# Builds RelayAudio.driver, Relay's CoreAudio HAL plugin.
#
#   scripts/macos/build-hal-plugin.sh [--debug] [--universal] [--out DIR]
#
# The bundle lands in target/release (or target/debug with --debug), next to
# the engine binary, where the engine's installer looks for it. Install it by
# hand with:
#   sudo cp -R target/release/RelayAudio.driver /Library/Audio/Plug-Ins/HAL/
#   sudo launchctl kickstart -k system/com.apple.audio.coreaudiod
set -euo pipefail

cd "$(dirname "$0")/../.."

profile=release
universal=0
out=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --debug) profile=debug ;;
        --universal) universal=1 ;;
        --out) out="$2"; shift ;;
        *) echo "unknown option: $1" >&2; exit 2 ;;
    esac
    shift
done
out="${out:-target/$profile}"

cargo_flags=(-p relay-hal-plugin --lib)
[[ $profile == release ]] && cargo_flags+=(--release)

lib=librelay_hal_plugin.dylib
if [[ $universal == 1 ]]; then
    bins=()
    for t in aarch64-apple-darwin x86_64-apple-darwin; do
        rustup target add "$t" >/dev/null
        cargo build "${cargo_flags[@]}" --target "$t"
        bins+=("target/$t/$profile/$lib")
    done
    built="target/$profile/RelayAudio.universal"
    mkdir -p "target/$profile"
    lipo -create -output "$built" "${bins[@]}"
else
    cargo build "${cargo_flags[@]}"
    built="target/$profile/$lib"
fi

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
bundle="$out/RelayAudio.driver"
rm -rf "$bundle"
mkdir -p "$bundle/Contents/MacOS" "$bundle/Contents/Resources"
cp "$built" "$bundle/Contents/MacOS/RelayAudio"
install_name_tool -id "@rpath/RelayAudio" "$bundle/Contents/MacOS/RelayAudio" 2>/dev/null || true

# Factory UUID is Relay's own; the type UUID is kAudioServerPlugInTypeUUID.
cat > "$bundle/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleDevelopmentRegion</key>
    <string>en</string>
    <key>CFBundleExecutable</key>
    <string>RelayAudio</string>
    <key>CFBundleIdentifier</key>
    <string>io.github.bentsignal.relay.audio</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>CFBundleName</key>
    <string>RelayAudio</string>
    <key>CFBundlePackageType</key>
    <string>BNDL</string>
    <key>CFBundleShortVersionString</key>
    <string>$version</string>
    <key>CFBundleVersion</key>
    <string>$version</string>
    <key>CFBundleSignature</key>
    <string>????</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>CFPlugInFactories</key>
    <dict>
        <key>CA48F5E8-DC20-4AA0-A8CC-1CFA8EA965F3</key>
        <string>RelayAudio_Create</string>
    </dict>
    <key>CFPlugInTypes</key>
    <dict>
        <key>443ABAB8-E7B3-491A-B985-BEB9187030DB</key>
        <array>
            <string>CA48F5E8-DC20-4AA0-A8CC-1CFA8EA965F3</string>
        </array>
    </dict>
</dict>
</plist>
PLIST
plutil -lint "$bundle/Contents/Info.plist" >/dev/null

codesign --force --sign - --timestamp=none "$bundle"
codesign --verify --strict "$bundle"

if ! nm -gU "$bundle/Contents/MacOS/RelayAudio" | grep -q '_RelayAudio_Create$'; then
    echo "factory symbol RelayAudio_Create is not exported" >&2
    exit 1
fi

echo "$bundle"

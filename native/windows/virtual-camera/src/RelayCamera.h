// Relay Camera: identifiers and the shared-memory frame protocol between the
// media source (inside the Windows Camera Frame Server service) and the Relay
// engine (in the user's session).
//
// Keep in sync with crates/relay-video/src/windows/camera.rs.
//
// SPDX-License-Identifier: MIT
#pragma once
#include <cstddef>
#include <cstdint>

// COM class of the media source: {E7AE42DD-B97D-4F1A-B794-0BD645A3D031}.
// Fixed forever: it's what MFCreateVirtualCamera is given, and what the
// HKLM\Software\Classes\CLSID registration points at.
#define RELAY_CAMERA_CLSID_STRING L"{E7AE42DD-B97D-4F1A-B794-0BD645A3D031}"
#define RELAY_CAMERA_CLSID_UUID "E7AE42DD-B97D-4F1A-B794-0BD645A3D031"
#define RELAY_CAMERA_NAME L"Relay Camera"

namespace relaycam
{
    // The media source creates this mapping (Frame Server runs in session 0,
    // whose local namespace is the global one); the engine opens it. When the
    // source runs in a user's own process (tests), Global\ can't be created
    // without SeCreateGlobalPrivilege and the session-local name is used.
    constexpr wchar_t kMappingName[] = L"Global\\RelayCameraFrames";
    constexpr wchar_t kLocalMappingName[] = L"Local\\RelayCameraFrames";

    constexpr uint32_t kMagic = 0x4D414352; // "RCAM"
    constexpr uint32_t kVersion = 1;
    constexpr uint32_t kHeaderSize = 4096;  // frame slots start here
    constexpr uint32_t kSlotCount = 3;
    constexpr uint32_t kMaxWidth = 1920;
    constexpr uint32_t kMaxHeight = 1080;
    constexpr uint32_t kSlotSize = kMaxWidth * kMaxHeight * 3 / 2; // one NV12 frame
    constexpr uint64_t kMappingSize = uint64_t(kHeaderSize) + uint64_t(kSlotCount) * kSlotSize;
    constexpr uint32_t kFourccNV12 = 0x3231564E; // 'N','V','1','2'
    constexpr uint32_t kFps = 30;
    constexpr uint64_t kStaleMs = 1000; // older frames are replaced by black

    // One published frame. NV12: `height` rows of Y then `height / 2` rows of
    // interleaved UV, both `pitch` bytes apart, starting at the slot's data.
    struct Slot
    {
        volatile uint32_t seq;        // odd while the engine is writing it
        uint32_t width;
        uint32_t height;
        uint32_t pitch;
        uint32_t format;              // kFourccNV12
        uint32_t reserved0;
        uint64_t frame;               // engine's frame counter
        uint64_t writeTick;           // GetTickCount64() when published
        int64_t timestamp100ns;       // sender's presentation time
        uint8_t reserved[16];
    };

    struct Header
    {
        // Written once by the media source when it creates the mapping
        // (magic last).
        uint32_t magic;
        uint32_t version;
        uint32_t headerSize;
        uint32_t slotCount;
        uint32_t slotSize;
        uint32_t maxWidth;
        uint32_t maxHeight;
        uint32_t reserved0;
        // Media source -> engine: whether an app is streaming, and at which size.
        // The engine skips all work unless active != 0 and readTick is recent.
        volatile uint32_t active;
        volatile uint32_t wantWidth;
        volatile uint32_t wantHeight;
        volatile uint32_t wantFps;
        volatile uint64_t readTick;   // GetTickCount64() at the last frame delivered
        uint64_t reserved1;
        // Engine -> media source.
        volatile uint64_t frameCounter;
        volatile uint32_t latestSlot;
        volatile uint32_t writerPid;
        uint8_t reserved2[48];
        Slot slots[kSlotCount];
    };

    static_assert(sizeof(Slot) == 64, "protocol layout");
    static_assert(offsetof(Header, active) == 32, "protocol layout");
    static_assert(offsetof(Header, readTick) == 48, "protocol layout");
    static_assert(offsetof(Header, frameCounter) == 64, "protocol layout");
    static_assert(offsetof(Header, slots) == 128, "protocol layout");
    static_assert(sizeof(Header) <= kHeaderSize, "protocol layout");
}

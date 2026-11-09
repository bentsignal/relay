// Relay Camera: the media source's side of the shared-memory frame protocol.
// SPDX-License-Identifier: MIT
#include "pch.h"
#include "SharedFrames.h"

namespace relaycam
{
    // Full access for SYSTEM, LocalService (Frame Server), Administrators and the
    // interactive user (the Relay engine); medium integrity so a normal
    // (medium-integrity) engine may write to it.
    static constexpr wchar_t kSddl[] = L"D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GA;;;IU)S:(ML;;NW;;;ME)";
    static constexpr wchar_t kSddlNoLabel[] = L"D:P(A;;GA;;;SY)(A;;GA;;;LS)(A;;GA;;;BA)(A;;GA;;;IU)";

    SharedFrames::~SharedFrames()
    {
        Close();
    }

    static HANDLE CreateMapping(const wchar_t* name, const wchar_t* sddl, bool* existed)
    {
        PSECURITY_DESCRIPTOR sd = nullptr;
        if (!ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl, SDDL_REVISION_1, &sd, nullptr))
        {
            return nullptr;
        }
        SECURITY_ATTRIBUTES sa = { sizeof(sa), sd, FALSE };
        HANDLE h = CreateFileMappingW(INVALID_HANDLE_VALUE, &sa, PAGE_READWRITE, static_cast<DWORD>(kMappingSize >> 32),
                                      static_cast<DWORD>(kMappingSize), name);
        *existed = GetLastError() == ERROR_ALREADY_EXISTS;
        LocalFree(sd);
        return h;
    }

    HRESULT SharedFrames::Open()
    {
        if (m_header != nullptr)
        {
            return S_OK;
        }
        bool existed = false;
        HANDLE h = nullptr;
        for (const wchar_t* name : { kMappingName, kLocalMappingName })
        {
            h = CreateMapping(name, kSddl, &existed);
            if (h == nullptr)
            {
                // Setting the integrity label can be refused; the default (medium) is fine.
                h = CreateMapping(name, kSddlNoLabel, &existed);
            }
            if (h != nullptr)
            {
                DEBUG_MSG(L"mapping %s", name);
                break;
            }
        }
        RETURN_LAST_ERROR_IF_NULL(h);
        void* view = MapViewOfFile(h, FILE_MAP_READ | FILE_MAP_WRITE, 0, 0, static_cast<SIZE_T>(kMappingSize));
        if (view == nullptr)
        {
            DWORD err = GetLastError();
            CloseHandle(h);
            return HRESULT_FROM_WIN32(err);
        }
        m_mapping = h;
        m_header = static_cast<Header*>(view);
        if (!existed || m_header->magic != kMagic || m_header->version != kVersion)
        {
            ZeroMemory(m_header, kHeaderSize);
            m_header->version = kVersion;
            m_header->headerSize = kHeaderSize;
            m_header->slotCount = kSlotCount;
            m_header->slotSize = kSlotSize;
            m_header->maxWidth = kMaxWidth;
            m_header->maxHeight = kMaxHeight;
            MemoryBarrier();
            m_header->magic = kMagic;
        }
        DEBUG_MSG(L"shared frames open (existed=%d)", existed);
        return S_OK;
    }

    void SharedFrames::Close()
    {
        if (m_header != nullptr)
        {
            m_header->active = 0;
            UnmapViewOfFile(m_header);
            m_header = nullptr;
        }
        if (m_mapping != nullptr)
        {
            CloseHandle(m_mapping);
            m_mapping = nullptr;
        }
    }

    void SharedFrames::SetActive(uint32_t width, uint32_t height, uint32_t fps)
    {
        if (m_header == nullptr)
        {
            return;
        }
        m_header->wantWidth = width;
        m_header->wantHeight = height;
        m_header->wantFps = fps;
        m_header->readTick = GetTickCount64();
        MemoryBarrier();
        m_header->active = 1;
    }

    void SharedFrames::SetInactive()
    {
        if (m_header != nullptr)
        {
            m_header->active = 0;
        }
    }

    void FillBlack(BYTE* dst, LONG pitch, uint32_t width, uint32_t height)
    {
        for (uint32_t y = 0; y < height; y++)
        {
            memset(dst + static_cast<size_t>(y) * pitch, 16, width);
        }
        BYTE* uv = dst + static_cast<size_t>(pitch) * height;
        for (uint32_t y = 0; y < height / 2; y++)
        {
            memset(uv + static_cast<size_t>(y) * pitch, 128, width);
        }
    }

    bool SharedFrames::CopyFrame(BYTE* dst, LONG pitch, DWORD length, uint32_t width, uint32_t height)
    {
        if (pitch < static_cast<LONG>(width) || static_cast<uint64_t>(pitch) * height * 3 / 2 > length)
        {
            return false;
        }
        Header* h = m_header;
        if (h == nullptr)
        {
            FillBlack(dst, pitch, width, height);
            return false;
        }
        uint64_t now = GetTickCount64();
        h->readTick = now;
        if (h->frameCounter != 0)
        {
            uint32_t index = h->latestSlot;
            MemoryBarrier();
            if (index < kSlotCount)
            {
                Slot& s = h->slots[index];
                uint32_t seq = s.seq;
                MemoryBarrier();
                bool usable = (seq & 1) == 0 && s.format == kFourccNV12 && s.width == width && s.height == height &&
                              s.pitch >= width && static_cast<uint64_t>(s.pitch) * height * 3 / 2 <= kSlotSize &&
                              now - s.writeTick <= kStaleMs;
                if (usable)
                {
                    const BYTE* src = reinterpret_cast<const BYTE*>(h) + kHeaderSize + static_cast<size_t>(index) * kSlotSize;
                    const size_t srcPitch = s.pitch;
                    for (uint32_t y = 0; y < height; y++)
                    {
                        memcpy(dst + static_cast<size_t>(y) * pitch, src + y * srcPitch, width);
                    }
                    const BYTE* srcUv = src + srcPitch * height;
                    BYTE* dstUv = dst + static_cast<size_t>(pitch) * height;
                    for (uint32_t y = 0; y < height / 2; y++)
                    {
                        memcpy(dstUv + static_cast<size_t>(y) * pitch, srcUv + y * srcPitch, width);
                    }
                    // A torn frame (engine lapped us) is still a recent picture; show it.
                    return true;
                }
            }
        }
        FillBlack(dst, pitch, width, height);
        return false;
    }
}

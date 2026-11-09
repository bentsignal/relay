// Relay Camera: the media source's side of the shared-memory frame protocol.
// SPDX-License-Identifier: MIT
#pragma once

namespace relaycam
{
    class SharedFrames
    {
    public:
        SharedFrames() = default;
        ~SharedFrames();
        SharedFrames(const SharedFrames&) = delete;
        SharedFrames& operator=(const SharedFrames&) = delete;

        // Creates (or opens) the mapping. Safe to call repeatedly.
        HRESULT Open();
        void Close();

        // Tells the engine an app is streaming at this size (or no longer is).
        void SetActive(uint32_t width, uint32_t height, uint32_t fps);
        void SetInactive();

        // Fills an NV12 picture of width x height with the newest engine frame,
        // or black when there is none, it's the wrong size, or it's older than
        // kStaleMs.  Returns true when a real frame was copied.
        bool CopyFrame(BYTE* dst, LONG pitch, DWORD length, uint32_t width, uint32_t height);

    private:
        HANDLE m_mapping = nullptr;
        Header* m_header = nullptr;
    };

    void FillBlack(BYTE* dst, LONG pitch, uint32_t width, uint32_t height);
}

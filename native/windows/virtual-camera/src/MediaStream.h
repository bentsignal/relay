// Relay Camera: the single NV12 video stream.
//
// Derived from SimpleMediaStream in the Windows-Camera VirtualCamera sample.
// Copyright (c) Microsoft Corporation. Licensed under the MIT License; see
// LICENSE-Windows-Camera-sample.txt. Changes for Relay are also MIT licensed.
#pragma once
#include "SharedFrames.h"

namespace relaycam
{
    struct MediaSource;

    struct MediaStream : winrt::implements<MediaStream, IMFMediaStream2>
    {
        friend struct MediaSource;

        ~MediaStream();

        // IMFMediaEventGenerator
        IFACEMETHODIMP BeginGetEvent(IMFAsyncCallback* pCallback, IUnknown* punkState) override;
        IFACEMETHODIMP EndGetEvent(IMFAsyncResult* pResult, IMFMediaEvent** ppEvent) override;
        IFACEMETHODIMP GetEvent(DWORD dwFlags, IMFMediaEvent** ppEvent) override;
        IFACEMETHODIMP QueueEvent(MediaEventType met, REFGUID guidExtendedType, HRESULT hrStatus,
                                  const PROPVARIANT* pvValue) override;

        // IMFMediaStream
        IFACEMETHODIMP GetMediaSource(IMFMediaSource** ppMediaSource) override;
        IFACEMETHODIMP GetStreamDescriptor(IMFStreamDescriptor** ppStreamDescriptor) override;
        IFACEMETHODIMP RequestSample(IUnknown* pToken) override;

        // IMFMediaStream2
        IFACEMETHODIMP SetStreamState(MF_STREAM_STATE state) override;
        IFACEMETHODIMP GetStreamState(MF_STREAM_STATE* pState) override;

        HRESULT Initialize(IMFMediaSource* pSource, DWORD streamId);
        HRESULT Start(IMFMediaType* pMediaType);
        HRESULT Stop(bool sendEvent);
        HRESULT Shutdown();
        HRESULT SetSampleAllocator(IMFVideoSampleAllocator* pAllocator);
        DWORD Id() const { return m_streamId; }

    private:
        _Requires_lock_held_(m_lock) HRESULT CheckShutdownRequiresLock();
        _Requires_lock_held_(m_lock) HRESULT StartInternal(bool sendEvent, IMFMediaType* pNewMediaType);
        _Requires_lock_held_(m_lock) HRESULT StopInternal(bool sendEvent);
        _Requires_lock_held_(m_lock) HRESULT DeliverSampleRequiresLock();
        void PumpThread();

        winrt::slim_mutex m_lock;

        wil::com_ptr_nothrow<IMFMediaSource> m_parent;
        wil::com_ptr_nothrow<IMFMediaEventQueue> m_eventQueue;
        wil::com_ptr_nothrow<IMFAttributes> m_attributes;
        wil::com_ptr_nothrow<IMFStreamDescriptor> m_streamDesc;
        wil::com_ptr_nothrow<IMFVideoSampleAllocator> m_allocator;
        wil::com_ptr_nothrow<IMFMediaType> m_mediaType;

        // Outstanding RequestSample tokens (nullptr tokens included), answered
        // by the pump thread at the stream's frame rate.
        std::deque<wil::com_ptr_nothrow<IUnknown>> m_requests;
        std::thread m_pump;
        wil::unique_event m_wake{ wil::EventOptions::None };
        volatile bool m_quit = false;

        SharedFrames m_frames;
        uint32_t m_width = 0;
        uint32_t m_height = 0;
        uint32_t m_fps = kFps;
        LONGLONG m_nextDue = 0; // MFGetSystemTime of the next frame

        bool m_isShutdown = false;
        MF_STREAM_STATE m_streamState = MF_STREAM_STATE_STOPPED;
        DWORD m_streamId = 0;
    };
}

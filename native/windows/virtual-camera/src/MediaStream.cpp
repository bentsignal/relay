// Relay Camera: the single NV12 video stream.
//
// Derived from SimpleMediaStream in the Windows-Camera VirtualCamera sample.
// Copyright (c) Microsoft Corporation. Licensed under the MIT License; see
// LICENSE-Windows-Camera-sample.txt. Changes for Relay are also MIT licensed.
//
// Frames come from the Relay engine through shared memory (SharedFrames).
// Requests are answered by a pump thread at the nominal frame rate whether or
// not the engine is sending, so apps never stall: without a fresh frame they get
// black.
#include "pch.h"
#include "MediaStream.h"

namespace relaycam
{
    namespace
    {
        struct Size
        {
            uint32_t width;
            uint32_t height;
        };
        // Advertised formats, preferred first.
        constexpr Size kSizes[] = { { 1920, 1080 }, { 1280, 720 } };

        HRESULT CreateNv12Type(uint32_t width, uint32_t height, uint32_t fps, IMFMediaType** out)
        {
            wil::com_ptr_nothrow<IMFMediaType> t;
            RETURN_IF_FAILED(MFCreateMediaType(&t));
            RETURN_IF_FAILED(t->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video));
            RETURN_IF_FAILED(t->SetGUID(MF_MT_SUBTYPE, MFVideoFormat_NV12));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_ALL_SAMPLES_INDEPENDENT, TRUE));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_FIXED_SIZE_SAMPLES, TRUE));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_SAMPLE_SIZE, width * height * 3 / 2));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_DEFAULT_STRIDE, width));
            RETURN_IF_FAILED(MFSetAttributeSize(t.get(), MF_MT_FRAME_SIZE, width, height));
            RETURN_IF_FAILED(MFSetAttributeRatio(t.get(), MF_MT_FRAME_RATE, fps, 1));
            RETURN_IF_FAILED(MFSetAttributeRatio(t.get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_AVG_BITRATE, width * height * 12 * fps));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709));
            RETURN_IF_FAILED(t->SetUINT32(MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709));
            *out = t.detach();
            return S_OK;
        }

        HRESULT SetStreamAttributes(IMFAttributes* attrs, DWORD streamId)
        {
            RETURN_IF_FAILED(attrs->SetGUID(MF_DEVICESTREAM_STREAM_CATEGORY, PINNAME_VIDEO_CAPTURE));
            RETURN_IF_FAILED(attrs->SetUINT32(MF_DEVICESTREAM_STREAM_ID, streamId));
            RETURN_IF_FAILED(attrs->SetUINT32(MF_DEVICESTREAM_FRAMESERVER_SHARED, 1));
            RETURN_IF_FAILED(attrs->SetUINT32(MF_DEVICESTREAM_ATTRIBUTE_FRAMESOURCE_TYPES, MFFrameSourceTypes_Color));
            return S_OK;
        }
    }

    MediaStream::~MediaStream()
    {
        m_quit = true;
        if (m_wake)
        {
            m_wake.SetEvent();
        }
        if (m_pump.joinable())
        {
            m_pump.join();
        }
    }

    HRESULT MediaStream::Initialize(IMFMediaSource* pSource, DWORD streamId)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_HR_IF_NULL(E_INVALIDARG, pSource);
        m_parent = pSource;
        m_streamId = streamId;

        constexpr DWORD count = ARRAYSIZE(kSizes);
        wil::com_ptr_nothrow<IMFMediaType> types[count];
        IMFMediaType* raw[count] = {};
        for (DWORD i = 0; i < count; i++)
        {
            RETURN_IF_FAILED(CreateNv12Type(kSizes[i].width, kSizes[i].height, kFps, &types[i]));
            raw[i] = types[i].get();
        }

        RETURN_IF_FAILED(MFCreateAttributes(&m_attributes, 10));
        RETURN_IF_FAILED(SetStreamAttributes(m_attributes.get(), m_streamId));
        RETURN_IF_FAILED(MFCreateEventQueue(&m_eventQueue));
        RETURN_IF_FAILED(MFCreateStreamDescriptor(m_streamId, count, raw, &m_streamDesc));

        wil::com_ptr_nothrow<IMFMediaTypeHandler> handler;
        RETURN_IF_FAILED(m_streamDesc->GetMediaTypeHandler(&handler));
        RETURN_IF_FAILED(handler->SetCurrentMediaType(raw[0]));
        RETURN_IF_FAILED(SetStreamAttributes(m_streamDesc.get(), m_streamId));

        m_pump = std::thread([this] { PumpThread(); });
        return S_OK;
    }

    // IMFMediaEventGenerator
    IFACEMETHODIMP MediaStream::BeginGetEvent(IMFAsyncCallback* pCallback, IUnknown* punkState)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_IF_FAILED(m_eventQueue->BeginGetEvent(pCallback, punkState));
        return S_OK;
    }

    IFACEMETHODIMP MediaStream::EndGetEvent(IMFAsyncResult* pResult, IMFMediaEvent** ppEvent)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_IF_FAILED(m_eventQueue->EndGetEvent(pResult, ppEvent));
        return S_OK;
    }

    IFACEMETHODIMP MediaStream::GetEvent(DWORD dwFlags, IMFMediaEvent** ppEvent)
    {
        // GetEvent can block indefinitely, so don't hold the lock.
        wil::com_ptr_nothrow<IMFMediaEventQueue> queue;
        {
            winrt::slim_lock_guard lock(m_lock);
            RETURN_IF_FAILED(CheckShutdownRequiresLock());
            queue = m_eventQueue;
        }
        RETURN_IF_FAILED(queue->GetEvent(dwFlags, ppEvent));
        return S_OK;
    }

    IFACEMETHODIMP MediaStream::QueueEvent(MediaEventType eventType, REFGUID guidExtendedType, HRESULT hrStatus,
                                           PROPVARIANT const* pvValue)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_IF_FAILED(m_eventQueue->QueueEventParamVar(eventType, guidExtendedType, hrStatus, pvValue));
        return S_OK;
    }

    // IMFMediaStream
    IFACEMETHODIMP MediaStream::GetMediaSource(IMFMediaSource** ppMediaSource)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_HR_IF_NULL(E_POINTER, ppMediaSource);
        *ppMediaSource = nullptr;
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_IF_FAILED(m_parent.copy_to(ppMediaSource));
        return S_OK;
    }

    IFACEMETHODIMP MediaStream::GetStreamDescriptor(IMFStreamDescriptor** ppStreamDescriptor)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_HR_IF_NULL(E_POINTER, ppStreamDescriptor);
        *ppStreamDescriptor = nullptr;
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_HR_IF_NULL(E_UNEXPECTED, m_streamDesc);
        RETURN_IF_FAILED(m_streamDesc.copy_to(ppStreamDescriptor));
        return S_OK;
    }

    IFACEMETHODIMP MediaStream::RequestSample(IUnknown* pToken)
    {
        {
            winrt::slim_lock_guard lock(m_lock);
            RETURN_IF_FAILED(CheckShutdownRequiresLock());
            if (m_streamState != MF_STREAM_STATE_RUNNING)
            {
                RETURN_HR_MSG(MF_E_INVALIDREQUEST, "stream not running, state:%d", m_streamState);
            }
            wil::com_ptr_nothrow<IUnknown> token = pToken;
            m_requests.push_back(std::move(token));
        }
        m_wake.SetEvent();
        return S_OK;
    }

    // IMFMediaStream2
    IFACEMETHODIMP MediaStream::SetStreamState(MF_STREAM_STATE state)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        if (m_streamState == state)
        {
            return S_OK;
        }
        switch (state)
        {
        case MF_STREAM_STATE_PAUSED:
            if (m_streamState != MF_STREAM_STATE_RUNNING)
            {
                return MF_E_INVALID_STATE_TRANSITION;
            }
            m_streamState = MF_STREAM_STATE_PAUSED;
            break;
        case MF_STREAM_STATE_RUNNING:
            RETURN_IF_FAILED(StartInternal(false, nullptr));
            break;
        case MF_STREAM_STATE_STOPPED:
            RETURN_IF_FAILED(StopInternal(false));
            break;
        default:
            return MF_E_INVALID_STATE_TRANSITION;
        }
        return S_OK;
    }

    IFACEMETHODIMP MediaStream::GetStreamState(MF_STREAM_STATE* pState)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_HR_IF_NULL(E_INVALIDARG, pState);
        *pState = m_streamState;
        return S_OK;
    }

    HRESULT MediaStream::Start(IMFMediaType* pMediaType)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_HR_IF_NULL(E_INVALIDARG, pMediaType);
        if (m_mediaType == nullptr)
        {
            m_mediaType = pMediaType;
        }
        RETURN_IF_FAILED(StartInternal(true, pMediaType));
        return S_OK;
    }

    HRESULT MediaStream::Stop(bool sendEvent)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_IF_FAILED(StopInternal(sendEvent));
        return S_OK;
    }

    HRESULT MediaStream::Shutdown()
    {
        {
            winrt::slim_lock_guard lock(m_lock);
            m_isShutdown = true;
            m_parent.reset();
            if (m_eventQueue != nullptr)
            {
                m_eventQueue->Shutdown();
                m_eventQueue.reset();
            }
            m_attributes.reset();
            m_streamDesc.reset();
            m_requests.clear();
            m_streamState = MF_STREAM_STATE_STOPPED;
            m_quit = true;
        }
        m_wake.SetEvent();
        if (m_pump.joinable() && m_pump.get_id() != std::this_thread::get_id())
        {
            m_pump.join();
        }
        winrt::slim_lock_guard lock(m_lock);
        m_frames.Close();
        m_allocator.reset();
        return S_OK;
    }

    HRESULT MediaStream::SetSampleAllocator(IMFVideoSampleAllocator* pAllocator)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        if (m_streamState == MF_STREAM_STATE_RUNNING)
        {
            RETURN_HR_MSG(MF_E_INVALIDREQUEST, "Cannot update allocator while streaming");
        }
        m_allocator = pAllocator;
        return S_OK;
    }

    HRESULT MediaStream::CheckShutdownRequiresLock()
    {
        if (m_isShutdown)
        {
            return MF_E_SHUTDOWN;
        }
        if (m_eventQueue == nullptr)
        {
            return E_UNEXPECTED;
        }
        return S_OK;
    }

    HRESULT MediaStream::StartInternal(bool sendEvent, IMFMediaType* pNewMediaType)
    {
        BOOL match = FALSE;
        if (m_mediaType && pNewMediaType)
        {
            (void)m_mediaType->Compare(pNewMediaType, MF_ATTRIBUTES_MATCH_ALL_ITEMS, &match);
            if (!match)
            {
                m_mediaType = pNewMediaType;
            }
        }
        RETURN_HR_IF_NULL(MF_E_NOT_INITIALIZED, m_mediaType);

        if (m_streamState != MF_STREAM_STATE_RUNNING || !match)
        {
            // Frame Server always hands us an allocator
            // (MFSampleAllocatorUsage_UsesProvidedAllocator); create one if not.
            if (m_allocator == nullptr)
            {
                RETURN_IF_FAILED(MFCreateVideoSampleAllocatorEx(IID_PPV_ARGS(&m_allocator)));
            }
            UINT32 width = 0, height = 0, num = kFps, den = 1;
            RETURN_IF_FAILED(MFGetAttributeSize(m_mediaType.get(), MF_MT_FRAME_SIZE, &width, &height));
            (void)MFGetAttributeRatio(m_mediaType.get(), MF_MT_FRAME_RATE, &num, &den);
            RETURN_HR_IF(MF_E_INVALIDMEDIATYPE, width == 0 || height == 0 || width > kMaxWidth || height > kMaxHeight);
            DEBUG_MSG(L"start %ux%u @ %u/%u", width, height, num, den);
            RETURN_IF_FAILED(m_allocator->InitializeSampleAllocator(10, m_mediaType.get()));
            m_width = width;
            m_height = height;
            m_fps = (den != 0 && num / den > 0) ? num / den : kFps;
        }

        HRESULT openHr = m_frames.Open();
        if (FAILED(openHr))
        {
            DEBUG_MSG(L"shared frames unavailable (0x%08x); sending black", openHr);
        }
        m_frames.SetActive(m_width, m_height, m_fps);
        m_nextDue = 0;

        if (sendEvent)
        {
            RETURN_IF_FAILED(m_eventQueue->QueueEventParamVar(MEStreamStarted, GUID_NULL, S_OK, nullptr));
        }
        m_streamState = MF_STREAM_STATE_RUNNING;
        m_wake.SetEvent();
        return S_OK;
    }

    HRESULT MediaStream::StopInternal(bool sendEvent)
    {
        m_streamState = MF_STREAM_STATE_STOPPED;
        m_requests.clear();
        m_frames.SetInactive();
        if (sendEvent)
        {
            RETURN_IF_FAILED(m_eventQueue->QueueEventParamVar(MEStreamStopped, GUID_NULL, S_OK, nullptr));
        }
        return S_OK;
    }

    HRESULT MediaStream::DeliverSampleRequiresLock()
    {
        RETURN_HR_IF_NULL(E_UNEXPECTED, m_allocator);
        wil::com_ptr_nothrow<IMFSample> sample;
        HRESULT hr = m_allocator->AllocateSample(&sample);
        if (hr == MF_E_SAMPLEALLOCATOR_EMPTY)
        {
            return S_FALSE; // the app still holds all our samples; try next tick
        }
        RETURN_IF_FAILED(hr);

        wil::com_ptr_nothrow<IMFMediaBuffer> buffer;
        RETURN_IF_FAILED(sample->GetBufferByIndex(0, &buffer));
        if (auto buffer2d = buffer.try_query<IMF2DBuffer2>())
        {
            BYTE* scanline0 = nullptr;
            BYTE* start = nullptr;
            LONG pitch = 0;
            DWORD length = 0;
            RETURN_IF_FAILED(buffer2d->Lock2DSize(MF2DBuffer_LockFlags_Write, &scanline0, &pitch, &start, &length));
            DWORD usable = length - static_cast<DWORD>(scanline0 - start);
            if (pitch > 0)
            {
                m_frames.CopyFrame(scanline0, pitch, usable, m_width, m_height);
            }
            RETURN_IF_FAILED(buffer2d->Unlock2D());
        }
        else
        {
            BYTE* data = nullptr;
            DWORD maxLength = 0;
            RETURN_IF_FAILED(buffer->Lock(&data, &maxLength, nullptr));
            m_frames.CopyFrame(data, static_cast<LONG>(m_width), maxLength, m_width, m_height);
            RETURN_IF_FAILED(buffer->Unlock());
            RETURN_IF_FAILED(buffer->SetCurrentLength(m_width * m_height * 3 / 2));
        }

        RETURN_IF_FAILED(sample->SetSampleTime(MFGetSystemTime()));
        RETURN_IF_FAILED(sample->SetSampleDuration(10'000'000LL / m_fps));
        wil::com_ptr_nothrow<IUnknown> token = std::move(m_requests.front());
        m_requests.pop_front();
        if (token)
        {
            RETURN_IF_FAILED(sample->SetUnknown(MFSampleExtension_Token, token.get()));
        }
        RETURN_IF_FAILED(m_eventQueue->QueueEventParamUnk(MEMediaSample, GUID_NULL, S_OK, sample.get()));
        return S_OK;
    }

    void MediaStream::PumpThread()
    {
        wil::unique_handle timer(
            CreateWaitableTimerExW(nullptr, nullptr, CREATE_WAITABLE_TIMER_HIGH_RESOLUTION, TIMER_ALL_ACCESS));
        if (!timer)
        {
            timer.reset(CreateWaitableTimerExW(nullptr, nullptr, 0, TIMER_ALL_ACCESS));
        }
        while (!m_quit)
        {
            LONGLONG wait = -1; // 100 ns units until the next frame is due; -1 = idle
            {
                winrt::slim_lock_guard lock(m_lock);
                if (!m_isShutdown && m_streamState == MF_STREAM_STATE_RUNNING && !m_requests.empty())
                {
                    const LONGLONG period = 10'000'000LL / m_fps;
                    const LONGLONG now = MFGetSystemTime();
                    if (m_nextDue == 0 || now - m_nextDue > period)
                    {
                        m_nextDue = now; // first frame, or we fell behind: don't burst
                    }
                    if (now >= m_nextDue)
                    {
                        HRESULT hr = DeliverSampleRequiresLock();
                        if (hr == S_OK)
                        {
                            m_nextDue += period;
                        }
                        else if (FAILED(hr))
                        {
                            DEBUG_MSG(L"deliver failed 0x%08x", hr);
                            m_nextDue = now + period;
                        }
                        else
                        {
                            m_nextDue = now + period / 4; // allocator empty
                        }
                    }
                    wait = m_requests.empty() ? -1 : std::max<LONGLONG>(m_nextDue - MFGetSystemTime(), 0);
                }
            }
            if (wait < 0)
            {
                WaitForSingleObject(m_wake.get(), 1000);
            }
            else if (wait > 0 && timer)
            {
                LARGE_INTEGER due;
                due.QuadPart = -wait;
                SetWaitableTimer(timer.get(), &due, 0, nullptr, nullptr, FALSE);
                HANDLE handles[] = { timer.get(), m_wake.get() };
                // Wake only matters for shutdown/state changes while waiting.
                WaitForMultipleObjects(2, handles, FALSE, INFINITE);
            }
            else if (wait > 0)
            {
                Sleep(static_cast<DWORD>(wait / 10'000));
            }
        }
    }
}

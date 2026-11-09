// Relay Camera: the media source Frame Server activates.
//
// Derived from SimpleMediaSource in the Windows-Camera VirtualCamera sample.
// Copyright (c) Microsoft Corporation. Licensed under the MIT License; see
// LICENSE-Windows-Camera-sample.txt. Changes for Relay are also MIT licensed.
#include "pch.h"
#include "MediaSource.h"

namespace relaycam
{
    HRESULT MediaSource::Initialize(IMFAttributes* pActivateAttributes)
    {
        winrt::slim_lock_guard lock(m_lock);
        if (m_state != State::Invalid)
        {
            return MF_E_ALREADY_INITIALIZED;
        }
        RETURN_IF_FAILED(CreateSourceAttributes(pActivateAttributes));
        RETURN_IF_FAILED(MFCreateEventQueue(&m_eventQueue));

        m_stream = winrt::make_self<MediaStream>();
        RETURN_IF_FAILED(m_stream->Initialize(static_cast<IMFMediaSourceEx*>(this), 0));
        wil::com_ptr_nothrow<IMFStreamDescriptor> descriptor;
        RETURN_IF_FAILED(m_stream->GetStreamDescriptor(&descriptor));
        IMFStreamDescriptor* descriptors[] = { descriptor.get() };
        RETURN_IF_FAILED(MFCreatePresentationDescriptor(1, descriptors, &m_presentation));
        m_state = State::Stopped;
        return S_OK;
    }

    // IMFMediaEventGenerator
    IFACEMETHODIMP MediaSource::BeginGetEvent(IMFAsyncCallback* pCallback, IUnknown* punkState)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_IF_FAILED(m_eventQueue->BeginGetEvent(pCallback, punkState));
        return S_OK;
    }

    IFACEMETHODIMP MediaSource::EndGetEvent(IMFAsyncResult* pResult, IMFMediaEvent** ppEvent)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_IF_FAILED(m_eventQueue->EndGetEvent(pResult, ppEvent));
        return S_OK;
    }

    IFACEMETHODIMP MediaSource::GetEvent(DWORD dwFlags, IMFMediaEvent** ppEvent)
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

    IFACEMETHODIMP MediaSource::QueueEvent(MediaEventType eventType, REFGUID guidExtendedType, HRESULT hrStatus,
                                           PROPVARIANT const* pvValue)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_IF_FAILED(m_eventQueue->QueueEventParamVar(eventType, guidExtendedType, hrStatus, pvValue));
        return S_OK;
    }

    // IMFMediaSource
    IFACEMETHODIMP MediaSource::CreatePresentationDescriptor(IMFPresentationDescriptor** ppPresentationDescriptor)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_HR_IF_NULL(E_POINTER, ppPresentationDescriptor);
        *ppPresentationDescriptor = nullptr;
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_IF_FAILED(m_presentation->Clone(ppPresentationDescriptor));
        // Our one stream is selected by default, like a real camera's. (Our own
        // descriptor tracks what the caller actually started.)
        RETURN_IF_FAILED((*ppPresentationDescriptor)->SelectStream(0));
        return S_OK;
    }

    IFACEMETHODIMP MediaSource::GetCharacteristics(DWORD* pdwCharacteristics)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_HR_IF_NULL(E_POINTER, pdwCharacteristics);
        *pdwCharacteristics = 0;
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        *pdwCharacteristics = MFMEDIASOURCE_IS_LIVE;
        return S_OK;
    }

    IFACEMETHODIMP MediaSource::Pause()
    {
        return MF_E_INVALID_STATE_TRANSITION;
    }

    IFACEMETHODIMP MediaSource::Shutdown()
    {
        winrt::com_ptr<MediaStream> stream;
        {
            winrt::slim_lock_guard lock(m_lock);
            m_state = State::Shutdown;
            m_attributes.reset();
            m_presentation.reset();
            if (m_eventQueue != nullptr)
            {
                m_eventQueue->Shutdown();
                m_eventQueue.reset();
            }
            stream = std::move(m_stream);
        }
        // Outside our lock: the stream joins its pump thread.
        if (stream)
        {
            stream->Shutdown();
        }
        return S_OK;
    }

    IFACEMETHODIMP MediaSource::Start(IMFPresentationDescriptor* pPresentationDescriptor, const GUID* pguidTimeFormat,
                                      const PROPVARIANT* pvarStartPos)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_HR_IF(E_INVALIDARG, pPresentationDescriptor == nullptr || pvarStartPos == nullptr);
        RETURN_HR_IF(MF_E_UNSUPPORTED_TIME_FORMAT, pguidTimeFormat != nullptr && *pguidTimeFormat != GUID_NULL);
        RETURN_HR_IF(MF_E_INVALID_STATE_TRANSITION, m_state == State::Invalid);

        DWORD count = 0;
        RETURN_IF_FAILED(pPresentationDescriptor->GetStreamDescriptorCount(&count));
        RETURN_HR_IF(E_INVALIDARG, count != 1);

        wil::unique_prop_variant startTime;
        RETURN_IF_FAILED(InitPropVariantFromInt64(MFGetSystemTime(), &startTime));

        for (DWORD i = 0; i < count; i++)
        {
            BOOL selected = FALSE;
            wil::com_ptr_nothrow<IMFStreamDescriptor> desc;
            RETURN_IF_FAILED(pPresentationDescriptor->GetStreamDescriptorByIndex(i, &selected, &desc));
            DWORD streamId = 0;
            RETURN_IF_FAILED(desc->GetStreamIdentifier(&streamId));

            DWORD index = 0;
            bool wasSelected = false;
            wil::com_ptr_nothrow<IMFStreamDescriptor> local;
            RETURN_IF_FAILED(GetStreamDescriptorById(streamId, &index, &wasSelected, &local));

            if (selected)
            {
                RETURN_IF_FAILED(m_presentation->SelectStream(index));
                wil::com_ptr_nothrow<IMFMediaTypeHandler> handler;
                wil::com_ptr_nothrow<IMFMediaType> type;
                RETURN_IF_FAILED(desc->GetMediaTypeHandler(&handler));
                RETURN_IF_FAILED(handler->GetCurrentMediaType(&type));

                wil::com_ptr_nothrow<IUnknown> unk;
                RETURN_IF_FAILED(m_stream->QueryInterface(IID_PPV_ARGS(&unk)));
                RETURN_IF_FAILED(
                    m_eventQueue->QueueEventParamUnk(wasSelected ? MEUpdatedStream : MENewStream, GUID_NULL, S_OK, unk.get()));
                RETURN_IF_FAILED(m_stream->Start(type.get())); // sends MEStreamStarted
            }
            else if (wasSelected)
            {
                RETURN_IF_FAILED(m_presentation->DeselectStream(index));
                RETURN_IF_FAILED(m_stream->Stop(false));
            }
        }

        RETURN_IF_FAILED(m_eventQueue->QueueEventParamVar(MESourceStarted, GUID_NULL, S_OK, &startTime));
        m_state = State::Started;
        return S_OK;
    }

    IFACEMETHODIMP MediaSource::Stop()
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_HR_IF(MF_E_INVALID_STATE_TRANSITION, m_state != State::Started);
        m_state = State::Stopped;

        wil::unique_prop_variant stopTime;
        RETURN_IF_FAILED(InitPropVariantFromInt64(MFGetSystemTime(), &stopTime));
        RETURN_IF_FAILED(m_stream->Stop(true));
        RETURN_IF_FAILED(m_presentation->DeselectStream(0));
        RETURN_IF_FAILED(m_eventQueue->QueueEventParamVar(MESourceStopped, GUID_NULL, S_OK, &stopTime));
        return S_OK;
    }

    // IMFMediaSourceEx
    IFACEMETHODIMP MediaSource::GetSourceAttributes(IMFAttributes** sourceAttributes)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_HR_IF_NULL(E_POINTER, sourceAttributes);
        *sourceAttributes = nullptr;
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        // For video capture sources this must be a reference, not a copy.
        RETURN_IF_FAILED(m_attributes.copy_to(sourceAttributes));
        return S_OK;
    }

    IFACEMETHODIMP MediaSource::GetStreamAttributes(DWORD dwStreamIdentifier, IMFAttributes** ppAttributes)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_HR_IF_NULL(E_POINTER, ppAttributes);
        *ppAttributes = nullptr;
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        winrt::com_ptr<MediaStream> stream;
        RETURN_IF_FAILED(GetStreamById(dwStreamIdentifier, stream.put()));
        RETURN_IF_FAILED(stream->m_attributes.copy_to(ppAttributes));
        return S_OK;
    }

    IFACEMETHODIMP MediaSource::SetD3DManager(IUnknown*)
    {
        // Best effort hint only; we produce system-memory frames.
        return E_NOTIMPL;
    }

    IFACEMETHODIMP MediaSource::GetService(REFGUID, REFIID, LPVOID* ppvObject)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_HR_IF_NULL(E_POINTER, ppvObject);
        *ppvObject = nullptr;
        return MF_E_UNSUPPORTED_SERVICE;
    }

    // IKsControl. ERROR_SET_NOT_FOUND is what a driver without the property
    // set returns.
    IFACEMETHODIMP MediaSource::KsProperty(PKSPROPERTY pProperty, ULONG ulPropertyLength, LPVOID, ULONG, ULONG*)
    {
        RETURN_HR_IF(E_INVALIDARG, pProperty == nullptr || ulPropertyLength < sizeof(KSPROPERTY));
        return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);
    }

    IFACEMETHODIMP MediaSource::KsMethod(PKSMETHOD, ULONG, LPVOID, ULONG, ULONG*)
    {
        return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);
    }

    IFACEMETHODIMP MediaSource::KsEvent(PKSEVENT, ULONG, LPVOID, ULONG, ULONG*)
    {
        return HRESULT_FROM_WIN32(ERROR_SET_NOT_FOUND);
    }

    // IMFSampleAllocatorControl
    IFACEMETHODIMP MediaSource::SetDefaultAllocator(DWORD dwOutputStreamID, IUnknown* pAllocator)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_HR_IF_NULL(E_POINTER, pAllocator);
        wil::com_ptr_nothrow<IMFVideoSampleAllocator> allocator;
        RETURN_IF_FAILED(pAllocator->QueryInterface(IID_PPV_ARGS(&allocator)));
        winrt::com_ptr<MediaStream> stream;
        RETURN_IF_FAILED(GetStreamById(dwOutputStreamID, stream.put()));
        RETURN_IF_FAILED(stream->SetSampleAllocator(allocator.get()));
        return S_OK;
    }

    IFACEMETHODIMP MediaSource::GetAllocatorUsage(DWORD dwOutputStreamID, DWORD* pdwInputStreamID,
                                                  MFSampleAllocatorUsage* peUsage)
    {
        winrt::slim_lock_guard lock(m_lock);
        RETURN_IF_FAILED(CheckShutdownRequiresLock());
        RETURN_HR_IF_NULL(E_POINTER, pdwInputStreamID);
        RETURN_HR_IF_NULL(E_POINTER, peUsage);
        winrt::com_ptr<MediaStream> stream;
        RETURN_IF_FAILED(GetStreamById(dwOutputStreamID, stream.put()));
        *peUsage = MFSampleAllocatorUsage_UsesProvidedAllocator;
        *pdwInputStreamID = dwOutputStreamID;
        return S_OK;
    }

    HRESULT MediaSource::CheckShutdownRequiresLock()
    {
        if (m_state == State::Shutdown)
        {
            return MF_E_SHUTDOWN;
        }
        if (m_eventQueue == nullptr || m_stream == nullptr)
        {
            return E_UNEXPECTED;
        }
        return S_OK;
    }

    HRESULT MediaSource::CreateSourceAttributes(IMFAttributes* pActivateAttributes)
    {
        RETURN_IF_FAILED(MFCreateAttributes(&m_attributes, 1));
        if (pActivateAttributes)
        {
            RETURN_IF_FAILED(pActivateAttributes->CopyAllItems(m_attributes.get()));
        }

        // The legacy profile is mandatory so profile-unaware apps keep working.
        wil::com_ptr_nothrow<IMFSensorProfileCollection> profiles;
        wil::com_ptr_nothrow<IMFSensorProfile> profile;
        RETURN_IF_FAILED(MFCreateSensorProfileCollection(&profiles));
        RETURN_IF_FAILED(MFCreateSensorProfile(KSCAMERAPROFILE_Legacy, 0, nullptr, &profile));
        RETURN_IF_FAILED(profile->AddProfileFilter(0, L"((RES==;FRT<=30,1;SUT==))"));
        RETURN_IF_FAILED(profiles->AddProfile(profile.get()));
        RETURN_IF_FAILED(m_attributes->SetUnknown(MF_DEVICEMFT_SENSORPROFILE_COLLECTION, profiles.get()));
        return S_OK;
    }

    HRESULT MediaSource::GetStreamDescriptorById(DWORD streamId, DWORD* index, bool* selected,
                                                 IMFStreamDescriptor** descriptor)
    {
        *descriptor = nullptr;
        *index = 0;
        *selected = false;
        DWORD count = 0;
        RETURN_IF_FAILED(m_presentation->GetStreamDescriptorCount(&count));
        for (DWORD i = 0; i < count; i++)
        {
            wil::com_ptr_nothrow<IMFStreamDescriptor> desc;
            BOOL sel = FALSE;
            RETURN_IF_FAILED(m_presentation->GetStreamDescriptorByIndex(i, &sel, &desc));
            DWORD id = 0;
            RETURN_IF_FAILED(desc->GetStreamIdentifier(&id));
            if (id == streamId)
            {
                *index = i;
                *selected = !!sel;
                *descriptor = desc.detach();
                return S_OK;
            }
        }
        return MF_E_NOT_FOUND;
    }

    HRESULT MediaSource::GetStreamById(DWORD streamId, MediaStream** stream)
    {
        *stream = nullptr;
        if (m_stream && m_stream->Id() == streamId)
        {
            m_stream.copy_to(stream);
            return S_OK;
        }
        return MF_E_NOT_FOUND;
    }
}

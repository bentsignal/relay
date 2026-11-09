// Relay Camera: the media source Frame Server activates.
//
// Derived from SimpleMediaSource in the Windows-Camera VirtualCamera sample.
// Copyright (c) Microsoft Corporation. Licensed under the MIT License; see
// LICENSE-Windows-Camera-sample.txt. Changes for Relay are also MIT licensed.
#pragma once
#include "MediaStream.h"

namespace relaycam
{
    struct MediaSource
        : winrt::implements<MediaSource, IMFMediaSourceEx, IMFGetService, IKsControl, IMFSampleAllocatorControl>
    {
        // IMFMediaEventGenerator
        IFACEMETHODIMP BeginGetEvent(IMFAsyncCallback* pCallback, IUnknown* punkState) override;
        IFACEMETHODIMP EndGetEvent(IMFAsyncResult* pResult, IMFMediaEvent** ppEvent) override;
        IFACEMETHODIMP GetEvent(DWORD dwFlags, IMFMediaEvent** ppEvent) override;
        IFACEMETHODIMP QueueEvent(MediaEventType met, REFGUID guidExtendedType, HRESULT hrStatus,
                                  const PROPVARIANT* pvValue) override;

        // IMFMediaSource
        IFACEMETHODIMP CreatePresentationDescriptor(IMFPresentationDescriptor** ppPresentationDescriptor) override;
        IFACEMETHODIMP GetCharacteristics(DWORD* pdwCharacteristics) override;
        IFACEMETHODIMP Pause() override;
        IFACEMETHODIMP Shutdown() override;
        IFACEMETHODIMP Start(IMFPresentationDescriptor* pPresentationDescriptor, const GUID* pguidTimeFormat,
                             const PROPVARIANT* pvarStartPosition) override;
        IFACEMETHODIMP Stop() override;

        // IMFMediaSourceEx
        IFACEMETHODIMP GetSourceAttributes(IMFAttributes** ppAttributes) override;
        IFACEMETHODIMP GetStreamAttributes(DWORD dwStreamIdentifier, IMFAttributes** ppAttributes) override;
        IFACEMETHODIMP SetD3DManager(IUnknown* pManager) override;

        // IMFGetService
        IFACEMETHODIMP GetService(REFGUID guidService, REFIID riid, LPVOID* ppvObject) override;

        // IKsControl: no custom controls.
        IFACEMETHODIMP KsProperty(PKSPROPERTY pProperty, ULONG ulPropertyLength, LPVOID pPropertyData,
                                  ULONG ulDataLength, ULONG* pBytesReturned) override;
        IFACEMETHODIMP KsMethod(PKSMETHOD pMethod, ULONG ulMethodLength, LPVOID pMethodData, ULONG ulDataLength,
                                ULONG* pBytesReturned) override;
        IFACEMETHODIMP KsEvent(PKSEVENT pEvent, ULONG ulEventLength, LPVOID pEventData, ULONG ulDataLength,
                               ULONG* pBytesReturned) override;

        // IMFSampleAllocatorControl
        IFACEMETHODIMP SetDefaultAllocator(DWORD dwOutputStreamID, IUnknown* pAllocator) override;
        IFACEMETHODIMP GetAllocatorUsage(DWORD dwOutputStreamID, DWORD* pdwInputStreamID,
                                         MFSampleAllocatorUsage* peUsage) override;

        HRESULT Initialize(IMFAttributes* pActivateAttributes);

    private:
        enum class State
        {
            Invalid,
            Stopped,
            Started,
            Shutdown
        };

        _Requires_lock_held_(m_lock) HRESULT CheckShutdownRequiresLock();
        _Requires_lock_held_(m_lock) HRESULT CreateSourceAttributes(IMFAttributes* pActivateAttributes);
        _Requires_lock_held_(m_lock) HRESULT GetStreamDescriptorById(DWORD streamId, DWORD* index, bool* selected,
                                                                     IMFStreamDescriptor** descriptor);
        _Requires_lock_held_(m_lock) HRESULT GetStreamById(DWORD streamId, MediaStream** stream);

        winrt::slim_mutex m_lock;
        State m_state = State::Invalid;
        wil::com_ptr_nothrow<IMFMediaEventQueue> m_eventQueue;
        wil::com_ptr_nothrow<IMFPresentationDescriptor> m_presentation;
        wil::com_ptr_nothrow<IMFAttributes> m_attributes;
        winrt::com_ptr<MediaStream> m_stream; // one stream, id 0
    };
}

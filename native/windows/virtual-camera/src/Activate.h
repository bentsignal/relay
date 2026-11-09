// Relay Camera: the COM class Frame Server creates (an IMFActivate that makes the
// media source).
//
// Derived from VirtualCameraMediaSourceActivate in the Windows-Camera
// VirtualCamera sample. Copyright (c) Microsoft Corporation. Licensed under the
// MIT License; see LICENSE-Windows-Camera-sample.txt. Changes for Relay are also
// MIT licensed.
#pragma once
#include "MediaSource.h"

namespace relaycam
{
    struct __declspec(uuid(RELAY_CAMERA_CLSID_UUID)) Activate : winrt::implements<Activate, IMFActivate>
    {
        HRESULT Initialize();

        // IMFActivate
        IFACEMETHODIMP ActivateObject(REFIID riid, void** ppv) override;
        IFACEMETHODIMP ShutdownObject() override;
        IFACEMETHODIMP DetachObject() override;

        // IMFAttributes, forwarded to m_attributes.
        IFACEMETHODIMP GetItem(REFGUID guidKey, PROPVARIANT* pValue) override;
        IFACEMETHODIMP GetItemType(REFGUID guidKey, MF_ATTRIBUTE_TYPE* pType) override;
        IFACEMETHODIMP CompareItem(REFGUID guidKey, REFPROPVARIANT Value, BOOL* pbResult) override;
        IFACEMETHODIMP Compare(IMFAttributes* pTheirs, MF_ATTRIBUTES_MATCH_TYPE MatchType, BOOL* pbResult) override;
        IFACEMETHODIMP GetUINT32(REFGUID guidKey, UINT32* punValue) override;
        IFACEMETHODIMP GetUINT64(REFGUID guidKey, UINT64* punValue) override;
        IFACEMETHODIMP GetDouble(REFGUID guidKey, double* pfValue) override;
        IFACEMETHODIMP GetGUID(REFGUID guidKey, GUID* pguidValue) override;
        IFACEMETHODIMP GetStringLength(REFGUID guidKey, UINT32* pcchLength) override;
        IFACEMETHODIMP GetString(REFGUID guidKey, LPWSTR pwszValue, UINT32 cchBufSize, UINT32* pcchLength) override;
        IFACEMETHODIMP GetAllocatedString(REFGUID guidKey, LPWSTR* ppwszValue, UINT32* pcchLength) override;
        IFACEMETHODIMP GetBlobSize(REFGUID guidKey, UINT32* pcbBlobSize) override;
        IFACEMETHODIMP GetBlob(REFGUID guidKey, UINT8* pBuf, UINT32 cbBufSize, UINT32* pcbBlobSize) override;
        IFACEMETHODIMP GetAllocatedBlob(REFGUID guidKey, UINT8** ppBuf, UINT32* pcbSize) override;
        IFACEMETHODIMP GetUnknown(REFGUID guidKey, REFIID riid, LPVOID* ppv) override;
        IFACEMETHODIMP SetItem(REFGUID guidKey, REFPROPVARIANT Value) override;
        IFACEMETHODIMP DeleteItem(REFGUID guidKey) override;
        IFACEMETHODIMP DeleteAllItems() override;
        IFACEMETHODIMP SetUINT32(REFGUID guidKey, UINT32 unValue) override;
        IFACEMETHODIMP SetUINT64(REFGUID guidKey, UINT64 unValue) override;
        IFACEMETHODIMP SetDouble(REFGUID guidKey, double fValue) override;
        IFACEMETHODIMP SetGUID(REFGUID guidKey, REFGUID guidValue) override;
        IFACEMETHODIMP SetString(REFGUID guidKey, LPCWSTR wszValue) override;
        IFACEMETHODIMP SetBlob(REFGUID guidKey, const UINT8* pBuf, UINT32 cbBufSize) override;
        IFACEMETHODIMP SetUnknown(REFGUID guidKey, IUnknown* pUnknown) override;
        IFACEMETHODIMP LockStore() override;
        IFACEMETHODIMP UnlockStore() override;
        IFACEMETHODIMP GetCount(UINT32* pcItems) override;
        IFACEMETHODIMP GetItemByIndex(UINT32 unIndex, GUID* pguidKey, PROPVARIANT* pValue) override;
        IFACEMETHODIMP CopyAllItems(IMFAttributes* pDest) override;

    private:
        wil::com_ptr_nothrow<IMFAttributes> m_attributes;
        winrt::com_ptr<MediaSource> m_source;
    };

    struct ActivateFactory : winrt::implements<ActivateFactory, IClassFactory>
    {
        STDMETHODIMP CreateInstance(IUnknown* outer, REFIID riid, void** result) noexcept final;
        STDMETHODIMP LockServer(BOOL) noexcept final { return S_OK; }
    };
}

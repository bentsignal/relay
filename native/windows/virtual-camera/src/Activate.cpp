// Relay Camera: the COM class Frame Server creates.
//
// Derived from VirtualCameraMediaSourceActivate in the Windows-Camera
// VirtualCamera sample. Copyright (c) Microsoft Corporation. Licensed under the
// MIT License; see LICENSE-Windows-Camera-sample.txt. Changes for Relay are also
// MIT licensed.
#include "pch.h"
#include "Activate.h"

namespace relaycam
{
    HRESULT Activate::Initialize()
    {
        RETURN_IF_FAILED(MFCreateAttributes(&m_attributes, 4));
        return S_OK;
    }

    IFACEMETHODIMP Activate::ActivateObject(REFIID riid, void** ppv)
    {
        RETURN_HR_IF_NULL(E_POINTER, ppv);
        *ppv = nullptr;
        if (!m_source)
        {
            auto source = winrt::make_self<MediaSource>();
            RETURN_IF_FAILED(source->Initialize(this));
            m_source = source;
        }
        RETURN_IF_FAILED(m_source->QueryInterface(riid, ppv));
        return S_OK;
    }

    IFACEMETHODIMP Activate::ShutdownObject()
    {
        if (m_source)
        {
            m_source->Shutdown();
            m_source = nullptr;
        }
        return S_OK;
    }

    IFACEMETHODIMP Activate::DetachObject()
    {
        m_source = nullptr;
        return S_OK;
    }

#define FORWARD(call)                                                                                                  \
    {                                                                                                                  \
        if (!m_attributes)                                                                                             \
        {                                                                                                              \
            RETURN_IF_FAILED(MFCreateAttributes(&m_attributes, 4));                                                    \
        }                                                                                                              \
        return m_attributes->call;                                                                                     \
    }

    IFACEMETHODIMP Activate::GetItem(REFGUID k, PROPVARIANT* v) FORWARD(GetItem(k, v))
    IFACEMETHODIMP Activate::GetItemType(REFGUID k, MF_ATTRIBUTE_TYPE* t) FORWARD(GetItemType(k, t))
    IFACEMETHODIMP Activate::CompareItem(REFGUID k, REFPROPVARIANT v, BOOL* r) FORWARD(CompareItem(k, v, r))
    IFACEMETHODIMP Activate::Compare(IMFAttributes* a, MF_ATTRIBUTES_MATCH_TYPE m, BOOL* r) FORWARD(Compare(a, m, r))
    IFACEMETHODIMP Activate::GetUINT32(REFGUID k, UINT32* v) FORWARD(GetUINT32(k, v))
    IFACEMETHODIMP Activate::GetUINT64(REFGUID k, UINT64* v) FORWARD(GetUINT64(k, v))
    IFACEMETHODIMP Activate::GetDouble(REFGUID k, double* v) FORWARD(GetDouble(k, v))
    IFACEMETHODIMP Activate::GetGUID(REFGUID k, GUID* v) FORWARD(GetGUID(k, v))
    IFACEMETHODIMP Activate::GetStringLength(REFGUID k, UINT32* n) FORWARD(GetStringLength(k, n))
    IFACEMETHODIMP Activate::GetString(REFGUID k, LPWSTR s, UINT32 c, UINT32* n) FORWARD(GetString(k, s, c, n))
    IFACEMETHODIMP Activate::GetAllocatedString(REFGUID k, LPWSTR* s, UINT32* n) FORWARD(GetAllocatedString(k, s, n))
    IFACEMETHODIMP Activate::GetBlobSize(REFGUID k, UINT32* n) FORWARD(GetBlobSize(k, n))
    IFACEMETHODIMP Activate::GetBlob(REFGUID k, UINT8* b, UINT32 c, UINT32* n) FORWARD(GetBlob(k, b, c, n))
    IFACEMETHODIMP Activate::GetAllocatedBlob(REFGUID k, UINT8** b, UINT32* n) FORWARD(GetAllocatedBlob(k, b, n))
    IFACEMETHODIMP Activate::GetUnknown(REFGUID k, REFIID r, LPVOID* p) FORWARD(GetUnknown(k, r, p))
    IFACEMETHODIMP Activate::SetItem(REFGUID k, REFPROPVARIANT v) FORWARD(SetItem(k, v))
    IFACEMETHODIMP Activate::DeleteItem(REFGUID k) FORWARD(DeleteItem(k))
    IFACEMETHODIMP Activate::DeleteAllItems() FORWARD(DeleteAllItems())
    IFACEMETHODIMP Activate::SetUINT32(REFGUID k, UINT32 v) FORWARD(SetUINT32(k, v))
    IFACEMETHODIMP Activate::SetUINT64(REFGUID k, UINT64 v) FORWARD(SetUINT64(k, v))
    IFACEMETHODIMP Activate::SetDouble(REFGUID k, double v) FORWARD(SetDouble(k, v))
    IFACEMETHODIMP Activate::SetGUID(REFGUID k, REFGUID v) FORWARD(SetGUID(k, v))
    IFACEMETHODIMP Activate::SetString(REFGUID k, LPCWSTR v) FORWARD(SetString(k, v))
    IFACEMETHODIMP Activate::SetBlob(REFGUID k, const UINT8* b, UINT32 c) FORWARD(SetBlob(k, b, c))
    IFACEMETHODIMP Activate::SetUnknown(REFGUID k, IUnknown* u) FORWARD(SetUnknown(k, u))
    IFACEMETHODIMP Activate::LockStore() FORWARD(LockStore())
    IFACEMETHODIMP Activate::UnlockStore() FORWARD(UnlockStore())
    IFACEMETHODIMP Activate::GetCount(UINT32* n) FORWARD(GetCount(n))
    IFACEMETHODIMP Activate::GetItemByIndex(UINT32 i, GUID* k, PROPVARIANT* v) FORWARD(GetItemByIndex(i, k, v))
    IFACEMETHODIMP Activate::CopyAllItems(IMFAttributes* d) FORWARD(CopyAllItems(d))

#undef FORWARD

    STDMETHODIMP ActivateFactory::CreateInstance(IUnknown* outer, REFIID riid, void** result) noexcept
    try
    {
        RETURN_HR_IF_NULL(E_POINTER, result);
        *result = nullptr;
        RETURN_HR_IF(CLASS_E_NOAGGREGATION, outer != nullptr);
        auto activate = winrt::make_self<Activate>();
        RETURN_IF_FAILED(activate->Initialize());
        return activate->QueryInterface(riid, result);
    }
    CATCH_RETURN()
}

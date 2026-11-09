// Relay Camera media source: common includes.
//
// Derived from the Windows-Camera VirtualCamera sample
// (https://github.com/microsoft/Windows-Camera/tree/master/Samples/VirtualCamera).
// Copyright (c) Microsoft Corporation. Licensed under the MIT License; see
// LICENSE-Windows-Camera-sample.txt next to CMakeLists.txt. Changes for Relay
// are also MIT licensed.
#pragma once

#include <unknwn.h>
#include <windows.h>
#include <ole2.h> // before the C++/WinRT headers
#include <initguid.h>
#include <propvarutil.h>
#include <strsafe.h>
#include <sddl.h>
#include <ks.h>
#include <ksproxy.h>
#include <ksmedia.h>
#include <mfapi.h>
#include <mfidl.h>
#include <mfobjects.h>
#include <mferror.h>
#include <mfvirtualcamera.h>

#include <deque>
#include <thread>

#include <wil/cppwinrt.h> // before the first C++/WinRT header
#include <wil/result.h>
#include <wil/com.h>
#include <wil/resource.h>
#include <winrt/base.h>

#include "RelayCamera.h"

// Debug output goes to OutputDebugString, and is also appended to the file named
// by %RELAY_CAMERA_LOG% when that is set (handy when the source runs in-process).
void DebugWrite(LPCWSTR text);

inline void DebugPrint(LPCWSTR format, ...)
{
    WCHAR buffer[512] = {};
    va_list args;
    va_start(args, format);
    StringCbVPrintfW(buffer, sizeof(buffer), format, args);
    va_end(args);
    DebugWrite(buffer);
}

#define DEBUG_MSG(msg, ...)                                                                                            \
    {                                                                                                                  \
        DebugPrint(L"[RelayCamera %s@%d] ", TEXT(__FUNCTION__), __LINE__);                                             \
        DebugPrint(msg, __VA_ARGS__);                                                                                  \
        DebugPrint(L"\n");                                                                                             \
    }

namespace winrt
{
    // C++/WinRT needs to know the base interfaces of these classic COM ones.
    template <> bool is_guid_of<IMFMediaSourceEx>(guid const& id) noexcept;
    template <> bool is_guid_of<IMFMediaStream2>(guid const& id) noexcept;
    template <> bool is_guid_of<IMFActivate>(guid const& id) noexcept;
}

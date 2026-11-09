// Relay Camera: DLL entry points and COM registration.
//
// Derived from dllmain.cpp and winrtCommon.cpp in the Windows-Camera
// VirtualCamera sample. Copyright (c) Microsoft Corporation. Licensed under the
// MIT License; see LICENSE-Windows-Camera-sample.txt. Changes for Relay are also
// MIT licensed.
//
// regsvr32 relay-camera.dll (elevated) registers the COM class machine-wide in
// HKLM\Software\Classes\CLSID, which Frame Server needs. The virtual camera
// itself is created per user by the Relay engine with MFCreateVirtualCamera.
#include "pch.h"
#include "Activate.h"

static HMODULE g_module = nullptr;

void DebugWrite(LPCWSTR text)
{
    OutputDebugStringW(text);
    static wchar_t path[MAX_PATH] = {};
    static const bool enabled = [] {
        DWORD n = GetEnvironmentVariableW(L"RELAY_CAMERA_LOG", path, MAX_PATH);
        return n > 0 && n < MAX_PATH;
    }();
    if (!enabled)
    {
        return;
    }
    static SRWLOCK lock = SRWLOCK_INIT;
    AcquireSRWLockExclusive(&lock);
    HANDLE f = CreateFileW(path, FILE_APPEND_DATA, FILE_SHARE_READ | FILE_SHARE_WRITE, nullptr, OPEN_ALWAYS, 0, nullptr);
    if (f != INVALID_HANDLE_VALUE)
    {
        char utf8[1024];
        int n = WideCharToMultiByte(CP_UTF8, 0, text, -1, utf8, sizeof(utf8), nullptr, nullptr);
        DWORD written = 0;
        if (n > 1)
        {
            WriteFile(f, utf8, static_cast<DWORD>(n - 1), &written, nullptr);
        }
        CloseHandle(f);
    }
    ReleaseSRWLockExclusive(&lock);
}

static void __stdcall LogWilFailure(const wil::FailureInfo& failure) noexcept
{
    wchar_t message[1024];
    wil::GetFailureLogString(message, ARRAYSIZE(message), failure);
    DebugWrite(message);
}

BOOL APIENTRY DllMain(HMODULE module, DWORD reason, LPVOID)
{
    if (reason == DLL_PROCESS_ATTACH)
    {
        g_module = module;
        DisableThreadLibraryCalls(module);
        wil::SetResultLoggingCallback(LogWilFailure);
    }
    return TRUE;
}

WINRT_EXPORT namespace winrt
{
    template <> bool is_guid_of<IMFMediaSourceEx>(guid const& id) noexcept
    {
        return is_guid_of<IMFMediaSourceEx, IMFMediaSource, IMFMediaEventGenerator>(id);
    }
    template <> bool is_guid_of<IMFMediaStream2>(guid const& id) noexcept
    {
        return is_guid_of<IMFMediaStream2, IMFMediaStream, IMFMediaEventGenerator>(id);
    }
    template <> bool is_guid_of<IMFActivate>(guid const& id) noexcept
    {
        return is_guid_of<IMFActivate, IMFAttributes>(id);
    }
}

STDAPI DllCanUnloadNow()
{
    if (winrt::get_module_lock())
    {
        return S_FALSE;
    }
    winrt::clear_factory_cache();
    return S_OK;
}

STDAPI DllGetClassObject(REFCLSID clsid, REFIID iid, void** result)
try
{
    RETURN_HR_IF_NULL(E_POINTER, result);
    *result = nullptr;
    if (clsid == __uuidof(relaycam::Activate))
    {
        return winrt::make_self<relaycam::ActivateFactory>()->QueryInterface(iid, result);
    }
    return CLASS_E_CLASSNOTAVAILABLE;
}
CATCH_RETURN()

static constexpr wchar_t kClassKey[] = L"Software\\Classes\\CLSID\\" RELAY_CAMERA_CLSID_STRING;

static HRESULT SetValue(HKEY root, const wchar_t* subkey, const wchar_t* name, const wchar_t* value)
{
    wil::unique_hkey key;
    RETURN_IF_WIN32_ERROR(RegCreateKeyExW(root, subkey, 0, nullptr, 0, KEY_WRITE, nullptr, &key, nullptr));
    RETURN_IF_WIN32_ERROR(RegSetValueExW(key.get(), name, 0, REG_SZ, reinterpret_cast<const BYTE*>(value),
                                         static_cast<DWORD>((wcslen(value) + 1) * sizeof(wchar_t))));
    return S_OK;
}

STDAPI DllRegisterServer()
{
    wchar_t path[MAX_PATH * 2] = {};
    DWORD n = GetModuleFileNameW(g_module, path, ARRAYSIZE(path));
    RETURN_LAST_ERROR_IF(n == 0 || n >= ARRAYSIZE(path));
    wchar_t inproc[ARRAYSIZE(kClassKey) + 20];
    RETURN_IF_FAILED(StringCchPrintfW(inproc, ARRAYSIZE(inproc), L"%s\\InprocServer32", kClassKey));
    RETURN_IF_FAILED(SetValue(HKEY_LOCAL_MACHINE, kClassKey, nullptr, RELAY_CAMERA_NAME));
    RETURN_IF_FAILED(SetValue(HKEY_LOCAL_MACHINE, inproc, nullptr, path));
    RETURN_IF_FAILED(SetValue(HKEY_LOCAL_MACHINE, inproc, L"ThreadingModel", L"Both"));
    return S_OK;
}

STDAPI DllUnregisterServer()
{
    LSTATUS s = RegDeleteTreeW(HKEY_LOCAL_MACHINE, kClassKey);
    if (s != ERROR_SUCCESS && s != ERROR_FILE_NOT_FOUND)
    {
        return HRESULT_FROM_WIN32(s);
    }
    return S_OK;
}

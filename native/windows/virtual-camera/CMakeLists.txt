# Relay Camera: a Media Foundation virtual camera media source (x64 DLL).
# Build: scripts/windows/build-camera.ps1, or
#   cmake -S native/windows/virtual-camera -B target/camera -A x64
#   cmake --build target/camera --config Release
cmake_minimum_required(VERSION 3.21)
project(relay_camera LANGUAGES CXX)

if(NOT CMAKE_SIZEOF_VOID_P EQUAL 8)
    message(FATAL_ERROR "Relay Camera is x64 only (Frame Server is a 64-bit service)")
endif()

# Static CRT: the DLL runs inside the Frame Server service, which must not depend
# on a Visual C++ redistributable being installed.
set(CMAKE_MSVC_RUNTIME_LIBRARY "MultiThreaded$<$<CONFIG:Debug>:Debug>")
set(CMAKE_CXX_STANDARD 20)
set(CMAKE_CXX_STANDARD_REQUIRED ON)

# WIL (MIT, header-only). Pinned release; RELAY_WIL_DIR points at a local copy for
# offline builds.
set(RELAY_WIL_DIR "" CACHE PATH "Existing checkout of microsoft/wil (optional)")
if(RELAY_WIL_DIR)
    set(WIL_INCLUDE "${RELAY_WIL_DIR}/include")
else()
    include(FetchContent)
    FetchContent_Declare(wil
        URL https://github.com/microsoft/wil/archive/refs/tags/v1.0.260126.7.tar.gz
        URL_HASH SHA256=de9e03b38ff0ff8d22048f00b111cb631d21c550328f12530ccba71c05c9e361
        DOWNLOAD_EXTRACT_TIMESTAMP TRUE
        # Headers only: don't run WIL's own CMake project.
        SOURCE_SUBDIR headers-only)
    FetchContent_MakeAvailable(wil)
    set(WIL_INCLUDE "${wil_SOURCE_DIR}/include")
endif()
if(NOT EXISTS "${WIL_INCLUDE}/wil/result.h")
    message(FATAL_ERROR "WIL headers not found in ${WIL_INCLUDE}")
endif()

add_library(relay-camera SHARED
    src/dllmain.cpp
    src/Activate.cpp
    src/MediaSource.cpp
    src/MediaStream.cpp
    src/SharedFrames.cpp
    src/relay-camera.def)

target_include_directories(relay-camera PRIVATE src "${WIL_INCLUDE}")
target_compile_definitions(relay-camera PRIVATE
    UNICODE _UNICODE NOMINMAX _WIN32_WINNT=0x0A00)
target_compile_options(relay-camera PRIVATE /W4 /permissive- /EHsc /bigobj /utf-8 /Zi /guard:cf)
target_link_options(relay-camera PRIVATE /DEBUG /OPT:REF /OPT:ICF /GUARD:CF)
target_link_libraries(relay-camera PRIVATE
    mfplat mfuuid mfsensorgroup windowsapp advapi32 ole32)
set_target_properties(relay-camera PROPERTIES OUTPUT_NAME relay-camera)

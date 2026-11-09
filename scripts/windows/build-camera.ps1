# Builds the Relay Camera media source (native/windows/virtual-camera) with CMake
# and Visual Studio's C++ tools, and writes target\camera\relay-camera.dll.
# Needs Visual Studio 2022 (or its Build Tools) with the C++ workload and a
# Windows SDK >= 10.0.22000; CMake from PATH, Visual Studio, or pip.
param(
    [string]$Config = 'Release',
    [string]$OutDir
)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
$src = Join-Path $root 'native\windows\virtual-camera'
$build = Join-Path $root 'target\camera\build'
if (-not $OutDir) { $OutDir = Join-Path $root 'target\camera' }

function Find-CMake {
    $cmd = Get-Command cmake -ErrorAction SilentlyContinue
    if ($cmd) { return $cmd.Source }
    $candidates = @()
    $vswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
    if (Test-Path $vswhere) {
        foreach ($vs in (& $vswhere -all -products * -property installationPath)) {
            $candidates += Join-Path $vs 'Common7\IDE\CommonExtensions\Microsoft\CMake\CMake\bin\cmake.exe'
        }
    }
    $candidates += Join-Path $env:ProgramFiles 'CMake\bin\cmake.exe'
    $candidates += Get-ChildItem -Path (Join-Path $env:APPDATA 'Python') -Filter cmake.exe -Recurse -ErrorAction SilentlyContinue |
        ForEach-Object FullName
    foreach ($c in $candidates) { if ($c -and (Test-Path $c)) { return $c } }
    throw 'CMake not found (install it, or add it to PATH)'
}

$cmake = Find-CMake
Write-Host "using $cmake"
# Default generator: the newest Visual Studio installed.
& $cmake -S $src -B $build -A x64
if ($LASTEXITCODE -ne 0) { throw 'cmake configure failed' }
& $cmake --build $build --config $Config --parallel
if ($LASTEXITCODE -ne 0) { throw 'cmake build failed' }

New-Item -ItemType Directory -Force $OutDir | Out-Null
$dll = Join-Path $build "$Config\relay-camera.dll"
Copy-Item $dll $OutDir -Force
$pdb = Join-Path $build "$Config\relay-camera.pdb"
if (Test-Path $pdb) { Copy-Item $pdb $OutDir -Force }
Write-Host "wrote $(Join-Path $OutDir 'relay-camera.dll')"

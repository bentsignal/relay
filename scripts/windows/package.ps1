# Builds Relay for Windows and writes dist\relay-windows-x64.zip.
# Env: RELAY_VERSION (optional) is baked into the binaries.
param([switch]$SkipBuild)
$ErrorActionPreference = 'Stop'
$root = Resolve-Path (Join-Path $PSScriptRoot '..\..')
Set-Location $root
if (-not $SkipBuild) {
    cargo build --release --locked -p relay-engine -p relay-app
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
}
$dist = Join-Path $root 'dist'
$stage = Join-Path $dist 'stage-windows'
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force (Join-Path $stage 'usbip') | Out-Null
python scripts\windows\fetch-usbip.py (Join-Path $stage 'usbip')
if ($LASTEXITCODE -ne 0) { throw 'fetching usbip-win2 failed' }
Copy-Item scripts\windows\install-driver.ps1 (Join-Path $stage 'usbip')
Copy-Item target\release\relay.exe, target\release\relay-engine.exe $stage
Copy-Item LICENSE, THIRD-PARTY-NOTICES.md $stage
$zip = Join-Path $dist 'relay-windows-x64.zip'
Remove-Item $zip -ErrorAction SilentlyContinue
Push-Location $stage
# bsdtar writes portable forward-slash paths.
tar.exe -a -c -f $zip *
Pop-Location
if ($LASTEXITCODE -ne 0) { throw 'zip failed' }
Write-Host "wrote $zip"

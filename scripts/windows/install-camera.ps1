# Installs the Relay Camera media source machine-wide. Needs admin; idempotent.
# Run by install-driver.ps1 (Relay's one-time setup), or by itself, elevated:
#   powershell -ExecutionPolicy Bypass -File install-camera.ps1 [-Dll path] [-Uninstall]
#
# Windows' Camera Frame Server (a LocalService service) loads the DLL, so it is
# copied out of the per-user app folder into Program Files (under a name that
# includes its hash, so an update never has to overwrite a DLL Frame Server
# still has loaded) and its COM class is registered in HKLM. The camera device
# itself is created per user by the Relay engine, which needs no admin.
# Exit code 0 on success.
param(
    [string]$Dll,
    [string]$Log = (Join-Path $env:TEMP 'relay-driver-install.log'),
    [switch]$Uninstall
)
$ErrorActionPreference = 'Stop'
$clsid = '{E7AE42DD-B97D-4F1A-B794-0BD645A3D031}'
$installDir = Join-Path $env:ProgramFiles 'Relay\Camera'
$classKey = "HKLM:\SOFTWARE\Classes\CLSID\$clsid"
function Write-Log($msg) { "camera: $msg" | Out-File $Log -Append; Write-Host "camera: $msg" }

function Invoke-Regsvr($argList) {
    $p = Start-Process -FilePath (Join-Path $env:SystemRoot 'System32\regsvr32.exe') -ArgumentList $argList -Wait -PassThru -WindowStyle Hidden
    return $p.ExitCode
}

try {
    if ($Uninstall) {
        $current = (Get-ItemProperty -LiteralPath "$classKey\InprocServer32" -ErrorAction SilentlyContinue).'(default)'
        if ($current -and (Test-Path -LiteralPath $current)) { Invoke-Regsvr @('/u', '/s', "`"$current`"") | Out-Null }
        Remove-Item -LiteralPath $classKey -Recurse -Force -ErrorAction SilentlyContinue
        Remove-Item -LiteralPath $installDir -Recurse -Force -ErrorAction SilentlyContinue
        Write-Log 'uninstalled'
        exit 0
    }

    if (-not $Dll) {
        $Dll = @(
            (Join-Path $PSScriptRoot '..\camera\relay-camera.dll'),
            (Join-Path $PSScriptRoot 'camera\relay-camera.dll'),
            (Join-Path $PSScriptRoot 'relay-camera.dll')
        ) | Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
    }
    if (-not $Dll) {
        Write-Log 'no camera in this package; skipped'
        exit 0
    }
    $hash = (Get-FileHash -LiteralPath $Dll -Algorithm SHA256).Hash
    $target = Join-Path $installDir ('relay-camera-' + $hash.Substring(0, 16).ToLower() + '.dll')
    $current = (Get-ItemProperty -LiteralPath "$classKey\InprocServer32" -ErrorAction SilentlyContinue).'(default)'

    if ($current -eq $target -and (Test-Path -LiteralPath $target) -and
        (Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -eq $hash) {
        Write-Log "already installed ($target)"
        exit 0
    }

    New-Item -ItemType Directory -Force $installDir | Out-Null
    Copy-Item -LiteralPath $Dll -Destination $target -Force
    if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ne $hash) { throw 'copy failed' }
    # Frame Server runs as LocalService (S-1-5-19).
    & icacls.exe $target /grant '*S-1-5-19:(RX)' | Out-Null

    $code = Invoke-Regsvr @('/s', "`"$target`"")
    if ($code -ne 0) { throw "regsvr32 failed ($code)" }
    $registered = (Get-ItemProperty -LiteralPath "$classKey\InprocServer32").'(default)'
    if ($registered -ne $target) { throw "COM registration points at '$registered'" }
    Write-Log "registered $target"

    if ($current) {
        # An older version may be loaded: restart Frame Server so new sessions
        # pick up this one, then drop old copies that are no longer in use.
        foreach ($svc in 'FrameServer', 'FrameServerMonitor') {
            $s = Get-Service -Name $svc -ErrorAction SilentlyContinue
            if ($s -and $s.Status -eq 'Running') { Restart-Service -Name $svc -Force -ErrorAction SilentlyContinue }
        }
    }
    Get-ChildItem -LiteralPath $installDir -Filter 'relay-camera-*.dll' |
        Where-Object { $_.FullName -ne $target } |
        ForEach-Object { Remove-Item -LiteralPath $_.FullName -Force -ErrorAction SilentlyContinue }
    exit 0
} catch {
    Write-Log "FAILED: $($_.Exception.Message)"
    exit 1
}

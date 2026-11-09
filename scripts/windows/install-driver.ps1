# One-time setup for Relay on Windows. Runs elevated (one UAC prompt).
#  1. Installs the Microsoft-signed usbip-win2 virtual USB host controller,
#     which lets Relay plug in its own user-mode audio devices.
#     The filter package is only staged, so existing USB hubs aren't restarted.
#  2. Lets Relay's LAN traffic through Windows Firewall on private networks.
# Exit code 0 on success. A log is written next to this script's parent temp dir.

$ErrorActionPreference = 'Stop'
$dir = $PSScriptRoot
$log = Join-Path $env:TEMP 'relay-driver-install.log'
"Relay driver install $(Get-Date -Format o)" | Out-File $log

$expected = @{
    'devnode.exe'       = '5B9C45F5BE344676BB434C14697965DE608FC3B0F6CAE7E5670B1D90C9196806'
    'usbip2_filter.cat' = 'BB2183D204B64EAA52C6EAC86C8CFB859AD0A5A5001AF5E6B197BF04CC49AC2E'
    'usbip2_filter.inf' = 'A1F07BBF5CFA7734A52046534DD66EE2333D34AA53912356179A6EAF25D1D60E'
    'usbip2_filter.sys' = '671110E3FE09628D3D7F416B5EE35172ECF5046F90BE6E8735631105D38FDEC5'
    'usbip2_ude.cat'    = '99C6807A2D05FD01A689389BE81D5088BB4777831ADB3C7E119B14A1F5B088C7'
    'usbip2_ude.inf'    = 'D975AC1AE14246611824518CEB05FA45227176D8E366366220F58A7AB977C379'
    'usbip2_ude.sys'    = 'D4D98DB62D78A5B6D32EEC3F8F1F9F6B0D2EDA925A56C96367C5B7985AB52CD8'
}

try {
    foreach ($entry in $expected.GetEnumerator()) {
        $path = Join-Path $dir $entry.Key
        if (-not (Test-Path -LiteralPath $path)) { throw "Missing $($entry.Key)" }
        if ((Get-FileHash -LiteralPath $path -Algorithm SHA256).Hash -ne $entry.Value) { throw "Unexpected contents: $($entry.Key)" }
    }

    if (Get-Service -Name 'usbip2_ude' -ErrorAction SilentlyContinue) {
        "Controller driver already installed" | Out-File $log -Append
    } else {
        & pnputil /add-driver (Join-Path $dir 'usbip2_filter.inf') 2>&1 | Out-File $log -Append
        if ($LASTEXITCODE -ne 0 -and $LASTEXITCODE -ne 259) { throw "Staging the filter driver failed ($LASTEXITCODE)" }
        & (Join-Path $dir 'devnode.exe') install (Join-Path $dir 'usbip2_ude.inf') 'ROOT\USBIP_WIN2\UDE' 2>&1 | Out-File $log -Append
        if ($LASTEXITCODE -ne 0) { throw "Creating the virtual USB controller failed ($LASTEXITCODE)" }
        $deadline = (Get-Date).AddSeconds(15)
        do {
            Start-Sleep -Milliseconds 500
            $svc = Get-Service -Name 'usbip2_ude' -ErrorAction SilentlyContinue
        } while ((-not $svc -or $svc.Status -ne 'Running') -and (Get-Date) -lt $deadline)
        if (-not $svc -or $svc.Status -ne 'Running') { throw 'The virtual USB controller did not start' }
    }

    Get-NetFirewallRule -DisplayName 'Relay (LAN)' -ErrorAction SilentlyContinue | Remove-NetFirewallRule
    New-NetFirewallRule -DisplayName 'Relay (LAN)' -Direction Inbound -Protocol UDP -LocalPort 47800,47801,5353 `
        -Profile Private,Domain -Action Allow | Out-Null
    "Firewall rule added" | Out-File $log -Append
    "OK" | Out-File $log -Append
    exit 0
} catch {
    "FAILED: $($_.Exception.Message)" | Out-File $log -Append
    exit 1
}

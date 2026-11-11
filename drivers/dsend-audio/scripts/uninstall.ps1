# dsend's own audio output: remove its root device and driver package.
# Run as administrator. Leaves test signing and the trusted test
# certificates as they are.
$ErrorActionPreference = "Stop"
$hwid = "ROOT\dsend_audio"

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) { throw "run as administrator; nothing changed" }

Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains $hwid } | ForEach-Object {
    pnputil /remove-device "$($_.InstanceId)" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "pnputil /remove-device $($_.InstanceId) failed ($LASTEXITCODE)" }
    Write-Output "removed device $($_.InstanceId)"
}
pnputil /enum-drivers | Out-String -Stream | Select-String -Context 1,0 'dsendaudio.inf' | ForEach-Object {
    if ($_.Context.PreContext[0] -match '(oem\d+\.inf)') {
        pnputil /delete-driver $Matches[1] /uninstall | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "pnputil /delete-driver $($Matches[1]) failed ($LASTEXITCODE)" }
        Write-Output "removed driver package $($Matches[1])"
    }
}
Write-Output "dsend audio output removed"

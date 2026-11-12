# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Tests of tools/windows/lan-selfheal.ps1 (dstack #239), run by CI's Windows
# job under Windows PowerShell 5.1:
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File tools\windows\tests\lan-selfheal.Tests.ps1
# A plain assert script, not Pester: the inbox Pester 3.4 and the runner's
# Pester 5 differ in syntax. Exits 1 if any check fails.
# Nothing here touches a network adapter: the repair steps are tested only up
# to the guard, and the run path only with a gateway that answers (loopback)
# or one that cannot (TEST-NET-1), below the threshold.

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\lan-selfheal.ps1')

$script:failed = 0
$script:passed = 0
function Check {
    param([string]$Name, [scriptblock]$Body)
    try {
        $ok = & $Body
        if ($ok -eq $true) { $script:passed++; Write-Host "ok   $Name" }
        else { $script:failed++; Write-Host "FAIL $Name (got '$ok')" }
    } catch {
        $script:failed++; Write-Host "FAIL $Name (threw: $($_.Exception.Message))"
    }
}
function Throws {
    param([scriptblock]$Body, [string]$Like)
    try { & $Body | Out-Null; return $false } catch { return ($_.Exception.Message -like $Like) }
}

$good = '{"lanAdapter":"LAN","gateway":"192.0.2.1","danteAdapter":"Dante70","thresholdMinutes":3}'

# --- counter and threshold
Check 'a gateway answer resets the counter' {
    $n = Get-NextState -Failures 2 -GatewayOk $true -Threshold 3
    $n.Failures -eq 0 -and -not $n.Heal -and $n.Previous -eq 2
}
Check 'a failed minute adds one, below the threshold no repair' {
    $a = Get-NextState -Failures 0 -GatewayOk $false -Threshold 3
    $b = Get-NextState -Failures $a.Failures -GatewayOk $false -Threshold 3
    $a.Failures -eq 1 -and -not $a.Heal -and $b.Failures -eq 2 -and -not $b.Heal
}
Check 'the third consecutive failed minute repairs and restarts the count' {
    $n = Get-NextState -Failures 2 -GatewayOk $false -Threshold 3
    $n.Heal -and $n.Failures -eq 0
}
Check 'one answer between failures starts the count again' {
    $s = 0
    foreach ($ok in $false, $false, $true, $false, $false) {
        $n = Get-NextState -Failures $s -GatewayOk $ok -Threshold 3
        if ($n.Heal) { return $false }
        $s = $n.Failures
    }
    $s -eq 2
}
Check 'a threshold of 1 repairs on the first failed minute' {
    (Get-NextState -Failures 0 -GatewayOk $false -Threshold 1).Heal
}

# --- config
Check 'a valid config parses, threshold and VLAN display name defaults' {
    $c = Read-HealConfig -Json '{"lanAdapter":" LAN ","gateway":"192.0.2.1","danteAdapter":"Dante70"}'
    $c.LanAdapter -eq 'LAN' -and $c.ThresholdMinutes -eq 3 -and $c.VlanDisplayName -eq 'VLAN ID'
}
Check 'the configured threshold is read' {
    (Read-HealConfig -Json $good.Replace('"thresholdMinutes":3', '"thresholdMinutes":5')).ThresholdMinutes -eq 5
}
Check 'a missing Dante adapter name is refused' {
    Throws { Read-HealConfig -Json '{"lanAdapter":"LAN","gateway":"192.0.2.1"}' } "*'danteAdapter'*"
}
Check 'a gateway that is not IPv4 is refused' {
    Throws { Read-HealConfig -Json $good.Replace('192.0.2.1', 'router') } "*'gateway'*"
}
Check 'a LAN adapter named as the Dante adapter is refused' {
    Throws { Read-HealConfig -Json $good.Replace('"LAN"', '"dante70"') } '*same adapter*'
}
Check 'a threshold below 1 is refused' {
    Throws { Read-HealConfig -Json $good.Replace('"thresholdMinutes":3', '"thresholdMinutes":0') } '*thresholdMinutes*'
}

# --- never the Dante adapter
Check 'the repair plan is the five steps in order, all on the LAN adapter' {
    $p = @(Get-RepairPlan -LanAdapter 'LAN' -DanteAdapter 'Dante70')
    ($p.Action -join ',') -eq 'Enable,VlanIdEmpty,BindIPv4,Dhcp,Restart' -and
    @($p | Where-Object { $_.Target -ne 'LAN' }).Count -eq 0
}
Check 'no plan step ever names the Dante adapter' {
    @(Get-RepairPlan -LanAdapter 'LAN' -DanteAdapter 'Dante70' | Where-Object { $_.Target -ieq 'Dante70' }).Count -eq 0
}
Check 'a plan aimed at the Dante adapter is refused' {
    Throws { Get-RepairPlan -LanAdapter 'Dante70' -DanteAdapter 'dante70' } '*Dante adapter*'
}
Check 'a repair step aimed at the Dante adapter is refused before any change' {
    $cfg = Read-HealConfig -Json $good
    $all = foreach ($a in 'Enable', 'VlanIdEmpty', 'BindIPv4', 'Dhcp', 'Restart') {
        Throws { Invoke-RepairStep -Step ([pscustomobject]@{ Action = $a; Target = ' dante70' }) -Config $cfg } '*Dante adapter*'
    }
    @($all | Where-Object { -not $_ }).Count -eq 0
}

# --- VLAN ID
Check 'an empty or 0 VLAN ID is left alone; any other is reset' {
    -not (Test-VlanNeedsReset -Value '') -and -not (Test-VlanNeedsReset -Value $null) -and
    -not (Test-VlanNeedsReset -Value '0') -and (Test-VlanNeedsReset -Value '70') -and (Test-VlanNeedsReset -Value '1')
}

# --- log rotation
Check 'the log rotates at 1 MB' {
    (Test-LogNeedsRotation -Size 1MB -Max 1MB) -and -not (Test-LogNeedsRotation -Size (1MB - 1) -Max 1MB)
}
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("lan-selfheal-test-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    Check 'a full log moves to .1 and a new one starts' {
        $log = Join-Path $tmp 'rot.log'
        [System.IO.File]::WriteAllBytes($log, (New-Object byte[] (1MB)))
        Write-HealLog -Path $log -Message 'after rotation'
        (Test-Path "$log.1") -and (Get-Item "$log.1").Length -eq 1MB -and
        (Get-Content $log -Raw) -match 'after rotation' -and (Get-Item $log).Length -lt 1KB
    }

    # --- the run path, in a scratch directory (no adapter is touched)
    Check 'no config: nothing done, exit 0' {
        $d = Join-Path $tmp 'noconf'; New-Item -ItemType Directory -Path $d | Out-Null
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and -not (Test-Path (Join-Path $d 'lan-selfheal.state.json')) -and
        (Get-Content (Join-Path $d 'lan-selfheal.log') -Raw) -match 'config missing'
    }
    Check 'a broken config: nothing done, exit 1' {
        $d = Join-Path $tmp 'badconf'; New-Item -ItemType Directory -Path $d | Out-Null
        Set-Content -Path (Join-Path $d 'lan-selfheal.json') -Value '{"lanAdapter":"LAN"}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and (Get-Content (Join-Path $d 'lan-selfheal.log') -Raw) -match 'ERROR'
    }
    Check 'a gateway that answers logs "gateway ok" and resets the count' {
        $d = Join-Path $tmp 'up'; New-Item -ItemType Directory -Path $d | Out-Null
        Set-Content -Path (Join-Path $d 'lan-selfheal.json') -Value $good.Replace('192.0.2.1', '127.0.0.1')
        Set-Content -Path (Join-Path $d 'lan-selfheal.state.json') -Value '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $state = Get-Content (Join-Path $d 'lan-selfheal.state.json') -Raw | ConvertFrom-Json
        $rc -eq 0 -and $state.failures -eq 0 -and
        (Get-Content (Join-Path $d 'lan-selfheal.log') -Raw) -match 'gateway ok \(down count 2 reset\)'
    }
    Check 'a gateway that does not answer counts 1/3 and repairs nothing' {
        $d = Join-Path $tmp 'down'; New-Item -ItemType Directory -Path $d | Out-Null
        Set-Content -Path (Join-Path $d 'lan-selfheal.json') -Value $good
        $rc = Invoke-Check -Dir $d
        $state = Get-Content (Join-Path $d 'lan-selfheal.state.json') -Raw | ConvertFrom-Json
        $text = Get-Content (Join-Path $d 'lan-selfheal.log') -Raw
        $rc -eq 0 -and $state.failures -eq 1 -and $text -match 'gateway down 1/3 min' -and $text -notmatch 'repair'
    }
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force
}

Write-Host "$($script:passed) passed, $($script:failed) failed"
if ($script:failed -gt 0) { exit 1 }
exit 0

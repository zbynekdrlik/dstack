# SPDX-License-Identifier: MIT OR Apache-2.0
#
# LAN self-heal for a one-NIC Windows PC with a Dante VLAN (dstack #239).
#
# The PC's one NIC carries the LAN untagged (the parent adapter) and Dante as
# a VLAN adapter. If the LAN leg dies (a vendor VLAN tool rewrote the parent,
# IPv4 got unbound, the adapter was disabled), a headless PC has no remote
# path left: the Dante address is closed to TCP by design. This script, run
# as SYSTEM by a scheduled task every minute, pings the LAN gateway; after the
# configured number of consecutive failed minutes it puts the LAN adapter
# back: enabled, VLAN ID 0, IPv4 bound, DHCP on, adapter restarted.
#
# It works only from C:\ProgramData\dstack\lan-selfheal.json and never acts
# without it. It never touches the Dante adapter: every repair step names the
# LAN adapter, and a guard refuses any step aimed at the Dante adapter.
#
# Modes:
#   lan-selfheal.ps1                one check (what the task runs)
#   lan-selfheal.ps1 -Mode install  copy to C:\ProgramData\dstack and register the task
#   lan-selfheal.ps1 -Mode uninstall  remove the task (config and log stay)
#
# Config (JSON, site data, never in the repo):
#   { "lanAdapter": "<LAN adapter name>", "gateway": "<LAN gateway IPv4>",
#     "danteAdapter": "<Dante VLAN adapter name>", "thresholdMinutes": 3,
#     "vlanDisplayName": "VLAN ID" }      # optional, the driver's display name

[CmdletBinding()]
param(
    [ValidateSet('run', 'install', 'uninstall')]
    [string]$Mode = 'run',
    [string]$Dir = 'C:\ProgramData\dstack'
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$script:TaskName = 'dstack LAN self-heal'
$script:LogMaxBytes = 1MB

# ---------------------------------------------------------------- pure layer

function Read-HealConfig {
    # Parses and validates the config text. Throws on anything unusable.
    param([Parameter(Mandatory)][string]$Json)
    $c = $Json | ConvertFrom-Json
    foreach ($k in 'lanAdapter', 'gateway', 'danteAdapter') {
        if (-not ($c.PSObject.Properties.Name -contains $k) -or
            [string]::IsNullOrWhiteSpace([string]$c.$k)) {
            throw "config: '$k' is missing or empty"
        }
    }
    $ip = $null
    if (-not [System.Net.IPAddress]::TryParse([string]$c.gateway, [ref]$ip) -or
        $ip.AddressFamily -ne [System.Net.Sockets.AddressFamily]::InterNetwork) {
        throw "config: 'gateway' is not an IPv4 address: '$($c.gateway)'"
    }
    $threshold = 3
    if ($c.PSObject.Properties.Name -contains 'thresholdMinutes') {
        $threshold = [int]$c.thresholdMinutes
    }
    if ($threshold -lt 1) { throw "config: 'thresholdMinutes' must be 1 or more, not $threshold" }
    $vlanName = 'VLAN ID'
    if ($c.PSObject.Properties.Name -contains 'vlanDisplayName' -and
        -not [string]::IsNullOrWhiteSpace([string]$c.vlanDisplayName)) {
        $vlanName = [string]$c.vlanDisplayName
    }
    $cfg = [pscustomobject]@{
        LanAdapter       = ([string]$c.lanAdapter).Trim()
        Gateway          = [string]$c.gateway
        DanteAdapter     = ([string]$c.danteAdapter).Trim()
        ThresholdMinutes = $threshold
        VlanDisplayName  = $vlanName
    }
    if ($cfg.LanAdapter -ieq $cfg.DanteAdapter) {
        throw "config: 'lanAdapter' and 'danteAdapter' are the same adapter ('$($cfg.LanAdapter)')"
    }
    $cfg
}

function Get-NextState {
    # The down-counter: a gateway answer resets it; a failed minute adds one;
    # reaching the threshold asks for a repair and starts the count again, so
    # a LAN still dead after a repair is repaired again a threshold later.
    param(
        [Parameter(Mandatory)][int]$Failures,
        [Parameter(Mandatory)][bool]$GatewayOk,
        [Parameter(Mandatory)][int]$Threshold
    )
    if ($GatewayOk) {
        return [pscustomobject]@{ Failures = 0; Heal = $false; Previous = $Failures }
    }
    $n = $Failures + 1
    if ($n -ge $Threshold) {
        return [pscustomobject]@{ Failures = 0; Heal = $true; Previous = $Failures }
    }
    [pscustomobject]@{ Failures = $n; Heal = $false; Previous = $Failures }
}

function Test-VlanNeedsReset {
    # The parent adapter carries the LAN untagged: its VLAN ID must be empty or 0.
    param([AllowNull()][AllowEmptyString()][string]$Value)
    if ([string]::IsNullOrWhiteSpace($Value)) { return $false }
    $Value.Trim() -ne '0'
}

function Assert-NotDanteTarget {
    param(
        [Parameter(Mandatory)][string]$Target,
        [Parameter(Mandatory)][string]$DanteAdapter
    )
    if ($Target.Trim() -ieq $DanteAdapter.Trim()) {
        throw "refused: '$Target' is the Dante adapter; the self-heal never touches it"
    }
}

function Get-RepairPlan {
    # The ordered repair steps, each aimed at the LAN adapter only.
    param(
        [Parameter(Mandatory)][string]$LanAdapter,
        [Parameter(Mandatory)][string]$DanteAdapter
    )
    $steps = foreach ($a in 'Enable', 'VlanIdEmpty', 'BindIPv4', 'Dhcp', 'Restart') {
        [pscustomobject]@{ Action = $a; Target = $LanAdapter }
    }
    foreach ($s in $steps) { Assert-NotDanteTarget -Target $s.Target -DanteAdapter $DanteAdapter }
    $steps
}

function Test-LogNeedsRotation {
    # The log rotates once it reaches the size limit (one old file kept, .1).
    param([Parameter(Mandatory)][long]$Size, [Parameter(Mandatory)][long]$Max)
    $Size -ge $Max
}

# --------------------------------------------------------- side-effect layer

function Write-HealLog {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][string]$Message)
    if ((Test-Path -LiteralPath $Path) -and
        (Test-LogNeedsRotation -Size (Get-Item -LiteralPath $Path).Length -Max $script:LogMaxBytes)) {
        Move-Item -LiteralPath $Path -Destination "$Path.1" -Force
    }
    $line = '{0} {1}' -f (Get-Date).ToString('yyyy-MM-ddTHH:mm:sszzz'), $Message
    Add-Content -LiteralPath $Path -Value $line -Encoding UTF8
}

function Test-Gateway {
    # Three pings, one second each: any answer counts. Only the LAN leg routes
    # to the gateway (the Dante leg has no gateway and another subnet).
    param([Parameter(Mandatory)][string]$Address)
    $p = New-Object System.Net.NetworkInformation.Ping
    try {
        for ($i = 0; $i -lt 3; $i++) {
            try {
                if ($p.Send($Address, 1000).Status -eq 'Success') { return $true }
            } catch [System.Net.NetworkInformation.PingException] {
                # no route (the LAN leg is gone): a failed try, not an error
            }
        }
        $false
    } finally { $p.Dispose() }
}

function Invoke-RepairStep {
    # Applies one step if it is needed; returns what it did, for the log.
    param(
        [Parameter(Mandatory)]$Step,
        [Parameter(Mandatory)]$Config
    )
    Assert-NotDanteTarget -Target $Step.Target -DanteAdapter $Config.DanteAdapter
    $name = $Step.Target
    switch ($Step.Action) {
        'Enable' {
            $a = Get-NetAdapter -Name $name
            if ($a.Status -eq 'Disabled') {
                Enable-NetAdapter -Name $name -Confirm:$false
                return "enabled the adapter (was Disabled)"
            }
            return "adapter enabled already ($($a.Status))"
        }
        'VlanIdEmpty' {
            $p = Get-NetAdapterAdvancedProperty -Name $name |
                Where-Object { $_.DisplayName -eq $Config.VlanDisplayName } | Select-Object -First 1
            if ($null -eq $p) { return "no '$($Config.VlanDisplayName)' property on the driver: nothing to reset" }
            $value = [string]($p.RegistryValue -join ',')
            if (Test-VlanNeedsReset -Value $value) {
                Set-NetAdapterAdvancedProperty -Name $name -RegistryKeyword $p.RegistryKeyword -RegistryValue '0' -NoRestart
                return "set $($p.RegistryKeyword) from '$value' to 0"
            }
            return "$($p.RegistryKeyword) is '$value' already"
        }
        'BindIPv4' {
            $b = Get-NetAdapterBinding -Name $name -ComponentID ms_tcpip
            if (-not $b.Enabled) {
                Enable-NetAdapterBinding -Name $name -ComponentID ms_tcpip
                return "bound IPv4 (ms_tcpip was unbound)"
            }
            return "IPv4 bound already"
        }
        'Dhcp' {
            $i = Get-NetIPInterface -InterfaceAlias $name -AddressFamily IPv4
            if ($i.Dhcp -ne 'Enabled') {
                Set-NetIPInterface -InterfaceAlias $name -AddressFamily IPv4 -Dhcp Enabled
                return "turned DHCP on (was $($i.Dhcp))"
            }
            return "DHCP on already"
        }
        'Restart' {
            Restart-NetAdapter -Name $name -Confirm:$false
            return "restarted the adapter"
        }
        default { throw "unknown repair step '$($Step.Action)'" }
    }
}

function Assert-DistinctAdapters {
    # The names differ (config check); here the LAN name must also not resolve
    # to the Dante adapter's interface.
    param([Parameter(Mandatory)]$Config)
    $lan = Get-NetAdapter -Name $Config.LanAdapter -IncludeHidden
    $dante = Get-NetAdapter -Name $Config.DanteAdapter -IncludeHidden -ErrorAction SilentlyContinue
    if ($null -ne $dante -and $lan.ifIndex -eq $dante.ifIndex) {
        throw "refused: '$($Config.LanAdapter)' is the Dante adapter's interface"
    }
}

function Invoke-Check {
    param([Parameter(Mandatory)][string]$Dir)
    $configPath = Join-Path $Dir 'lan-selfheal.json'
    $statePath = Join-Path $Dir 'lan-selfheal.state.json'
    $log = Join-Path $Dir 'lan-selfheal.log'
    if (-not (Test-Path -LiteralPath $configPath)) {
        # Never act without the PC's own config.
        if (Test-Path -LiteralPath $Dir) { Write-HealLog -Path $log -Message "config missing ($configPath): nothing done" }
        return 0
    }
    try {
        $cfg = Read-HealConfig -Json (Get-Content -LiteralPath $configPath -Raw)
    } catch {
        Write-HealLog -Path $log -Message "ERROR $($_.Exception.Message): nothing done"
        return 1
    }
    $failures = 0
    if (Test-Path -LiteralPath $statePath) {
        try { $failures = [int](Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json).failures }
        catch { Write-HealLog -Path $log -Message "state file unreadable, counting from 0: $($_.Exception.Message)" }
    }
    $ok = Test-Gateway -Address $cfg.Gateway
    $next = Get-NextState -Failures $failures -GatewayOk $ok -Threshold $cfg.ThresholdMinutes
    (@{ failures = $next.Failures } | ConvertTo-Json -Compress) | Set-Content -LiteralPath $statePath -Encoding ASCII
    if ($ok) {
        $note = if ($next.Previous -gt 0) { " (down count $($next.Previous) reset)" } else { '' }
        Write-HealLog -Path $log -Message "gateway ok$note"
        return 0
    }
    if (-not $next.Heal) {
        Write-HealLog -Path $log -Message "gateway down $($next.Failures)/$($cfg.ThresholdMinutes) min"
        return 0
    }
    Write-HealLog -Path $log -Message ("gateway down {0}/{0} min: repairing LAN adapter '{1}' (Dante adapter '{2}' untouched)" -f $cfg.ThresholdMinutes, $cfg.LanAdapter, $cfg.DanteAdapter)
    $code = 0
    try {
        Assert-DistinctAdapters -Config $cfg
        $plan = Get-RepairPlan -LanAdapter $cfg.LanAdapter -DanteAdapter $cfg.DanteAdapter
    } catch {
        Write-HealLog -Path $log -Message "ERROR $($_.Exception.Message): no repair"
        return 1
    }
    foreach ($step in $plan) {
        try {
            $did = Invoke-RepairStep -Step $step -Config $cfg
            Write-HealLog -Path $log -Message "  $($step.Action): $did"
        } catch {
            # One failed step does not stop the rest; the run exits non-zero.
            Write-HealLog -Path $log -Message "  $($step.Action): ERROR $($_.Exception.Message)"
            $code = 1
        }
    }
    Write-HealLog -Path $log -Message "repair done (exit $code)"
    $code
}

function Install-SelfHeal {
    param([Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][string]$Source)
    $configPath = Join-Path $Dir 'lan-selfheal.json'
    if (-not (Test-Path -LiteralPath $configPath)) { throw "no config at ${configPath}, write it first" }
    $cfg = Read-HealConfig -Json (Get-Content -LiteralPath $configPath -Raw)
    Get-NetAdapter -Name $cfg.LanAdapter | Out-Null
    Get-NetAdapter -Name $cfg.DanteAdapter -IncludeHidden | Out-Null
    Assert-DistinctAdapters -Config $cfg
    # A script SYSTEM runs must not be writable by ordinary users.
    & icacls.exe $Dir /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX' | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls on $Dir failed ($LASTEXITCODE)" }
    $target = Join-Path $Dir 'lan-selfheal.ps1'
    if ((Resolve-Path -LiteralPath $Source).Path -ne $target) {
        Copy-Item -LiteralPath $Source -Destination $target -Force
    }
    $arg = '-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "{0}"' -f $target
    $action = New-ScheduledTaskAction -Execute 'powershell.exe' -Argument $arg
    $trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 1)
    $boot = New-ScheduledTaskTrigger -AtStartup
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 5)
    Register-ScheduledTask -TaskName $script:TaskName -Action $action -Trigger @($trigger, $boot) `
        -Principal $principal -Settings $settings -Force | Out-Null
    Write-HealLog -Path (Join-Path $Dir 'lan-selfheal.log') -Message "installed: task '$($script:TaskName)' every minute as SYSTEM, script $target"
    "installed '$($script:TaskName)' ($target)"
}

function Uninstall-SelfHeal {
    param([Parameter(Mandatory)][string]$Dir)
    if (Get-ScheduledTask -TaskName $script:TaskName -ErrorAction SilentlyContinue) {
        Unregister-ScheduledTask -TaskName $script:TaskName -Confirm:$false
    }
    if (Test-Path -LiteralPath $Dir) {
        Write-HealLog -Path (Join-Path $Dir 'lan-selfheal.log') -Message "uninstalled: task '$($script:TaskName)' removed"
    }
    "removed '$($script:TaskName)' (config and log kept in $Dir)"
}

# ---------------------------------------------------------------------- main

# Dot-sourced (the tests): define the functions only.
if ($MyInvocation.InvocationName -ne '.') {
    switch ($Mode) {
        'run' {
            # Only the last value is the exit code (a cmdlet's stray output must not become it).
            $rc = Invoke-Check -Dir $Dir
            $code = [int]($rc | Select-Object -Last 1)
            exit $code
        }
        'install' { Install-SelfHeal -Dir $Dir -Source $PSCommandPath }
        'uninstall' { Uninstall-SelfHeal -Dir $Dir }
    }
}

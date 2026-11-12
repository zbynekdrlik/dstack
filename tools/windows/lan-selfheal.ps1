# SPDX-License-Identifier: MIT OR Apache-2.0
#
# LAN self-heal for a one-NIC Windows PC with a Dante VLAN (dstack #239).
#
# The PC's one NIC carries both legs, in one of two layouts
# (docs/two-networks.md):
#   A. the LAN untagged on the physical (parent) adapter, Dante on a VLAN
#      adapter (the SongPlayer PC: lanVlanId 0);
#   B. Dante untagged on the physical adapter, the LAN on a VLAN adapter
#      (dasio's test PC: lanVlanId 89).
# If the LAN leg dies (a vendor VLAN tool rewrote an adapter, IPv4 got
# unbound, the adapter was disabled), a headless PC has no remote path left:
# the Dante address is closed to TCP by design. This script, run as SYSTEM by
# a scheduled task every minute, checks the LAN leg (an IPv4 lease on the LAN
# adapter, the gateway's route through it, the gateway's answer); after the
# configured number of consecutive failed minutes it puts the LAN adapter
# back: enabled, its VLAN ID the config's lanVlanId (set only through the
# driver's "VLAN ID" property; without one the mismatch it reads is logged and
# nothing is guessed), IPv4 bound, DHCP on, and a restart of the adapter.
#
# In layout A the restart also restarts the Dante VLAN adapter on top of it (a
# few seconds of Dante, a new MAC). So it runs when a step changed something
# or the LAN adapter holds no lease, and otherwise (a router outage with a
# healthy adapter) at most once per RestartGapMinutes.
#
# It works only from C:\ProgramData\dstack\lan-selfheal.json and never acts
# without it. It never touches the Dante adapter: every repair step names the
# LAN adapter, a guard refuses any step aimed at the Dante adapter, and no
# repair runs unless both adapters resolve, by exact name, to two different
# interfaces.
#
# Modes:
#   lan-selfheal.ps1                  one check (what the task runs)
#   lan-selfheal.ps1 -Mode install    copy to C:\ProgramData\dstack and register the task
#   lan-selfheal.ps1 -Mode uninstall  remove the task (config and log stay)
#
# Config (JSON, site data, never in the repo):
#   { "lanAdapter": "<LAN adapter name>", "gateway": "<LAN gateway IPv4>",
#     "danteAdapter": "<Dante adapter name>", "thresholdMinutes": 3,
#     "lanVlanId": 0,                     # required: 0 = the LAN untagged on the
#                                         # parent adapter, N = the LAN on a VLAN
#                                         # adapter on VLAN N (1-4094)
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
$script:RestartGapMinutes = 30

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
    foreach ($k in 'lanAdapter', 'danteAdapter') {
        # The NetAdapter cmdlets read -Name as a wildcard: a name with * ? [ ]
        # could match the Dante adapter too.
        if ([System.Management.Automation.WildcardPattern]::ContainsWildcardCharacters([string]$c.$k)) {
            throw "config: '$k' ('$($c.$k)') holds a wildcard character (* ? [ ]); rename the adapter"
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
    # The LAN adapter's VLAN: no default, a wrong guess breaks the LAN (#239).
    if (-not ($c.PSObject.Properties.Name -contains 'lanVlanId')) {
        throw ("config: 'lanVlanId' is missing: 0 for a LAN untagged on the parent adapter, " +
            'N for a LAN on a VLAN adapter on VLAN N')
    }
    $v = $c.lanVlanId
    if (-not ($v -is [int] -or $v -is [long]) -or $v -lt 0 -or $v -gt 4094) {
        throw "config: 'lanVlanId' must be a whole number from 0 to 4094, not '$v'"
    }
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
        LanVlanId        = [int]$v
        VlanDisplayName  = $vlanName
    }
    if ($cfg.LanAdapter -ieq $cfg.DanteAdapter) {
        throw "config: 'lanAdapter' and 'danteAdapter' are the same adapter ('$($cfg.LanAdapter)')"
    }
    $cfg
}

function Get-NextState {
    # The down-counter: a healthy leg resets it; a failed minute adds one;
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

function Test-UsableIPv4 {
    # A DHCP lease or static address, not link-local (APIPA, 169.254/16).
    param([AllowNull()][AllowEmptyCollection()][string[]]$Addresses)
    @($Addresses | Where-Object { $_ -and $_ -notlike '169.254.*' }).Count -gt 0
}

function Get-LegFault {
    # Why the LAN leg is down, or '' when it is healthy. The gateway's answer
    # counts only when the LAN adapter holds a usable address and the route
    # to the gateway goes out of the LAN adapter: a reply over another leg
    # proves nothing (docs/two-networks.md, the safe VLAN procedure).
    param(
        [AllowNull()][AllowEmptyCollection()][string[]]$Addresses,
        [int]$RouteIfIndex,
        [Parameter(Mandatory)][int]$LanIfIndex,
        [bool]$PingOk
    )
    if (-not (Test-UsableIPv4 -Addresses $Addresses)) { return 'no usable IPv4 address on the LAN adapter' }
    if ($RouteIfIndex -ne $LanIfIndex) { return "the route to the gateway is not on the LAN adapter (ifIndex $RouteIfIndex, LAN $LanIfIndex)" }
    if (-not $PingOk) { return 'the gateway does not answer' }
    ''
}

function Get-RestartDecision {
    # The restart drops Dante for a few seconds (the VLAN adapter restarts
    # with its parent): do it when a step changed something; otherwise (a
    # router or DHCP server outage, with or without a lease) at most once
    # per gap.
    param(
        [Parameter(Mandatory)][bool]$Changed,
        [Parameter(Mandatory)][bool]$HasLease,   # only for the log line
        [Parameter(Mandatory)][double]$MinutesSinceRestart,   # negative: never restarted
        [Parameter(Mandatory)][int]$GapMinutes
    )
    if ($Changed) { return [pscustomobject]@{ Restart = $true; Why = 'a step changed the adapter' } }
    $state = if ($HasLease) { 'adapter healthy' } else { 'adapter without a usable IPv4 address' }
    if ($MinutesSinceRestart -lt 0 -or $MinutesSinceRestart -ge $GapMinutes) {
        return [pscustomobject]@{ Restart = $true; Why = "$state, no restart in the last $GapMinutes min" }
    }
    [pscustomobject]@{ Restart = $false; Why = ("$state, last restart {0:N0} min ago (gap $GapMinutes min)" -f $MinutesSinceRestart) }
}

function Test-VlanNeedsSet {
    # True when the VLAN ID read from the LAN adapter is not the configured
    # one. Empty reads as 0 (untagged); anything not a number differs.
    param(
        [AllowNull()][AllowEmptyString()][string]$Value,
        [Parameter(Mandatory)][int]$Expected
    )
    $current = 0
    if (-not [string]::IsNullOrWhiteSpace($Value) -and -not [int]::TryParse($Value.Trim(), [ref]$current)) {
        return $true
    }
    $current -ne $Expected
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
    $steps = foreach ($a in 'Enable', 'VlanId', 'BindIPv4', 'Dhcp', 'Restart') {
        [pscustomobject]@{ Action = $a; Target = $LanAdapter }
    }
    foreach ($s in $steps) { Assert-NotDanteTarget -Target $s.Target -DanteAdapter $DanteAdapter }
    $steps
}

function Test-AceGrantsWrite {
    # True when an allow ACE's mask lets its holder change the directory or
    # a file in it: write/append data or attributes, delete (also of
    # children), change permissions, take ownership, or a generic write/all.
    param([Parameter(Mandatory)][long]$Rights)
    $write = 0x2 -bor 0x4 -bor 0x10 -bor 0x100 -bor 0x40 -bor 0x10000 -bor 0x40000 -bor 0x80000 -bor 0x40000000 -bor 0x10000000
    ($Rights -band $write) -ne 0
}

function Test-LogNeedsRotation {
    # The log rotates once it reaches the size limit (one old file kept, .1).
    param([Parameter(Mandatory)][long]$Size, [Parameter(Mandatory)][long]$Max)
    $Size -ge $Max
}

function Read-HealState {
    # The state file's text to { Failures, LastRestart (UTC) }; a missing file
    # counts from zero (a broken one throws; the caller logs it).
    param([AllowNull()][AllowEmptyString()][string]$Json)
    $s = [pscustomobject]@{ Failures = 0; LastRestart = $null }
    if ([string]::IsNullOrWhiteSpace($Json)) { return $s }
    $o = $Json | ConvertFrom-Json
    if ($o.PSObject.Properties.Name -contains 'failures') { $s.Failures = [int]$o.failures }
    if ($o.PSObject.Properties.Name -contains 'lastRestart' -and $o.lastRestart) {
        $s.LastRestart = [datetime]::Parse([string]$o.lastRestart, [Globalization.CultureInfo]::InvariantCulture,
            [Globalization.DateTimeStyles]::RoundtripKind).ToUniversalTime()
    }
    $s
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

function Save-HealState {
    param([Parameter(Mandatory)][string]$Path, [Parameter(Mandatory)][int]$Failures, $LastRestart)
    $lr = $null
    if ($null -ne $LastRestart) { $lr = ([datetime]$LastRestart).ToUniversalTime().ToString('o') }
    (@{ failures = $Failures; lastRestart = $lr } | ConvertTo-Json -Compress) |
        Set-Content -LiteralPath $Path -Encoding ASCII
}

function Resolve-HealAdapters {
    # Both adapters by exact name. Fails closed when the LAN name is not
    # exactly one adapter, or when the Dante name is several adapters or the
    # LAN adapter's own interface. A missing Dante adapter (a vendor tool
    # removed it, or it hides while its parent is disabled) does not block
    # the repair: the LAN target is already one exact, wildcard-free name.
    param([Parameter(Mandatory)]$Config)
    $all = @(Get-NetAdapter -IncludeHidden)
    $lan = @($all | Where-Object { $_.Name -eq $Config.LanAdapter })
    $dante = @($all | Where-Object { $_.Name -eq $Config.DanteAdapter })
    if ($lan.Count -ne 1) { throw "refused: $($lan.Count) adapters named '$($Config.LanAdapter)' (need exactly 1)" }
    if ($dante.Count -gt 1) { throw "refused: $($dante.Count) adapters named '$($Config.DanteAdapter)' (need at most 1)" }
    if ($dante.Count -eq 1 -and $lan[0].ifIndex -eq $dante[0].ifIndex) {
        throw "refused: '$($Config.LanAdapter)' is the Dante adapter's interface (ifIndex $($lan[0].ifIndex))"
    }
    $d = $null
    if ($dante.Count -eq 1) { $d = $dante[0] }
    [pscustomobject]@{ Lan = $lan[0]; Dante = $d }
}

function Get-LanIPv4 {
    param([Parameter(Mandatory)][int]$IfIndex)
    @(Get-NetIPAddress -InterfaceIndex $IfIndex -AddressFamily IPv4 -ErrorAction SilentlyContinue |
        ForEach-Object { [string]$_.IPAddress })
}

function Test-Gateway {
    # Three pings, one second each: any answer counts.
    param([Parameter(Mandatory)][string]$Address)
    $p = New-Object System.Net.NetworkInformation.Ping
    try {
        for ($i = 0; $i -lt 3; $i++) {
            try {
                if ($p.Send($Address, 1000).Status -eq 'Success') { return $true }
            } catch [System.Net.NetworkInformation.PingException] {
                # no route: a failed try, not an error
            }
        }
        $false
    } finally { $p.Dispose() }
}

function Get-LanLegFault {
    # Reads the LAN leg's state and returns its fault ('' = healthy).
    param([Parameter(Mandatory)]$Config, [Parameter(Mandatory)]$Adapters)
    $lanIndex = [int]$Adapters.Lan.ifIndex
    $addresses = Get-LanIPv4 -IfIndex $lanIndex
    $routeIndex = -1
    $route = @(Find-NetRoute -RemoteIPAddress $Config.Gateway -ErrorAction SilentlyContinue |
        Where-Object { $_.PSObject.Properties.Name -contains 'InterfaceIndex' })
    if ($route.Count -gt 0) { $routeIndex = [int]$route[0].InterfaceIndex }
    $ping = $false
    if ((Test-UsableIPv4 -Addresses $addresses) -and $routeIndex -eq $lanIndex) {
        $ping = Test-Gateway -Address $Config.Gateway
    }
    Get-LegFault -Addresses $addresses -RouteIfIndex $routeIndex -LanIfIndex $lanIndex -PingOk $ping
}

function Invoke-RepairStep {
    # Applies one of the first four steps if it is needed. Returns
    # { Changed; Text } for the log. The restart is decided by the caller.
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
                return [pscustomobject]@{ Changed = $true; Text = 'enabled the adapter (was Disabled)' }
            }
            return [pscustomobject]@{ Changed = $false; Text = "adapter enabled already ($($a.Status))" }
        }
        'VlanId' {
            # The LAN adapter gets exactly the configured VLAN ID, and only
            # through its own driver's VLAN property.
            $want = [int]$Config.LanVlanId
            $p = Get-NetAdapterAdvancedProperty -Name $name |
                Where-Object { $_.DisplayName -eq $Config.VlanDisplayName } | Select-Object -First 1
            if ($null -eq $p) {
                # The VLAN lives elsewhere (another tool, a parent's setting):
                # report what the adapter reads, never guess a fix.
                $a = Get-NetAdapter -Name $name
                $read = ''
                if ($a.PSObject.Properties.Name -contains 'VlanID') { $read = [string]$a.VlanID }
                $none = "no '$($Config.VlanDisplayName)' property on the driver"
                if (Test-VlanNeedsSet -Value $read -Expected $want) {
                    return [pscustomobject]@{ Changed = $false; Text = "WARN VLAN mismatch: $none; the adapter reads '$read', the config expects $want; not changed (no guess)" }
                }
                return [pscustomobject]@{ Changed = $false; Text = "${none}; the adapter reads VLAN $want as configured: nothing to set" }
            }
            $value = [string]($p.RegistryValue -join ',')
            if (Test-VlanNeedsSet -Value $value -Expected $want) {
                Set-NetAdapterAdvancedProperty -Name $name -RegistryKeyword $p.RegistryKeyword -RegistryValue ([string]$want) -NoRestart
                return [pscustomobject]@{ Changed = $true; Text = "set $($p.RegistryKeyword) from '$value' to $want" }
            }
            return [pscustomobject]@{ Changed = $false; Text = "$($p.RegistryKeyword) is '$value' already (VLAN $want)" }
        }
        'BindIPv4' {
            $b = Get-NetAdapterBinding -Name $name -ComponentID ms_tcpip
            if (-not $b.Enabled) {
                Enable-NetAdapterBinding -Name $name -ComponentID ms_tcpip
                return [pscustomobject]@{ Changed = $true; Text = 'bound IPv4 (ms_tcpip was unbound)' }
            }
            return [pscustomobject]@{ Changed = $false; Text = 'IPv4 bound already' }
        }
        'Dhcp' {
            # Right after IPv4 is bound again its interface takes a moment to appear.
            $i = $null
            for ($t = 0; $t -lt 10 -and $null -eq $i; $t++) {
                $i = Get-NetIPInterface -InterfaceAlias $name -AddressFamily IPv4 -ErrorAction SilentlyContinue
                if ($null -eq $i) { Start-Sleep -Seconds 1 }
            }
            if ($null -eq $i) { throw "no IPv4 interface on '$name' after 10 s" }
            if ($i.Dhcp -ne 'Enabled') {
                Set-NetIPInterface -InterfaceAlias $name -AddressFamily IPv4 -Dhcp Enabled
                return [pscustomobject]@{ Changed = $true; Text = "turned DHCP on (was $($i.Dhcp))" }
            }
            return [pscustomobject]@{ Changed = $false; Text = 'DHCP on already' }
        }
        'Restart' {
            Restart-NetAdapter -Name $name -Confirm:$false
            return [pscustomobject]@{ Changed = $true; Text = 'restarted the adapter' }
        }
        default { throw "unknown repair step '$($Step.Action)'" }
    }
}

function Invoke-Repair {
    # The repair after the threshold. Returns the exit code; updates
    # $State.LastRestart when it restarts.
    param([Parameter(Mandatory)]$Config, [Parameter(Mandatory)]$State, [Parameter(Mandatory)][string]$Log)
    try {
        $adapters = Resolve-HealAdapters -Config $Config
        $plan = @(Get-RepairPlan -LanAdapter $Config.LanAdapter -DanteAdapter $Config.DanteAdapter)
    } catch {
        Write-HealLog -Path $Log -Message "ERROR $($_.Exception.Message): no repair"
        return 1
    }
    $code = 0
    $changed = $false
    foreach ($step in $plan) {
        try {
            if ($step.Action -eq 'Restart') {
                $since = -1.0
                if ($null -ne $State.LastRestart) { $since = ([datetime]::UtcNow - [datetime]$State.LastRestart).TotalMinutes }
                $lease = Test-UsableIPv4 -Addresses (Get-LanIPv4 -IfIndex ([int]$adapters.Lan.ifIndex))
                $d = Get-RestartDecision -Changed $changed -HasLease $lease -MinutesSinceRestart $since -GapMinutes $script:RestartGapMinutes
                if (-not $d.Restart) {
                    Write-HealLog -Path $Log -Message "  Restart: skipped, $($d.Why)"
                    continue
                }
                $r = Invoke-RepairStep -Step $step -Config $Config
                $State.LastRestart = [datetime]::UtcNow
                Write-HealLog -Path $Log -Message "  Restart: $($r.Text) ($($d.Why))"
                continue
            }
            $r = Invoke-RepairStep -Step $step -Config $Config
            if ($r.Changed) { $changed = $true }
            Write-HealLog -Path $Log -Message "  $($step.Action): $($r.Text)"
        } catch {
            # One failed step does not stop the rest; the run exits non-zero.
            Write-HealLog -Path $Log -Message "  $($step.Action): ERROR $($_.Exception.Message)"
            $changed = $true
            $code = 1
        }
    }
    $code
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
        $state = Read-HealState -Json $null
        if (Test-Path -LiteralPath $statePath) {
            try { $state = Read-HealState -Json (Get-Content -LiteralPath $statePath -Raw) }
            catch { Write-HealLog -Path $log -Message "state file unreadable, counting from 0: $($_.Exception.Message)" }
        }
        $adapters = Resolve-HealAdapters -Config $cfg
        if ($null -eq $adapters.Dante) {
            Write-HealLog -Path $log -Message "WARN no adapter named '$($cfg.DanteAdapter)': the LAN adapter is checked by its exact name"
        }
        $fault = Get-LanLegFault -Config $cfg -Adapters $adapters
        $next = Get-NextState -Failures $state.Failures -GatewayOk ($fault -eq '') -Threshold $cfg.ThresholdMinutes
        $code = 0
        if ($fault -eq '') {
            $note = if ($next.Previous -gt 0) { " (down count $($next.Previous) reset)" } else { '' }
            Write-HealLog -Path $log -Message "gateway ok$note"
        } elseif (-not $next.Heal) {
            Write-HealLog -Path $log -Message "LAN down $($next.Failures)/$($cfg.ThresholdMinutes) min: $fault"
        } else {
            Write-HealLog -Path $log -Message ("LAN down {0}/{0} min: {1}; repairing LAN adapter '{2}' (Dante adapter '{3}' untouched)" -f $cfg.ThresholdMinutes, $fault, $cfg.LanAdapter, $cfg.DanteAdapter)
            $code = Invoke-Repair -Config $cfg -State $state -Log $log
            Write-HealLog -Path $log -Message "repair done (exit $code)"
        }
        try { Save-HealState -Path $statePath -Failures $next.Failures -LastRestart $state.LastRestart }
        catch {
            Write-HealLog -Path $log -Message "ERROR state not saved: $($_.Exception.Message)"
            $code = 1
        }
        return $code
    } catch {
        # Anything unexpected before a repair: one log line, exit 1.
        try { Write-HealLog -Path $log -Message "ERROR $($_.Exception.Message): nothing done" } catch { Write-Error -ErrorAction Continue "lan-selfheal: log not writable: $($_.Exception.Message)" }
        return 1
    }
}

function Set-AdminOnlyAcl {
    # A script SYSTEM runs must not be writable by anyone else: owner
    # Administrators, inheritance reset, SYSTEM and Administrators full,
    # Users read; then read the ACL back and refuse anything else that writes.
    param([Parameter(Mandatory)][string]$Path)
    if ((Get-Item -LiteralPath $Path -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw "refused: $Path is a junction or link; remove it and create a plain directory"
    }
    $icacls = Join-Path $env:SystemRoot 'System32\icacls.exe'
    & $icacls $Path /setowner '*S-1-5-32-544' /T /C /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls /setowner on $Path failed ($LASTEXITCODE)" }
    & $icacls $Path /reset /T /C /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls /reset on $Path failed ($LASTEXITCODE)" }
    & $icacls $Path /inheritance:r /grant:r '*S-1-5-18:(OI)(CI)F' '*S-1-5-32-544:(OI)(CI)F' '*S-1-5-32-545:(OI)(CI)RX' /Q | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "icacls /grant on $Path failed ($LASTEXITCODE)" }
    $allowed = 'S-1-5-18', 'S-1-5-32-544'
    foreach ($item in @(Get-Item -LiteralPath $Path) + @(Get-ChildItem -LiteralPath $Path -Recurse -Force)) {
        $acl = Get-Acl -LiteralPath $item.FullName
        $owner = $acl.GetOwner([System.Security.Principal.SecurityIdentifier]).Value
        if ($allowed -notcontains $owner) { throw "ACL check: $($item.FullName) is owned by $($acl.Owner)" }
        foreach ($ace in $acl.GetAccessRules($true, $true, [System.Security.Principal.SecurityIdentifier])) {
            if ($ace.AccessControlType -eq 'Allow' -and (Test-AceGrantsWrite -Rights ([long]$ace.FileSystemRights)) -and
                $allowed -notcontains $ace.IdentityReference.Value) {
                throw "ACL check: $($ace.IdentityReference.Value) may write $($item.FullName)"
            }
        }
    }
}

function Install-SelfHeal {
    param([Parameter(Mandatory)][string]$Dir, [Parameter(Mandatory)][string]$Source)
    $configPath = Join-Path $Dir 'lan-selfheal.json'
    if (-not (Test-Path -LiteralPath $configPath)) { throw "no config at ${configPath}, write it first" }
    $cfg = Read-HealConfig -Json (Get-Content -LiteralPath $configPath -Raw)
    Resolve-HealAdapters -Config $cfg | Out-Null
    Set-AdminOnlyAcl -Path $Dir
    $target = Join-Path $Dir 'lan-selfheal.ps1'
    if ((Resolve-Path -LiteralPath $Source).Path -ne $target) {
        Copy-Item -LiteralPath $Source -Destination $target -Force
        Set-AdminOnlyAcl -Path $Dir
    }
    $exe = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $arg = '-NoProfile -NonInteractive -WindowStyle Hidden -ExecutionPolicy Bypass -File "{0}"' -f $target
    $action = New-ScheduledTaskAction -Execute $exe -Argument $arg
    $trigger = New-ScheduledTaskTrigger -Once -At (Get-Date).AddMinutes(1) -RepetitionInterval (New-TimeSpan -Minutes 1)
    $boot = New-ScheduledTaskTrigger -AtStartup
    $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount -RunLevel Highest
    $settings = New-ScheduledTaskSettingsSet -AllowStartIfOnBatteries -DontStopIfGoingOnBatteries `
        -StartWhenAvailable -MultipleInstances IgnoreNew -ExecutionTimeLimit (New-TimeSpan -Minutes 5)
    Register-ScheduledTask -TaskName $script:TaskName -Action $action -Trigger @($trigger, $boot) `
        -Principal $principal -Settings $settings -Force | Out-Null
    # Read back what was registered.
    $t = Get-ScheduledTask -TaskName $script:TaskName
    if ($t.Principal.UserId -notmatch 'SYSTEM$') { throw "install check: the task runs as '$($t.Principal.UserId)', not SYSTEM" }
    $rep = @($t.Triggers | Where-Object { $_.Repetition -and $_.Repetition.Interval -eq 'PT1M' })
    if ($rep.Count -ne 1 -or -not [string]::IsNullOrEmpty($rep[0].Repetition.Duration)) {
        throw 'install check: no trigger repeating every minute without end'
    }
    if ($t.Actions[0].Execute -ne $exe -or $t.Actions[0].Arguments -ne $arg) { throw 'install check: the task action differs' }
    Write-HealLog -Path (Join-Path $Dir 'lan-selfheal.log') -Message "installed: task '$($script:TaskName)' every minute and at startup as SYSTEM, script $target, LAN adapter '$($cfg.LanAdapter)' on VLAN $($cfg.LanVlanId) (0 = untagged)"
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

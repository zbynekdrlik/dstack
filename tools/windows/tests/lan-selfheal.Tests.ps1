# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Tests of tools/windows/lan-selfheal.ps1 (dstack #239), run by CI's Windows
# job under Windows PowerShell 5.1:
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File tools\windows\tests\lan-selfheal.Tests.ps1
# A plain assert script, not Pester: the inbox Pester 3.4 and the runner's
# Pester 5 differ in syntax. Exits 1 if any check fails.
#
# No real network adapter is touched: the NetAdapter/NetTCPIP cmdlets the
# script calls are replaced below by functions of the same name (a function
# wins over a cmdlet), which act on a fake LAN adapter and a fake Dante
# adapter and record every call with its target.

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\lan-selfheal.ps1')

$script:failed = 0
$script:passed = 0
function Check {
    param([string]$Name, [scriptblock]$Body)
    try {
        $ok = & $Body
        if ($ok -is [bool] -and $ok) { $script:passed++; Write-Host "ok   $Name" }
        else {
            $type = if ($null -eq $ok) { 'null' } else { $ok.GetType().Name }
            $script:failed++; Write-Host "FAIL $Name (got '$ok', $type)"
        }
    } catch {
        $script:failed++; Write-Host "FAIL $Name (threw: $($_.Exception.Message))"
    }
}
function Throws {
    param([scriptblock]$Body, [string]$Like)
    try { & $Body | Out-Null; return $false } catch { return [bool]($_.Exception.Message -like $Like) }
}

# --------------------------------------------------- the fake adapters
function Reset-Fake {
    $script:Fake = @{
        Calls        = New-Object System.Collections.ArrayList
        LanName      = 'LAN'
        DanteName    = 'Dante70'
        LanIndex     = 5
        DanteIndex   = 9
        Status       = 'Up'
        Vlan         = ''
        Bound        = $true
        Dhcp         = 'Enabled'
        Addresses    = @('10.0.0.20')
        RouteIndex   = 5
        FailStep     = ''
        ExtraLan     = $false
        PingOk       = $true
        NoIpIf       = $false
        VlanProp     = $true    # the LAN driver exposes the "VLAN ID" property
        AdapterVlan  = ''       # Get-NetAdapter's VlanID of the LAN adapter
        Rules        = [ordered]@{}   # firewall rules by Name
        StuckRule    = ''             # a rule Enable-NetFirewallRule leaves disabled
        StuckOff     = ''             # a rule Disable-NetFirewallRule leaves enabled
    }
    # The dstack155 rule that blocks TCP in on the Dante adapter (#239).
    $script:Fake.Rules['{dante-tcp-in}'] = [pscustomobject]@{
        Name = '{dante-tcp-in}'; DisplayName = 'Dante70: block TCP Inbound'; Enabled = 'True'
        Action = 'Block'; Direction = 'Inbound'; Group = 'dstack155'; Protocol = 'TCP'; InterfaceAlias = 'Dante70'
    }
}
function Note { param([string]$Verb, [string]$Target) [void]$script:Fake.Calls.Add("$Verb $Target") }
function Assert-FakeTarget {
    param([string]$Verb, [string]$Target)
    Note $Verb $Target
    if ($script:Fake.FailStep -eq $Verb) { throw "fake failure in $Verb" }
}
function Get-NetAdapter {
    [CmdletBinding()] param([string]$Name, [switch]$IncludeHidden)
    $all = @(
        [pscustomobject]@{ Name = $script:Fake.LanName; ifIndex = $script:Fake.LanIndex; Status = $script:Fake.Status; VlanID = $script:Fake.AdapterVlan },
        [pscustomobject]@{ Name = $script:Fake.DanteName; ifIndex = $script:Fake.DanteIndex; Status = 'Up'; VlanID = '' }
    )
    if ($script:Fake.ExtraLan) { $all += [pscustomobject]@{ Name = $script:Fake.LanName; ifIndex = 7; Status = 'Up' } }
    if ($Name) { Note 'Get-NetAdapter' $Name; return @($all | Where-Object { $_.Name -eq $Name })[0] }
    $all
}
function Enable-NetAdapter {
    [CmdletBinding(SupportsShouldProcess)] param([string]$Name)
    Assert-FakeTarget 'Enable-NetAdapter' $Name; $script:Fake.Status = 'Up'
}
function Restart-NetAdapter {
    [CmdletBinding(SupportsShouldProcess)] param([string]$Name)
    Assert-FakeTarget 'Restart-NetAdapter' $Name
}
function Get-NetAdapterAdvancedProperty {
    [CmdletBinding()] param([string]$Name)
    Note 'Get-NetAdapterAdvancedProperty' $Name
    if (-not $script:Fake.VlanProp) { return }
    [pscustomobject]@{ DisplayName = 'VLAN ID'; RegistryKeyword = 'RegVlanID'; RegistryValue = @($script:Fake.Vlan) }
}
function Set-NetAdapterAdvancedProperty {
    [CmdletBinding()] param([string]$Name, [string]$RegistryKeyword, [string]$RegistryValue, [switch]$NoRestart)
    Assert-FakeTarget 'Set-NetAdapterAdvancedProperty' $Name; $script:Fake.Vlan = $RegistryValue
}
function Get-NetAdapterBinding {
    [CmdletBinding()] param([string]$Name, [string]$ComponentID)
    Note 'Get-NetAdapterBinding' $Name
    [pscustomobject]@{ Enabled = $script:Fake.Bound }
}
function Enable-NetAdapterBinding {
    [CmdletBinding()] param([string]$Name, [string]$ComponentID)
    Assert-FakeTarget 'Enable-NetAdapterBinding' $Name; $script:Fake.Bound = $true
}
function Get-NetIPInterface {
    [CmdletBinding()] param([string]$InterfaceAlias, [string]$AddressFamily)
    Note 'Get-NetIPInterface' $InterfaceAlias
    if ($script:Fake.NoIpIf) { return }
    [pscustomobject]@{ Dhcp = $script:Fake.Dhcp }
}
function Set-NetIPInterface {
    [CmdletBinding()] param([string]$InterfaceAlias, [string]$AddressFamily, [string]$Dhcp)
    Assert-FakeTarget 'Set-NetIPInterface' $InterfaceAlias; $script:Fake.Dhcp = $Dhcp
}
function Get-NetIPAddress {
    [CmdletBinding()] param([int]$InterfaceIndex, [string]$AddressFamily)
    if ($InterfaceIndex -ne $script:Fake.LanIndex) { return }
    foreach ($a in $script:Fake.Addresses) { [pscustomobject]@{ IPAddress = $a } }
}
function Test-Gateway {
    # The ping, faked like the cmdlets: no test reaches the network.
    param([string]$Address)
    Note 'Test-Gateway' $Address
    $script:Fake.PingOk
}
function Start-Sleep { param([int]$Seconds) }
function Find-NetRoute {
    [CmdletBinding()] param([string]$RemoteIPAddress)
    [pscustomobject]@{ InterfaceIndex = $script:Fake.RouteIndex }
}
# The firewall cmdlets (#239), faked the same way: rules live in
# $script:Fake.Rules, every call is recorded with the rule's Name.
function Get-NetFirewallRule {
    [CmdletBinding()] param([string]$Name, [string]$DisplayName)
    if ($Name) { Note 'Get-NetFirewallRule' $Name; return @($script:Fake.Rules.Values | Where-Object { $_.Name -eq $Name }) }
    Note 'Get-NetFirewallRule' $DisplayName
    @($script:Fake.Rules.Values | Where-Object { $_.DisplayName -like $DisplayName })
}
function New-NetFirewallRule {
    [CmdletBinding()] param([string]$Name, [string]$DisplayName, [string]$Group, [string]$Enabled,
        [string]$Direction, [string]$Action, [string]$Protocol, [string]$Profile, [string]$InterfaceAlias,
        [string[]]$RemoteAddress, [string[]]$LocalPort)
    Assert-FakeTarget 'New-NetFirewallRule' $Name
    $r = [pscustomobject]@{
        Name = $Name; DisplayName = $DisplayName; Enabled = $Enabled; Action = $Action; Direction = $Direction
        Group = $Group; Protocol = $Protocol; InterfaceAlias = $InterfaceAlias
        RemoteAddress = $RemoteAddress; LocalPort = $LocalPort
    }
    $script:Fake.Rules[$Name] = $r
    $r
}
function Set-NetFirewallRule {
    [CmdletBinding()] param([string]$Name, [string]$Direction, [string]$Action, [string]$Protocol,
        [string]$Profile, [string]$InterfaceAlias, [string[]]$RemoteAddress, [string[]]$LocalPort)
    Assert-FakeTarget 'Set-NetFirewallRule' $Name
    $r = $script:Fake.Rules[$Name]
    $r.Action = $Action; $r.Direction = $Direction; $r.Protocol = $Protocol; $r.InterfaceAlias = $InterfaceAlias
    $r.RemoteAddress = $RemoteAddress; $r.LocalPort = $LocalPort
}
function Enable-NetFirewallRule {
    [CmdletBinding()] param([string]$Name)
    Assert-FakeTarget 'Enable-NetFirewallRule' $Name
    if ($script:Fake.StuckRule -ne $Name) { $script:Fake.Rules[$Name].Enabled = 'True' }
}
function Disable-NetFirewallRule {
    [CmdletBinding()] param([string]$Name)
    Assert-FakeTarget 'Disable-NetFirewallRule' $Name
    if ($script:Fake.StuckOff -ne $Name) { $script:Fake.Rules[$Name].Enabled = 'False' }
}
function Get-NetFirewallPortFilter {
    [CmdletBinding()] param($AssociatedNetFirewallRule)
    [pscustomobject]@{ Protocol = $AssociatedNetFirewallRule.Protocol }
}
function Get-NetFirewallInterfaceFilter {
    [CmdletBinding()] param($AssociatedNetFirewallRule)
    [pscustomobject]@{ InterfaceAlias = $AssociatedNetFirewallRule.InterfaceAlias }
}
function FwCalls { @($script:Fake.Calls | Where-Object { $_ -match 'NetFirewallRule ' }) }
function FwChanges { @($script:Fake.Calls | Where-Object { $_ -match '^(New|Set|Enable|Disable)-NetFirewallRule ' }) }
function IndexOf { param([string]$Call) [array]::IndexOf(@($script:Fake.Calls), $Call) }
function RuleOn { param([string]$Name) $script:Fake.Rules.Contains($Name) -and $script:Fake.Rules[$Name].Enabled -eq 'True' }
function Changes {
    # The calls that change an adapter (not the reads).
    @($script:Fake.Calls | Where-Object { $_ -match '^(Enable|Set|Restart)-' })
}
function DanteCalls { @($script:Fake.Calls | Where-Object { $_ -match ' Dante70$' }) }
Reset-Fake

$good = '{"lanAdapter":"LAN","gateway":"192.0.2.1","danteAdapter":"Dante70","thresholdMinutes":3,"lanVlanId":0}'
# The reverse layout (dasio's test PC): Dante is the physical adapter,
# untagged; the LAN is a VLAN adapter on VLAN 123.
$reverse = $good.Replace('"lanVlanId":0', '"lanVlanId":123')
# The emergency path on (#239): source 192.0.2.50, ssh 22.
$emerg = $good.Replace('"lanVlanId":0', '"lanVlanId":0,"emergencySource":"192.0.2.50","danteBlockRule":"Dante70: block TCP Inbound"')
$others = 'dstack-emergency-block-others'
$ports = 'dstack-emergency-block-source-ports'

# --- counter and threshold
Check 'a healthy leg resets the counter' {
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
Check 'one healthy minute between failures starts the count again' {
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

# --- the leg's health
Check 'a healthy leg: lease, route on the LAN, gateway answers' {
    (Get-LegFault -Addresses @('10.0.0.20') -RouteIfIndex 5 -LanIfIndex 5 -PingOk $true) -eq ''
}
Check 'a gateway answer over another leg does not count' {
    (Get-LegFault -Addresses @('10.0.0.20') -RouteIfIndex 9 -LanIfIndex 5 -PingOk $true) -like '*route*'
}
Check 'no address or only APIPA on the LAN adapter is down' {
    ((Get-LegFault -Addresses @() -RouteIfIndex 5 -LanIfIndex 5 -PingOk $true) -like '*no usable IPv4*') -and
    ((Get-LegFault -Addresses @('169.254.3.4') -RouteIfIndex 5 -LanIfIndex 5 -PingOk $true) -like '*no usable IPv4*')
}
Check 'no gateway answer is down' {
    (Get-LegFault -Addresses @('10.0.0.20') -RouteIfIndex 5 -LanIfIndex 5 -PingOk $false) -like '*does not answer*'
}

# --- the restart decision
Check 'a step that changed something restarts' {
    (Get-RestartDecision -Changed $true -HasLease $true -MinutesSinceRestart 1 -GapMinutes 30).Restart
}
Check 'no lease restarts, but at most once per gap (a DHCP server outage)' {
    -not (Get-RestartDecision -Changed $false -HasLease $false -MinutesSinceRestart 3 -GapMinutes 30).Restart -and
    (Get-RestartDecision -Changed $false -HasLease $false -MinutesSinceRestart -1 -GapMinutes 30).Restart -and
    (Get-RestartDecision -Changed $false -HasLease $false -MinutesSinceRestart 31 -GapMinutes 30).Restart
}
Check 'an ACE writes when it holds a write, delete, owner or generic write bit' {
    $w = foreach ($m in 0x2, 0x4, 0x10000, 0x40, 0x40000, 0x80000, 0x40000000, 0x10000000, 0x1F01FF) { Test-AceGrantsWrite -Rights $m }
    $r = foreach ($m in 0x1200A9, 0x120089, 0x80000000) { Test-AceGrantsWrite -Rights $m }
    @($w | Where-Object { -not $_ }).Count -eq 0 -and @($r | Where-Object { $_ }).Count -eq 0
}
Check 'a healthy adapter restarts at most once per gap (a router outage)' {
    -not (Get-RestartDecision -Changed $false -HasLease $true -MinutesSinceRestart 3 -GapMinutes 30).Restart -and
    (Get-RestartDecision -Changed $false -HasLease $true -MinutesSinceRestart 30 -GapMinutes 30).Restart -and
    (Get-RestartDecision -Changed $false -HasLease $true -MinutesSinceRestart -1 -GapMinutes 30).Restart
}

# --- config
Check 'a valid config parses, threshold and VLAN display name defaults' {
    $c = Read-HealConfig -Json '{"lanAdapter":" LAN ","gateway":"192.0.2.1","danteAdapter":"Dante70","lanVlanId":0}'
    $c.LanAdapter -eq 'LAN' -and $c.ThresholdMinutes -eq 3 -and $c.VlanDisplayName -eq 'VLAN ID' -and $c.LanVlanId -eq 0
}
Check 'the LAN VLAN ID is read (123: a LAN on a VLAN adapter)' {
    (Read-HealConfig -Json $reverse).LanVlanId -eq 123
}
Check 'a config without lanVlanId is refused, naming the key' {
    Throws { Read-HealConfig -Json '{"lanAdapter":"LAN","gateway":"192.0.2.1","danteAdapter":"Dante70"}' } "*'lanVlanId' is missing*"
}
Check 'a lanVlanId that is not a whole number from 0 to 4094 is refused' {
    $bad = foreach ($v in '4095', '-1', '"123a"', '"123"', '1.5', '""', 'null', 'true') {
        Throws { Read-HealConfig -Json $good.Replace('"lanVlanId":0', ('"lanVlanId":' + $v)) } "*'lanVlanId'*"
    }
    @($bad | Where-Object { -not $_ }).Count -eq 0
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
Check 'an adapter name with a wildcard character is refused' {
    (Throws { Read-HealConfig -Json $good.Replace('"LAN"', '"Ethernet*"') } '*wildcard*') -and
    (Throws { Read-HealConfig -Json $good.Replace('"Dante70"', '"Realtek [VLAN 70]"') } '*wildcard*')
}
Check 'a threshold below 1 is refused' {
    Throws { Read-HealConfig -Json $good.Replace('"thresholdMinutes":3', '"thresholdMinutes":0') } '*thresholdMinutes*'
}
Check 'a missing state counts from zero' {
    $z = Read-HealState -Json ''
    $z.Failures -eq 0 -and $null -eq $z.LastRestart
}
Check 'the restart gap counts in UTC' {
    $s = Read-HealState -Json '{"failures":2,"lastRestart":"2026-11-12T16:20:00.0000000+01:00"}'
    $s.Failures -eq 2 -and $s.LastRestart.Kind -eq 'Utc' -and $s.LastRestart.Hour -eq 15
}

# --- the emergency path's config and scopes (#239)
Check 'emergency config: off without emergencySource' {
    $c = Read-HealConfig -Json $good
    $null -eq $c.EmergencySource -and $null -eq $c.DanteBlockRule
}
Check 'emergency config: source, default port 22 and the rule name are read' {
    $c = Read-HealConfig -Json $emerg
    $c.EmergencySource -eq '192.0.2.50' -and $c.EmergencyPort -eq 22 -and $c.DanteBlockRule -eq 'Dante70: block TCP Inbound'
}
Check 'emergency config: emergencyPort is read' {
    (Read-HealConfig -Json $emerg.Replace('"emergencySource"', '"emergencyPort":2222,"emergencySource"')).EmergencyPort -eq 2222
}
Check 'emergency config: a source that is not one IPv4 host is refused' {
    (Throws { Read-HealConfig -Json $emerg.Replace('192.0.2.50', '192.0.2.0/24') } "config: 'emergencySource'*") -and
    (Throws { Read-HealConfig -Json $emerg.Replace('192.0.2.50', '2001:db8::1') } "config: 'emergencySource'*") -and
    (Throws { Read-HealConfig -Json $emerg.Replace('192.0.2.50', '255.255.255.255') } "config: 'emergencySource'*") -and
    (Throws { Read-HealConfig -Json $emerg.Replace('192.0.2.50', '10.1') } "config: 'emergencySource'*")
}
Check 'emergency config: a port out of 1-65535 is refused' {
    (Throws { Read-HealConfig -Json $emerg.Replace('"emergencySource"', '"emergencyPort":0,"emergencySource"') } "config: 'emergencyPort'*") -and
    (Throws { Read-HealConfig -Json $emerg.Replace('"emergencySource"', '"emergencyPort":65536,"emergencySource"') } "config: 'emergencyPort'*") -and
    (Throws { Read-HealConfig -Json $emerg.Replace('"emergencySource"', '"emergencyPort":"22","emergencySource"') } "config: 'emergencyPort'*")
}
Check 'emergency config: a missing or wildcard danteBlockRule is refused' {
    (Throws { Read-HealConfig -Json $emerg.Replace(',"danteBlockRule":"Dante70: block TCP Inbound"', '') } "config: 'danteBlockRule' is missing*") -and
    (Throws { Read-HealConfig -Json $emerg.Replace('Dante70: block TCP Inbound', '') } "config: 'danteBlockRule' is missing*") -and
    (Throws { Read-HealConfig -Json $emerg.Replace('Dante70: block TCP Inbound', 'Dante* block') } "config: 'danteBlockRule'*wildcard*")
}
Check 'emergency scope: every address but the source, IPv6 included' {
    ((Get-AddressComplement -Address '10.1.2.3') -join ',') -eq '0.0.0.0-10.1.2.2,10.1.2.4-255.255.255.255,::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff' -and
    ((Get-AddressComplement -Address '10.1.2.0') -join ',') -eq '0.0.0.0-10.1.1.255,10.1.2.1-255.255.255.255,::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff' -and
    ((Get-AddressComplement -Address '200.1.255.255') -join ',') -eq '0.0.0.0-200.1.255.254,200.2.0.0-255.255.255.255,::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff'
}
Check 'emergency scope: every port but one' {
    ((Get-PortComplement -Port 22) -join ',') -eq '1-21,23-65535' -and ((Get-PortComplement -Port 1) -join ',') -eq '2-65535' -and
    ((Get-PortComplement -Port 2) -join ',') -eq '1,3-65535' -and ((Get-PortComplement -Port 65535) -join ',') -eq '1-65534' -and
    ((Get-PortComplement -Port 65534) -join ',') -eq '1-65533,65535'
}

# --- never the Dante adapter
Check 'the repair plan is the five steps in order, all on the LAN adapter' {
    $p = @(Get-RepairPlan -LanAdapter 'LAN' -DanteAdapter 'Dante70')
    ($p.Action -join ',') -eq 'Enable,VlanId,BindIPv4,Dhcp,Restart' -and
    @($p | Where-Object { $_.Target -ne 'LAN' }).Count -eq 0
}
Check 'a plan aimed at the Dante adapter is refused' {
    Throws { Get-RepairPlan -LanAdapter 'Dante70' -DanteAdapter 'dante70' } '*Dante adapter*'
}
Check 'a repair step aimed at the Dante adapter is refused before any change' {
    Reset-Fake
    $cfg = Read-HealConfig -Json $good
    $all = foreach ($a in 'Enable', 'VlanId', 'BindIPv4', 'Dhcp', 'Restart') {
        Throws { Invoke-RepairStep -Step ([pscustomobject]@{ Action = $a; Target = ' dante70' }) -Config $cfg } '*Dante adapter*'
    }
    @($all | Where-Object { -not $_ }).Count -eq 0 -and $script:Fake.Calls.Count -eq 0
}
Check 'the adapters resolve by exact name to two interfaces; a missing Dante adapter does not block' {
    $cfg = Read-HealConfig -Json $good
    Reset-Fake; $script:Fake.DanteIndex = 5
    $same = Throws { Resolve-HealAdapters -Config $cfg } '*Dante adapter*'
    Reset-Fake; $script:Fake.DanteName = 'Other'
    $r = Resolve-HealAdapters -Config $cfg
    $missing = $r.Lan.ifIndex -eq 5 -and $null -eq $r.Dante
    Reset-Fake; $script:Fake.ExtraLan = $true
    $two = Throws { Resolve-HealAdapters -Config $cfg } "*2 adapters named 'LAN'*"
    Reset-Fake; $script:Fake.LanName = 'Gone'
    $nolan = Throws { Resolve-HealAdapters -Config $cfg } "*0 adapters named 'LAN'*"
    $same -and $missing -and $two -and $nolan
}

# --- each repair step: needed and not needed
$cfgGood = Read-HealConfig -Json $good
$cfgReverse = Read-HealConfig -Json $reverse
function Step {
    param([string]$A, $Cfg = $cfgGood)
    Invoke-RepairStep -Step ([pscustomobject]@{ Action = $A; Target = 'LAN' }) -Config $Cfg
}
Check 'Enable: a disabled adapter is enabled, an enabled one left alone' {
    Reset-Fake; $script:Fake.Status = 'Disabled'
    $a = Step 'Enable'
    $after = $script:Fake.Status
    Reset-Fake; $b = Step 'Enable'
    $a.Changed -and $after -eq 'Up' -and -not $b.Changed -and @(Changes).Count -eq 0
}
Check 'VlanId, lanVlanId 0: 70 is set to 0; empty and 0 are left alone' {
    Reset-Fake; $script:Fake.Vlan = '70'
    $a = Step 'VlanId'; $after = $script:Fake.Vlan
    Reset-Fake; $b = Step 'VlanId'
    Reset-Fake; $script:Fake.Vlan = '0'; $c = Step 'VlanId'
    $a.Changed -and $after -eq '0' -and -not $b.Changed -and -not $c.Changed -and @(Changes).Count -eq 0
}
Check 'VlanId, lanVlanId 123: 123 is left alone, never set to 0' {
    Reset-Fake; $script:Fake.Vlan = '123'
    $a = Step 'VlanId' $cfgReverse
    -not $a.Changed -and $script:Fake.Vlan -eq '123' -and @(Changes).Count -eq 0
}
Check 'VlanId, lanVlanId 123: empty, 0 and 70 are set to 123' {
    $ok = foreach ($v in '', '0', '70') {
        Reset-Fake; $script:Fake.Vlan = $v
        $r = Step 'VlanId' $cfgReverse
        $r.Changed -and $script:Fake.Vlan -eq '123' -and (@(Changes) -join ';') -eq 'Set-NetAdapterAdvancedProperty LAN'
    }
    @($ok | Where-Object { -not $_ }).Count -eq 0
}
Check 'VlanId, no VLAN property, lanVlanId 0: nothing to set, nothing changed' {
    Reset-Fake; $script:Fake.VlanProp = $false
    $a = Step 'VlanId'
    -not $a.Changed -and $a.Text -like "*no 'VLAN ID' property*" -and @(Changes).Count -eq 0
}
Check 'VlanId, no VLAN property, lanVlanId 0, the adapter reads 70: the mismatch is logged, nothing changed' {
    Reset-Fake; $script:Fake.VlanProp = $false; $script:Fake.AdapterVlan = '70'
    $a = Step 'VlanId'
    -not $a.Changed -and $a.Text -like "WARN VLAN mismatch*reads '70'*expects 0*not changed*" -and @(Changes).Count -eq 0
}
Check 'VlanId, no VLAN property, lanVlanId 123: the mismatch read is logged, nothing guessed' {
    Reset-Fake; $script:Fake.VlanProp = $false; $script:Fake.AdapterVlan = '0'
    $a = Step 'VlanId' $cfgReverse
    Reset-Fake; $script:Fake.VlanProp = $false; $script:Fake.AdapterVlan = ''
    $b = Step 'VlanId' $cfgReverse
    Reset-Fake; $script:Fake.VlanProp = $false; $script:Fake.AdapterVlan = '123'
    $c = Step 'VlanId' $cfgReverse
    -not $a.Changed -and $a.Text -like "*VLAN mismatch*reads '0'*expects 123*not changed*" -and
    -not $b.Changed -and $b.Text -like "*VLAN mismatch*reads ''*expects 123*" -and
    -not $c.Changed -and $c.Text -notlike '*mismatch*' -and $c.Text -like "*VLAN 123*" -and @(Changes).Count -eq 0
}
Check 'BindIPv4: an unbound IPv4 is bound, a bound one left alone' {
    Reset-Fake; $script:Fake.Bound = $false
    $a = Step 'BindIPv4'; $after = $script:Fake.Bound
    Reset-Fake; $b = Step 'BindIPv4'
    $a.Changed -and $after -and -not $b.Changed -and @(Changes).Count -eq 0
}
Check 'Dhcp: no IPv4 interface after the wait is an error' {
    Reset-Fake; $script:Fake.NoIpIf = $true
    Throws { Step 'Dhcp' } "*no IPv4 interface on 'LAN'*"
}
Check 'Dhcp: a static interface gets DHCP, a DHCP one left alone' {
    Reset-Fake; $script:Fake.Dhcp = 'Disabled'
    $a = Step 'Dhcp'; $after = $script:Fake.Dhcp
    Reset-Fake; $b = Step 'Dhcp'
    $a.Changed -and $after -eq 'Enabled' -and -not $b.Changed -and @(Changes).Count -eq 0
}

# --- log rotation
Check 'the log rotates at 1 MB' {
    (Test-LogNeedsRotation -Size 1MB -Max 1MB) -and -not (Test-LogNeedsRotation -Size (1MB - 1) -Max 1MB)
}
$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("lan-selfheal-test-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $tmp | Out-Null
function NewRun {
    param([string]$Name, [string]$Config, [string]$State)
    $d = Join-Path $tmp $Name
    New-Item -ItemType Directory -Path $d | Out-Null
    if ($Config) { Set-Content -Path (Join-Path $d 'lan-selfheal.json') -Value $Config }
    if ($State) { Set-Content -Path (Join-Path $d 'lan-selfheal.state.json') -Value $State }
    $d
}
function LogOf { param([string]$D) Get-Content (Join-Path $D 'lan-selfheal.log') -Raw }
function StateOf { param([string]$D) Get-Content (Join-Path $D 'lan-selfheal.state.json') -Raw | ConvertFrom-Json }
try {
    Check 'a full log moves to .1 and a new one starts' {
        $log = Join-Path $tmp 'rot.log'
        [System.IO.File]::WriteAllBytes($log, (New-Object byte[] (1MB)))
        Write-HealLog -Path $log -Message 'after rotation'
        (Test-Path "$log.1") -and (Get-Item "$log.1").Length -eq 1MB -and
        (Get-Content $log -Raw) -match 'after rotation' -and (Get-Item $log).Length -lt 1KB
    }

    # --- the run path
    Check 'no config: nothing done, exit 0' {
        Reset-Fake
        $d = NewRun 'noconf'
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and -not (Test-Path (Join-Path $d 'lan-selfheal.state.json')) -and
        (LogOf $d) -match 'config missing' -and $script:Fake.Calls.Count -eq 0
    }
    Check 'a broken config: nothing done, exit 1' {
        Reset-Fake
        $d = NewRun 'badconf' '{"lanAdapter":"LAN"}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and (LogOf $d) -match 'ERROR' -and $script:Fake.Calls.Count -eq 0
    }
    Check 'a healthy leg logs "gateway ok" and resets the count' {
        Reset-Fake
        $d = NewRun 'up' $good.Replace('192.0.2.1', '127.0.0.1') '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and (StateOf $d).failures -eq 0 -and (LogOf $d) -match 'gateway ok \(down count 2 reset\)' -and
        @(Changes).Count -eq 0
    }
    Check 'a config without lanVlanId: nothing done, exit 1, the log names the key' {
        Reset-Fake
        $d = NewRun 'novlan' '{"lanAdapter":"LAN","gateway":"192.0.2.1","danteAdapter":"Dante70"}' '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and (LogOf $d) -match "ERROR config: 'lanVlanId' is missing" -and @(Changes).Count -eq 0
    }
    Check 'the reverse layout (LAN on VLAN 123): the repair keeps VLAN 123 and never touches the Dante adapter' {
        Reset-Fake
        $script:Fake.Status = 'Disabled'; $script:Fake.Vlan = '123'; $script:Fake.Addresses = @()
        $d = NewRun 'reverse' $reverse '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $ch = @(Changes)
        $rc -eq 0 -and $script:Fake.Vlan -eq '123' -and
        ($ch -join ';') -eq 'Enable-NetAdapter LAN;Restart-NetAdapter LAN' -and @(DanteCalls).Count -eq 0
    }
    Check 'the reverse layout with the LAN VLAN cleared: the repair sets 123 back' {
        Reset-Fake
        $script:Fake.Vlan = ''; $script:Fake.Addresses = @()
        $d = NewRun 'reverse0' $reverse '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and $script:Fake.Vlan -eq '123' -and (@(Changes) -contains 'Set-NetAdapterAdvancedProperty LAN') -and
        (LogOf $d) -match "VlanId: set RegVlanID from '' to 123" -and @(DanteCalls).Count -eq 0
    }
    Check 'a gateway that does not answer counts 1/3 and changes nothing' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'down' $good
        $rc = Invoke-Check -Dir $d
        $text = LogOf $d
        $rc -eq 0 -and (StateOf $d).failures -eq 1 -and $text -match 'LAN down 1/3 min: the gateway does not answer' -and
        $text -notmatch 'repair' -and @(Changes).Count -eq 0
    }
    Check 'the third failed minute repairs the LAN adapter in order and never the Dante adapter' {
        Reset-Fake
        $script:Fake.Status = 'Disabled'; $script:Fake.Vlan = '70'; $script:Fake.Bound = $false
        $script:Fake.Dhcp = 'Disabled'; $script:Fake.Addresses = @()
        $d = NewRun 'heal' $good '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $ch = @(Changes)
        $rc -eq 0 -and ($ch -join ';') -eq ('Enable-NetAdapter LAN;Set-NetAdapterAdvancedProperty LAN;' +
            'Enable-NetAdapterBinding LAN;Set-NetIPInterface LAN;Restart-NetAdapter LAN') -and
        @(DanteCalls).Count -eq 0 -and (StateOf $d).failures -eq 0 -and (StateOf $d).lastRestart -and
        (LogOf $d) -match "repairing LAN adapter 'LAN' \(Dante adapter 'Dante70' untouched\)"
    }
    Check 'a healthy adapter in a router outage is not restarted again within the gap' {
        Reset-Fake; $script:Fake.PingOk = $false
        $recent = [datetime]::UtcNow.AddMinutes(-3).ToString('o')
        $d = NewRun 'outage' $good ('{"failures":2,"lastRestart":"' + $recent + '"}')
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and @(Changes).Count -eq 0 -and (LogOf $d) -match 'Restart: skipped, adapter healthy' -and
        (StateOf $d).lastRestart -eq $recent
    }
    Check 'a failed step does not stop the rest, the run exits 1' {
        Reset-Fake
        $script:Fake.Vlan = '70'; $script:Fake.Bound = $false; $script:Fake.FailStep = 'Set-NetAdapterAdvancedProperty'
        $script:Fake.PingOk = $false
        $d = NewRun 'stepfail' $good '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $ch = @(Changes)
        $rc -eq 1 -and ($ch -contains 'Enable-NetAdapterBinding LAN') -and ($ch -contains 'Restart-NetAdapter LAN') -and
        (LogOf $d) -match 'VlanId: ERROR fake failure' -and @(DanteCalls).Count -eq 0
    }
    Check 'the state file round-trips through Save and Read' {
        $f = Join-Path $tmp 'rt.json'
        $t = [datetime]::UtcNow
        Save-HealState -Path $f -Failures 2 -LastRestart $t
        $s = Read-HealState -Json (Get-Content $f -Raw)
        $s.Failures -eq 2 -and $s.LastRestart -eq $t
    }
    Check 'the state file round-trips the emergency rule, and leaves it out when none' {
        $f = Join-Path $tmp 'rt2.json'
        Save-HealState -Path $f -Failures 0 -LastRestart $null -EmergencyRule '{x}'
        $a = (Read-HealState -Json (Get-Content $f -Raw)).EmergencyRule
        Save-HealState -Path $f -Failures 0 -LastRestart $null
        $a -eq '{x}' -and $null -eq (Read-HealState -Json (Get-Content $f -Raw)).EmergencyRule -and
        (Get-Content $f -Raw) -notmatch 'emergencyRule'
    }
    Check 'a corrupt state file is logged and counts from zero' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'corrupt' $good '{not json'
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and (LogOf $d) -match 'state file unreadable' -and (StateOf $d).failures -eq 1
    }
    Check 'a missing Dante adapter is a warning: the LAN is still repaired, by name' {
        Reset-Fake; $script:Fake.DanteName = 'Other'; $script:Fake.Status = 'Disabled'; $script:Fake.Addresses = @()
        $d = NewRun 'nodante' $good '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $ch = @(Changes)
        $rc -eq 0 -and ($ch -contains 'Enable-NetAdapter LAN') -and (LogOf $d) -match "WARN no adapter named 'Dante70'"
    }
    Check 'the Dante adapter on the same interface: no repair at all, exit 1' {
        Reset-Fake
        $script:Fake.DanteIndex = 5; $script:Fake.Status = 'Disabled'
        $d = NewRun 'same' $good '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and @(Changes).Count -eq 0 -and (LogOf $d) -match 'ERROR refused'
    }

    # --- the emergency path (#239, shape 1)
    Check 'emergency: the open order is the emergency rules on, then the dstack155 rule off' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'emopen' $emerg '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $off = IndexOf 'Disable-NetFirewallRule {dante-tcp-in}'
        $a = IndexOf "Enable-NetFirewallRule $others"; $b = IndexOf "Enable-NetFirewallRule $ports"
        $o = $script:Fake.Rules[$others]; $p = $script:Fake.Rules[$ports]
        $text = LogOf $d
        $rc -eq 0 -and $a -ge 0 -and $b -ge 0 -and $off -gt $a -and $off -gt $b -and
        -not (RuleOn '{dante-tcp-in}') -and (RuleOn $others) -and (RuleOn $ports) -and
        $o.Group -eq 'dstack emergency' -and $o.Action -eq 'Block' -and $o.Direction -eq 'Inbound' -and
        $o.Protocol -eq 'TCP' -and $o.InterfaceAlias -eq 'Dante70' -and
        ($o.RemoteAddress -join ',') -eq '0.0.0.0-192.0.2.49,192.0.2.51-255.255.255.255,::-ffff:ffff:ffff:ffff:ffff:ffff:ffff:ffff' -and
        ($o.LocalPort -join ',') -eq 'Any' -and
        ($p.RemoteAddress -join ',') -eq '192.0.2.50' -and ($p.LocalPort -join ',') -eq '1-21,23-65535' -and
        $p.InterfaceAlias -eq 'Dante70' -and $p.Action -eq 'Block' -and
        $text -match "emergency: created rule '$others' \(disabled\)" -and $text -match "emergency: enabled rule '$ports'" -and
        $text -match "emergency path open: disabled 'Dante70: block TCP Inbound'; TCP 22 on 'Dante70' from 192.0.2.50 only" -and
        $text -match 'repair done' -and (@(Changes) -contains 'Restart-NetAdapter LAN')
    }
    Check 'emergency: an emergency rule that does not enable leaves the dstack155 rule on, exit 1' {
        Reset-Fake; $script:Fake.PingOk = $false; $script:Fake.StuckRule = $ports
        $d = NewRun 'emstuck' $emerg '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and (RuleOn '{dante-tcp-in}') -and (IndexOf 'Disable-NetFirewallRule {dante-tcp-in}') -lt 0 -and
        (LogOf $d) -match "ERROR emergency path: rule '$ports' reads disabled after enabling it; 'Dante70: block TCP Inbound' left enabled"
    }
    Check 'emergency: a failed rule creation leaves the dstack155 rule on, exit 1' {
        Reset-Fake; $script:Fake.PingOk = $false; $script:Fake.FailStep = 'New-NetFirewallRule'
        $d = NewRun 'emnew' $emerg '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and (RuleOn '{dante-tcp-in}') -and (IndexOf 'Disable-NetFirewallRule {dante-tcp-in}') -lt 0
    }
    Check 'emergency: a dstack155 rule that does not disable is an error, exit 1' {
        Reset-Fake; $script:Fake.PingOk = $false; $script:Fake.StuckOff = '{dante-tcp-in}'
        $d = NewRun 'emstuckoff' $emerg '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and (RuleOn '{dante-tcp-in}') -and
        (LogOf $d) -match "ERROR emergency path: 'Dante70: block TCP Inbound' reads enabled after disabling it"
    }
    Check 'emergency: a dstack155 rule disabled without the emergency rules is enabled first, then the path opens in order' {
        Reset-Fake; $script:Fake.PingOk = $false; $script:Fake.Rules['{dante-tcp-in}'].Enabled = 'False'
        $d = NewRun 'emhalf' $emerg '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $on = IndexOf 'Enable-NetFirewallRule {dante-tcp-in}'; $new = IndexOf "New-NetFirewallRule $others"
        $off = IndexOf 'Disable-NetFirewallRule {dante-tcp-in}'; $b = IndexOf "Enable-NetFirewallRule $ports"
        $rc -eq 0 -and $on -ge 0 -and $new -gt $on -and $off -gt $b -and -not (RuleOn '{dante-tcp-in}') -and
        (LogOf $d) -match "emergency: enabled 'Dante70: block TCP Inbound' first"
    }
    Check 'emergency: a changed port re-scopes the rules while the dstack155 rule is still on' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'emrescope' $emerg '{"failures":2}'
        $null = Invoke-Check -Dir $d
        $script:Fake.PingOk = $true; $null = Invoke-Check -Dir $d
        Set-Content -Path (Join-Path $d 'lan-selfheal.json') -Value $emerg.Replace('"emergencySource"', '"emergencyPort":2222,"emergencySource"')
        Set-Content -Path (Join-Path $d 'lan-selfheal.state.json') -Value '{"failures":2}'
        $script:Fake.PingOk = $false; $script:Fake.Calls.Clear()
        $rc = Invoke-Check -Dir $d
        $set = IndexOf "Set-NetFirewallRule $ports"; $off = IndexOf 'Disable-NetFirewallRule {dante-tcp-in}'
        $rc -eq 0 -and $set -ge 0 -and $off -gt $set -and
        ($script:Fake.Rules[$ports].LocalPort -join ',') -eq '1-2221,2223-65535' -and (RuleOn $ports)
    }
    Check 'emergency: keys removed from the config while open, the LAN up closes it from the state file' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'emgone' $emerg '{"failures":2}'
        $null = Invoke-Check -Dir $d
        Set-Content -Path (Join-Path $d 'lan-selfheal.json') -Value $good
        $script:Fake.PingOk = $true; $script:Fake.Calls.Clear()
        $rc = Invoke-Check -Dir $d
        $on = IndexOf 'Enable-NetFirewallRule {dante-tcp-in}'; $a = IndexOf "Disable-NetFirewallRule $others"
        $rc -eq 0 -and $on -ge 0 -and $a -gt $on -and (RuleOn '{dante-tcp-in}') -and -not (RuleOn $others) -and
        -not (RuleOn $ports) -and -not ((StateOf $d).PSObject.Properties.Name -contains 'emergencyRule')
    }
    Check 'emergency: a danteBlockRule on another protocol or interface is refused' {
        Reset-Fake; $script:Fake.PingOk = $false; $script:Fake.Rules['{dante-tcp-in}'].Protocol = 'Any'
        $d = NewRun 'emproto' $emerg '{"failures":2}'
        $rc1 = Invoke-Check -Dir $d
        $t1 = LogOf $d
        Reset-Fake; $script:Fake.PingOk = $false; $script:Fake.Rules['{dante-tcp-in}'].InterfaceAlias = @('Dante70', 'LAN')
        $d2 = NewRun 'emif' $emerg '{"failures":2}'
        $rc2 = Invoke-Check -Dir $d2
        $rc1 -eq 1 -and $t1 -match "filters protocol 'Any', not TCP" -and $rc2 -eq 1 -and
        (LogOf $d2) -match "is bound to 'Dante70,LAN', not only to 'Dante70'" -and @(FwChanges).Count -eq 0
    }
    Check 'emergency: below the threshold nothing is opened' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'embelow' $emerg
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and @(FwCalls).Count -eq 0 -and (RuleOn '{dante-tcp-in}')
    }
    Check 'emergency: a second open finds it open and logs so' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'emtwice' $emerg '{"failures":2}'
        $null = Invoke-Check -Dir $d
        Set-Content -Path (Join-Path $d 'lan-selfheal.state.json') -Value '{"failures":2}'
        $script:Fake.Calls.Clear()
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and -not (RuleOn '{dante-tcp-in}') -and (RuleOn $others) -and (RuleOn $ports) -and
        @(FwChanges).Count -eq 0 -and (LogOf $d) -match 'emergency path open already' -and
        (StateOf $d).emergencyRule -eq '{dante-tcp-in}'
    }
    Check 'emergency: the close order is the dstack155 rule on first, then the emergency rules off' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'emclose' $emerg '{"failures":2}'
        $null = Invoke-Check -Dir $d
        $script:Fake.Calls.Clear(); $script:Fake.PingOk = $true
        $rc = Invoke-Check -Dir $d
        $on = IndexOf 'Enable-NetFirewallRule {dante-tcp-in}'
        $a = IndexOf "Disable-NetFirewallRule $others"; $b = IndexOf "Disable-NetFirewallRule $ports"
        $text = LogOf $d
        $rc -eq 0 -and $on -ge 0 -and $a -gt $on -and $b -gt $on -and
        (RuleOn '{dante-tcp-in}') -and -not (RuleOn $others) -and -not (RuleOn $ports) -and
        $text -match "LAN up: enabled 'Dante70: block TCP Inbound' again" -and
        $text -match "LAN up: disabled emergency rule '$others'" -and $text -match "LAN up: disabled emergency rule '$ports'"
    }
    Check 'emergency: a dstack155 rule that does not enable keeps the emergency rules on, exit 1' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'emclosefail' $emerg '{"failures":2}'
        $null = Invoke-Check -Dir $d
        $script:Fake.PingOk = $true; $script:Fake.StuckRule = '{dante-tcp-in}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and (RuleOn $others) -and (RuleOn $ports) -and -not (RuleOn '{dante-tcp-in}') -and
        (LogOf $d) -match 'ERROR emergency path check: .*emergency rules left as they are'
    }
    Check 'emergency: never while up, a disabled dstack155 rule is enabled again and logged' {
        Reset-Fake; $script:Fake.Rules['{dante-tcp-in}'].Enabled = 'False'
        $d = NewRun 'emup1' $emerg
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and (RuleOn '{dante-tcp-in}') -and
        (@(FwChanges) -join ';') -eq 'Enable-NetFirewallRule {dante-tcp-in}' -and
        (LogOf $d) -match "LAN up: enabled 'Dante70: block TCP Inbound' again \(it was disabled\)"
    }
    Check 'emergency: never while up, an enabled emergency rule is disabled and logged' {
        Reset-Fake
        $script:Fake.Rules[$ports] = [pscustomobject]@{ Name = $ports; DisplayName = 'x'; Enabled = 'True'; Action = 'Block'; Direction = 'Inbound' }
        $d = NewRun 'emup2' $emerg
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and -not (RuleOn $ports) -and (RuleOn '{dante-tcp-in}') -and
        (@(FwChanges) -join ';') -eq "Disable-NetFirewallRule $ports" -and
        (LogOf $d) -match "LAN up: disabled emergency rule '$ports'"
    }
    Check 'emergency: the normal state while up changes nothing and logs only "gateway ok"' {
        Reset-Fake
        $d = NewRun 'emok' $emerg
        $rc = Invoke-Check -Dir $d
        $rc -eq 0 -and @(FwChanges).Count -eq 0 -and @(Get-Content (Join-Path $d 'lan-selfheal.log')).Count -eq 1
    }
    Check 'emergency: without emergencySource no firewall rule is read or changed, down or up' {
        Reset-Fake; $script:Fake.PingOk = $false; $script:Fake.Rules['{dante-tcp-in}'].Enabled = 'False'
        $d = NewRun 'emoff' $good '{"failures":2}'
        $rc1 = Invoke-Check -Dir $d
        $script:Fake.PingOk = $true
        $rc2 = Invoke-Check -Dir $d
        $rc1 -eq 0 -and $rc2 -eq 0 -and @(FwCalls).Count -eq 0 -and $script:Fake.Rules.Count -eq 1 -and
        -not (RuleOn '{dante-tcp-in}') -and (LogOf $d) -notmatch 'emergency'
    }
    Check 'emergency: a config with emergencySource but no danteBlockRule is refused and logged' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'emnorule' $good.Replace('"lanVlanId":0', '"lanVlanId":0,"emergencySource":"192.0.2.50"') '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and (LogOf $d) -match "ERROR config: 'danteBlockRule' is missing or empty" -and
        @(FwCalls).Count -eq 0 -and @(Changes).Count -eq 0
    }
    Check 'emergency: a danteBlockRule that names no rule is refused and logged, nothing opened, the LAN still repaired' {
        Reset-Fake; $script:Fake.PingOk = $false
        $d = NewRun 'emnoname' $emerg.Replace('Dante70: block TCP Inbound', 'No such rule') '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and @(FwChanges).Count -eq 0 -and (RuleOn '{dante-tcp-in}') -and
        (LogOf $d) -match "ERROR emergency path: refused: 0 firewall rules named 'No such rule' \(need exactly 1\); not opened" -and
        (@(Changes) -contains 'Restart-NetAdapter LAN')
    }
    Check 'emergency: a danteBlockRule that is not an inbound Block rule is refused' {
        Reset-Fake; $script:Fake.PingOk = $false; $script:Fake.Rules['{dante-tcp-in}'].Action = 'Allow'
        $d = NewRun 'emallow' $emerg '{"failures":2}'
        $rc = Invoke-Check -Dir $d
        $rc -eq 1 -and @(FwChanges).Count -eq 0 -and (LogOf $d) -match 'not an inbound Block rule'
    }
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force
}

Write-Host "$($script:passed) passed, $($script:failed) failed"
if ($script:failed -gt 0) { exit 1 }
exit 0

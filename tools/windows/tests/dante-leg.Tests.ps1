# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Tests of tools/windows/dante-leg.ps1 (dstack #264), run by CI's Windows job
# under Windows PowerShell 5.1:
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File tools\windows\tests\dante-leg.Tests.ps1
# A plain assert script, not Pester (see lan-selfheal.Tests.ps1). Exits 1 if
# any check fails.
#
# Three parts: the complement arithmetic (pure); install, reinstall, check and
# uninstall against fake firewall cmdlets (functions of the same name win over
# the cmdlets); and the group created for real on the runner's adapter, but
# DISABLED and under a test group name, read back with the real cmdlets (the
# shapes the firewall gives back), then removed. The real part needs an
# elevated shell, which the CI runner is; elsewhere it fails, never skips.
# The port sets themselves are checked against the code by
# crates/dstack/tests/dante_leg.rs.

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\dante-leg.ps1')

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
function Same {
    param([object[]]$Got, [object[]]$Want)
    (@($Got) -join ';') -eq (@($Want) -join ';')
}

# ------------------------------------------------------------ the arithmetic
Check 'the TCP exception leaves ssh and the MCP open, as the dstack155 rule did' {
    Same (Get-PortComplement -Ports @('22', '8092')) @('1-21', '23-8091', '8093-65535')
}
Check 'the Dante UDP ports complement covers every other port' {
    Same (Get-PortComplement -Ports $script:DanteUdpPorts) @(
        '1-318', '321-4320', '4322-4439', '4487-5352', '5354-8699', '8709-8799', '8832-14335',
        '14592-29799', '31848-65535')
}
Check 'overlapping and unsorted ranges merge, and the ends 1 and 65535 are kept' {
    (Same (Get-PortComplement -Ports @('10-20', '1', '15-30', '65535')) @('2-9', '31-65534')) -and
    (Same (Get-PortComplement -Ports @('2-65534')) @('1', '65535'))
}
Check 'adjacent ranges leave no gap between them' {
    Same (Get-PortComplement -Ports @('100-199', '200-300')) @('1-99', '301-65535')
}
Check 'a bad port or range is refused' {
    (Throws { Get-PortComplement -Ports @('0') } '*not a port*') -and
    (Throws { Get-PortComplement -Ports @('65536') } '*not a port*') -and
    (Throws { Get-PortComplement -Ports @('20-10') } '*ends before*') -and
    (Throws { Get-PortComplement -Ports @('1-2-3') } '*not a range*')
}
Check 'the Dante groups complement is every other IPv4 address' {
    Same (Get-AddressComplement -Ranges $script:DanteGroups) @(
        '0.0.0.0-223.255.255.255', '224.0.1.0-224.0.1.128', '224.0.1.133-238.255.255.255',
        '240.0.0.0-255.255.255.255')
}
Check 'IPv4 numbers convert both ways at the ends' {
    ((ConvertTo-IPv4Number '255.255.255.255') -eq 4294967295) -and
    ((ConvertFrom-IPv4Number 4294967295) -eq '255.255.255.255') -and
    ((ConvertFrom-IPv4Number (ConvertTo-IPv4Number '224.0.1.129')) -eq '224.0.1.129')
}
Check 'the group is ten Block rules: TCP in and out, UDP in twice, the mDNS hosts both ways' {
    $rules = Get-DanteLegRules
    $names = @($rules | ForEach-Object { $_.DisplayName })
    $rules.Count -eq 10 -and @($names | Select-Object -Unique).Count -eq 10 -and
    @($rules | Where-Object { $_.Contains('Program') }).Count -eq 6 -and
    @($rules | Where-Object { $_.Protocol -eq 'UDP' -and $_.Direction -eq 'Inbound' -and $_.Contains('LocalAddress') }).Count -eq 2
}
Check 'UDP in: every port but Dante''s and the ephemeral ones, from anywhere' {
    $r = @(Get-DanteLegRules | Where-Object { $_.DisplayName -like '*: UDP in' })[0]
    (Same $r.LocalPort @('1-318', '321-4320', '4322-4439', '4487-5352', '5354-8699', '8709-8799',
            '8832-14335', '14592-29799', '31848-49151')) -and -not $r.Contains('RemotePort')
}
Check 'UDP in on the ephemeral ports: from anything but remote port 5353' {
    $r = @(Get-DanteLegRules | Where-Object { $_.DisplayName -like '*ephemeral ports' })[0]
    (Same $r.LocalPort @('49152-65535')) -and (Same $r.RemotePort @('1-5352', '5354-65535'))
}

# --------------------------------------------------- the fake firewall
function Reset-Fake {
    $script:Store = New-Object System.Collections.ArrayList
    $script:Adapters = @('Dante70', 'LAN')
    $script:ProfilesOff = @()
    $script:FailAt = 0
    $script:Made = 0
}
function Get-NetAdapter {
    $script:Adapters | ForEach-Object { [pscustomobject]@{ Name = $_ } }
}
function Get-NetFirewallProfile {
    param([string]$PolicyStore)
    if ($PolicyStore -ne 'ActiveStore') { throw "the profiles read from '$PolicyStore', not the active store" }
    'Domain', 'Private', 'Public' | ForEach-Object {
        [pscustomobject]@{ Name = $_; Enabled = if ($script:ProfilesOff -contains $_) { 'False' } else { 'True' } }
    }
}
function New-NetFirewallRule {
    param($DisplayName, $Direction, $Protocol, $LocalPort, $RemotePort, $LocalAddress, $Program,
        $Group, $Action, $Enabled, $Profile, $InterfaceAlias)
    if ($script:FailAt -gt 0 -and $script:Made + 1 -eq $script:FailAt) { throw 'refused by the fake' }
    $script:Made++
    $or = { param($v) if ($null -eq $v) { 'Any' } else { $v } }
    $rule = [pscustomobject]@{
        Name = [guid]::NewGuid().ToString(); DisplayName = $DisplayName; Group = $Group
        Direction = $Direction; Action = $Action; Enabled = "$Enabled"; Protocol = $Protocol
        LocalPort = (& $or $LocalPort); RemotePort = (& $or $RemotePort)
        LocalAddress = (& $or $LocalAddress); Program = (& $or $Program); InterfaceAlias = @($InterfaceAlias)
    }
    [void]$script:Store.Add($rule)
    $rule
}
function Get-NetFirewallRule {
    param([string]$Group, $ErrorAction)
    @($script:Store | Where-Object { $_.Group -eq $Group })
}
function Remove-NetFirewallRule {
    param([Parameter(ValueFromPipeline)]$InputObject)
    process { $script:Store.Remove($InputObject) }
}
function Get-NetFirewallPortFilter {
    param([Parameter(ValueFromPipeline)]$InputObject)
    process { [pscustomobject]@{ Protocol = $InputObject.Protocol; LocalPort = $InputObject.LocalPort; RemotePort = $InputObject.RemotePort } }
}
function Get-NetFirewallAddressFilter {
    param([Parameter(ValueFromPipeline)]$InputObject)
    process { [pscustomobject]@{ LocalAddress = $InputObject.LocalAddress } }
}
function Get-NetFirewallApplicationFilter {
    param([Parameter(ValueFromPipeline)]$InputObject)
    process { [pscustomobject]@{ Program = $InputObject.Program } }
}
function Get-NetFirewallInterfaceFilter {
    param([Parameter(ValueFromPipeline)]$InputObject)
    process { [pscustomobject]@{ InterfaceAlias = $InputObject.InterfaceAlias } }
}
function Old-Rule {
    param([string]$Group, [string]$Name)
    [void]$script:Store.Add([pscustomobject]@{
            Name = [guid]::NewGuid().ToString(); DisplayName = $Name; Group = $Group; Direction = 'Inbound'; Action = 'Block'; Enabled = 'True'
            Protocol = 'TCP'; LocalPort = 'Any'; RemotePort = 'Any'; LocalAddress = 'Any'; Program = 'Any'
            InterfaceAlias = @('Dante70')
        })
}
function Last { param([object[]]$Out) [int](@($Out) | Select-Object -Last 1) }

Reset-Fake
Check 'install creates the ten rules on the Dante adapter and reads them back' {
    $out = Install-DanteLeg -Interface 'Dante70'
    (Last $out) -eq 0 -and $script:Store.Count -eq 10 -and
    @($script:Store | Where-Object { (@($_.InterfaceAlias) -join ',') -ne 'Dante70' -or $_.Action -ne 'Block' }).Count -eq 0
}
Check 'a second install replaces the group: still ten rules, check passes' {
    $out = Install-DanteLeg -Interface 'Dante70'
    (Last $out) -eq 0 -and $script:Store.Count -eq 10 -and (Last (Test-DanteLeg -Interface 'Dante70')) -eq 0
}
Check '-RemoveGroup removes the per-box group and leaves other groups' {
    Old-Rule -Group 'dstack155' -Name 'old tcp in'
    Old-Rule -Group 'dstack155' -Name 'old obs'
    Old-Rule -Group 'someone else' -Name 'theirs'
    $out = Install-DanteLeg -Interface 'Dante70' -Remove @('dstack155')
    (Last $out) -eq 0 -and @($script:Store | Where-Object { $_.Group -eq 'dstack155' }).Count -eq 0 -and
    @($script:Store | Where-Object { $_.Group -eq 'someone else' }).Count -eq 1 -and
    (@($out) -join "`n") -match "removed group 'dstack155' \(2 rules\)"
}
Check 'check fails on a changed port list, naming the rule' {
    $r = $script:Store | Where-Object { $_.DisplayName -eq "$($script:Group): UDP in" }
    $r.LocalPort = @('1-318')
    $out = Test-DanteLeg -Interface 'Dante70'
    (Last $out) -eq 1 -and (@($out) -join "`n") -match 'UDP in'': reads'
}
Check 'check fails on a disabled rule, an extra rule and another adapter' {
    [void](Install-DanteLeg -Interface 'Dante70')
    ($script:Store | Where-Object { $_.DisplayName -eq "$($script:Group): TCP in" }).Enabled = 'False'
    $disabled = Last (Test-DanteLeg -Interface 'Dante70')
    [void](Install-DanteLeg -Interface 'Dante70')
    Old-Rule -Group $script:Group -Name 'stray'
    $extra = Last (Test-DanteLeg -Interface 'Dante70')
    [void](Install-DanteLeg -Interface 'Dante70')
    $other = Last (Test-DanteLeg -Interface 'LAN')
    $disabled -eq 1 -and $extra -eq 1 -and $other -eq 1
}
Check 'a firewall profile that is off fails the install and the check, the rules stay' {
    $script:ProfilesOff = @('Public')
    $out = Install-DanteLeg -Interface 'Dante70'
    $check = Test-DanteLeg -Interface 'Dante70'
    $script:ProfilesOff = @()
    (Last $out) -eq 1 -and (Last $check) -eq 1 -and @(Get-NetFirewallRule -Group $script:Group).Count -eq 10 -and
    (@($out) -join "`n") -match "profile 'Public' is off"
}
Check 'no adapter, one not there, or a wildcard name is refused before anything changes' {
    $before = $script:Store.Count
    (Throws { Install-DanteLeg -Interface '' } '*-InterfaceAlias <the Dante adapter> is required*') -and
    (Throws { Test-DanteLeg -Interface '' } '*-InterfaceAlias <the Dante adapter> is required*') -and
    (Throws { Install-DanteLeg -Interface 'Nope' } "*no network adapter named 'Nope'*") -and
    (Throws { Install-DanteLeg -Interface 'Dante*' } '*wildcard*') -and
    (Throws { Install-DanteLeg -Interface 'Dante[7]0' } '*wildcard*') -and
    $script:Store.Count -eq $before
}
Check 'a rule refused part way leaves the earlier group and the per-box group as they were' {
    Reset-Fake
    [void](Install-DanteLeg -Interface 'Dante70')
    Old-Rule -Group 'dstack155' -Name 'old tcp in'
    $names = @($script:Store | ForEach-Object { $_.Name }) -join ','
    $script:Made = 0; $script:FailAt = 4
    $threw = Throws { Install-DanteLeg -Interface 'Dante70' -Remove @('dstack155') } '*not created*rules are as before*'
    $script:FailAt = 0
    $threw -and (@($script:Store | ForEach-Object { $_.Name }) -join ',') -eq $names
}
Check 'uninstall removes only the group' {
    Old-Rule -Group 'someone else' -Name 'theirs 2'
    $n = @($script:Store | Where-Object { $_.Group -ne $script:Group }).Count
    $out = Uninstall-DanteLeg
    (Last $out) -eq 0 -and $script:Store.Count -eq $n
}

# ------------------------------------------- the real firewall, disabled
foreach ($f in 'Get-NetAdapter', 'Get-NetFirewallProfile', 'New-NetFirewallRule', 'Get-NetFirewallRule',
    'Remove-NetFirewallRule', 'Get-NetFirewallPortFilter', 'Get-NetFirewallAddressFilter',
    'Get-NetFirewallApplicationFilter', 'Get-NetFirewallInterfaceFilter') {
    Remove-Item -Path "Function:\$f"
}
$script:Group = "dstack-test-dante-leg-$PID"
try {
    Check 'the real firewall reads the group back as created (disabled, on the runner adapter)' {
        # An alias the firewall binds: a connected IPv4 interface. The first
        # physical adapter that is Up can be one the IP stack (and so the
        # firewall) does not know on some runner images ("The specified
        # interface was not found on the system", main CI of v0.6.117).
        # The tool also wants a visible adapter of that name (Assert-Adapter).
        $up = @(Get-NetAdapter | Where-Object { $_.Status -eq 'Up' } | ForEach-Object { $_.Name })
        $found = @(Get-NetIPInterface -AddressFamily IPv4 -ConnectionState Connected |
            Where-Object { $_.InterfaceAlias -notlike 'Loopback*' -and $up -contains $_.InterfaceAlias })
        if ($found.Count -eq 0) { throw 'no connected IPv4 interface on the runner' }
        $alias = $found[0].InterfaceAlias
        $out = Install-DanteLeg -Interface $alias -Enabled 'False'
        $problems = @(Get-GroupProblems -Interface $alias -Enabled 'False')
        foreach ($line in @($out) + $problems) { Write-Host "     $line" }
        $problems.Count -eq 0 -and @(Get-NetFirewallRule -Group $script:Group).Count -eq 10 -and
        @(Get-NetFirewallRule -Group $script:Group | Where-Object { "$($_.Enabled)" -ne 'False' }).Count -eq 0
    }
} finally {
    Get-NetFirewallRule -Group $script:Group -ErrorAction SilentlyContinue | Remove-NetFirewallRule
}
Check 'the test group is gone from the runner' {
    @(Get-NetFirewallRule -Group $script:Group -ErrorAction SilentlyContinue).Count -eq 0
}

Write-Host "$($script:passed) passed, $($script:failed) failed"
if ($script:failed -gt 0) { exit 1 }
exit 0

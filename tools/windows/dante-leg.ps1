# SPDX-License-Identifier: MIT OR Apache-2.0
#
# The generic Dante-leg filter for a Windows PC (dstack #264,
# docs/two-networks.md): one firewall rule group, "dstack Dante leg", bound to
# the PC's Dante adapter only, identical on every PC and never edited per
# device or per direction. Every device of ours binds fixed ports from its slot
# inside the dstack flow range below, so the group names no device.
#
# The firewall stays on with default Allow in and out on every profile. An
# explicit Block rule beats every Allow rule on Windows, so the group is Block
# rules for everything that is not Dante on the Dante adapter (the complement
# of the sets below), never Allow rules:
#   - TCP in, except local 22 (ssh) and 8092 (the remoteos MCP) (owner,
#     13.11.2026, #239); TCP out, except to remote 22 and 8092 (an admin host
#     reaching another PC). A reply on an accepted connection is not filtered:
#     the firewall is stateful.
#   - UDP in, except to Dante's ports (Audinate's published ones and the
#     dstack flow range) and to Dante's multicast groups; on the OS's
#     ephemeral ports (49152-65535) only from remote port 5353 (the unicast
#     mDNS answers to a query sent to the group).
#   - UDP in and out of Windows' own services' host and Bonjour's mDNS host
#     (svchost.exe: mDNS, LLMNR, SSDP, WS-Discovery, the DHCP client;
#     mDNSResponder.exe), so no non-Dante service announces or answers on the
#     Dante leg. The Dante leg's address is static (docs/two-networks.md).
# IGMP and ICMP are not filtered (no rule names them).
#
# The host's own mDNS out is capped (dstack #288): Windows Firewall cannot
# limit a rate, so a QoS policy, "dstack Dante leg mDNS cap", throttles UDP to
# 224.0.0.251:5353 to 200 kbit/s (about 50 datagrams/s of 500 bytes; normal
# mDNS out is 5/s at most), so no program on the PC floods the Dante VLAN even
# with no switch cap. A QoS policy has no adapter condition: the cap holds on
# every adapter (only the Dante leg carries much mDNS on our PCs). It acts
# only where the QoS Packet Scheduler (ms_pacer) is bound, which the check
# reads on the Dante adapter.
#
# Modes (from an elevated Windows PowerShell 5.1):
#   dante-leg.ps1 -InterfaceAlias <Dante adapter> [-RemoveGroup dstack155]
#       install: the group and the mDNS cap are removed and created again
#       (idempotent), every rule and the cap read back; -RemoveGroup also
#       removes a PC's earlier per-box group
#   dante-leg.ps1 -Mode check -InterfaceAlias <Dante adapter>
#       exit 0 when the group and the cap on the PC are exactly this script's,
#       else 1
#   dante-leg.ps1 -Mode uninstall
#       removes the group and the cap
# The script never changes the firewall profiles; a profile that is off is
# reported (exit 1): the group does nothing there.
#
# The sets are read by crates/dstack/tests/dante_leg.rs, which checks them
# against the ports the code binds: change both together.

[CmdletBinding()]
param(
    [ValidateSet('install', 'check', 'uninstall')]
    [string]$Mode = 'install',
    [string]$InterfaceAlias = '',
    [string[]]$RemoveGroup = @()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$script:Group = 'dstack Dante leg'
# The QoS policy capping the host's own mDNS out (dstack #288).
$script:MdnsCap = 'dstack Dante leg mDNS cap'
$script:MdnsGroup = '224.0.0.251'
$script:MdnsPort = 5353
# 50 datagrams/s of about 500 bytes.
$script:MdnsCapBitsPerSecond = 200000

# Dante's ports and the dstack flow range: PTP 319/320, multicast audio 4321,
# every slot's ARC 4440-4471 and flow control 4455-4486 (Audinate's 4440,
# 4444, 4455 inside), mDNS 5353, the info ports 8700-8708 (8702/8708
# groups), every slot's CMC 8800-8831, Audinate's unicast audio 14336-14591
# (Dante Virtual Soundcard), and the dstack flow range (slot k: RX
# 29800+64k..+31, TX the next 32).
$script:DanteUdpPorts = @('319-320', '4321', '4440-4486', '5353', '8700-8708', '8800-8831', '14336-14591', '29800-31847')
# Dante's multicast groups: link-local, PTP's 224.0.1.129-132, multicast audio.
$script:DanteGroups = @('224.0.0.0-224.0.0.255', '224.0.1.129-224.0.1.132', '239.0.0.0-239.255.255.255')
# Remote access on the Dante address (owner, 13.11.2026, #239).
$script:RemoteTcpPorts = @('22', '8092')
# The OS's ephemeral ports, and the remote port whose UDP may reach them
# unasked: a device's unicast mDNS answer to a query sent to the group.
$script:EphemeralPorts = @('49152-65535')
$script:MdnsSourcePorts = @('5353')
# Non-Dante programs that announce or answer on every adapter.
$script:MdnsPrograms = @(
    '%SystemRoot%\System32\svchost.exe',
    '%ProgramFiles%\Bonjour\mDNSResponder.exe',
    '%ProgramFiles(x86)%\Bonjour\mDNSResponder.exe'
)

# ---------------------------------------------------------------- pure layer

# '<a>-<b>' or '<a>' as two numbers.
function ConvertTo-Span {
    param([Parameter(Mandatory)][string]$Text, [Parameter(Mandatory)][scriptblock]$Parse)
    $parts = $Text -split '-'
    if ($parts.Count -gt 2) { throw "not a range: '$Text'" }
    $first = & $Parse $parts[0]
    $last = if ($parts.Count -eq 2) { & $Parse $parts[1] } else { $first }
    if ($last -lt $first) { throw "a range that ends before it starts: '$Text'" }
    , @($first, $last)
}

# The spans of $Lo..$Hi not in $Taken (sorted, merged), as [first, last] pairs.
function Get-Gaps {
    param([Parameter(Mandatory)][object[]]$Taken, [double]$Lo, [double]$Hi)
    $sorted = @($Taken | Sort-Object { $_[0] })
    $gaps = New-Object System.Collections.ArrayList
    $next = $Lo
    foreach ($span in $sorted) {
        if ($span[0] -gt $next) { [void]$gaps.Add(@($next, ($span[0] - 1))) }
        if ($span[1] + 1 -gt $next) { $next = $span[1] + 1 }
    }
    if ($next -le $Hi) { [void]$gaps.Add(@($next, $Hi)) }
    # Each pair as one object (a bare array would be unrolled into its ends).
    foreach ($gap in $gaps.ToArray()) { , $gap }
}

# The ports 1-65535 not in $Ports (each '<n>' or '<a>-<b>'), as the firewall's
# LocalPort/RemotePort list.
function Get-PortComplement {
    param([Parameter(Mandatory)][string[]]$Ports)
    $parse = { param($t) $n = [int]$t; if ($n -lt 1 -or $n -gt 65535) { throw "not a port: '$t'" }; [double]$n }
    $taken = @($Ports | ForEach-Object { , (ConvertTo-Span -Text $_ -Parse $parse) })
    @(Get-Gaps -Taken $taken -Lo 1 -Hi 65535 | ForEach-Object {
            if ($_[0] -eq $_[1]) { '{0}' -f [int]$_[0] } else { '{0}-{1}' -f [int]$_[0], [int]$_[1] }
        })
}

function ConvertTo-IPv4Number {
    param([Parameter(Mandatory)][string]$Text)
    $ip = [System.Net.IPAddress]::Parse($Text)
    $b = $ip.GetAddressBytes()
    if ($b.Count -ne 4) { throw "not an IPv4 address: '$Text'" }
    [double]$b[0] * 16777216 + [double]$b[1] * 65536 + [double]$b[2] * 256 + [double]$b[3]
}

function ConvertFrom-IPv4Number {
    param([double]$Number)
    $n = [uint64]$Number
    '{0}.{1}.{2}.{3}' -f (($n -shr 24) -band 255), (($n -shr 16) -band 255), (($n -shr 8) -band 255), ($n -band 255)
}

# The IPv4 addresses not in $Ranges (each '<a>-<b>' or one address), as the
# firewall's LocalAddress list.
function Get-AddressComplement {
    param([Parameter(Mandatory)][string[]]$Ranges)
    $parse = { param($t) ConvertTo-IPv4Number -Text $t }
    $taken = @($Ranges | ForEach-Object { , (ConvertTo-Span -Text $_ -Parse $parse) })
    @(Get-Gaps -Taken $taken -Lo 0 -Hi 4294967295 | ForEach-Object {
            '{0}-{1}' -f (ConvertFrom-IPv4Number $_[0]), (ConvertFrom-IPv4Number $_[1])
        })
}

# The group's rules, each a hashtable of New-NetFirewallRule's parameters
# (without the adapter, the group and the action, which every rule shares).
function Get-DanteLegRules {
    $rules = New-Object System.Collections.ArrayList
    [void]$rules.Add([ordered]@{
            DisplayName = "$($script:Group): TCP in"
            Direction   = 'Inbound'; Protocol = 'TCP'
            LocalPort   = Get-PortComplement -Ports $script:RemoteTcpPorts
        })
    [void]$rules.Add([ordered]@{
            DisplayName = "$($script:Group): TCP out"
            Direction   = 'Outbound'; Protocol = 'TCP'
            RemotePort  = Get-PortComplement -Ports $script:RemoteTcpPorts
        })
    # The rules' conditions are ANDed, so two: every port but Dante's and
    # the ephemeral ones from anywhere, the ephemeral ones from anything but
    # remote port 5353.
    [void]$rules.Add([ordered]@{
            DisplayName  = "$($script:Group): UDP in"
            Direction    = 'Inbound'; Protocol = 'UDP'
            LocalPort    = Get-PortComplement -Ports (@($script:DanteUdpPorts) + @($script:EphemeralPorts))
            LocalAddress = Get-AddressComplement -Ranges $script:DanteGroups
        })
    [void]$rules.Add([ordered]@{
            DisplayName  = "$($script:Group): UDP in, ephemeral ports"
            Direction    = 'Inbound'; Protocol = 'UDP'
            LocalPort    = $script:EphemeralPorts
            RemotePort   = Get-PortComplement -Ports $script:MdnsSourcePorts
            LocalAddress = Get-AddressComplement -Ranges $script:DanteGroups
        })
    foreach ($program in $script:MdnsPrograms) {
        $leaf = Split-Path -Leaf $program
        $where = if ($program -like '*(x86)*') { ' (x86)' } else { '' }
        foreach ($dir in 'Inbound', 'Outbound') {
            [void]$rules.Add([ordered]@{
                    DisplayName = "$($script:Group): $leaf$where UDP $($dir.ToLower())"
                    Direction   = $dir; Protocol = 'UDP'
                    Program     = $program
                })
        }
    }
    $rules.ToArray()
}

# The mDNS cap: New-NetQosPolicy's parameters.
function Get-MdnsCapPolicy {
    [ordered]@{
        Name                            = $script:MdnsCap
        IPProtocolMatchCondition        = 'UDP'
        IPDstPrefixMatchCondition       = "$($script:MdnsGroup)/32"
        IPDstPortMatchCondition         = $script:MdnsPort
        ThrottleRateActionBitsPerSecond = $script:MdnsCapBitsPerSecond
        NetworkProfile                  = 'All'
    }
}

# What the cap must read back as (Get-NetQosPolicy's fields), one line.
function Format-QosShape {
    param([string]$Protocol, [string]$DstPrefix, [string]$PortStart, [string]$PortEnd, [string]$Rate, [string]$Profile)
    'proto={0}|dst={1}|ports={2}-{3}|rate={4}|profile={5}' -f $Protocol, $DstPrefix, $PortStart, $PortEnd, $Rate, $Profile
}

function Get-ExpectedQosShape {
    # Get-NetQosPolicy gives a /32 prefix back as the bare address.
    Format-QosShape -Protocol 'UDP' -DstPrefix $script:MdnsGroup -PortStart "$($script:MdnsPort)" `
        -PortEnd "$($script:MdnsPort)" -Rate "$($script:MdnsCapBitsPerSecond)" -Profile 'All'
}

# What a rule on the PC must read back as: its filters, in the form
# Get-NetFirewall*Filter gives them, as one comparable line.
function Format-RuleShape {
    param(
        [string]$Direction, [string]$Protocol, [object]$LocalPort, [object]$RemotePort,
        [object]$LocalAddress, [string]$Program, [string]$Interface
    )
    $list = { param($v) if ($null -eq $v -or "$v" -eq '' -or "$v" -eq 'Any') { 'Any' } else { (@($v) | ForEach-Object { "$_" }) -join ',' } }
    '{0}|{1}|lp={2}|rp={3}|la={4}|prog={5}|if={6}' -f $Direction, $Protocol, (& $list $LocalPort),
    (& $list $RemotePort), (& $list $LocalAddress), (& $list $Program), $Interface
}

function Get-ExpectedShape {
    param([Parameter(Mandatory)]$Rule, [Parameter(Mandatory)][string]$Interface)
    $get = { param($k) if ($Rule.Contains($k)) { $Rule[$k] } else { $null } }
    Format-RuleShape -Direction $Rule.Direction -Protocol $Rule.Protocol -LocalPort (& $get 'LocalPort') `
        -RemotePort (& $get 'RemotePort') -LocalAddress (& $get 'LocalAddress') -Program (& $get 'Program') `
        -Interface $Interface
}

# ------------------------------------------------------------------ the PC

# The rule on the PC as a shape line (Format-RuleShape).
function Get-InstalledShape {
    param([Parameter(Mandatory)]$Installed)
    $port = $Installed | Get-NetFirewallPortFilter
    $addr = $Installed | Get-NetFirewallAddressFilter
    $app = $Installed | Get-NetFirewallApplicationFilter
    $if = $Installed | Get-NetFirewallInterfaceFilter
    Format-RuleShape -Direction "$($Installed.Direction)" -Protocol "$($port.Protocol)" `
        -LocalPort $port.LocalPort -RemotePort $port.RemotePort -LocalAddress $addr.LocalAddress `
        -Program $app.Program -Interface ((@($if.InterfaceAlias) | ForEach-Object { "$_" }) -join ',')
}

# Problems with the group on the PC against Get-DanteLegRules, one line each
# (none: the group is this script's). Every rule must read back enabled
# ($Enabled 'True'; the tests install a disabled copy on a CI runner).
function Get-GroupProblems {
    param([Parameter(Mandatory)][string]$Interface, [string]$Enabled = 'True')
    $problems = New-Object System.Collections.ArrayList
    $installed = @(Get-NetFirewallRule -Group $script:Group -ErrorAction SilentlyContinue)
    $wanted = Get-DanteLegRules
    if ($installed.Count -ne $wanted.Count) {
        [void]$problems.Add("the group holds $($installed.Count) rules, not $($wanted.Count)")
    }
    foreach ($rule in $wanted) {
        $found = @($installed | Where-Object { $_.DisplayName -eq $rule.DisplayName })
        if ($found.Count -ne 1) {
            [void]$problems.Add("'$($rule.DisplayName)': $($found.Count) rules with that name")
            continue
        }
        $r = $found[0]
        if ("$($r.Enabled)" -ne $Enabled -or "$($r.Action)" -ne 'Block') {
            [void]$problems.Add("'$($rule.DisplayName)': enabled $($r.Enabled), action $($r.Action)")
        }
        $want = Get-ExpectedShape -Rule $rule -Interface $Interface
        $have = Get-InstalledShape -Installed $r
        if ($have -ne $want) {
            [void]$problems.Add("'$($rule.DisplayName)': reads '$have', not '$want'")
        }
    }
    $problems.ToArray()
}

# Problems with the mDNS cap on the PC, one line each (none: it is this
# script's).
function Get-MdnsCapProblems {
    $found = @(Get-NetQosPolicy -Name $script:MdnsCap -ErrorAction SilentlyContinue)
    if ($found.Count -ne 1) { return @("the mDNS cap '$($script:MdnsCap)': $($found.Count) QoS policies with that name") }
    $p = $found[0]
    $have = Format-QosShape -Protocol "$($p.IPProtocolMatchCondition)" -DstPrefix "$($p.IPDstPrefixMatchCondition)" `
        -PortStart "$($p.IPDstPortStartMatchCondition)" -PortEnd "$($p.IPDstPortEndMatchCondition)" `
        -Rate "$($p.ThrottleRateAction)" -Profile "$($p.NetworkProfile)"
    $want = Get-ExpectedQosShape
    if ($have -ne $want) { return @("the mDNS cap '$($script:MdnsCap)': reads '$have', not '$want'") }
    @()
}

# The QoS Packet Scheduler off on the Dante adapter: the cap does nothing there.
function Get-PacerProblems {
    param([Parameter(Mandatory)][string]$Interface)
    $b = @(Get-NetAdapterBinding -Name $Interface -ComponentID ms_pacer -ErrorAction SilentlyContinue)
    if ($b.Count -ne 1 -or "$($b[0].Enabled)" -ne 'True') {
        return @("the QoS Packet Scheduler (ms_pacer) is not enabled on '$Interface': the mDNS cap does nothing there")
    }
    @()
}

# Creates the mDNS cap again (an earlier one of that name removed first).
function Install-MdnsCap {
    $old = @(Get-NetQosPolicy -Name $script:MdnsCap -ErrorAction SilentlyContinue)
    if ($old.Count -gt 0) { Remove-NetQosPolicy -Name $script:MdnsCap -Confirm:$false }
    $params = Get-MdnsCapPolicy
    try {
        [void](New-NetQosPolicy @params)
    } catch {
        throw "the mDNS cap '$($script:MdnsCap)' was not created ($($_.Exception.Message))"
    }
    "mDNS cap '$($script:MdnsCap)': UDP to $($script:MdnsGroup):$($script:MdnsPort) at $($script:MdnsCapBitsPerSecond) bit/s"
}

# A firewall profile that is off, one line each: the group does nothing there.
function Get-ProfileProblems {
    @(Get-NetFirewallProfile -PolicyStore ActiveStore | Where-Object { "$($_.Enabled)" -ne 'True' } | ForEach-Object {
            "the firewall profile '$($_.Name)' is off: the group does nothing there"
        })
}

function Assert-Adapter {
    param([string]$Interface)
    if ($Interface -eq '') { throw '-InterfaceAlias <the Dante adapter> is required' }
    # The cmdlets read * ? [ as wildcards: such a name could match several.
    if ($Interface -match '[\*\?\[\]]') { throw "an adapter name with a wildcard character: '$Interface'" }
    $adapter = @(Get-NetAdapter | Where-Object { $_.Name -eq $Interface })
    if ($adapter.Count -ne 1) { throw "no network adapter named '$Interface'" }
}

# The report lines of $Problems and the exit code last: 0 when there are none.
function Write-Outcome {
    param([object[]]$Problems, [Parameter(Mandatory)][string]$Done)
    $all = @($Problems | Where-Object { $null -ne $_ })
    foreach ($p in $all) { "ERROR $p" }
    if ($all.Count -gt 0) { return 1 }
    $Done
    0
}

# Creates the group on $Interface, then removes the rules it held before and
# the groups $Remove, then reads it back. The new rules exist before any old
# one goes, so a failure part way leaves the old filter in place (the rules
# made so far are removed again). Output: report lines, the exit code last.
function Install-DanteLeg {
    # Not Mandatory: an empty name reaches Assert-Adapter, which names the switch.
    param([string]$Interface = '', [string[]]$Remove = @(), [string]$Enabled = 'True')
    Assert-Adapter -Interface $Interface
    $before = @(Get-NetFirewallRule -Group $script:Group -ErrorAction SilentlyContinue)
    $made = New-Object System.Collections.ArrayList
    try {
        foreach ($rule in Get-DanteLegRules) {
            $params = @{}
            foreach ($k in $rule.Keys) { $params[$k] = $rule[$k] }
            $new = New-NetFirewallRule @params -Group $script:Group -Action Block -Enabled $Enabled `
                -Profile Any -InterfaceAlias $Interface
            [void]$made.Add($new)
        }
    } catch {
        foreach ($r in $made) { $r | Remove-NetFirewallRule }
        throw "the group was not created ($($_.Exception.Message)); the PC's rules are as before"
    }
    if ($before.Count -gt 0) {
        $before | Remove-NetFirewallRule
        "replaced the group's $($before.Count) earlier rules"
    }
    foreach ($old in @($Remove)) {
        $rules = @(Get-NetFirewallRule -Group $old -ErrorAction SilentlyContinue)
        if ($rules.Count -gt 0) {
            $rules | Remove-NetFirewallRule
            "removed group '$old' ($($rules.Count) rules)"
        }
    }
    Install-MdnsCap
    $problems = @(Get-GroupProblems -Interface $Interface -Enabled $Enabled) + @(Get-MdnsCapProblems) +
    @(Get-PacerProblems -Interface $Interface) + @(Get-ProfileProblems)
    Write-Outcome -Problems $problems -Done "installed group '$($script:Group)' on '$Interface' ($(@(Get-DanteLegRules).Count) rules) and the mDNS cap, read back"
}

# Whether the group on the PC is exactly the script's: report lines, the
# exit code last.
function Test-DanteLeg {
    param([string]$Interface = '')
    Assert-Adapter -Interface $Interface
    $problems = @(Get-GroupProblems -Interface $Interface) + @(Get-MdnsCapProblems) +
    @(Get-PacerProblems -Interface $Interface) + @(Get-ProfileProblems)
    Write-Outcome -Problems $problems -Done "group '$($script:Group)' on '$Interface' and the mDNS cap: as this script installs them"
}

function Uninstall-DanteLeg {
    $rules = @(Get-NetFirewallRule -Group $script:Group -ErrorAction SilentlyContinue)
    if ($rules.Count -gt 0) { $rules | Remove-NetFirewallRule }
    "removed group '$($script:Group)' ($($rules.Count) rules)"
    $caps = @(Get-NetQosPolicy -Name $script:MdnsCap -ErrorAction SilentlyContinue)
    if ($caps.Count -gt 0) { Remove-NetQosPolicy -Name $script:MdnsCap -Confirm:$false }
    "removed the mDNS cap '$($script:MdnsCap)' ($($caps.Count) policies)"
    0
}

# ---------------------------------------------------------------------- main

# Dot-sourced (the tests): define the functions only.
if ($MyInvocation.InvocationName -ne '.') {
    $out = switch ($Mode) {
        'install' { Install-DanteLeg -Interface $InterfaceAlias -Remove $RemoveGroup }
        'check' { Test-DanteLeg -Interface $InterfaceAlias }
        'uninstall' { Uninstall-DanteLeg }
    }
    # Only the last value is the exit code; the lines before it are the report.
    $lines = @($out)
    $lines | Select-Object -SkipLast 1 | ForEach-Object { Write-Output $_ }
    exit [int]($lines | Select-Object -Last 1)
}

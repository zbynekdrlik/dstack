# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Checks a Windows PC's Realtek NICs against dstack's NIC standard (dstack
# #465), read-only: it never sets a value, installs a driver or restarts an
# adapter.
#
# The standard is docs/nic-standard.json (a path or a URL; the default is the
# file on dstack's main branch, a project pins a tag's copy instead). For
# every Realtek adapter that is up (a physical NIC or a Realtek Virtual
# Adapter) it finds the standard's entry by the adapter's first hardware id,
# reads the driver (DriverFileName, DriverVersion) and every advanced keyword
# (Get-NetAdapterAdvancedProperty -AllProperties: the registry values, the
# hidden ones included), and prints one line per value:
#
#   PASS  Ethernet 4  PendingTransmits  expected=8  actual=8
#   FAIL  Ethernet 3  *EEE  expected=0  actual=1  (songplayer#285)
#
# PASS and FAIL judge a value; INFO prints a value the standard does not
# decide yet; SKIP is a value of another profile or an adapter that is down.
# A Realtek adapter without an entry FAILs. Exit 1 on any FAIL, 2 when the
# standard cannot be read, else 0. The output is plain pipeline text, so a
# caller can capture it (`$lines = & .\nic-standard-check.ps1`).
#
#   powershell -ExecutionPolicy Bypass -File nic-standard-check.ps1 [-Standard <path|url>] [-LatencyProfile normal|low_latency] [-IncludeDown]
#
# The judging mirrors tools/linux/nic-standard-check.py (same file, same
# rules); each is tested on its own (tests/nic-standard-check.Tests.ps1,
# tools/linux/tests/nic_standard_check_test.py).

[CmdletBinding()]
param(
    [string]$Standard = 'https://raw.githubusercontent.com/zbynekdrlik/dstack/main/docs/nic-standard.json',
    [ValidateSet('normal', 'low_latency')]
    [string]$LatencyProfile = 'normal',
    [switch]$IncludeDown
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$script:NicSchema = 'dstack-nic-standard/1'
$script:NicExit = 0

# A property of a parsed JSON object, or $Default when it has none (StrictMode
# throws on a missing property).
function Get-NicProp {
    param($Object, [string]$Name, $Default = $null)
    if ($null -ne $Object -and $null -ne $Object.PSObject.Properties[$Name]) {
        return $Object.PSObject.Properties[$Name].Value
    }
    return $Default
}

function Read-NicStandard {
    param([string]$Source)
    if ($Source -match '^https?://') {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
        $text = (Invoke-WebRequest -UseBasicParsing -Uri $Source -TimeoutSec 20).Content
    } else {
        $text = Get-Content -LiteralPath $Source -Raw -Encoding UTF8
    }
    $std = $text | ConvertFrom-Json
    if ((Get-NicProp $std 'schema') -ne $script:NicSchema) {
        throw "not a $($script:NicSchema) file: $Source"
    }
    if (-not ((Get-NicProp $std 'version') -is [int])) { throw "no integer version: $Source" }
    return $std
}

# The entry whose hardware_id prefix starts the adapter's hardware id
# (case-insensitive), or $null.
function Find-NicEntry {
    param($Std, [string]$HardwareId)
    if (-not $HardwareId) { return $null }
    foreach ($entry in @($Std.adapters)) {
        if ($entry.os -ne 'windows') { continue }
        foreach ($prefix in @(Get-NicProp $entry.match 'hardware_id' @())) {
            if ($HardwareId.ToUpperInvariant().StartsWith(([string]$prefix).ToUpperInvariant())) { return $entry }
        }
    }
    return $null
}

function Test-NicValueHolds {
    param($Spec, [string]$Actual)
    $compare = Get-NicProp $Spec 'compare' 'equals'
    foreach ($want in @($Spec.expected)) {
        $w = [string]$want
        if ($compare -eq 'equals' -and $Actual.Trim() -eq $w) { return $true }
        if ($compare -eq 'contains' -and $Actual.Contains($w)) { return $true }
        if ($compare -eq 'bits') {
            $a = [long]0; $m = [long]0
            if (-not [long]::TryParse($Actual.Trim(), [ref]$a) -or -not [long]::TryParse($w, [ref]$m)) { return $false }
            if (($a -band $m) -eq $m) { return $true }
        }
    }
    return $false
}

function New-NicRow {
    param([string]$Verdict, [string]$Key, [string]$Expected, [string]$Actual, [string]$Note)
    [pscustomobject]@{ Verdict = $Verdict; Key = $Key; Expected = $Expected; Actual = $Actual; Note = $Note }
}

# One row for a value against the facts (a hashtable keyword -> value,
# case-insensitive as the registry is).
function Test-NicValue {
    param($Spec, [hashtable]$Facts, [string]$LatencyProfile = 'normal')
    $key = [string]$Spec.key
    $note = [string]$Spec.evidence
    $status = Get-NicProp $Spec 'status' 'settled'
    $only = Get-NicProp $Spec 'profile' 'normal'
    $present = $Facts.ContainsKey($key)
    $actual = if ($present) { [string]$Facts[$key] } else { '(absent)' }
    $expected = if ($status -eq 'report') { '(no standard yet)' } else { (@($Spec.expected) | ForEach-Object { [string]$_ }) -join '|' }
    if ($only -ne 'normal' -and $only -ne $LatencyProfile) { return New-NicRow 'SKIP' $key $expected $actual "$only profile only" }
    if ($status -eq 'report') { return New-NicRow 'INFO' $key $expected $actual $note }
    if ($status -eq 'open') { $note = 'open: ' + [string](Get-NicProp $Spec 'plan' '') }
    if (-not $present) {
        $verdict = if ((Get-NicProp $Spec 'absent' 'fail') -eq 'pass') { 'PASS' } else { 'FAIL' }
        return New-NicRow $verdict $key $expected $actual $note
    }
    $verdict = if (Test-NicValueHolds $Spec $actual) { 'PASS' } else { 'FAIL' }
    return New-NicRow $verdict $key $expected $actual $note
}

# -1, 0 or 1: dotted versions compared number by number.
function Compare-NicVersion {
    param([string]$A, [string]$B)
    $x = @([regex]::Matches($A, '\d+') | ForEach-Object { [long]$_.Value })
    $y = @([regex]::Matches($B, '\d+') | ForEach-Object { [long]$_.Value })
    for ($i = 0; $i -lt [Math]::Max($x.Count, $y.Count); $i++) {
        $p = if ($i -lt $x.Count) { $x[$i] } else { 0 }
        $q = if ($i -lt $y.Count) { $y[$i] } else { 0 }
        if ($p -lt $q) { return -1 }
        if ($p -gt $q) { return 1 }
    }
    return 0
}

# The driver's rows: a bad driver, an allowed file, the minimum version.
function Test-NicDriver {
    param($Spec, [string]$File, [string]$Version)
    $note = [string]$Spec.evidence
    if ((Get-NicProp $Spec 'status' 'settled') -eq 'open' -and (Get-NicProp $Spec 'plan')) { $note = 'open: ' + $Spec.plan }
    $actual = "$File $Version".Trim()
    foreach ($bad in @(Get-NicProp $Spec 'bad' @())) {
        $bf = Get-NicProp $bad 'file' $File
        $bv = Get-NicProp $bad 'version' $Version
        if ($bf -ieq $File -and $bv -eq $Version) {
            $what = (@((Get-NicProp $bad 'file'), (Get-NicProp $bad 'version')) | Where-Object { $_ }) -join ' '
            New-NicRow 'FAIL' 'driver' "not $what" $actual ([string]$bad.evidence)
            return
        }
    }
    $allowed = @(Get-NicProp $Spec 'files' @())
    if (-not ($allowed | Where-Object { $_ -ieq $File })) {
        New-NicRow 'FAIL' 'driver' ($allowed -join '|') $(if ($File) { $File } else { '(none)' }) $note
        return
    }
    New-NicRow 'PASS' 'driver' ($allowed -join '|') $File $note
    $min = Get-NicProp $Spec 'min_version'
    if ($min) {
        $verdict = if ($Version -and (Compare-NicVersion $Version $min) -ge 0) { 'PASS' } else { 'FAIL' }
        New-NicRow $verdict 'driver:version' ">=$min" $(if ($Version) { $Version } else { '(none)' }) $note
    } else {
        New-NicRow 'INFO' 'driver:version' '(no standard yet)' $Version $note
    }
}

function Format-NicRow {
    param([string]$Name, $Row)
    $line = '{0,-4}  {1}  {2}  expected={3}  actual={4}' -f $Row.Verdict, $Name, $Row.Key, $Row.Expected, $Row.Actual
    if ($Row.Verdict -eq 'PASS') { return $line }
    return "$line  ($($Row.Note))"
}

function Get-NicHardwareId {
    param($Adapter)
    try {
        $ids = @((Get-PnpDeviceProperty -InstanceId $Adapter.PnPDeviceID -KeyName 'DEVPKEY_Device_HardwareIds').Data)
        if ($ids.Count -gt 0) { return [string]$ids[0] }
    } catch {
        Write-Verbose "no hardware id for '$($Adapter.Name)': $($_.Exception.Message)"
    }
    return ''
}

function Get-NicFacts {
    param([string]$Name)
    $facts = @{}
    foreach ($p in @(Get-NetAdapterAdvancedProperty -Name $Name -AllProperties)) {
        if (-not $p.RegistryKeyword) { continue }
        $facts[[string]$p.RegistryKeyword] = (@($p.RegistryValue) | ForEach-Object { [string]$_ }) -join ','
    }
    return $facts
}

function Test-RealtekAdapter {
    param($Adapter, [string]$HardwareId)
    return ([string]$Adapter.InterfaceDescription -match 'Realtek') -or ($HardwareId -match '^(USB\\VID_0BDA&|PCI\\VEN_10EC&|RTK_)')
}

# Writes the report lines and sets $script:NicExit (0, 1 on a FAIL, 2 when
# the standard cannot be read).
function Invoke-NicStandardCheck {
    param([string]$Source, [string]$LatencyProfile = 'normal', [switch]$IncludeDown)
    try {
        $std = Read-NicStandard $Source
    } catch {
        "FAIL  cannot read the standard: $($_.Exception.Message)"
        $script:NicExit = 2
        return
    }
    "nic-standard v$($std.version) ($Source), profile $LatencyProfile"
    $failed = 0; $checked = 0
    foreach ($a in @(Get-NetAdapter)) {
        if ($a.Status -eq 'Not Present' -or $a.Status -eq 'Disabled') { continue }
        $hw = Get-NicHardwareId $a
        if (-not (Test-RealtekAdapter $a $hw)) { continue }
        if ($a.Status -ne 'Up' -and -not $IncludeDown) {
            "SKIP  $($a.Name)  $hw is $($a.Status)"
            continue
        }
        $checked++
        $entry = Find-NicEntry $std $hw
        if ($null -eq $entry) {
            "FAIL  $($a.Name)  '$($a.InterfaceDescription)' ${hw}: no standard for this Realtek adapter on Windows"
            $failed++
            continue
        }
        "--    $($a.Name)  $($entry.chip) ($($entry.id))"
        $facts = Get-NicFacts $a.Name
        $rows = @(Test-NicDriver $entry.driver ([string]$a.DriverFileName) ([string]$a.DriverVersion))
        foreach ($v in @($entry.values)) { $rows += Test-NicValue $v $facts $LatencyProfile }
        foreach ($r in $rows) {
            Format-NicRow $a.Name $r
            if ($r.Verdict -eq 'FAIL') { $failed++ }
        }
    }
    "$checked Realtek NIC(s) checked, $failed FAIL"
    $script:NicExit = if ($failed -gt 0) { 1 } else { 0 }
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-NicStandardCheck -Source $Standard -LatencyProfile $LatencyProfile -IncludeDown:$IncludeDown
    exit $script:NicExit
}

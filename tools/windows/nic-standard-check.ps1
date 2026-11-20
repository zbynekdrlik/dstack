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
# decide yet; SKIP is a value of another profile or an adapter that is down
# or disabled. A Realtek adapter without an entry FAILs, and so does one whose
# values cannot be read. Exit 1 on any FAIL, 2 when the standard cannot be
# read or is malformed, else 0. The output is plain pipeline text, so a caller
# can capture it (`$lines = & .\nic-standard-check.ps1`).
#
#   powershell -ExecutionPolicy Bypass -File nic-standard-check.ps1 [-Standard <path|url>] [-LatencyProfile normal|low_latency] [-IncludeDown] [-Name <adapter>[,<adapter>...]]
#
# The validation and the judging are the same as
# tools/linux/nic-standard-check.py's (same file, same rules, every rule of
# the file's shape case-sensitive); each checker is tested on its own
# (tests/nic-standard-check.Tests.ps1, tools/linux/tests/nic_standard_check_test.py).

[CmdletBinding()]
param(
    [string]$Standard = 'https://raw.githubusercontent.com/zbynekdrlik/dstack/main/docs/nic-standard.json',
    [ValidateSet('normal', 'low_latency')]
    [string]$LatencyProfile = 'normal',
    [switch]$IncludeDown,
    # Check only these adapters (by name), up or not; one that is missing or
    # no Realtek fails.
    [string[]]$Name = @()
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 2.0

$script:NicSchema = 'dstack-nic-standard/1'
$script:NicExit = 0
$script:NicMatchKeys = @{ windows = @('hardware_id'); linux = @('usb', 'pci') }

# A property of a parsed JSON object, or $Default when it has none (StrictMode
# throws on a missing property).
function Get-NicProp {
    param($Object, [string]$Name, $Default = $null)
    if ($null -ne $Object -and $Object -is [psobject] -and $null -ne $Object.PSObject.Properties[$Name]) {
        return $Object.PSObject.Properties[$Name].Value
    }
    return $Default
}

function Test-NicHasProp {
    param($Object, [string]$Name)
    return ($null -ne $Object -and $null -ne $Object.PSObject.Properties[$Name])
}

# A property kept as it is: a list stays a list (`return` would unroll a
# one-item list into its item and an empty one into nothing). Assign it
# (`$x = Get-NicList ...`), never pipe it.
function Get-NicList {
    param($Object, [string]$Name)
    if (-not (Test-NicHasProp $Object $Name)) { return $null }
    $value = $Object.PSObject.Properties[$Name].Value
    if ($value -is [array]) { return , $value }
    return $value
}

# A non-empty list of non-empty strings.
function Test-NicStrings {
    param($Value)
    if ($null -eq $Value -or -not ($Value -is [array])) { return $false }
    if (@($Value).Count -eq 0) { return $false }
    foreach ($x in @($Value)) { if (-not ($x -is [string]) -or $x -eq '') { return $false } }
    return $true
}

# A decimal number of at most 18 digits (ASCII only, no sign or space), so it
# always fits a 64-bit integer.
function Test-NicNumber {
    param($Text)
    return ($Text -is [string] -and $Text -cmatch '\A[0-9]{1,18}\z')
}

# The keys each object may hold: an unknown key is a typo, never ignored
# (compared case-sensitively, as JSON keys are).
$script:NicKeys = @{
    top    = @('schema', 'version', 'updated', 'issue', 'doc', 'scope', 'fields', 'adapters')
    entry  = @('id', 'chip', 'covers', 'os', 'match', 'driver', 'values')
    driver = @('files', 'modules', 'min_version', 'bad', 'status', 'evidence', 'plan', 'firmware')
    bad    = @('file', 'version', 'evidence')
    value  = @('key', 'name', 'expected', 'compare', 'absent', 'status', 'profile', 'evidence', 'plan')
}
# The driver's names per OS: Windows judges the driver file, Linux the module.
$script:NicDriverNames = @{ windows = 'files'; linux = 'modules' }

function Assert-NicOnly {
    param([string]$Where, $Object, [string]$Kind)
    $unknown = @($Object.PSObject.Properties | Where-Object { -not ($script:NicKeys[$Kind] -ccontains $_.Name) } | ForEach-Object { $_.Name })
    if ($unknown.Count -gt 0) { throw "${Where}: unknown key(s) $(($unknown | Sort-Object) -join ', ')" }
}

# A string field: absent unless required, else a non-empty string.
function Assert-NicText {
    param([string]$Where, $Object, [string]$Key, [switch]$Required)
    if (-not (Test-NicHasProp $Object $Key)) {
        if ($Required) { throw "${Where}: no $Key" }
        return
    }
    $v = $Object.PSObject.Properties[$Key].Value
    if (-not ($v -is [string]) -or $v -eq '') { throw "${Where}: $Key must be a non-empty string" }
}

function Test-NicStandardDriver {
    param([string]$Id, [string]$Os, $Driver)
    if ($null -eq $Driver -or -not ($Driver -is [psobject])) { throw "${Id}: no driver" }
    Assert-NicOnly "$Id/driver" $Driver 'driver'
    Assert-NicText "$Id/driver" $Driver 'evidence' -Required
    $namesKey = $script:NicDriverNames[$Os]
    $otherKey = if ($Os -ceq 'windows') { 'modules' } else { 'files' }
    $names = Get-NicList $Driver $namesKey
    if (-not (Test-NicStrings $names) -or (Test-NicHasProp $Driver $otherKey)) {
        throw "${Id}: a $Os driver names its $namesKey (and no $otherKey)"
    }
    foreach ($k in @('min_version', 'plan', 'firmware')) { Assert-NicText "$Id/driver" $Driver $k }
    if ($Os -ceq 'windows' -and (Test-NicHasProp $Driver 'firmware')) { throw "${Id}: Windows has no firmware read; a firmware rule is Linux only" }
    $status = Get-NicProp $Driver 'status' 'settled'
    if (-not ($status -ceq 'settled' -or $status -ceq 'open')) { throw "${Id}: unknown driver status '$status'" }
    if ($status -ceq 'open' -and -not (Test-NicHasProp $Driver 'plan')) { throw "${Id}: an open driver rule needs its plan" }
    if (Test-NicHasProp $Driver 'bad') {
        $bads = Get-NicList $Driver 'bad'
        if ($null -eq $bads -or -not ($bads -is [array])) { throw "${Id}: bad must be a list" }
        foreach ($bad in $bads) {
            if ($null -eq $bad -or -not ($bad -is [psobject]) -or $bad -is [string]) { throw "${Id}: a bad driver is an object" }
            Assert-NicOnly "$Id/bad" $bad 'bad'
            Assert-NicText "$Id/bad" $bad 'evidence' -Required
            Assert-NicText "$Id/bad" $bad 'file'
            Assert-NicText "$Id/bad" $bad 'version'
            if (-not (Test-NicHasProp $bad 'file') -and -not (Test-NicHasProp $bad 'version')) { throw "${Id}: a bad driver names a file or a version" }
        }
    }
}

function Test-NicStandardValue {
    param([string]$Id, $V, [hashtable]$Keys)
    $key = Get-NicProp $V 'key'
    $where = "$Id/$key"
    if (-not ($key -is [string]) -or $key -eq '') { throw "${where}: a value needs a key" }
    Assert-NicOnly $where $V 'value'
    Assert-NicText $where $V 'evidence' -Required
    Assert-NicText $where $V 'name'
    Assert-NicText $where $V 'plan'
    if ($Keys.ContainsKey($key.ToLowerInvariant())) { throw "${where}: the key is repeated" }
    $Keys[$key.ToLowerInvariant()] = $true
    $status = Get-NicProp $V 'status' 'settled'
    if (-not (@('settled', 'open', 'report') -ccontains $status)) { throw "${where}: unknown status '$status'" }
    if (-not (@('equals', 'bits', 'contains') -ccontains (Get-NicProp $V 'compare' 'equals'))) { throw "${where}: unknown compare" }
    if (-not (@('fail', 'pass') -ccontains (Get-NicProp $V 'absent' 'fail'))) { throw "${where}: absent must be fail or pass" }
    if (-not (@('normal', 'low_latency') -ccontains (Get-NicProp $V 'profile' 'normal'))) { throw "${where}: unknown profile" }
    $expected = Get-NicList $V 'expected'
    if ($status -ceq 'report') {
        if ($null -ne $expected) { throw "${where}: a report value has no expected value" }
        return
    }
    if (-not (($expected -is [string] -and $expected -ne '') -or (Test-NicStrings $expected))) {
        throw "${where}: expected must be a string or a list of strings"
    }
    if ((Get-NicProp $V 'compare') -ceq 'bits' -and @(@($expected) | Where-Object { -not (Test-NicNumber $_) }).Count -gt 0) {
        throw "${where}: a bits value is a decimal number"
    }
    if ($status -ceq 'open' -and -not (Test-NicHasProp $V 'plan')) { throw "${where}: an open value needs its plan" }
}

# Throws unless the parsed file is a well-formed standard: a broken file stops
# the run (exit 2) instead of passing a PC by accident. The same rules as the
# Python checker's `validate`.
function Test-NicStandard {
    param($Std)
    if ((Get-NicProp $Std 'schema') -cne $script:NicSchema) { throw "not a $($script:NicSchema) file" }
    Assert-NicOnly 'the file' $Std 'top'
    $version = Get-NicProp $Std 'version'
    if (-not ($version -is [int] -or $version -is [long])) { throw 'no integer version' }
    $adapters = Get-NicList $Std 'adapters'
    if ($null -eq $adapters -or -not ($adapters -is [array])) { throw 'no adapters list' }
    $seen = @{}
    foreach ($entry in $adapters) {
        $id = Get-NicProp $entry 'id'
        if (-not ($id -is [string]) -or $id -eq '' -or $seen.ContainsKey($id)) { throw "adapter id missing or repeated: '$id'" }
        $seen[$id] = $true
        Assert-NicOnly $id $entry 'entry'
        $os = Get-NicProp $entry 'os'
        if (-not ($os -ceq 'windows' -or $os -ceq 'linux')) { throw "${id}: os must be windows or linux" }
        $match = Get-NicProp $entry 'match'
        $props = @()
        if ($null -ne $match -and $match -is [psobject]) { $props = @($match.PSObject.Properties) }
        if ($props.Count -eq 0) { throw "${id}: match needs $($script:NicMatchKeys[$os] -join ' or ') id lists" }
        foreach ($p in $props) {
            if (-not ($script:NicMatchKeys[$os] -ccontains $p.Name) -or -not (Test-NicStrings $p.Value)) {
                throw "${id}: match needs $($script:NicMatchKeys[$os] -join ' or ') id lists"
            }
        }
        Test-NicStandardDriver $id $os (Get-NicProp $entry 'driver')
        $values = Get-NicList $entry 'values'
        if ($null -eq $values -or -not ($values -is [array])) { throw "${id}: no values list" }
        $keys = @{}
        foreach ($v in $values) { Test-NicStandardValue $id $v $keys }
    }
}

function Read-NicStandard {
    param([string]$Source)
    if ($Source -match '^https?://') {
        # Add TLS 1.2 for this process, keeping what the caller allowed.
        [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
        $text = (Invoke-WebRequest -UseBasicParsing -Uri $Source -TimeoutSec 20).Content
    } else {
        $text = Get-Content -LiteralPath $Source -Raw -Encoding UTF8
    }
    $std = $text | ConvertFrom-Json
    Test-NicStandard $std
    return $std
}

# The entry whose hardware_id prefix starts the adapter's hardware id
# (case-insensitive), or $null.
function Find-NicEntry {
    param($Std, [string]$HardwareId)
    if (-not $HardwareId) { return $null }
    foreach ($entry in @($Std.adapters)) {
        if ($entry.os -cne 'windows') { continue }
        foreach ($prefix in @(Get-NicProp $entry.match 'hardware_id' @())) {
            if ($HardwareId.ToUpperInvariant().StartsWith(([string]$prefix).ToUpperInvariant())) { return $entry }
        }
    }
    return $null
}

# Whether $Actual holds the value $Spec asks for: equals and contains ignore
# case, bits takes decimal numbers.
function Test-NicValueHolds {
    param($Spec, [string]$Actual)
    $compare = Get-NicProp $Spec 'compare' 'equals'
    $value = $Actual.Trim()
    foreach ($want in @($Spec.expected)) {
        $w = [string]$want
        if ($compare -ceq 'equals' -and $value -ieq $w) { return $true }
        if ($compare -ceq 'contains' -and $value.IndexOf($w, [StringComparison]::OrdinalIgnoreCase) -ge 0) { return $true }
        if ($compare -ceq 'bits') {
            if (-not (Test-NicNumber $value)) { return $false }
            $m = [long]$w
            if (([long]$value -band $m) -eq $m) { return $true }
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
    $expected = if ($status -ceq 'report') { '(no standard yet)' } else { (@($Spec.expected) | ForEach-Object { [string]$_ }) -join '|' }
    if ($only -cne 'normal' -and $only -cne $LatencyProfile) { return New-NicRow 'SKIP' $key $expected $actual "$only profile only" }
    if ($status -ceq 'report') { return New-NicRow 'INFO' $key $expected $actual $note }
    if ($status -ceq 'open') { $note = 'open: ' + [string]$Spec.plan }
    if (-not $present) {
        $verdict = if ((Get-NicProp $Spec 'absent' 'fail') -ceq 'pass') { 'PASS' } else { 'FAIL' }
        return New-NicRow $verdict $key $expected $actual $note
    }
    $verdict = if (Test-NicValueHolds $Spec $actual) { 'PASS' } else { 'FAIL' }
    return New-NicRow $verdict $key $expected $actual $note
}

# -1, 0 or 1: dotted versions compared number by number, the shorter padded
# with zeros (10.79.20 equals 10.79.20.0).
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
    if ((Get-NicProp $Spec 'status' 'settled') -ceq 'open') { $note = 'open: ' + [string]$Spec.plan }
    $actual = "$File $Version".Trim()
    foreach ($bad in @(Get-NicProp $Spec 'bad' @())) {
        $bf = Get-NicProp $bad 'file' $File
        $bv = Get-NicProp $bad 'version' $Version
        if ($bf -ieq $File -and $bv -ceq $Version) {
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
        New-NicRow 'INFO' 'driver:version' '(no standard yet)' $(if ($Version) { $Version } else { '(none)' }) $note
    }
}

function Format-NicRow {
    param([string]$Name, $Row)
    $line = '{0,-4}  {1}  {2}  expected={3}  actual={4}' -f $Row.Verdict, $Name, $Row.Key, $Row.Expected, $Row.Actual
    if ($Row.Verdict -ceq 'PASS') { return $line }
    return "$line  ($($Row.Note))"
}

# The adapter's first hardware id and, when it could not be read, why.
function Get-NicHardwareId {
    param($Adapter)
    try {
        $ids = @((Get-PnpDeviceProperty -InstanceId $Adapter.PnPDeviceID -KeyName 'DEVPKEY_Device_HardwareIds').Data)
        if ($ids.Count -gt 0 -and $ids[0]) { return [pscustomobject]@{ Id = [string]$ids[0]; Error = '' } }
        return [pscustomobject]@{ Id = ''; Error = 'no hardware id' }
    } catch {
        return [pscustomobject]@{ Id = ''; Error = $_.Exception.Message }
    }
}

function Get-NicFacts {
    param([string]$Name)
    $facts = @{}
    foreach ($p in @(Get-NetAdapterAdvancedProperty -Name $Name -AllProperties)) {
        # A keyword with no registry value is absent (the driver then uses its
        # own default), as a missing fact is on Linux.
        if (-not $p.RegistryKeyword -or $null -eq $p.RegistryValue) { continue }
        $facts[[string]$p.RegistryKeyword] = (@($p.RegistryValue) | ForEach-Object { [string]$_ }) -join ','
    }
    return $facts
}

function Test-RealtekAdapter {
    param($Adapter, [string]$HardwareId)
    return ([string]$Adapter.InterfaceDescription -match 'Realtek') -or ($HardwareId -match '^(USB\\VID_0BDA&|PCI\\VEN_10EC&|RTK_)')
}

# The rows for one adapter (its entry found): its driver and every value, or
# one FAIL when its values cannot be read.
function Get-NicRows {
    param($Adapter, $Entry, [string]$LatencyProfile)
    try {
        $facts = Get-NicFacts $Adapter.Name
    } catch {
        New-NicRow 'FAIL' 'values' '(readable)' '(unreadable)' $_.Exception.Message
        return
    }
    Test-NicDriver $Entry.driver ([string]$Adapter.DriverFileName) ([string]$Adapter.DriverVersion)
    foreach ($v in @($Entry.values)) { Test-NicValue $v $facts $LatencyProfile }
}

# Writes the report lines and sets $script:NicExit (0, 1 on a FAIL, 2 when
# the standard cannot be read or is malformed).
function Invoke-NicStandardCheck {
    param([string]$Source, [string]$LatencyProfile = 'normal', [switch]$IncludeDown, [string[]]$Name = @())
    try {
        $std = Read-NicStandard $Source
    } catch {
        "FAIL  cannot read the standard: $($_.Exception.Message)"
        $script:NicExit = 2
        return
    }
    "nic-standard v$($std.version) ($Source), profile $LatencyProfile"
    $failed = 0; $checked = 0
    $named = @($Name | Where-Object { $_ })
    $found = @{}
    foreach ($a in @(Get-NetAdapter)) {
        if ($named.Count -gt 0 -and -not ($named -contains $a.Name)) { continue }
        if ($a.Status -eq 'Not Present') { continue }
        $hw = Get-NicHardwareId $a
        if (-not (Test-RealtekAdapter $a $hw.Id)) { continue }
        $found[[string]$a.Name] = $true
        if ($a.Status -eq 'Disabled' -or ($a.Status -ne 'Up' -and -not $IncludeDown -and $named.Count -eq 0)) {
            "SKIP  $($a.Name)  $($hw.Id) is $($a.Status)"
            continue
        }
        $checked++
        $entry = Find-NicEntry $std $hw.Id
        if ($null -eq $entry) {
            $why = if ($hw.Error) { " (hardware id unreadable: $($hw.Error))" } else { '' }
            "FAIL  $($a.Name)  '$($a.InterfaceDescription)' $($hw.Id): no standard for this Realtek adapter on Windows$why"
            $failed++
            continue
        }
        "--    $($a.Name)  $($entry.chip) ($($entry.id))"
        foreach ($r in @(Get-NicRows $a $entry $LatencyProfile)) {
            Format-NicRow $a.Name $r
            if ($r.Verdict -ceq 'FAIL') { $failed++ }
        }
    }
    # A named adapter that is missing, not present or no Realtek fails: a
    # gate with a typo in the name must not pass.
    foreach ($n in $named) {
        if (-not $found.ContainsKey($n)) {
            "FAIL  ${n}: no such Realtek adapter"
            $failed++
        }
    }
    "$checked Realtek NIC(s) checked, $failed FAIL"
    $script:NicExit = if ($failed -gt 0) { 1 } else { 0 }
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-NicStandardCheck -Source $Standard -LatencyProfile $LatencyProfile -IncludeDown:$IncludeDown -Name $Name
    exit $script:NicExit
}

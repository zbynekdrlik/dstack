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
# First, whatever the vendor, it checks the standard's vendor-independent
# rule `eee` on EVERY wired physical NIC (hardware, not virtual, 802.3), up,
# down or disabled, with or without an entry and whatever -Name says (the
# owner, 20.11.2026: EEE enabled on any rig device is a serious failure): the
# listed keywords (*EEE, EEE, AdvancedEEE, Green Ethernet, Intel's
# EEELinkAdvertisement; hidden ones too) must be off, any other keyword whose
# display name says Energy Efficient / EEE / Green Ethernet is judged by its
# display value. A FAIL names the NIC, its driver and the keyword:
#
#   FAIL  Ethernet 3  *EEE  expected=0  actual=1 (Enabled)  (driver rtu56cx22x64.sys 1156.22.113.2026; ...)
#
# PASS and FAIL judge a value; INFO prints a value the standard does not
# decide yet; SKIP is a value of another profile or an adapter that is down
# or disabled. A Realtek adapter without an entry FAILs, and so does one whose
# values cannot be read. The negotiated link is judged too: link:speed
# (Get-NetAdapter's LinkSpeed) and link:duplex (FullDuplex: Full or Half)
# against the entry, as Linux judges ethtool's Speed and Duplex, so a NIC that
# came up at 100 Mbps half duplex fails even with *SpeedDuplex on auto. An
# adapter with no link (Disconnected: a cable-less NIC) gets one INFO "no
# link" line and INFO "no link" speed/duplex rows, its driver and other
# values still judged. A
# Wi-Fi adapter (NdisPhysicalMedium 9 or PhysicalMediaType 802.11, the Realtek
# WiFi RTL8852BE too) is out of the wired scan. Exit 1 on any FAIL, 2 when
# the standard cannot be read or is malformed, else 0. The output is plain
# pipeline text, so a caller can capture it (`$lines = & .\nic-standard-check.ps1`).
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

$script:NicSchema = 'dstack-nic-standard/2'
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
    top    = @('schema', 'version', 'updated', 'issue', 'doc', 'scope', 'fields', 'eee', 'adapters')
    eee    = @('name', 'scope', 'windows', 'linux', 'evidence')
    eeewin = @('keywords', 'off', 'display_name', 'display_off', 'display_on')
    eeelin = @('off')
    entry  = @('id', 'chip', 'covers', 'os', 'match', 'driver', 'values')
    driver = @('allowed', 'modules', 'bad', 'status', 'evidence', 'plan', 'firmware')
    allowed = @('file', 'min_version', 'evidence')
    bad    = @('file', 'version', 'evidence')
    value  = @('key', 'name', 'expected', 'compare', 'absent', 'status', 'profile', 'evidence', 'plan')
}
# The driver's names per OS: Windows judges the driver file (each allowed
# file with its own minimum, #465), Linux the module.
$script:NicDriverNames = @{ windows = 'allowed'; linux = 'modules' }

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

# Whether $Allowed is a non-empty list of allowed files: objects, each naming
# its file once (any case), with an optional minimum and evidence. A malformed
# item throws. The same rule as the Python checker's _allowed.
function Test-NicAllowed {
    param([string]$Id, $Allowed)
    if ($null -eq $Allowed -or -not ($Allowed -is [array]) -or @($Allowed).Count -eq 0) { return $false }
    $seen = @{}
    foreach ($item in @($Allowed)) {
        if ($null -eq $item -or -not ($item -is [psobject]) -or $item -is [string]) { return $false }
        Assert-NicOnly "$Id/allowed" $item 'allowed'
        Assert-NicText "$Id/allowed" $item 'file' -Required
        Assert-NicText "$Id/allowed" $item 'min_version'
        Assert-NicText "$Id/allowed" $item 'evidence'
        $key = ([string]$item.file).ToLowerInvariant()
        if ($seen.ContainsKey($key)) { throw "${Id}: the allowed file $($item.file) is named twice" }
        $seen[$key] = $true
    }
    return $true
}

function Test-NicStandardDriver {
    param([string]$Id, [string]$Os, $Driver)
    if ($null -eq $Driver -or -not ($Driver -is [psobject])) { throw "${Id}: no driver" }
    Assert-NicOnly "$Id/driver" $Driver 'driver'
    Assert-NicText "$Id/driver" $Driver 'evidence' -Required
    $namesKey = $script:NicDriverNames[$Os]
    $otherKey = if ($Os -ceq 'windows') { 'modules' } else { 'allowed' }
    $names = Get-NicList $Driver $namesKey
    $shaped = if ($Os -ceq 'windows') { Test-NicAllowed $Id $names } else { Test-NicStrings $names }
    if (-not $shaped -or (Test-NicHasProp $Driver $otherKey)) {
        throw "${Id}: a $Os driver names its $namesKey (and no $otherKey)"
    }
    foreach ($k in @('plan', 'firmware')) { Assert-NicText "$Id/driver" $Driver $k }
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

# The vendor-independent EEE rule: Windows keywords and display patterns,
# Linux passing states (dstack #465, owner 20.11.2026). The same rules as the
# Python checker's `_validate_eee`.
function Test-NicStandardEee {
    param($Rule)
    if ($null -eq $Rule -or -not ($Rule -is [psobject]) -or $Rule -is [array] -or $Rule -is [string]) { throw 'no eee rule' }
    Assert-NicOnly 'eee' $Rule 'eee'
    Assert-NicText 'eee' $Rule 'evidence' -Required
    Assert-NicText 'eee' $Rule 'name'
    Assert-NicText 'eee' $Rule 'scope'
    $win = Get-NicProp $Rule 'windows'
    $lin = Get-NicProp $Rule 'linux'
    foreach ($part in @($win, $lin)) {
        if ($null -eq $part -or -not ($part -is [psobject]) -or $part -is [array] -or $part -is [string]) { throw 'eee: needs its windows and linux parts' }
    }
    Assert-NicOnly 'eee/windows' $win 'eeewin'
    Assert-NicOnly 'eee/linux' $lin 'eeelin'
    foreach ($check in @(@('eee/windows', $win, 'keywords'), @('eee/windows', $win, 'off'), @('eee/linux', $lin, 'off'))) {
        $list = Get-NicList $check[1] $check[2]
        if (-not (Test-NicStrings $list)) { throw "$($check[0]): $($check[2]) must be a list of strings" }
    }
    foreach ($key in @('display_name', 'display_off', 'display_on')) {
        Assert-NicText 'eee/windows' $win $key -Required
        try { [void][regex]::new([string](Get-NicProp $win $key)) } catch { throw "eee/windows: $key is no regular expression: $($_.Exception.Message)" }
    }
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
    Test-NicStandardEee (Get-NicProp $Std 'eee')
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
    if (($script:NicLinkStateKeys -contains $key) -and $Facts.ContainsKey('link:detected') -and [string]$Facts['link:detected'] -ceq 'no') {
        return New-NicRow 'INFO' $key $expected 'no link' $script:NicNoLinkNote
    }
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

# The driver's rows: a bad driver, an allowed file, that file's own minimum
# version.
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
    $files = @(Get-NicProp $Spec 'allowed' @())
    $allowed = @($files | ForEach-Object { [string]$_.file })
    $match = @($files | Where-Object { [string]$_.file -ieq $File })
    if ($match.Count -eq 0) {
        New-NicRow 'FAIL' 'driver' ($allowed -join '|') $(if ($File) { $File } else { '(none)' }) $note
        return
    }
    New-NicRow 'PASS' 'driver' ($allowed -join '|') $File $note
    $min = Get-NicProp $match[0] 'min_version'
    if ($min) {
        $verdict = if ($Version -and (Compare-NicVersion $Version $min) -ge 0) { 'PASS' } else { 'FAIL' }
        New-NicRow $verdict 'driver:version' ">=$min" $(if ($Version) { $Version } else { '(none)' }) $note
    } else {
        New-NicRow 'INFO' 'driver:version' '(no standard yet)' $(if ($Version) { $Version } else { '(none)' }) $note
    }
}

# The EEE rows of a wired NIC from its advanced properties, each with Keyword,
# Value, DisplayName and DisplayValue (a hidden keyword has no display name).
# A listed keyword (any case) passes when its value is off. Another keyword
# whose display name matches is judged by its display value: off passes, on
# fails; with no display value its value decides; anything else (Realtek's
# EEEMaxSupportSpeed, a speed) is INFO. No EEE keyword at all is one INFO row:
# the driver's default holds and Windows shows no state for it. A FAIL's note
# names the driver. The same judge as the Python checker's judge_eee_windows.
function Test-NicEeeWindows {
    param($Rule, $Props, [string]$Driver)
    $win = $Rule.windows
    $keywords = @(@($win.keywords) | ForEach-Object { ([string]$_).ToLowerInvariant() })
    $off = @(@($win.off) | ForEach-Object { [string]$_ })
    $shownDriver = if ($Driver.Trim()) { $Driver.Trim() } else { '(unknown)' }
    $note = "driver $shownDriver; $($Rule.evidence)"
    $rows = @()
    foreach ($p in @($Props)) {
        $keyword = [string]$p.Keyword
        $value = if ($null -eq $p.Value) { '' } else { ([string]$p.Value).Trim() }
        $dname = [string](Get-NicProp $p 'DisplayName' '')
        $dvalue = ([string](Get-NicProp $p 'DisplayValue' '')).Trim()
        $shown = if ($dvalue) { "$value ($dvalue)" } else { $value }
        $isOff = [bool]($off | Where-Object { $_ -ieq $value })
        $listed = $keywords -contains $keyword.ToLowerInvariant()
        if ($listed -and $value -ne '') {
            $verdict = if ($isOff) { 'PASS' } else { 'FAIL' }
            $rows += New-NicRow $verdict $keyword ($off -join '|') $shown $note
        } elseif (($listed -and $dvalue) -or (-not $listed -and $dname -and [regex]::IsMatch($dname, [string]$win.display_name, 'IgnoreCase'))) {
            # A listed keyword with no registry value but a display value (the
            # driver's default shown) is judged as an unlisted one.
            $verdict = 'INFO'
            if ($dvalue -and [regex]::IsMatch($dvalue, [string]$win.display_off, 'IgnoreCase')) { $verdict = 'PASS' }
            elseif ($dvalue -and [regex]::IsMatch($dvalue, [string]$win.display_on, 'IgnoreCase')) { $verdict = 'FAIL' }
            elseif (-not $dvalue -and $isOff) { $verdict = 'PASS' }
            $rows += New-NicRow $verdict $keyword 'off' $shown $note
        }
    }
    if ($rows.Count -eq 0) { $rows += New-NicRow 'INFO' 'eee' '(a keyword)' '(no EEE keyword)' "the driver's default holds, unread; $note" }
    return $rows
}

# The advanced properties the EEE rule reads, the hidden ones included
# (-AllProperties returns the registry-only keywords, as the 11.19 RTL8156B
# driver hides its EEE keywords). A keyword with no registry value keeps its
# display value (the judge reads it), with Value ''.
function Get-NicEeeProps {
    param([string]$Name)
    foreach ($p in @(Get-NetAdapterAdvancedProperty -Name $Name -AllProperties)) {
        if (-not $p.RegistryKeyword) { continue }
        [pscustomobject]@{
            Keyword      = [string]$p.RegistryKeyword
            Value        = if ($null -eq $p.RegistryValue) { '' } else { (@($p.RegistryValue) | ForEach-Object { [string]$_ }) -join ',' }
            DisplayName  = [string](Get-NicProp $p 'DisplayName' '')
            DisplayValue = [string](Get-NicProp $p 'DisplayValue' '')
        }
    }
}

# Whether an adapter is a wired physical NIC: hardware, not virtual, an 802.3
# medium (NdisPhysicalMedium 14; Wi-Fi is 9, Bluetooth 10).
function Test-NicWired {
    param($Adapter)
    return ((Get-NicProp $Adapter 'HardwareInterface') -eq $true) -and ((Get-NicProp $Adapter 'Virtual') -ne $true) -and
        ((Get-NicProp $Adapter 'NdisPhysicalMedium') -eq 14)
}

# Whether an adapter is a Wi-Fi one (NdisPhysicalMedium 9, or a
# PhysicalMediaType of 802.11, "Native 802.11"): out of the wired scan, the
# Realtek WiFi (RTL8852BE, PCI\VEN_10EC&DEV_B852) included.
function Test-NicWireless {
    param($Adapter)
    return ((Get-NicProp $Adapter 'NdisPhysicalMedium') -eq 9) -or
        ([string](Get-NicProp $Adapter 'PhysicalMediaType' '') -match '802\.11')
}

# Whether an adapter has no link (no carrier: a cable-less NIC), by its
# MediaConnectionState or its Status.
function Test-NicNoLink {
    param($Adapter)
    return ([string](Get-NicProp $Adapter 'MediaConnectionState' '') -eq 'Disconnected') -or
        ([string](Get-NicProp $Adapter 'Status' '') -eq 'Disconnected')
}

$script:NicNoLinkNote = 'no carrier: no speed or duplex verdict without a link; the driver and the other values are judged'
# The values an adapter has only with a link, as on Linux (LINK_STATE_KEYS):
# without a carrier they get INFO "no link", never a verdict.
$script:NicLinkStateKeys = @('link:speed', 'link:duplex')

# The negotiated link as facts (#465, 28.11.2026: resolume-pp's RTL8156B came
# up at 100 Mbps half duplex after a boot while *SpeedDuplex, a setting, read
# auto): link:speed is Get-NetAdapter's LinkSpeed ("2.5 Gbps", "1 Gbps",
# "100 Mbps"), link:duplex its FullDuplex as Full or Half, link:detected "no"
# for an adapter without a link. A property the adapter lacks, or holds as
# null, gives no fact (the row then reads absent and fails).
function Get-NicLinkFacts {
    param($Adapter)
    $facts = @{}
    if (Test-NicNoLink $Adapter) { $facts['link:detected'] = 'no' } else { $facts['link:detected'] = 'yes' }
    $speed = Get-NicProp $Adapter 'LinkSpeed'
    if ($null -ne $speed) { $facts['link:speed'] = [string]$speed }
    $full = Get-NicProp $Adapter 'FullDuplex'
    if ($null -ne $full) { $facts['link:duplex'] = if ([string]$full -ieq 'True') { 'Full' } else { 'Half' } }
    return $facts
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
    # LinkSpeed is a script property of the NetAdapter module: an error in it
    # is one FAIL line, and the link rows then read absent.
    try {
        $link = Get-NicLinkFacts $Adapter
        foreach ($k in $link.Keys) { $facts[$k] = $link[$k] }
    } catch {
        New-NicRow 'FAIL' 'link' '(readable)' '(unreadable)' $_.Exception.Message
    }
    Test-NicDriver $Entry.driver ([string]$Adapter.DriverFileName) ([string]$Adapter.DriverVersion)
    foreach ($v in @($Entry.values)) { Test-NicValue $v $facts $LatencyProfile }
    # *SpeedDuplex is the setting, link:speed / link:duplex the negotiated
    # link (above); an adapter without a link also gets one line saying so.
    if (Test-NicNoLink $Adapter) { New-NicRow 'INFO' 'link' '(a link)' 'no link' $script:NicNoLinkNote }
}

# The EEE lines of every wired NIC, any vendor, up, down or disabled (the
# owner, 20.11.2026: EEE on any rig device is a serious failure); sets
# $script:NicEeeFailed to the number of FAIL lines.
function Invoke-NicEeeCheck {
    param($Rule, $Adapters)
    "--    every wired NIC: $(Get-NicProp $Rule 'name' 'EEE off') (eee)"
    $failed = 0; $checked = 0
    foreach ($a in @($Adapters)) {
        if (-not (Test-NicWired $a)) { continue }
        if ($a.Status -eq 'Not Present') { "SKIP  $($a.Name)  EEE: Not Present"; continue }
        $checked++
        $driver = "$($a.DriverFileName) $($a.DriverVersion)"
        try {
            $props = @(Get-NicEeeProps $a.Name)
        } catch {
            "FAIL  $($a.Name)  eee  expected=(readable)  actual=(unreadable)  (driver $($driver.Trim()); $($_.Exception.Message))"
            $failed++
            continue
        }
        foreach ($r in @(Test-NicEeeWindows $Rule $props $driver)) {
            Format-NicRow $a.Name $r
            if ($r.Verdict -ceq 'FAIL') { $failed++ }
        }
    }
    "$checked wired NIC(s) checked for EEE, $failed FAIL"
    $script:NicEeeFailed = $failed
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
    $adapters = @(Get-NetAdapter)
    # EEE first: every wired NIC of the PC, whatever -Name says.
    $script:NicEeeFailed = 0
    Invoke-NicEeeCheck $std.eee $adapters
    $eeeFailed = $script:NicEeeFailed
    $failed = 0; $checked = 0
    $named = @($Name | Where-Object { $_ })
    $found = @{}
    foreach ($a in $adapters) {
        if ($named.Count -gt 0 -and -not ($named -contains $a.Name)) { continue }
        if ($a.Status -eq 'Not Present') { continue }
        # A Realtek Wi-Fi is out of the wired scan.
        if (Test-NicWireless $a) { continue }
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
    $script:NicExit = if ($failed + $eeeFailed -gt 0) { 1 } else { 0 }
}

if ($MyInvocation.InvocationName -ne '.') {
    Invoke-NicStandardCheck -Source $Standard -LatencyProfile $LatencyProfile -IncludeDown:$IncludeDown -Name $Name
    exit $script:NicExit
}

# SPDX-License-Identifier: MIT OR Apache-2.0
#
# Tests of tools/windows/nic-standard-check.ps1 (dstack #465), run by CI's
# Windows job under Windows PowerShell 5.1:
#   powershell.exe -NoProfile -ExecutionPolicy Bypass -File tools\windows\tests\nic-standard-check.Tests.ps1
# A plain assert script, not Pester (the inbox Pester 3.4 and the runner's
# Pester 5 differ). Exits 1 if any check fails.
#
# No real adapter is read: Get-NetAdapter, Get-PnpDeviceProperty and
# Get-NetAdapterAdvancedProperty are replaced below by functions of the same
# name (a function wins over a cmdlet) that return adapters shaped like the
# fleet's, as read on 20.11.2026 (resolume-pp's RTL8156B on 11.19, mbc's on
# the 1156.22 driver, dsg's RTL8168). The standard is the repo's own file.

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '..\nic-standard-check.ps1')
$standardPath = Join-Path $PSScriptRoot '..\..\..\docs\nic-standard.json'

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

$std = Read-NicStandard $standardPath
function Entry { param([string]$Id) @($std.adapters) | Where-Object { $_.id -eq $Id } }
function ValueSpec { param([string]$Id, [string]$Key) @((Entry $Id).values) | Where-Object { $_.key -eq $Key } }

# ------------------------------------------------------------ the file
Check 'the repo file reads as the schema, version 1 or later' { $std.schema -eq 'dstack-nic-standard/1' -and $std.version -ge 1 }
Check 'PendingTransmits stays 8, open, with its plan' {
    $v = ValueSpec 'rtl8156b-windows' 'PendingTransmits'
    $v.expected -eq '8' -and $v.status -eq 'open' -and $v.plan -like '*#425*'
}
$standardText = Get-Content -LiteralPath $standardPath -Raw -Encoding UTF8
# The repo file with one change: $true when Test-NicStandard refuses it.
function Refused {
    param([scriptblock]$Change)
    $s = $standardText | ConvertFrom-Json
    & $Change $s
    try { Test-NicStandard $s; return $false } catch { return $true }
}
Check 'the repo file validates' { $s = $standardText | ConvertFrom-Json; Test-NicStandard $s; $true }
Check 'a malformed file is refused, as the Python checker refuses it' {
    $v = { param($s) $s.adapters[0].values }
    (Refused { param($s) $s.schema = 'other/1' }) -and
    (Refused { param($s) $s.version = '1' }) -and
    (Refused { param($s) $s.PSObject.Properties.Remove('adapters') }) -and
    (Refused { param($s) (& $v $s)[1].PSObject.Properties.Remove('evidence') }) -and
    (Refused { param($s) (& $v $s)[1] | Add-Member -NotePropertyName status -NotePropertyValue 'report' }) -and
    (Refused { param($s) (& $v $s)[1] | Add-Member -NotePropertyName status -NotePropertyValue 'Report' }) -and
    (Refused { param($s) (& $v $s)[1] | Add-Member -NotePropertyName compare -NotePropertyValue 'near' }) -and
    (Refused { param($s) (& $v $s)[1] | Add-Member -NotePropertyName absent -NotePropertyValue 'PASS' }) -and
    (Refused { param($s) (& $v $s)[1] | Add-Member -NotePropertyName profile -NotePropertyValue 'lowlatency' }) -and
    (Refused { param($s) (& $v $s)[1].expected = 64 }) -and
    (Refused { param($s) (& $v $s)[1].key = (& $v $s)[0].key.ToLowerInvariant() }) -and
    (Refused { param($s) (& $v $s)[0].PSObject.Properties.Remove('plan') }) -and
    (Refused { param($s) (@(& $v $s) | Where-Object { $_.PSObject.Properties['compare'] })[0].expected = '0x08' }) -and
    (Refused { param($s) $s.adapters[1].id = $s.adapters[0].id }) -and
    (Refused { param($s) $s.adapters[0].driver.PSObject.Properties.Remove('evidence') }) -and
    (Refused { param($s) $s.adapters[0].driver.PSObject.Properties.Remove('files') }) -and
    (Refused { param($s) $s.adapters[0].driver.bad[0].PSObject.Properties.Remove('evidence') }) -and
    (Refused { param($s) $s.adapters[0].driver | Add-Member -NotePropertyName firmware -NotePropertyValue 'x' }) -and
    (Refused { param($s) $s.adapters[0].os = 'macos' }) -and
    (Refused { param($s) $s.adapters[0].match = [pscustomobject]@{ usb = @('0bda:8156') } }) -and
    (Refused { param($s) $s.adapters[0].PSObject.Properties.Remove('values') }) -and
    (Refused { param($s) $s.version = $true }) -and
    (Refused { param($s) (& $v $s)[1].expected = @() })
}
Check 'an unknown key, a wrong type or the wrong driver names are refused' {
    $v = { param($s) $s.adapters[0].values }
    $d = { param($s) $s.adapters[0].driver }
    $bits = { param($s) (@(& $v $s) | Where-Object { $_.PSObject.Properties['compare'] })[0] }
    (Refused { param($s) $s | Add-Member -NotePropertyName adpaters -NotePropertyValue @() }) -and
    (Refused { param($s) $s.adapters[0] | Add-Member -NotePropertyName chips -NotePropertyValue 'RTL8156B' }) -and
    (Refused { param($s) (& $d $s) | Add-Member -NotePropertyName min_versoin -NotePropertyValue '1' }) -and
    (Refused { param($s) (& $v $s)[1] | Add-Member -NotePropertyName expect -NotePropertyValue '64' }) -and
    (Refused { param($s) (& $d $s).bad[0] | Add-Member -NotePropertyName why -NotePropertyValue 'x' }) -and
    (Refused { param($s) (& $d $s).bad = [pscustomobject]@{ file = 'x.sys'; evidence = 'e' } }) -and
    (Refused { param($s) (& $d $s).bad = $null }) -and
    (Refused { param($s) (& $d $s).min_version = 11 }) -and
    (Refused { param($s) (& $d $s) | Add-Member -NotePropertyName plan -NotePropertyValue 7 }) -and
    (Refused { param($s) (& $d $s) | Add-Member -NotePropertyName modules -NotePropertyValue @('r8152') }) -and
    (Refused { param($s) $s.adapters[1].driver | Add-Member -NotePropertyName files -NotePropertyValue @('r8152.ko') }) -and
    (Refused { param($s) $s.adapters[1].driver | Add-Member -NotePropertyName firmware -NotePropertyValue 3 }) -and
    (Refused { param($s) (& $bits $s).expected = [string][char]0x00B2 }) -and
    (Refused { param($s) (& $bits $s).expected = ('1' * 19) })
}
Check 'a one-item list and an empty list in the file stay lists' {
    $s = $standardText | ConvertFrom-Json
    $files = Get-NicList (@($s.adapters) | Where-Object { $_.id -eq 'rtl8156b-windows' }).driver 'files'
    $none = Get-NicList (@($s.adapters) | Where-Object { $_.id -eq 'realtek-vlan-windows' }) 'values'
    ($files -is [array]) -and $files.Count -eq 1 -and ($none -is [array]) -and $none.Count -eq 0
}
Check 'a file of another schema is refused' {
    $tmp = [IO.Path]::GetTempFileName()
    try {
        Set-Content -LiteralPath $tmp -Value '{"schema": "other/1", "version": 1, "adapters": []}' -Encoding UTF8
        try { Read-NicStandard $tmp | Out-Null; $false } catch { $_.Exception.Message -like 'not a dstack-nic-standard/1 file*' }
    } finally { Remove-Item -LiteralPath $tmp -Force }
}

# ------------------------------------------------------------ matching
Check 'an RTL8156B by its hardware id, any revision, any case' {
    (Find-NicEntry $std 'USB\VID_0BDA&PID_8156&REV_31F4').id -eq 'rtl8156b-windows' -and
    (Find-NicEntry $std 'usb\vid_0bda&pid_8156').id -eq 'rtl8156b-windows'
}
Check 'the PCIe RTL8125 and RTL8168 share one entry' {
    (Find-NicEntry $std 'PCI\VEN_10EC&DEV_8168&SUBSYS_189B1025&REV_15').id -eq 'rtl8125-rtl8168-windows' -and
    (Find-NicEntry $std 'PCI\VEN_10EC&DEV_8125&SUBSYS_E0001458&REV_05').id -eq 'rtl8125-rtl8168-windows'
}
Check 'the Realtek Virtual Adapter has its entry' { (Find-NicEntry $std 'RTK_VLANMP').id -eq 'realtek-vlan-windows' }
Check 'an RTL8153 has no entry' { $null -eq (Find-NicEntry $std 'USB\VID_0BDA&PID_8153&REV_3000') }
Check 'no hardware id, no entry' { $null -eq (Find-NicEntry $std '') }

# ------------------------------------------------------------ judging
Check 'equals, a list, bits, contains' {
    (Test-NicValueHolds ([pscustomobject]@{ expected = '8' }) ' 8 ') -and
    -not (Test-NicValueHolds ([pscustomobject]@{ expected = '8' }) '32') -and
    (Test-NicValueHolds ([pscustomobject]@{ expected = @('15', '15000') }) '15000') -and
    (Test-NicValueHolds ([pscustomobject]@{ expected = '8'; compare = 'bits' }) '24') -and
    -not (Test-NicValueHolds ([pscustomobject]@{ expected = '8'; compare = 'bits' }) '16') -and
    -not (Test-NicValueHolds ([pscustomobject]@{ expected = '8'; compare = 'bits' }) 'x') -and
    -not (Test-NicValueHolds ([pscustomobject]@{ expected = '8'; compare = 'bits' }) '0x18') -and
    -not (Test-NicValueHolds ([pscustomobject]@{ expected = '8'; compare = 'bits' }) ([string][char]0x00B2)) -and
    -not (Test-NicValueHolds ([pscustomobject]@{ expected = '8'; compare = 'bits' }) ('8' * 19)) -and
    (Test-NicValueHolds ([pscustomobject]@{ expected = '8'; compare = 'bits' }) "24`n") -and
    (Test-NicValueHolds ([pscustomobject]@{ expected = 'Full' }) 'full') -and
    (Test-NicValueHolds ([pscustomobject]@{ expected = 'b'; compare = 'contains' }) 'ABC') -and
    -not (Test-NicValueHolds ([pscustomobject]@{ expected = 'd'; compare = 'contains' }) 'abc')
}
Check 'an absent keyword fails unless the standard allows it' {
    $must = [pscustomobject]@{ key = 'k'; expected = '0'; evidence = 'e' }
    $may = [pscustomobject]@{ key = 'k'; expected = '0'; evidence = 'e'; absent = 'pass' }
    (Test-NicValue $must @{}).Verdict -eq 'FAIL' -and (Test-NicValue $may @{}).Verdict -eq 'PASS' -and
    (Test-NicValue $may @{ k = '1' }).Verdict -eq 'FAIL' -and (Test-NicValue $must @{ k = '1' }).Actual -eq '1'
}
Check 'a keyword matches whatever its case in the registry' {
    $v = ValueSpec 'rtl8125-rtl8168-windows' '*LsoV2IPv4'
    (Test-NicValue $v @{ '*lsov2ipv4' = '0' }).Verdict -eq 'PASS'
}
Check 'report is INFO, another profile is SKIP, open carries its plan' {
    $report = [pscustomobject]@{ key = 'k'; expected = $null; status = 'report'; evidence = 'e' }
    $low = [pscustomobject]@{ key = 'k'; expected = 'off'; profile = 'low_latency'; evidence = 'e' }
    $open = [pscustomobject]@{ key = 'k'; expected = '8'; status = 'open'; plan = 'measure it'; evidence = 'e' }
    $r = Test-NicValue $report @{ k = 'x' }
    $o = Test-NicValue $open @{ k = '32' }
    $r.Verdict -eq 'INFO' -and $r.Expected -eq '(no standard yet)' -and
    (Test-NicValue $low @{ k = 'on' }).Verdict -eq 'SKIP' -and
    (Test-NicValue $low @{ k = 'on' } 'low_latency').Verdict -eq 'FAIL' -and
    $o.Verdict -eq 'FAIL' -and $o.Note -eq 'open: measure it'
}
Check 'versions compare number by number' {
    (Compare-NicVersion '1156.22.113.2026' '11.19.602.2025') -eq 1 -and
    (Compare-NicVersion '11.4.211.2022' '11.19.602.2025') -eq -1 -and
    (Compare-NicVersion '10.79.50.1003' '10.79.50.1003') -eq 0 -and
    (Compare-NicVersion '10.79.20' '10.79.20.0') -eq 0
}
Check 'the RTL8156B driver: 11.19 passes, older or another file fails, NDIS names its evidence' {
    $d = (Entry 'rtl8156b-windows').driver
    $v = { param($f, $ver) (@(Test-NicDriver $d $f $ver) | ForEach-Object { $_.Verdict }) -join ',' }
    (& $v 'rtucx22x64.sys' '11.19.602.2025') -eq 'PASS,PASS' -and
    (& $v 'rtucx22x64.sys' '11.18.1.2025') -eq 'PASS,FAIL' -and
    (& $v 'rtucx21x64.sys' '11.4.211.2022') -eq 'FAIL' -and
    (& $v 'rtu56cx22x64.sys' '1156.22.113.2026') -eq 'FAIL' -and
    (@(Test-NicDriver $d 'rtump64x64.sys' '10.68.20.605')[0].Note -like '*#290*')
}
Check 'the rt640x64 driver: a bad version fails, 10.79.50 passes with the open plan' {
    $d = (Entry 'rtl8125-rtl8168-windows').driver
    $bad = @(Test-NicDriver $d 'rt640x64.sys' '10.72.524.2024')
    $good = @(Test-NicDriver $d 'rt640x64.sys' '10.79.50.1003')
    $bad.Count -eq 1 -and $bad[0].Verdict -eq 'FAIL' -and
    (($good | ForEach-Object { $_.Verdict }) -join ',') -eq 'PASS,PASS' -and $good[0].Note -like 'open: *'
}
Check 'a row prints the evidence only when it is no PASS' {
    $pass = Format-NicRow 'Ethernet 4' (New-NicRow 'PASS' 'k' '8' '8' 'e')
    $fail = Format-NicRow 'Ethernet 4' (New-NicRow 'FAIL' 'k' '8' '3' 'e')
    $pass -eq 'PASS  Ethernet 4  k  expected=8  actual=8' -and $fail -eq 'FAIL  Ethernet 4  k  expected=8  actual=3  (e)'
}

# ------------------------------------------------------------ the fake PC
$resolumePp = [ordered]@{
    '*SpeedDuplex' = '0'; '*IPChecksumOffloadIPv4' = '3'; '*TCPChecksumOffloadIPv4' = '3'; '*UDPChecksumOffloadIPv4' = '3'
    '*TCPChecksumOffloadIPv6' = '3'; '*UDPChecksumOffloadIPv6' = '3'; '*PriorityVLANTag' = '3'; PnPCapabilities = '24'
    EnableUSBInterrupt = '0'; PendingTransmits = '8'; TransmitBufferLen = '64'; PendingReceives = '6'; ReceiveBufferLen = '16'
    '*FlowControl' = '0'; '*EEE' = '0'; EEE = '0'; EnableGreenEthernet = '0'; AdvancedEEE = '0'; EnableExtraPowerSaving = '0'
    GigaLite = '0'; '*SelectiveSuspend' = '0'; NetworkAddress = $null; RegVlanID = $null
}
$mbc = [ordered]@{
    '*SpeedDuplex' = '0'; '*IPChecksumOffloadIPv4' = '3'; '*TCPChecksumOffloadIPv4' = '3'; '*UDPChecksumOffloadIPv4' = '3'
    '*TCPChecksumOffloadIPv6' = '3'; '*UDPChecksumOffloadIPv6' = '3'; '*PriorityVLANTag' = '3'; PendingTransmits = '3'
    TransmitBufferLen = '37'; PendingReceives = '10'; ReceiveBufferLen = '32'; PnPCapabilities = '24'; '*FlowControl' = '0'
    '*EEE' = '1'; EnableGreenEthernet = '1'; AdvancedEEE = '0'; EnableExtraPowerSaving = '1'; GigaLite = '1'
    '*JumboPacket' = '1514'; '*LsoV2IPv4' = '0'; '*LsoV2IPv6' = '0'; '*RscIPv4' = '1'; '*WakeOnMagicPacket' = '1'
}
$script:FakeAdapters = @()
$script:FakeValues = @{}
$script:FakeIds = @{}
$script:FakeDisplay = @{}
function Set-FakePc {
    param([object[]]$Adapters)
    $script:FakeAdapters = @(); $script:FakeValues = @{}; $script:FakeIds = @{}; $script:FakeDisplay = @{}
    foreach ($a in $Adapters) {
        $fake = [pscustomobject]@{
            Name = $a.Name; InterfaceDescription = $a.Desc; Status = $a.Status; DriverFileName = $a.File
            DriverVersion = $a.Version; PnPDeviceID = 'DEV\' + $a.Name
        }
        # A real MSFT_NetAdapter's medium fields, given only where a test sets
        # Wired (a wired physical NIC) or Medium (another kind).
        if ($a.ContainsKey('Wired') -or $a.ContainsKey('Medium')) {
            $medium = if ($a.ContainsKey('Medium')) { $a.Medium } else { 14 }
            $virtual = $a.ContainsKey('Virtual') -and $a.Virtual
            $fake | Add-Member -NotePropertyName HardwareInterface -NotePropertyValue (-not $virtual)
            $fake | Add-Member -NotePropertyName Virtual -NotePropertyValue $virtual
            $fake | Add-Member -NotePropertyName NdisPhysicalMedium -NotePropertyValue $medium
        }
        $script:FakeAdapters += $fake
        $script:FakeIds['DEV\' + $a.Name] = $a.Hw
        $script:FakeValues[$a.Name] = $a.Props
        $script:FakeDisplay[$a.Name] = if ($a.ContainsKey('Display')) { $a.Display } else { @{} }
    }
}
function Get-NetAdapter { [CmdletBinding()] param() $script:FakeAdapters }
function Get-PnpDeviceProperty {
    [CmdletBinding()] param([string]$InstanceId, [string]$KeyName)
    $id = $script:FakeIds[$InstanceId]
    if ($id -eq 'THROW') { throw 'the PnP store is unreadable' }
    [pscustomobject]@{ Data = @($id, 'second id') }
}
function Get-NetAdapterAdvancedProperty {
    [CmdletBinding()] param([string]$Name, [switch]$AllProperties)
    $values = $script:FakeValues[$Name]
    if ($null -eq $values) { throw "No MSFT_NetAdapterAdvancedPropertySettingData objects found with property 'Name' equal to '$Name'" }
    foreach ($k in $values.Keys) {
        # A keyword the test gives a display (name, value) is a visible one;
        # the others are hidden (no display name), as the 11.19 driver's.
        $d = $script:FakeDisplay[$Name]
        $shown = if ($null -ne $d -and $d.ContainsKey($k)) { $d[$k] } else { @('', '') }
        [pscustomobject]@{ RegistryKeyword = $k; RegistryValue = $values[$k]; DisplayName = $shown[0]; DisplayValue = $shown[1] }
    }
}
function Run-Fake {
    param([switch]$IncludeDown, [string]$LatencyProfile = 'normal', [string[]]$Name = @())
    $lines = @(Invoke-NicStandardCheck -Source $standardPath -LatencyProfile $LatencyProfile -IncludeDown:$IncludeDown -Name $Name)
    [pscustomobject]@{ Lines = $lines; Exit = $script:NicExit }
}
$usb = 'USB\VID_0BDA&PID_8156&REV_31F4'

Check 'resolume-pp as read: every value passes, exit 0' {
    Set-FakePc @(
        @{ Name = 'Ethernet 4'; Desc = 'Realtek Gaming USB 2.5GbE Family Controller'; Status = 'Up'; File = 'rtucx22x64.sys'; Version = '11.19.602.2025'; Hw = $usb; Props = $resolumePp },
        @{ Name = 'Dante70'; Desc = 'Realtek Virtual Adapter'; Status = 'Up'; File = 'RtVlan640.sys'; Version = '10.4.1117.2020'; Hw = 'RTK_VLANMP'; Props = @{ VlanID = '70' } },
        @{ Name = 'Wi-Fi'; Desc = 'Intel(R) Wi-Fi 6E AX211 160MHz'; Status = 'Disabled'; File = 'Netwtw14.sys'; Version = '23.150.0.4'; Hw = 'PCI\VEN_8086&DEV_51F0'; Props = @{} }
    )
    $r = Run-Fake
    $fails = @($r.Lines | Where-Object { $_ -like 'FAIL*' })
    $r.Exit -eq 0 -and $fails.Count -eq 0 -and ($r.Lines -contains 'PASS  Ethernet 4  PendingTransmits  expected=8  actual=8') -and
    ($r.Lines -contains '2 Realtek NIC(s) checked, 0 FAIL') -and -not ($r.Lines -match 'Wi-Fi')
}
Check 'mbc as read: the driver and the power values fail with both values, exit 1' {
    Set-FakePc @(
        @{ Name = 'Ethernet 3'; Desc = 'Realtek Gaming USB 2.5GbE Family Controller'; Status = 'Up'; File = 'rtu56cx22x64.sys'; Version = '1156.22.113.2026'; Hw = $usb; Props = $mbc }
    )
    $r = Run-Fake
    $r.Exit -eq 1 -and
    @($r.Lines | Where-Object { $_ -like 'FAIL  Ethernet 3  driver  expected=rtucx22x64.sys  actual=rtu56cx22x64.sys*' }).Count -eq 1 -and
    @($r.Lines | Where-Object { $_ -like 'FAIL  Ethernet 3  PendingTransmits  expected=8  actual=3  (open: *' }).Count -eq 1 -and
    @($r.Lines | Where-Object { $_ -like 'FAIL  Ethernet 3  `*EEE  expected=0  actual=1  (*' }).Count -eq 1 -and
    @($r.Lines | Where-Object { $_ -like 'FAIL  Ethernet 3  `*SelectiveSuspend  expected=0  actual=(absent)*' }).Count -eq 1 -and
    @($r.Lines | Where-Object { $_ -like 'INFO  Ethernet 3  `*RscIPv4  expected=(no standard yet)  actual=1*' }).Count -eq 1
}
Check 'an unknown Realtek fails as having no standard; a down one is skipped unless asked' {
    Set-FakePc @(
        @{ Name = 'Ethernet 5'; Desc = 'Realtek USB GbE Family Controller'; Status = 'Up'; File = 'rtux64w10.sys'; Version = '10.1.1'; Hw = 'USB\VID_0BDA&PID_8153&REV_3000'; Props = @{} },
        @{ Name = 'soundgrid'; Desc = 'Realtek PCIe GbE Family Controller #2'; Status = 'Disconnected'; File = 'rt640x64.sys'; Version = '10.79.50.1003'; Hw = 'PCI\VEN_10EC&DEV_8168&REV_15'; Props = @{} }
    )
    $r = Run-Fake
    $down = Run-Fake -IncludeDown
    $r.Exit -eq 1 -and @($r.Lines | Where-Object { $_ -like 'FAIL  Ethernet 5  *no standard for this Realtek adapter on Windows' }).Count -eq 1 -and
    ($r.Lines -contains 'SKIP  soundgrid  PCI\VEN_10EC&DEV_8168&REV_15 is Disconnected') -and
    @($down.Lines | Where-Object { $_ -like '--    soundgrid  *' }).Count -eq 1
}
Check 'a disabled Realtek is named as skipped, even with -IncludeDown' {
    Set-FakePc @(
        @{ Name = 'soundgrid-pcie'; Desc = 'Realtek PCIe GbE Family Controller'; Status = 'Disabled'; File = 'rt640x64.sys'; Version = '10.79.50.1003'; Hw = 'PCI\VEN_10EC&DEV_8168&REV_15'; Props = @{} }
    )
    $r = Run-Fake -IncludeDown
    $r.Exit -eq 0 -and ($r.Lines -contains 'SKIP  soundgrid-pcie  PCI\VEN_10EC&DEV_8168&REV_15 is Disabled') -and
    ($r.Lines -contains '0 Realtek NIC(s) checked, 0 FAIL')
}
Check 'an unreadable hardware id or value list fails with its cause, and the run goes on' {
    Set-FakePc @(
        @{ Name = 'Ethernet 6'; Desc = 'Realtek Gaming USB 2.5GbE Family Controller'; Status = 'Up'; File = 'rtucx22x64.sys'; Version = '11.19.602.2025'; Hw = 'THROW'; Props = @{} },
        @{ Name = 'Ethernet 7'; Desc = 'Realtek Gaming USB 2.5GbE Family Controller'; Status = 'Up'; File = 'rtucx22x64.sys'; Version = '11.19.602.2025'; Hw = $usb; Props = $null }
    )
    $r = Run-Fake
    $r.Exit -eq 1 -and
    @($r.Lines | Where-Object { $_ -like 'FAIL  Ethernet 6  *no standard for this Realtek adapter on Windows (hardware id unreadable: the PnP store is unreadable)' }).Count -eq 1 -and
    @($r.Lines | Where-Object { $_ -like 'FAIL  Ethernet 7  values  expected=(readable)  actual=(unreadable)  (No MSFT_*' }).Count -eq 1 -and
    ($r.Lines -contains '2 Realtek NIC(s) checked, 2 FAIL')
}
Check 'named adapters: only those are checked, a down one too; a missing or non-Realtek one fails' {
    Set-FakePc @(
        @{ Name = 'Ethernet 4'; Desc = 'Realtek Gaming USB 2.5GbE Family Controller'; Status = 'Up'; File = 'rtucx22x64.sys'; Version = '11.19.602.2025'; Hw = $usb; Props = $resolumePp },
        @{ Name = 'soundgrid'; Desc = 'Realtek PCIe GbE Family Controller #2'; Status = 'Disconnected'; File = 'rt640x64.sys'; Version = '10.79.50.1003'; Hw = 'PCI\VEN_10EC&DEV_8168&REV_15'; Props = @{} },
        @{ Name = 'Wi-Fi'; Desc = 'Intel(R) Wi-Fi 6E AX211 160MHz'; Status = 'Up'; File = 'Netwtw14.sys'; Version = '23.150.0.4'; Hw = 'PCI\VEN_8086&DEV_51F0'; Props = @{} }
    )
    $one = Run-Fake -Name @('Ethernet 4')
    $down = Run-Fake -Name @('soundgrid')
    $bad = Run-Fake -Name @('Ethernet 44', 'Wi-Fi')
    $one.Exit -eq 0 -and -not ($one.Lines -match 'soundgrid') -and ($one.Lines -contains '1 Realtek NIC(s) checked, 0 FAIL') -and
    @($down.Lines | Where-Object { $_ -like '--    soundgrid  *' }).Count -eq 1 -and
    $bad.Exit -eq 1 -and ($bad.Lines -contains 'FAIL  Ethernet 44: no such Realtek adapter') -and
    ($bad.Lines -contains 'FAIL  Wi-Fi: no such Realtek adapter') -and ($bad.Lines -contains '0 Realtek NIC(s) checked, 2 FAIL')
}
Check 'a keyword with no registry value reads as absent' {
    Set-FakePc @(
        @{ Name = 'Ethernet 4'; Desc = 'Realtek'; Status = 'Up'; File = 'x'; Version = '1'; Hw = $usb; Props = [ordered]@{ '*EEE' = $null; GigaLite = '0' } }
    )
    $f = Get-NicFacts 'Ethernet 4'
    -not $f.ContainsKey('*EEE') -and $f['GigaLite'] -eq '0'
}
# ------------------------------------------------------------ EEE on every wired NIC
# The vendor-independent rule (owner 20.11.2026, dstack #465). Properties as
# read on 20.11.2026: keyword, value, display name, display value; a hidden
# keyword has no display name.
$eee = $std.eee
function EeeProp { param([string]$K, [string]$V, [string]$N = '', [string]$D = '') [pscustomobject]@{ Keyword = $K; Value = $V; DisplayName = $N; DisplayValue = $D } }
function EeeVerdicts { param($Rows) (@($Rows) | ForEach-Object { "$($_.Key)=$($_.Verdict)" }) -join ',' }

Check 'the EEE rule is in the file' {
    (@($eee.windows.keywords) -ccontains 'EEELinkAdvertisement') -and (@($eee.windows.off) -join ',') -eq '0' -and
    (@($eee.linux.off) -join ',') -eq 'disabled,not supported' -and $std.version -ge 3
}
Check 'a broken EEE rule is refused, as the Python checker refuses it' {
    (Refused { param($s) $s.PSObject.Properties.Remove('eee') }) -and
    (Refused { param($s) $s.eee = @() }) -and
    (Refused { param($s) $s.eee.PSObject.Properties.Remove('evidence') }) -and
    (Refused { param($s) $s.eee | Add-Member -NotePropertyName vendor -NotePropertyValue 'any' }) -and
    (Refused { param($s) $s.eee.PSObject.Properties.Remove('windows') }) -and
    (Refused { param($s) $s.eee.PSObject.Properties.Remove('linux') }) -and
    (Refused { param($s) $s.eee.windows.keywords = @() }) -and
    (Refused { param($s) $s.eee.windows.off = '0' }) -and
    (Refused { param($s) $s.eee.windows.display_name = 'energy[(' }) -and
    (Refused { param($s) $s.eee.windows.display_on = '' }) -and
    (Refused { param($s) $s.eee.windows | Add-Member -NotePropertyName keyword -NotePropertyValue @('*EEE') }) -and
    (Refused { param($s) $s.eee.linux.off = @() }) -and
    (Refused { param($s) $s.eee.linux | Add-Member -NotePropertyName on -NotePropertyValue @('enabled') })
}
Check 'EEE, Realtek visible keywords (mbc): on fails with driver and keyword, the max speed is INFO' {
    $rows = @(Test-NicEeeWindows $eee @(
        (EeeProp '*EEE' '1' 'Energy-Efficient Ethernet' 'Enabled'),
        (EeeProp 'EEEMaxSupportSpeed' '1000' 'EEE Max Support Speed' '1.0 Gbps Full Duplex'),
        (EeeProp 'EnableGreenEthernet' '1' 'Green Ethernet' 'Enabled'),
        (EeeProp 'AdvancedEEE' '0' 'Advanced EEE' 'Disabled'),
        (EeeProp 'GigaLite' '1' 'Gigabit Lite' 'Enabled')
    ) 'rtu56cx22x64.sys 1156.22.113.2026')
    $fail = $rows | Where-Object { $_.Key -eq '*EEE' }
    (EeeVerdicts $rows) -eq '*EEE=FAIL,EEEMaxSupportSpeed=INFO,EnableGreenEthernet=FAIL,AdvancedEEE=PASS' -and
    $fail.Expected -eq '0' -and $fail.Actual -eq '1 (Enabled)' -and $fail.Note -like 'driver rtu56cx22x64.sys 1156.22.113.2026; *'
}
Check 'EEE, Realtek hidden keywords (resolume): off passes; a hidden max speed is not judged' {
    $hidden = @(Test-NicEeeWindows $eee @((EeeProp '*EEE' '0'), (EeeProp 'EEE' '0'), (EeeProp 'EnableGreenEthernet' '0'), (EeeProp 'AdvancedEEE' '0'), (EeeProp 'PendingTransmits' '8')) 'rtucx22x64.sys 11.19.602.2025')
    $dsg = @(Test-NicEeeWindows $eee @((EeeProp '*EEE' '0' 'Energy-Efficient Ethernet' 'Disabled'), (EeeProp 'EEEMaxSupportSpeed' '1000')) 'rt640x64.sys 10.79.50.1003')
    (EeeVerdicts $hidden) -eq '*EEE=PASS,EEE=PASS,EnableGreenEthernet=PASS,AdvancedEEE=PASS' -and (EeeVerdicts $dsg) -eq '*EEE=PASS'
}
Check 'EEE, Intel: EEELinkAdvertisement and *EEE, any case' {
    (EeeVerdicts (Test-NicEeeWindows $eee @((EeeProp 'EEELinkAdvertisement' '1' 'Energy Efficient Ethernet' 'On')) 'e2fn.sys 2.1.5.7')) -eq 'EEELinkAdvertisement=FAIL' -and
    (EeeVerdicts (Test-NicEeeWindows $eee @((EeeProp 'eeelinkadvertisement' '0' 'Energy Efficient Ethernet' 'Off')) 'e2fn.sys 2.1.5.7')) -eq 'eeelinkadvertisement=PASS' -and
    (EeeVerdicts (Test-NicEeeWindows $eee @((EeeProp '*EEE' '1' 'Energy Efficient Ethernet' 'On')) 'e1d68x64.sys 12.19.2.45')) -eq '*EEE=FAIL'
}
Check 'EEE, another vendor (Aquantia-style) by its display name and display value' {
    $v = { param($p) @(Test-NicEeeWindows $eee @($p) 'aqnic650.sys 3.1.6')[0].Verdict }
    (& $v (EeeProp 'EeeEnable' '1' 'Energy-Efficient Ethernet' 'Enabled')) -eq 'FAIL' -and
    (& $v (EeeProp 'EeeEnable' '1' 'Energy-Efficient Ethernet' 'Disabled')) -eq 'PASS' -and
    (& $v (EeeProp 'EeeEnable' '0' 'Energy Efficient Ethernet' '')) -eq 'PASS' -and
    (& $v (EeeProp 'GreenMode' '2' 'Green Ethernet' 'On (at 1 Gbps)')) -eq 'FAIL' -and
    (& $v (EeeProp 'EeeEnable' '2' 'EEE' 'Auto')) -eq 'INFO'
}
Check 'EEE, a listed keyword without a value goes by its display value' {
    (EeeVerdicts (Test-NicEeeWindows $eee @((EeeProp '*EEE' '' 'Energy-Efficient Ethernet' 'Enabled')) 'x.sys 1')) -eq '*EEE=FAIL' -and
    (EeeVerdicts (Test-NicEeeWindows $eee @((EeeProp '*EEE' '' 'Energy-Efficient Ethernet' 'Disabled')) 'x.sys 1')) -eq '*EEE=PASS' -and
    (EeeVerdicts (Test-NicEeeWindows $eee @((EeeProp '*EEE' '')) 'x.sys 1')) -eq 'eee=INFO'
}
Check 'EEE, a keyword with no registry value but a display value is read, not dropped' {
    Set-FakePc @(
        @{ Name = 'Ethernet 2'; Desc = 'Realtek PCIe GbE Family Controller'; Status = 'Up'; File = 'rt640x64.sys'; Version = '10.79.50.1003'; Hw = 'PCI\VEN_10EC&DEV_8168'; Props = [ordered]@{ '*EEE' = $null }; Display = @{ '*EEE' = @('Energy-Efficient Ethernet', 'Enabled') }; Wired = $true }
    )
    $p = @(Get-NicEeeProps 'Ethernet 2')
    $p.Count -eq 1 -and $p[0].Value -eq '' -and $p[0].DisplayValue -eq 'Enabled' -and
    (EeeVerdicts (Test-NicEeeWindows $eee $p 'rt640x64.sys 10.79.50.1003')) -eq '*EEE=FAIL'
}
Check 'EEE, no EEE keyword at all is one INFO row' {
    $rows = @(Test-NicEeeWindows $eee @((EeeProp 'IEEE11nMode' '3' '802.11n/ac/ax Wireless Mode' '4. 802.11ax'), (EeeProp 'GreenFieldCapable' '0')) 'x.sys 1')
    $rows.Count -eq 1 -and $rows[0].Verdict -eq 'INFO' -and $rows[0].Key -eq 'eee' -and $rows[0].Actual -eq '(no EEE keyword)'
}
Check 'EEE on the fake PC: every wired NIC, any vendor, up or disabled, whatever -Name; no Wi-Fi, no virtual, no absent one' {
    Set-FakePc @(
        @{ Name = 'Ethernet 4'; Desc = 'Realtek Gaming USB 2.5GbE Family Controller'; Status = 'Up'; File = 'rtucx22x64.sys'; Version = '11.19.602.2025'; Hw = $usb; Props = $resolumePp; Wired = $true },
        @{ Name = 'Dante70'; Desc = 'Realtek Virtual Adapter'; Status = 'Up'; File = 'RtVlan640.sys'; Version = '10.4.1117.2020'; Hw = 'RTK_VLANMP'; Props = @{ '*EEE' = '1' }; Wired = $true; Virtual = $true },
        @{ Name = 'Ethernet'; Desc = 'Intel(R) Ethernet Controller (2) I225-V'; Status = 'Disabled'; File = 'e2fn.sys'; Version = '2.1.5.7'; Hw = 'PCI\VEN_8086&DEV_15F3'; Props = [ordered]@{ EEELinkAdvertisement = '1' }; Display = @{ EEELinkAdvertisement = @('Energy Efficient Ethernet', 'On') }; Wired = $true },
        @{ Name = 'Ethernet 9'; Desc = 'Intel(R) Ethernet Controller I226-V'; Status = 'Not Present'; File = 'e2fn.sys'; Version = '2.1.5.7'; Hw = 'PCI\VEN_8086&DEV_125C'; Props = @{ '*EEE' = '1' }; Wired = $true },
        @{ Name = 'Wi-Fi'; Desc = 'Intel(R) Wi-Fi 6E AX211 160MHz'; Status = 'Up'; File = 'Netwtw14.sys'; Version = '23.150.0.4'; Hw = 'PCI\VEN_8086&DEV_51F0'; Props = @{ '*EEE' = '1' }; Medium = 9 }
    )
    $r = Run-Fake -Name @('Ethernet 4')
    $r.Exit -eq 1 -and
    ($r.Lines -contains 'PASS  Ethernet 4  *EEE  expected=0  actual=0') -and
    @($r.Lines | Where-Object { $_ -like 'FAIL  Ethernet  EEELinkAdvertisement  expected=0  actual=1 (On)  (driver e2fn.sys 2.1.5.7; *' }).Count -eq 1 -and
    ($r.Lines -contains 'SKIP  Ethernet 9  EEE: Not Present') -and
    -not ($r.Lines -match 'Wi-Fi') -and -not ($r.Lines -match 'Dante70') -and
    ($r.Lines -contains '2 wired NIC(s) checked for EEE, 1 FAIL') -and ($r.Lines -contains '1 Realtek NIC(s) checked, 0 FAIL')
}
Check 'EEE on the fake PC: off everywhere exits 0' {
    Set-FakePc @(
        @{ Name = 'Ethernet 4'; Desc = 'Realtek Gaming USB 2.5GbE Family Controller'; Status = 'Up'; File = 'rtucx22x64.sys'; Version = '11.19.602.2025'; Hw = $usb; Props = $resolumePp; Wired = $true },
        @{ Name = 'Ethernet'; Desc = 'Intel(R) Ethernet Controller (2) I225-V'; Status = 'Up'; File = 'e2fn.sys'; Version = '2.1.5.7'; Hw = 'PCI\VEN_8086&DEV_15F3'; Props = [ordered]@{ EEELinkAdvertisement = '0' }; Wired = $true }
    )
    $r = Run-Fake
    $r.Exit -eq 0 -and ($r.Lines -contains '2 wired NIC(s) checked for EEE, 0 FAIL')
}

Check 'a standard that cannot be read exits 2' {
    $lines = @(Invoke-NicStandardCheck -Source (Join-Path $PSScriptRoot 'no-such-standard.json'))
    $script:NicExit -eq 2 -and $lines[0] -like 'FAIL  cannot read the standard*'
}

Write-Host "$($script:passed) passed, $($script:failed) failed"
if ($script:failed -gt 0) { exit 1 }
exit 0

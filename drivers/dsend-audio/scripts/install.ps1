# dsend's own audio output: install the driver and its root device.
# Run as administrator from the unpacked package (dsendaudio.sys/.inf/.cat
# and the WDK test certificate .cer):
#   .\install.ps1
# Needs test signing on (bcdedit /set testsigning on, then a reboot); the
# script checks it and changes nothing when it is off. Run it again to
# update the driver on the existing device.
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$hwid = "ROOT\dsend_audio"

$admin = ([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
if (-not $admin) { throw "run as administrator; nothing changed" }
if (-not ((bcdedit /enum "{current}") -match "testsigning\s+Yes")) {
    throw "test signing is off on this PC (bcdedit /set testsigning on, then reboot); nothing changed"
}
foreach ($f in "dsendaudio.sys", "dsendaudio.inf", "dsendaudio.cat") {
    if (-not (Test-Path (Join-Path $here $f))) { throw "the package lacks $f; nothing changed" }
}
$certs = @(Get-ChildItem $here -Filter *.cer)
if ($certs.Count -eq 0) { throw "the package lacks its test certificate (.cer); nothing changed" }

# SetupAPI's root device creation and driver install (what devcon install
# does), so no WDK tool is needed on the PC.
Add-Type -TypeDefinition @"
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Text;

public static class DsendRootDevice
{
    [StructLayout(LayoutKind.Sequential)]
    struct SP_DEVINFO_DATA
    {
        public int cbSize;
        public Guid ClassGuid;
        public int DevInst;
        public IntPtr Reserved;
    }

    [DllImport("setupapi.dll", SetLastError = true)]
    static extern IntPtr SetupDiCreateDeviceInfoList(ref Guid classGuid, IntPtr hwndParent);

    [DllImport("setupapi.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool SetupDiCreateDeviceInfoW(IntPtr set, string deviceName, ref Guid classGuid,
        string description, IntPtr hwndParent, int flags, ref SP_DEVINFO_DATA data);

    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiSetDeviceRegistryPropertyW(IntPtr set, ref SP_DEVINFO_DATA data,
        int property, byte[] buffer, int size);

    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiCallClassInstaller(int function, IntPtr set, ref SP_DEVINFO_DATA data);

    [DllImport("setupapi.dll", SetLastError = true)]
    static extern bool SetupDiDestroyDeviceInfoList(IntPtr set);

    [DllImport("newdev.dll", SetLastError = true, CharSet = CharSet.Unicode)]
    static extern bool UpdateDriverForPlugAndPlayDevicesW(IntPtr hwndParent, string hardwareId,
        string infPath, int flags, out bool rebootRequired);

    const int DICD_GENERATE_ID = 0x1;
    const int SPDRP_HARDWAREID = 0x1;
    const int DIF_REGISTERDEVICE = 0x19;
    const int INSTALLFLAG_FORCE = 0x1;
    static readonly Guid MediaClass = new Guid("4d36e96c-e325-11ce-bfc1-08002be10318");

    public static void Create(string hardwareId)
    {
        Guid cls = MediaClass;
        IntPtr set = SetupDiCreateDeviceInfoList(ref cls, IntPtr.Zero);
        if (set == new IntPtr(-1)) throw new Win32Exception();
        try
        {
            SP_DEVINFO_DATA data = new SP_DEVINFO_DATA();
            data.cbSize = Marshal.SizeOf(typeof(SP_DEVINFO_DATA));
            if (!SetupDiCreateDeviceInfoW(set, "MEDIA", ref cls, null, IntPtr.Zero, DICD_GENERATE_ID, ref data))
                throw new Win32Exception();
            byte[] ids = Encoding.Unicode.GetBytes(hardwareId + "\0\0");
            if (!SetupDiSetDeviceRegistryPropertyW(set, ref data, SPDRP_HARDWAREID, ids, ids.Length))
                throw new Win32Exception();
            if (!SetupDiCallClassInstaller(DIF_REGISTERDEVICE, set, ref data))
                throw new Win32Exception();
        }
        finally
        {
            SetupDiDestroyDeviceInfoList(set);
        }
    }

    public static bool Install(string hardwareId, string infPath)
    {
        bool reboot;
        if (!UpdateDriverForPlugAndPlayDevicesW(IntPtr.Zero, hardwareId, infPath, INSTALLFLAG_FORCE, out reboot))
            throw new Win32Exception();
        return reboot;
    }
}
"@

# The WDK's test certificate signs the package: trust it on this PC.
foreach ($c in $certs) {
    Import-Certificate -FilePath $c.FullName -CertStoreLocation Cert:\LocalMachine\Root | Out-Null
    Import-Certificate -FilePath $c.FullName -CertStoreLocation Cert:\LocalMachine\TrustedPublisher | Out-Null
    Write-Output "trusted test certificate $($c.Name)"
}

function Get-DsendDevice {
    @(Get-PnpDevice -ErrorAction SilentlyContinue | Where-Object { $_.HardwareID -contains $hwid })
}

# One root device: created once, kept across updates.
$devices = Get-DsendDevice
if ($devices.Count -eq 0) {
    [DsendRootDevice]::Create($hwid)
    Write-Output "created the root device $hwid"
} else {
    Write-Output "root device $hwid exists: $($devices[0].InstanceId)"
}

$reboot = [DsendRootDevice]::Install($hwid, (Join-Path $here "dsendaudio.inf"))
Write-Output "driver installed on $hwid$(if ($reboot) { '; Windows asks for a reboot' })"

$devices = Get-DsendDevice
if ($devices.Count -eq 0) { throw "no device with $hwid after the install" }
$devices | Select-Object Status, FriendlyName, InstanceId | Format-Table -AutoSize | Out-String | Write-Output

# The audio endpoint appears a moment after the driver starts.
$endpoint = $null
foreach ($i in 1..20) {
    $endpoint = Get-PnpDevice -Class AudioEndpoint -ErrorAction SilentlyContinue |
        Where-Object { $_.FriendlyName -like "*dsend*" } | Select-Object -First 1
    if ($endpoint) { break }
    Start-Sleep -Milliseconds 500
}
if (-not $endpoint) { throw "the driver is installed but no audio endpoint named *dsend* appeared within 10 s" }
Write-Output "endpoint: $($endpoint.FriendlyName) ($($endpoint.Status))"
Write-Output "set it as the default output: Settings > System > Sound > Output, or mmsys.cpl > Playback > Set Default"

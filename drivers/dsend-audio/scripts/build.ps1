# Build dsend's audio driver and collect the installable package (CI: the
# dsend-audio-driver job and the release). Run from the repository root on
# a Windows runner with Visual Studio and nuget:
#   .\drivers\dsend-audio\scripts\build.ps1 -Out dsend-audio-package
# The recipe is dasio's ndis-pacer's: the WDK and SDK from NuGet, the 64-bit
# MSBuild, the WDK's default test signing (a fresh WDKTestCert per build).
param(
    [Parameter(Mandatory)][string]$Out
)
$ErrorActionPreference = "Stop"
$root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
$driver = Join-Path $root "driver"

$vs = & "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe" -latest -products * -requires Microsoft.Component.MSBuild -property installationPath
if (-not $vs) { throw "no Visual Studio with MSBuild found" }
# The 64-bit MSBuild: under the 32-bit one the WDK picks its x86 tools,
# which the NuGet WDK does not carry (dasio CI 41361790975).
$msbuild = Join-Path $vs "MSBuild\Current\Bin\amd64\MSBuild.exe"
Write-Output "MSBuild: $msbuild"
# The WDK and SDK from NuGet: the image's Visual Studio has no WDK
# integration (dasio CI 41360597269).
nuget restore (Join-Path $driver "packages.config") -PackagesDirectory (Join-Path $driver "packages") -NonInteractive
if ($LASTEXITCODE -ne 0) { throw "nuget restore failed ($LASTEXITCODE)" }
& $msbuild (Join-Path $driver "dsendaudio.vcxproj") /p:Configuration=Release /p:Platform=x64 /p:SpectreMitigation=false /m /nologo /v:minimal
if ($LASTEXITCODE -ne 0) { throw "driver build failed ($LASTEXITCODE)" }

# The package directory is the one holding the catalog; the test
# certificate sits beside it in the build output.
$release = Join-Path $driver "x64\Release"
$cat = @(Get-ChildItem $release -Recurse -Filter dsendaudio.cat)
if ($cat.Count -ne 1) { throw "expected one dsendaudio.cat under $release, found $($cat.Count)" }
$pkg = $cat[0].DirectoryName
$cer = @(Get-ChildItem $release -Recurse -Filter *.cer)
$thumbs = @($cer | ForEach-Object { (New-Object System.Security.Cryptography.X509Certificates.X509Certificate2($_.FullName)).Thumbprint } | Sort-Object -Unique)
if ($thumbs.Count -ne 1) { throw "expected one test certificate under $release, found $($thumbs.Count) ($($cer.Count) files)" }

New-Item -ItemType Directory -Force $Out | Out-Null
foreach ($f in "dsendaudio.sys", "dsendaudio.inf", "dsendaudio.cat") {
    $src = Join-Path $pkg $f
    if (-not (Test-Path $src)) { throw "the package lacks $f ($pkg)" }
    Copy-Item $src -Destination $Out
}
Copy-Item $cer[0].FullName -Destination $Out
foreach ($f in "scripts\install.ps1", "scripts\uninstall.ps1", "README.md", "LICENSE") {
    Copy-Item (Join-Path $root $f) -Destination $Out
}

# The INF is stamped for x64, and the catalog (which covers the driver) is
# signed by the certificate the package carries (install.ps1 trusts that one).
$inf = Get-Content (Join-Path $Out "dsendaudio.inf") -Raw
if ($inf -notmatch "NTamd64") { throw "dsendaudio.inf is not stamped for amd64" }
$cert = New-Object System.Security.Cryptography.X509Certificates.X509Certificate2((Join-Path $Out $cer[0].Name))
foreach ($f in @("dsendaudio.cat")) {
    $sig = Get-AuthenticodeSignature (Join-Path $Out $f)
    if (-not $sig.SignerCertificate) { throw "$f is not signed" }
    if ($sig.SignerCertificate.Thumbprint -ne $cert.Thumbprint) {
        throw "$f is signed by $($sig.SignerCertificate.Subject), not by the package's $($cert.Subject)"
    }
}
Get-ChildItem $Out | ForEach-Object { "$($_.Name) $($_.Length)" }
Write-Output "signed by $($cert.Subject) ($($cert.Thumbprint))"

# dsend-audio

A Windows virtual audio output of dstack's own, named "dsend" (dstack #184).

**Why.** dsend sends what Windows plays to Dante. It captures an output by
WASAPI loopback, so it needs an output that plays nowhere: Windows and the
browsers play into it, dsend sends the mix to its 2 Dante TX channels, and
nothing is audible locally. A third-party virtual device (VB-Audio Matrix
VAIO) gave dsend's loopback zeros on the FOH PC; this driver replaces it.

**What it does.** A root-enumerated PortCls WaveRT device with one render
endpoint and no capture endpoint and no hardware. It accepts 16-bit stereo
at 48 kHz (the default) or 44.1 kHz in shared mode and consumes the buffer
at the real-time rate: no file, no device, no sound. The audio engine's
loopback tap on the endpoint gives dsend the mix. Windows lists the endpoint
as "Speakers (dsend)"; dsend picks it by default when it exists.

**Install** (as administrator, from the unpacked release zip
`dsend-audio-<tag>-windows-x86_64.zip` or the CI artifact):

    .\install.ps1

The package is test-signed with the WDK's test certificate, made fresh by
each build, so it loads only with test signing on (`bcdedit /set testsigning
on`, then a reboot; Secure Boot off). The script checks it and changes
nothing when it is off. It trusts the package's certificate (LocalMachine
Root and TrustedPublisher), creates the root device `ROOT\dsend_audio` once,
installs the driver on it and prints the endpoint. Run it again to update
the driver. `.\uninstall.ps1` removes the device and the driver package.

Then set "Speakers (dsend)" as the default output (Settings > System >
Sound > Output, or `mmsys.cpl` > Playback > Set Default).

**Layout.**
- `driver/` the driver: Microsoft's simpleaudiosample flattened into one
  WDM project (`dsendaudio.vcxproj`), without the microphone array, the
  tone generator, the render-to-file writer and WDF. The WDK and SDK come
  from NuGet (`packages.config`, `Directory.Build.props`), as for dasio's
  NDIS filter.
- `scripts/` install and uninstall.

**Build** (CI job `dsend-audio-driver`, windows-2025):

    nuget restore drivers\dsend-audio\driver\packages.config -PackagesDirectory drivers\dsend-audio\driver\packages
    MSBuild\Current\Bin\amd64\MSBuild.exe drivers\dsend-audio\driver\dsendaudio.vcxproj /p:Configuration=Release /p:Platform=x64 /p:SpectreMitigation=false

**License.** This directory is a separate program from the rest of dstack:
it derives from Microsoft's Windows-driver-samples (audio/simpleaudiosample),
which is under the Microsoft Public License (MS-PL), so the whole directory
is under the MS-PL (see LICENSE). The rest of dstack stays MIT OR
Apache-2.0.

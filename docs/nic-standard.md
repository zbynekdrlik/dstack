# The Realtek NIC standard (dstack #465)

One standard for every Realtek NIC on a production PC: which driver it runs and every advanced value it holds, each with its evidence. It is part of the production network standard (`docs/two-networks.md`, #155), and dstack owns it. The owner's rule (20.11.2026): one uniform working setup, not a different setting and driver version per project.

- **The file:** [`docs/nic-standard.json`](nic-standard.json), machine-readable, `schema` `dstack-nic-standard/1`, `version` 1. It is the only source: this page explains it, the file decides.
- **The checkers:** `tools/windows/nic-standard-check.ps1` and `tools/linux/nic-standard-check.py`. Both are read-only: they never set a value, install a driver or restart an adapter.
- **Scope:** every Realtek NIC that is up on a PC with a Dante leg (`docs/two-networks.md`, scope: a PC without one is not checked). Other vendors' NICs (the editing PCs' Aquantia and Intel legs) are outside this file.

## How a project uses it

Read the file, or call the checker; never copy its values into your own constants.

- **Pin a tag.** Read `https://raw.githubusercontent.com/zbynekdrlik/dstack/<tag>/docs/nic-standard.json`, the tag your project already pins for the stack. The default URL (`main`) is for a person's manual check only.
- **Windows:** `powershell -ExecutionPolicy Bypass -File nic-standard-check.ps1 -Standard <path|url> [-LatencyProfile low_latency] [-IncludeDown] [-Name <adapter>,...]`. Windows PowerShell 5.1, no module beyond the inbox NetAdapter/PnpDevice cmdlets. The output is plain pipeline text, so a setup script can capture it (`$lines = & .\nic-standard-check.ps1 -Standard $url`).
- **Linux:** `python3 -I nic-standard-check.py --standard <path|url> [--profile low_latency] [--all] [IFACE ...]`. Python 3 and `ethtool`, no root.
- **Only some NICs:** `-Name` (Windows) or `IFACE` arguments (Linux) check just those, up or not; a named one that is missing or no Realtek fails. A PC whose second Realtek NIC is not a Dante leg (dsg's SoundGrid NIC) names its Dante NIC.
- **Exit status:** 0 when every judged value passes, 1 on any FAIL, 2 when the file cannot be read, is not of this schema, or is malformed.
- **The lines:** `PASS` and `FAIL` judge a value and show the expected and the actual value. `INFO` prints a value the standard does not decide yet. `SKIP` marks a value of another profile, or an adapter that is down. Lines other than PASS carry the evidence or the open plan.
- **A Realtek NIC without an entry FAILs** ("no standard for this Realtek adapter"). A new chip, or a known chip on a new OS, is measured and added here before it carries production traffic. The RTL8153 and a Linux RTL8125 have no entry today: no production PC uses one.
- **Profiles:** `normal` (default, every dlink app) and `low_latency` (the iemmixer box, dasio). A value marked `"profile": "low_latency"` (TSO/GSO off on Linux) is checked only with that profile.
- **Setting the values** stays the project's own setup (songplayer's `setup-prod-pc.ps1`, dasio's baseline, camera-box's NetworkManager profiles), written from this file, then proved with the checker.

## The file's shape

```json
{
  "schema": "dstack-nic-standard/1",
  "version": 1,
  "adapters": [
    {
      "id": "rtl8156b-windows", "chip": "RTL8156B", "os": "windows",
      "match": {"hardware_id": ["USB\\VID_0BDA&PID_8156"]},
      "driver": {"files": ["rtucx22x64.sys"], "min_version": "11.19.602.2025", "bad": [...], "evidence": "..."},
      "values": [
        {"key": "PendingTransmits", "name": "Transmit URBs", "expected": "8", "status": "open", "evidence": "...", "plan": "..."}
      ]
    }
  ]
}
```

- `match`: Windows by the device's first hardware id (prefix, any case); Linux by the sysfs `usb` or `pci` vendor:product id.
- `driver`: Windows `files` (the allowed `DriverFileName`), `min_version`, `bad` (a file, or a file and a version, that fails with its evidence); Linux `modules` and an optional `firmware`.
- A value: `key` (the Windows registry keyword; on Linux `feature:<ethtool -k name>`, `ring:rx|tx`, `coalesce:rx-usecs|tx-usecs`, `pause:rx|tx`, `eee:status`, `link:autoneg|speed|duplex`, `usb:power/control`, `pci:power/control`, `usbcore:no_lpm` (the `k` flag of usbcore's `quirks` for this USB id), `firmware`), `expected` (a string, or a list any of which passes), `compare` (`equals`, `bits`: every bit set, `contains`), `absent` (`fail`, or `pass` where a driver without the keyword is fine), `status` (`settled`, `open` with its `plan`, `report`: no value yet, never fails), `profile`, and `evidence`.
- **`evidence`** names a ticket, a measurement or a vendor document. Where nothing measured a value, the standard keeps the value the fleet's working PCs already share and says `none yet: fleet value`. No value is invented.
- **Versioning:** `version` goes up by one with every change of a value, a driver rule or an entry, in the same commit as its evidence. `schema` changes only when the shape changes, and both checkers refuse a schema they do not know. Both checkers validate the file first, with the same case-sensitive rules (Python `validate`, PowerShell `Test-NicStandard`), and exit 2 on a malformed one: an unknown key, a value with no evidence, an open value or driver rule with no plan, a `report` value with an expected value, an unknown status, compare, absent or profile, a repeated key, a non-decimal `bits` value, a Windows driver without `files` or a Linux one without `modules`. CI runs both test suites (`linux-tools`, `windows-tools`).

## The standard, version 1

The file holds every value; this is the summary.

| Chip | OS | Driver | Key values |
|---|---|---|---|
| RTL8156B (USB 2.5GbE; covers the RTL8156) | Windows | Realtek's NetAdapterCx `rtucx22x64.sys` ≥ 11.19.602.2025. Bad: Microsoft's inbox `rtucx21x64.sys` (3-URB queue), Realtek's NDIS `rtump64x64.sys` (19 ms DPCs) | Transmit URBs 8 (**open**), Transmit Buffers 64, Receive URBs 6, Receive Buffers 16; flow control, EEE, Green Ethernet, Advanced EEE, Idle Power Saving, Gigabit Lite, selective suspend all 0; PnPCapabilities with 0x08; LSO 0 where present; speed auto; Priority & VLAN 3; checksum offloads 3 |
| RTL8156B | Linux | `r8152`, in-tree or Realtek's v2.21.4 (**open**, #408) | RX ring 4096; rx-usecs the default 15 µs (never changed on a running link); pause off; EEE off; USB autosuspend off; usbcore NO_LPM quirk (`usbcore.quirks=0bda:8156:k`); TSO/GSO off in the low-latency profile |
| RTL8125 / RTL8168 (PCIe) | Windows | `rt640x64.sys` ≥ 10.79.20.1003, never 10.72.524.2024 (**open**: one version for the family; 10.79 measured clean only with dasio's deferred receive) | interrupt moderation 0; flow control, EEE, green, power saving 0; LSO 0; jumbo 1514; speed auto; wake-on-LAN 0; ASPM 0; PnPCapabilities with 0x08. Low-latency profile (dasio's tuning): Receive and Transmit Buffers 4096 (**open** on the RTL8168), RSS 1 with 4 queues |
| RTL8168 (PCIe) | Linux | the kernel's `r8169`, firmware `rtl8168h-2_0.0.2 02/26/15` | rings 256/256; coalescing 0; pause off; EEE off; runtime PM off; TSO/GSO off in the low-latency profile |
| Realtek Virtual Adapter (the VLAN leg) | Windows | `RtVlan640.sys` ≥ 10.4.1117.2020 | none (its VLAN ID is per PC); never disable or restart one (#239) |

### The open measurement: Transmit URBs 8 or 32

`PendingTransmits` stays **8** in the standard, marked `open`, until this is measured:

- **Why it is open.** On resolume-pp the RTL8156B's driver holds Dante behind NDI's video bursts in its one send queue: with two NDI receivers Dante waits up to 18.4 ms between NetAdapterCx and the miniport (p99 9.4 ms, capture ndiD, #425). 3 → 8 URBs already cut dsg's late packets about 100x. More URBs drain the USB queue faster but do not change Dante's place in it.
- **The method** (#425's, `.claude/rules/windows-pcs.md`): `pktmon` on resolume-pp limited to the TCPIP component on the Dante VLAN adapter, the NetAdapterCx edge and the `rtucx22x64` miniport (`--comp <ids> --pkt-size 64`), as a detached `Start-Process` capture of 60 s or less whose `finally` stops it and removes the filter. Dante packets are timed by their Dante stamp, maxima trusted over counts. The capture runs with two NDI receivers connected (the condition that gave 9.4 / 18.4 ms), at 8 and then at 32, in the same hour.
- **The decision rule:** the value with the lower TCPIP → miniport p99 and max becomes the standard. A change goes into the file with the two captures as its evidence and `version` 2, and every PC moves to it through its project. No single PC changes on its own (the owner, 20.11.2026: no PP-only Realtek change).

## Inventory (20.11.2026, read only)

Every reachable production PC, read through the fleet MCP (`Get-NetAdapter`, `Get-NetAdapterAdvancedProperty -AllProperties`; `ethtool -i/-k/-c/-g/-a/--show-eee`, sysfs). Nothing was changed. iem-snv is sold.

| PC | OS | Realtek NIC in use | Driver | Legs on it | What it holds |
|---|---|---|---|---|---|
| resolume-snv | Windows 11 | RTL8156B USB (REV_31F4) | `rtucx22x64.sys` 11.19.602.2025 | LAN untagged + Dante VLAN (Realtek Virtual Adapter, `RtVlan640.sys` 10.4.1117.2020) | Transmit URBs 8, Transmit Buffers 64, Receive URBs 6, Receive Buffers 16; flow control, EEE, Green, Advanced EEE, Idle Power Saving, Gigabit Lite, selective suspend 0 (hidden keywords: the 11.19 driver does not offer them); PnPCapabilities 24; no LSO keyword |
| resolume-pp | Windows 11 | the same RTL8156B, the same driver and values | `rtucx22x64.sys` 11.19.602.2025 | LAN untagged + Dante VLAN | as resolume-snv. A PCIe RTL8168 (`rt640x64` 10.79.50.1003) is disabled |
| mbc | Windows 11 | RTL8156B USB (REV_31F4) | `rtu56cx22x64.sys` 1156.22.113.2026 | one leg (Dante untagged) | Transmit URBs 3, Transmit Buffers 37, Receive URBs 10, Receive Buffers 32; EEE, Green Ethernet, Idle Power Saving, Gigabit Lite **on**; no `EEE` or `*SelectiveSuspend` keyword; LSO 0; RSC on; wake on magic packet on |
| dsg | Windows 11 | RTL8168H PCIe | `rt640x64.sys` 10.79.50.1003 | Dante untagged + LAN VLAN (virtual adapter) | Receive and Transmit Buffers 4096 (the INF's ranges are 32-512 and 32-128); interrupt moderation, flow control, EEE, green, power saving 0; LSO 0; RSS 1, 4 queues; Priority & VLAN 0; CLKREQ 1. A second RTL8168H (`soundgrid`, SoundGrid's leg, disconnected at the read) holds 512 / 128 and Priority & VLAN 3 |
| FOH | Windows 10 | RTL8125BG PCIe | `rt640x64.sys` 10.79.20.1003 | one leg | Receive and Transmit Buffers 4096 (ranges 32-4096, 64-4096); interrupt moderation, flow control, EEE, green, power saving 0; LSO 0; RSS 1, 4 queues; Priority & VLAN 3; CLKREQ 0 |
| stream-snv | Linux | RTL8156B USB (bcdDevice 31.00) | Realtek's out-of-tree `r8152` v2.21.4 | LAN untagged + Dante VLAN | RX ring 4096; rx-usecs 15 (µs); pause off; EEE off; TSO/GSO on; USB autosuspend off; NO_LPM quirk. Its onboard RTL8168 is down |
| the PP stream PC | Linux | RTL8156B USB (bcdDevice 31f4, firmware rtl8156b-2 v3 10/20/23) | the kernel's `r8152` | LAN untagged + Dante VLAN | RX ring 4096; rx-usecs 15000 (ns, the same 15 µs); pause off; EEE off; TSO/GSO on; USB autosuspend off; NO_LPM quirk |
| the iemmixer box (gen3) | Linux, RT kernel | RTL8168H PCIe | `r8169`, firmware rtl8168h-2_0.0.2 | one leg (Dante) | rings 256/256; coalescing 0; pause off; EEE off; **TSO/GSO off**; qdisc prio. A second RTL8168H is down |
| companion-snv | Linux | RTL8168H PCIe | `r8169`, the same firmware | LAN untagged + Dante VLAN | rings 256/256; coalescing 0; pause off; EEE not supported; TSO/GSO on |
| the PP presenter PC | Linux | RTL8168H PCIe | `r8169`, the same firmware | LAN untagged + Dante VLAN | rings 256/256; coalescing 0; pause off; **EEE enabled** (inactive); TSO/GSO on. A second RTL8168 is down |
| the editing PCs | Linux | none in use | | Dante on Aquantia (`atlantic`) and Intel (`ixgbe`) NICs | their onboard RTL8168H and RTL8125B are down; outside this standard |

Every USB NIC in the fleet is an RTL8156B: the 1156.22 driver's INF matches REV_31F4 to its `RTL8156Bx64` section, and Linux's in-tree driver loads `rtl8156b-2` firmware on bcdDevice 31f4. No RTL8156 (non-B) and no RTL8153 is in use.

Measured against version 1, from these reads: resolume-snv, resolume-pp, dsg's Dante NIC and FOH (low-latency profile, as dasio hosts; dsg's SoundGrid NIC was disconnected, conflict 12), stream-snv, the PP stream PC, companion-snv and the iemmixer box (low-latency profile) pass; **mbc fails 11 values**; **the PP presenter PC fails EEE**.

## Conflicts found

Where PCs or projects disagree today. Each is either decided in the file with its evidence or marked open there.

1. **mbc runs another RTL8156B driver and setup.** `rtu56cx22x64.sys` 1156.22.113.2026 (a newer Cx package) instead of the standard's `rtucx22x64.sys` 11.19.602.2025, with Transmit URBs 3 / Buffers 37 (the 11.19 PCs: 8 / 64), Receive URBs 10 / Buffers 32 (6 / 16), and EEE, Green Ethernet, Idle Power Saving and Gigabit Lite on. The 1156.22 driver was measured only against NDIS (#290), never at 8 / 64 against 11.19. mbc is dasio's PC: dstack does no Windows OS work there, so dasio moves it to the standard, or brings a measurement that puts 1156.22 into the file.
2. **Two rt640x64 versions.** FOH runs 10.79.20.1003, dsg 10.79.50.1003 (dasio#188, the owner's rule on dasio#120: newer drivers are tested and deployed). Both pass `min_version`; one version for the family is the open plan in the file.
3. **The rt640x64 version and dstack's own start line.** The 20-30 ms receive DPC was measured on 10.72 AND 10.79 on FOH's RTL8125, with 10.39.212.2020 clean there (dasio#1, `docs/windows-box.md`). dasio then kept 10.79.20.1003 together with its own deferred receive (`dasio_pacer` RxdEnable: TCP and discovery receive leave the NIC's DPC) and kernel TX, and its baseline records 10.79.20.1003 as best and 10.39 as worse on that path. So 10.79 is clean only on a dasio host. Every rt640x64 PC in the fleet is one, and the file says so; a PC on rt640x64 without dasio's deferred receive is measured before it carries Dante. dstack's start line follows the file (#466, `crates/dstack/src/lso.rs`, the file embedded at build time from `crates/dstack/nic-standard.json`, a copy a test keeps equal to this one): it warns on a bad driver, another file, a version under the minimum or a Realtek adapter without an entry, with the entry's evidence or plan; before #466 it kept its own list, which named 10.79.20.1003 bad and warned on FOH at every start. The checker cannot see dasio's deferred receive: it passes rt640x64 10.79 on any PC, so the "measured first" rule for a non-dasio PC is a process rule, not a check. The standard keeps 10.72.524.2024 bad, which nothing contradicts.
4. **Transmit Buffers in opposite directions on the PCIe Realteks.** dasio's baseline writes 4096 receive and transmit buffers to every rt640x64 Dante NIC; on dsg's RTL8168 that is above the INF's ranges (32-512, 32-128) and what the driver applies is not measured. `docs/windows-box.md` named "Transmit Buffers at the driver's minimum" as FOH's next step, never measured. The standard keeps the fleet's 4096 in the low-latency profile, marked open, with an A/B on dsg as the plan. These and RSS are dasio's tuning, so a normal-profile check skips them; the power, flow-control, offload and interrupt-moderation values hold for every profile (songplayer#285 sets them on every wired NIC).
5. **Priority & VLAN and CLKREQ** differ between dsg and FOH (0 / 3, 1 / 0). No measurement either way: `report`.
6. **Two r8152 drivers on Linux.** stream-snv runs Realtek's v2.21.4, the PP stream PC the kernel's; both lose `rx_missed` bursts next to near-line-rate video. Open in #408. Their `rx-usecs` read 15 and 15000, the same 15 µs in two units: the standard accepts both.
7. **EEE on the PP presenter PC** is enabled (inactive until the partner offers it), off or unsupported everywhere else: FAIL.
8. **TSO/GSO on Linux** are on everywhere but the iemmixer box. `docs/linux-box.md` measured them off on the gen3 RTL8168 for the 250 µs-1 ms profile only; nothing measured them for the normal profile, so the standard checks them only with `--profile low_latency`. The mechanism (one TX ring) applies to the r8152 too.
9. **Two writers with different scopes.** songplayer writes its power-saving values to every wired NIC (songplayer#285) and its RTL8156 queue to a Dante-leg RTL8156; dasio writes its rt640x64 values to the Dante NIC and pins its own driver (`config/baseline-pc-profile.toml`, `[[nic_driver]]`). Both now read this file instead (below).
10. **Hidden keywords.** On the 11.19 driver the power-saving keywords are in the registry but not offered by the driver (songplayer writes them). A value the driver does not declare proves the setting, not the PHY's state (songplayer#337's note); the standard judges the setting, as songplayer's gate does.
11. **The RTL8156B's receive queue.** The 11.19 PCs hold Receive URBs 6 / Receive Buffers 16; mbc reads 10 / 32 today, and read 32 / 256 on 18.11 before the NDIS trial reinstalled its driver (#290; `docs/two-networks.md` noted then that its Receive Buffers were at their maximum, 256). Nothing measured any of them: the standard keeps the working PCs' 6 / 16 as a fleet value.
12. **dsg's SoundGrid NIC.** dsg's second RTL8168H (`soundgrid`) is SoundGrid's leg, not a Dante leg, and was disconnected at the read. When it is up, a whole-PC run checks it too. Read 20.11: its power-saving, flow-control, interrupt-moderation and LSO values match the standard, but its speed is set to 1 Gbit/s full duplex (`*SpeedDuplex` 6, the standard 0), `WolShutdownLinkSpeed` 2 (0) and ARP/NS offload 0 (1), and under the low-latency profile its 512 / 128 buffers fail against the Dante NIC's 4096. Whether a SoundGrid NIC takes the Dante NIC's tuning is dasio's call; until then dasio checks dsg with `-Name` on the Dante NIC.

## The checkers run on real PCs (20.11.2026, read only, the final version 1 file)

resolume-snv, `nic-standard-check.ps1` (exit 0):

```
nic-standard v1 (<tmp>\s.json), profile normal
--    Ethernet 4  RTL8156B (rtl8156b-windows)
PASS  Ethernet 4  driver  expected=rtucx22x64.sys  actual=rtucx22x64.sys
PASS  Ethernet 4  driver:version  expected=>=11.19.602.2025  actual=11.19.602.2025
PASS  Ethernet 4  PendingTransmits  expected=8  actual=8
PASS  Ethernet 4  TransmitBufferLen  expected=64  actual=64
PASS  Ethernet 4  PendingReceives  expected=6  actual=6
PASS  Ethernet 4  ReceiveBufferLen  expected=16  actual=16
PASS  Ethernet 4  *FlowControl  expected=0  actual=0
PASS  Ethernet 4  *EEE  expected=0  actual=0
PASS  Ethernet 4  EEE  expected=0  actual=0
PASS  Ethernet 4  AdvancedEEE  expected=0  actual=0
PASS  Ethernet 4  EnableGreenEthernet  expected=0  actual=0
PASS  Ethernet 4  EnableExtraPowerSaving  expected=0  actual=0
PASS  Ethernet 4  GigaLite  expected=0  actual=0
PASS  Ethernet 4  *SelectiveSuspend  expected=0  actual=0
PASS  Ethernet 4  PnPCapabilities  expected=8  actual=24
PASS  Ethernet 4  *InterruptModeration  expected=0  actual=(absent)
PASS  Ethernet 4  *LsoV2IPv4  expected=0  actual=(absent)
PASS  Ethernet 4  *LsoV2IPv6  expected=0  actual=(absent)
PASS  Ethernet 4  *JumboPacket  expected=1514  actual=(absent)
PASS  Ethernet 4  *SpeedDuplex  expected=0  actual=0
PASS  Ethernet 4  *PriorityVLANTag  expected=3  actual=3
PASS  Ethernet 4  *IPChecksumOffloadIPv4  expected=3  actual=3
PASS  Ethernet 4  *TCPChecksumOffloadIPv4  expected=3  actual=3
PASS  Ethernet 4  *UDPChecksumOffloadIPv4  expected=3  actual=3
PASS  Ethernet 4  *TCPChecksumOffloadIPv6  expected=3  actual=3
PASS  Ethernet 4  *UDPChecksumOffloadIPv6  expected=3  actual=3
PASS  Ethernet 4  EnableUSBInterrupt  expected=0  actual=0
INFO  Ethernet 4  *RscIPv4  expected=(no standard yet)  actual=(absent)  (only mbc's 1156.22 driver has it (on); no measurement)
INFO  Ethernet 4  *WakeOnMagicPacket  expected=(no standard yet)  actual=(absent)  (only mbc's 1156.22 driver has it (on); no measurement)
--    Dante70  Realtek Virtual Adapter (realtek-vlan-windows)
PASS  Dante70  driver  expected=RtVlan640.sys  actual=RtVlan640.sys
PASS  Dante70  driver:version  expected=>=10.4.1117.2020  actual=10.4.1117.2020
2 Realtek NIC(s) checked, 0 FAIL
```

mbc, `nic-standard-check.ps1`, the lines other than PASS (exit 1; long notes cut):

```
--    Ethernet 3  RTL8156B (rtl8156b-windows)
FAIL  Ethernet 3  driver  expected=rtucx22x64.sys  actual=rtu56cx22x64.sys  (Realtek's NetAdapterCx driver 11.19.602.2025 carries the transmit-queue keywords ...)
FAIL  Ethernet 3  PendingTransmits  expected=8  actual=3  (open: 8 vs 32 on resolume-pp with two NDI receivers, by #425's ndiD capture ...)
FAIL  Ethernet 3  TransmitBufferLen  expected=64  actual=37  (with PendingTransmits: the same A/B (songplayer#255/#285, dstack #425))
FAIL  Ethernet 3  PendingReceives  expected=6  actual=10  (none yet: fleet value)
FAIL  Ethernet 3  ReceiveBufferLen  expected=16  actual=32  (none yet: fleet value)
FAIL  Ethernet 3  *EEE  expected=0  actual=1  (power saving off on every wired NIC (owner 7.11.2026, songplayer#285); docs/two-networks.md)
FAIL  Ethernet 3  EEE  expected=0  actual=(absent)  (songplayer#285)
FAIL  Ethernet 3  EnableGreenEthernet  expected=0  actual=1  (songplayer#285; docs/two-networks.md)
FAIL  Ethernet 3  EnableExtraPowerSaving  expected=0  actual=1  (songplayer#285)
FAIL  Ethernet 3  GigaLite  expected=0  actual=1  (songplayer#285)
FAIL  Ethernet 3  *SelectiveSuspend  expected=0  actual=(absent)  (songplayer#285; docs/two-networks.md)
INFO  Ethernet 3  *RscIPv4  expected=(no standard yet)  actual=1  (only mbc's 1156.22 driver has it (on); no measurement)
INFO  Ethernet 3  *WakeOnMagicPacket  expected=(no standard yet)  actual=1  (only mbc's 1156.22 driver has it (on); no measurement)
1 Realtek NIC(s) checked, 11 FAIL
```

stream-snv, `nic-standard-check.py` (exit 0):

```
nic-standard v1 (<tmp>/s.json), profile normal
SKIP  enp56s0  pci 10ec:8168 is down
--    rig0  RTL8156B (rtl8156b-linux)
PASS  rig0  driver  expected=r8152  actual=r8152
INFO  rig0  driver:version  expected=(no standard yet)  actual=v2.21.4 (2025/10/28)  (open: dstack #408: the in-tree r8152 against v2.21.4 on the PP stream PC under the same JPEG XS load, rx_missed steps over a working day)
PASS  rig0  ring:rx  expected=4096  actual=4096
PASS  rig0  coalesce:rx-usecs  expected=15|15000  actual=15
PASS  rig0  pause:rx  expected=off  actual=off
PASS  rig0  pause:tx  expected=off  actual=off
PASS  rig0  eee:status  expected=disabled|not supported  actual=disabled
SKIP  rig0  feature:tcp-segmentation-offload  expected=off  actual=on  (low_latency profile only)
SKIP  rig0  feature:generic-segmentation-offload  expected=off  actual=on  (low_latency profile only)
PASS  rig0  feature:rx-checksumming  expected=on  actual=on
PASS  rig0  feature:tx-checksumming  expected=on  actual=on
PASS  rig0  feature:scatter-gather  expected=on  actual=on
PASS  rig0  feature:generic-receive-offload  expected=on  actual=on
PASS  rig0  feature:rx-vlan-offload  expected=on  actual=on
PASS  rig0  feature:tx-vlan-offload  expected=on  actual=on
PASS  rig0  link:autoneg  expected=on  actual=on
PASS  rig0  link:speed  expected=2500Mb/s  actual=2500Mb/s
PASS  rig0  link:duplex  expected=Full  actual=Full
PASS  rig0  usb:power/control  expected=on  actual=on
PASS  rig0  usbcore:no_lpm  expected=on  actual=on
INFO  rig0  firmware  expected=(no standard yet)  actual=  (the in-tree driver names rtl8156b-2 v3 10/20/23, v2.21.4 names none)
1 Realtek NIC(s) checked, 0 FAIL
```

The PP presenter PC, `nic-standard-check.py`, the lines other than PASS (exit 1):

```
SKIP  enp1s0  pci 10ec:8168 is down
--    enp2s0  RTL8168 (rtl8168-linux)
INFO  enp2s0  driver:version  expected=(no standard yet)  actual=7.0.0-38-generic  (...)
FAIL  enp2s0  eee:status  expected=disabled|not supported  actual=enabled  (power saving off on every wired NIC (owner 7.11.2026, songplayer#285); ...)
SKIP  enp2s0  feature:tcp-segmentation-offload  expected=off  actual=on  (low_latency profile only)
SKIP  enp2s0  feature:generic-segmentation-offload  expected=off  actual=on  (low_latency profile only)
1 Realtek NIC(s) checked, 1 FAIL
```

The iemmixer box with `--profile low_latency`: 1 NIC checked, 0 FAIL (TSO and GSO off pass).

## The consuming projects

Each reads the file at its pinned dstack tag (or calls the checker) instead of its own constants, and keeps setting the values in its own setup.

| Project | Its PCs here | What it changes |
|---|---|---|
| songplayer | resolume-snv, resolume-pp | `RTL8156_VALUES`, `ALWAYS`, `WHERE_PRESENT` and the 11.19 driver gate (`prod_env_audio_wired.rs`, `setup-prod-pc.ps1`) read entry `rtl8156b-windows`. Both PCs pass. PP's `PendingTransmits` 32 experiment stays cancelled; 8 vs 32 is decided here. |
| dasio | FOH, dsg, mbc | the baseline's rt640x64 keys and `[[nic_driver]]` pin read entry `rtl8125-rtl8168-windows`; mbc fails 11 values (conflict 1); FOH and dsg on one driver version (conflict 2); the RTL8168 buffers' A/B (conflict 4). |
| camera-box | stream-snv, the PP stream PC (obs runs there) | its r8152 ring, coalescing and USB quirk settings read entry `rtl8156b-linux`; both pass; the driver choice stays #408. |
| presenter | companion-snv, the PP presenter PC | its setup reads entry `rtl8168-linux`; the PP presenter PC fails EEE (on). |
| iemmixer | the iemmixer box | its NIC checks call the checker with `--profile low_latency`; it passes. |
| wall-bridge, intercom, obs, fohmixer | no Realtek PC of their own in the inventory | any PC they set up runs the checker; a Realtek chip without an entry fails until dstack measures and adds it. |

## Changing the standard

1. A measurement on a PC (or a ticket's evidence) shows a better value or driver.
2. The value goes into `docs/nic-standard.json` with that evidence, `version` + 1, in one dstack commit; both checkers' tests stay green.
3. The release notes name the change; the consumers move with the weekly consumer sync (`.claude/rules/release.md`) and their setups write the new value, proved by the checker.

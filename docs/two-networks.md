# The two-network standard: Dante and the LAN on one PC (dstack #155)

This is the one procedure for every production PC that touches the Dante network. The owner ruled on 10.11.2026 that no project invents its own: the OS, the VLANs, the firewall and the behaviour of our applications follow this page, and every project points here instead of writing its own variant. dstack owns this page. A project that needs something it does not cover asks dstack, and dstack changes the page.

**Scope: only a PC with a Dante leg** (owner, 11.11.2026). Everything on this page applies only on a PC that runs a Dante device (an app with dlink or dstack inside, dasio, iemmixer, Dante Virtual Soundcard, a Dante card) and so has a Dante leg: the layout, the NIC and driver values, the firewall group, dsync's interface pin and the checks in §6. Dante is one audio path among others (ASIO, SoundGrid, VBAN), and the two-leg layout is the preferred layout, not a precondition. A PC without a Dante leg is not checked against this page and passes; a setup script or gate that fails such a PC is wrong. A project that takes a requirement over from here gates it on the same condition, and dstack names the condition with every requirement it hands over.

The goal: nothing reaches anything by chance. Which address a library picks after a restart, which NIC enumerates first, or which cable is in which port must never decide whether audio, video or control works.

The site's own values (subnets, VLAN numbers, host names, addresses, switch ports) are not in this public repo; they are in `~/.config/dstack/SITE.md` on dev1. Below, `<dante>` is the Dante VLAN's subnet and `<lan>` is the LAN's.

## 1. Network layout

- Two networks: the Dante VLAN (audio, PTP, Dante control) and the LAN (everything else: NDI, web UIs, remote access, updates, file copies).
- The router routes between them with no filter. So both addresses of a PC are reachable from the other side, and only the PC and the apps decide which one is used. That is why the rules below exist.
- The switches give DSCP 56 (PTP) and 46 (audio) strict priority on egress. That cannot help inside a PC whose single queue carries everything, which is why Dante gets its own leg.

## 2. One cable per PC

- **Every PC has exactly one physical NIC in use.** A second NIC only with the owner's explicit approval for that PC (owner, 9.11.2026).
- **Port:** the LAN untagged and the Dante VLAN tagged on the same cable (the switch port carries both).
- **Which PCs:** only a PC that needs both networks gets the two-leg layout, and **only with the owner's explicit approval for that PC** (owner, 10.11.2026). A PC that is on the Dante network only stays as it is unless the owner decides otherwise for it.
- The switch side is the network project's (mikrotik); dstack asks for each port and tells it when the PC side is ready.
- **Every other NIC is disabled in the OS** (onboard ports, Wi-Fi, old USB adapters), not just unplugged. A NIC that comes back by itself takes addresses and routes.

## 3. The two legs on the PC

| | LAN leg (untagged) | Dante leg (VLAN, tagged) |
|---|---|---|
| Address | DHCP, a fixed lease on the router | DHCP with a fixed lease, or static when the leg's MAC is not stable (below) |
| Default route | yes, the only one | none |
| DNS servers, DNS registration, NetBIOS | yes | none |
| Interface metric | low (automatic) | 900, fixed |
| File/print sharing, network client | as needed | unbound |
| Discovery: NDI, Bonjour/mDNS of non-Dante apps, web UIs | here only | never |
| Traffic | everything else | Dante, PTP and Dante's mDNS only |
| `.lan` name | its LAN address | none |

**Which leg is tagged.** The default is the LAN untagged and Dante tagged. A Windows PC with a low-latency Dante host (dasio: ASIO, Npcap, interrupt tuning on the physical NIC) keeps **Dante untagged on the physical adapter** and gets the **LAN as the tagged virtual adapter**, so the audio path, its NIC tuning and its stable MAC stay as they are. The table's rows hold for each leg whichever way it is tagged.

### Linux (NetworkManager)

- The LAN connection holds the only default route and the DNS.
- The Dante leg is an NM VLAN connection on the same NIC (`<nic>.<vlan>`): `ipv4.never-default yes`, `ipv4.ignore-auto-dns yes`, `ipv6.method disabled`, its connected route in a policy table with a rule `from <dante address> lookup <table>` (the address, not the subnet), so the main table has no route to `<dante>` and only sockets bound to the Dante address use the leg.
- Load `8021q` and persist it (`/etc/modules-load.d/8021q.conf`); without it NM fails with "Failed to find a compatible device".
- On a netplan host, NM may rewrite the VLAN's parent to a stale connection UUID: set `vlan.parent` to the active LAN connection's UUID.
- Route metric 900 on the leg's connection (`ipv4.route-metric 900`), so the numbers match Windows even though the main table has no `<dante>` route.
- nftables on the Dante leg (dcontroller's `dante-guard` is the reference implementation):
  - in: `ct state established,related`; IGMP; multicast UDP (PTP, Dante info and heartbeat groups, multicast audio); the Dante ports of the PC's devices (ARC 4440+, flow control 4455+, info 8700/8702/8708, CMC 8800+, audio 30000+/30100+); PTP 319/320; mDNS 5353 and **`udp sport 5353 accept`**; a controller's own receive ports (dcontroller's meter ports 8810–8825); DHCP 68 only on a leg that uses DHCP; remote access: TCP 22 (ssh) and 8092 (the remoteos MCP) from anywhere, no source or uid narrowing (owner, 13.11.2026, #239); IPv6 dropped; everything else dropped. A leg without conntrack (no `ct state`, e.g. to keep NDI traffic out of conntrack) accepts replies by source port over the whole slot ranges: `udp sport { 4440-4471, 4455-4486, 5353, 8700, 8800-8831 } udp dport 32768-60999`. Never accept by the base ports alone, because a device in slot k answers from 4440+k, 4455+k and 8800+k. On 11.11.2026 a filter that accepted only 4440/4455/8800 dropped a transmitter's flow answers from slot 1 (4456), and a stream PC's Dante input looped request, no answer, expire every 6 s for 18 hours, until 12.11 05:54Z;
  - out: only the PC's Dante programs (by user or program) and PTP 319/320, IGMP, DHCP 67/68, and only to `<dante>` or the groups 224.0.0.231/.233/.251 and the PTP/audio multicast groups; the replies of the accepted ssh and MCP connections (`ct state established,related`, or `tcp sport { 22, 8092 }` on a leg without conntrack); on an admin host that reaches other PCs on their Dante address (dev1), TCP 22 and 8092 out to `<dante>`; everything else dropped;
  - a device that transmits (TX flows) behind a leg WITHOUT conntrack sets `tx_port_base` (dlink: `Config::tx_port_base`), and the leg accepts that range from `<dante>`: `ip saddr <dante> udp dport <tx_port_base>-<tx_port_base + 31> accept`. A receiver sends its keepalives to the TX flow's socket from an ephemeral port; unset, that socket's port is ephemeral too, the filter cannot name it, the keepalives are dropped and the flow expires every few seconds while audio still goes out (dstack #262, the stream PC, 13.11.2026). Pick the range clear of `rx_port_base`'s 32 ports, of the slot ranges (4440-4486, 8800-8831) and of the OS's ephemeral range (Linux 32768-60999, Windows 49152-65535); the device refuses an overlap with the first two at start. On a leg with conntrack, check the same before relying on `ct state established,related`: a keepalive arrives from a port our flow's audio was never sent to, so it is not part of that flow's tracked conversation; a leg whose rules accept the PC's TX ports some other way needs nothing more;
  - a guard that allows only one program out must also allow every Dante device on the PC (a dlink app under another user, for example), with its ports in.
  - every Dante app on a guarded PC runs as its own service user (presenter as `presenter`, dcontroller as `dcontroller`), never as the login user: the guard names each by its uid (`meta skuid`), so a uid shared with the login user would let everything that user runs out. A cgroup match is not workable: nft resolves the cgroup at load, and a service restart breaks it (dcontroller#64).

### Windows

- **The NIC must do VLANs in its driver.** Intel's PROSet/ANS VLANs do not exist on Windows 11 and Windows 11 client editions have no LBFO, so an Intel I225/I226 on Windows 11 cannot carry two VLANs without Hyper-V. Use a Realtek NIC (the site's USB 2.5G adapter, RTL8156) with Realtek's **Ethernet Diagnostic Utility** (2.0.8.3 or newer, signed by Realtek), which creates a "Realtek Virtual Adapter" per VLAN. No Hyper-V on production PCs.
- The base adapter keeps the LAN (untagged); the virtual adapter is the Dante leg. Name it after the VLAN (`Dante70`), not `Dante`: an old adapter may still hold that name.
- The virtual adapter appears only after one restart of its base adapter, and **it takes a new random MAC at every restart of the base adapter**, so a DHCP reservation cannot follow it: give the Dante leg a **static** address, no gateway, and keep the router's lease for that address only as a reservation so the pool never hands it out.
- On the Dante leg: interface metric 900, "Register this connection's addresses in DNS" off, NetBIOS off (`NetbiosOptions=2`), File and Printer Sharing, Client for Microsoft Networks, IPv6, LLTD unbound.
- Set the base NIC's power and offload values with the app's own setup (EEE, green Ethernet, selective suspend, flow control off); they do not apply to the virtual adapter.
- Changing a static address with `New-NetIPAddress -DefaultGateway` fails on a stored route ("Instance DefaultGateway already exists"); `netsh interface ipv4 set address` changes it in one step.
- **The vendor VLAN tool never stays running** (dstack #239). One NIC carries both legs (the owner's decision, 12.11.2026: two NICs were left on purpose, for cabling and switches), so the PC must not depend on the tool after setup. Close the Realtek utility (`8169DiagV2.exe`, window "RTDiagLibV2") as soon as the VLAN adapter exists, and hide its shortcuts. A dialog it shows later ("LanSetV6 not found") re-applies its VLAN state when clicked and can cut the LAN leg. Whether it can be uninstalled without losing the VLAN adapter is tested on a non-production PC first.
- **A USB replug changes the Realtek VLAN adapter's MAC**, as a restart of its base adapter does, and it does not reset the base adapter's settings: a replug is no recovery.
- **Never disable or restart a Realtek Virtual Adapter** (dstack #239). `Disable-NetAdapter` on one (driver 10.4.1117.2020, over a Realtek Gaming 2.5GbE base) bugchecked dasio's test PC on 13.11.2026 (Kernel-Power 41, no dump), and `Restart-NetAdapter` is a disable plus an enable. Change its VLAN ID, bindings and DHCP in place (`Set-NetAdapterAdvancedProperty -NoRestart`, `Enable-NetAdapterBinding`, `Set-NetIPInterface`) and renew its lease; never by hand "to test", never in a script. Restarting the base adapter in layout A was tested on an RTL8156 USB base adapter (the SongPlayer PC, 13.11.2026); another base NIC or driver is tested on a non-production PC first.

#### The LAN self-heal task (one-NIC PCs)

A headless one-NIC PC whose LAN leg fails keeps only ssh and the MCP on its Dante address (the firewall below); remote desktop, NDI, web UIs and updates are gone until the LAN is back. To reach it then, bind the source to your own Dante address (`ssh -b <dev1's Dante address> <user>@<the PC's Dante address>`): dev1's main table routes `<dante>` through the LAN gateway with its LAN address, which the PC cannot answer while its LAN leg is dead (its Dante leg has no gateway, section 6.3). The task works with both layouts of section 3, and the config says which one the PC has (`lanVlanId`):

| Layout | Physical (base) adapter | VLAN adapter (Realtek Virtual Adapter) | `lanAdapter` | `danteAdapter` | `lanVlanId` |
|---|---|---|---|---|---|
| A, the default (the SongPlayer PC) | LAN, untagged | Dante, on the Dante VLAN | the base adapter | the VLAN adapter | `0` |
| B, a dasio host (dasio's test PC) | Dante, untagged | LAN, on the LAN VLAN N | the VLAN adapter | the base adapter | `N` |

In layout B a disabled or broken base adapter is not repaired: it is the Dante adapter, which the task never touches; the LAN's VLAN adapter on top of it cannot work until the base adapter does.

`tools/windows/lan-selfheal.ps1` (Windows PowerShell 5.1) runs as SYSTEM from a scheduled task, "dstack LAN self-heal", every minute and at startup, with no window. Each run checks the LAN leg itself: the LAN adapter holds a usable IPv4 address (not APIPA), the route to the LAN gateway goes out of the LAN adapter, and the gateway answers a ping (three tries, one second each). A ping answered over another leg proves nothing (section 5).

- a healthy leg resets the down-counter and logs "gateway ok";
- after the threshold (3) of consecutive failed minutes, it puts the LAN adapter back, in order: adapter enabled; the LAN adapter's VLAN ID set to the config's `lanVlanId` when it holds another value (empty counts as 0), through the LAN driver's own "VLAN ID" property (found by its display name; the keyword is the driver's: `RegVlanID` on the RTL8156's base adapter, `VlanID` on the Realtek Virtual Adapter). A LAN adapter without that property (its VLAN set elsewhere) is not changed: the run logs a "VLAN mismatch" WARN with the VLAN the adapter reads (`Get-NetAdapter`'s `VlanID`) when it differs, and guesses nothing; IPv4 (`ms_tcpip`) bound; DHCP on; `Restart-NetAdapter`. Then it counts again, so a LAN still dead is repaired again 3 minutes later.
- in layout A the restart also restarts the VLAN adapter on top of it (a new MAC, its static address unchanged): Dante drops for a few seconds. So it restarts when a step changed something, and otherwise (a router, switch or DHCP server outage, with or without a lease) at most once per 30 minutes; the state file keeps the last restart's time (UTC). Tested on the SongPlayer PC (13.11.2026).
- **in layout B the LAN adapter is a Realtek Virtual Adapter, which is never disabled, enabled or restarted** (since 0.6.115; the paragraph above on the bugcheck). The task reads the LAN adapter's `InterfaceDescription`: "Realtek Virtual Adapter", or one naming a VLAN or a vendor's virtual adapter, is a virtual VLAN adapter. Its repair is only: its VLAN ID (with `-NoRestart`), IPv4 bound, DHCP on, then `RenewDHCPLease` on its `Win32_NetworkAdapterConfiguration` (by interface index), on every repair. The log says `Restart: skipped, the LAN adapter is a virtual VLAN adapter ('<description>')`. The VLAN ID is written with `-NoRestart`, so when the repair had to change it the log adds a WARN (it may apply only when the adapter re-initializes: reboot the PC if the LAN stays down) and the run exits 1. A LAN adapter whose description cannot be read is repaired the same way, with a WARN (fail closed). A disabled virtual LAN adapter is not enabled: the run logs a WARN, does the other steps and exits 1; enable it by hand. The install's log line names the kind of LAN adapter (`a virtual VLAN adapter ...: its repair never restarts it` or `... its repair may restart it`).

It never touches the Dante adapter: every step names the LAN adapter, a guard refuses a step that names the Dante adapter, the config refuses adapter names with wildcard characters (`* ? [ ]`, which the NetAdapter cmdlets would expand), and no run goes on unless the LAN name is exactly one adapter and the Dante name is not several adapters or the LAN adapter's own interface. A missing Dante adapter (removed by a vendor tool, or hidden while its parent is disabled) only logs a WARN: the LAN adapter is still repaired by its exact name. It never acts without its config. Each run's decision and each change go to `C:\ProgramData\dstack\lan-selfheal.log`, rotated at 1 MB (one old file, `.log.1`); an unexpected error is an ERROR line and exit 1. The down-counter lives in `lan-selfheal.state.json`, written to a `.tmp` file with write-through and swapped in (since 0.6.115); a state file that is unreadable all the same (the NUL bytes a crash left on dasio's test PC) is logged as `state file unreadable, counting from 0` and the count starts again.

The config is the PC's own (site data, never in the repo), `C:\ProgramData\dstack\lan-selfheal.json`:

```json
{ "lanAdapter": "<LAN adapter name>", "gateway": "<LAN gateway IPv4>",
  "danteAdapter": "<Dante adapter name>", "thresholdMinutes": 3,
  "lanVlanId": 0 }
```

`lanVlanId` is required (since 0.6.105): `0` for a LAN untagged on the base adapter (layout A), `N` for a LAN on a VLAN adapter on VLAN N (layout B). A config without it, or with anything but a whole number from 0 to 4094, is refused: the run logs `ERROR config: 'lanVlanId' ...: nothing done` and exits 1, and the install stops. Read the LAN adapter's VLAN with `Get-NetAdapter <name> | Select VlanID` (empty = 0). v0.6.104 had no such key and set VLAN 0 on the LAN adapter in every case, which would cut the LAN in layout B.

Read the gateway with `Get-NetRoute -DestinationPrefix 0.0.0.0/0`. Install from an elevated shell, after writing the config: `powershell -ExecutionPolicy Bypass -File lan-selfheal.ps1 -Mode install`. It checks the config and both adapters, copies the script to `C:\ProgramData\dstack\`, sets that directory's owner to Administrators, resets its ACL to SYSTEM and Administrators full and Users read (and reads every entry back, refusing a junction and any other owner or writer, generic rights included), registers the task, and reads the task back (SYSTEM, every minute without end, the action). `-Mode uninstall` removes the task and keeps the config and log (it first runs the 0.6.109 cleanup below, and refuses if a `dstack155` rule does not come back on). Check it with `Get-ScheduledTaskInfo 'dstack LAN self-heal'` (LastTaskResult 0) and the log's "gateway ok" lines. A PC's acceptance test cuts its LAN leg on purpose and watches the log repair it: in layout A by disabling the base adapter, in layout B never by disabling the Realtek Virtual Adapter (unbind its IPv4, or take the LAN VLAN off the switch port).

#### Upgrading from 0.6.109 (the emergency path is gone)

0.6.109 had an emergency path: while the LAN was down it opened ssh on the Dante address for one host, by disabling the `dstack155` TCP-in rule and enabling two Block rules of its own (group `dstack emergency`). Since 0.6.111 ssh and the MCP are always reachable there (the firewall below), and the path is removed. A config that still carries `emergencySource`, `emergencyPort` or `danteBlockRule` is accepted; each run logs them as ignored (`config: ... ignored ...; remove them`). Every run, LAN up or down, also undoes what the old path may have left: a `dstack155` rule it disabled (by the state file's `emergencyRule`, and by the config's `danteBlockRule` name while one of its emergency rules is still on) is enabled **first** and read back, and only then are the rules `dstack-emergency-block-others` and `dstack-emergency-block-source-ports` removed. If the rule does not come back on, the emergency rules stay (they only block), the state keeps the rule's id, and the run logs an ERROR and exits 1; the LAN check and repair run either way. A run without a usable config does nothing, the cleanup included (the task never acts without its config); `-Mode uninstall` still runs the cleanup, from the state file when the config is missing or unreadable. Remove the three keys from the config when installing 0.6.111.

**12.11.2026 (dstack #239):** the Realtek utility, left running on the SongPlayer PC since its VLAN setup two days earlier, showed "LanSetV6 not found"; a click on OK re-applied its VLAN state and the LAN leg stopped carrying traffic, while Dante kept running. With the LAN gone there was no remote path (no MCP, RustDesk or ssh), a USB replug changed only the VLAN adapter's MAC, and the owner had to bring a monitor and keyboard to re-enable IPv4 on the LAN adapter. dstack then closed the utility.

### Firewall (Windows)

The Windows firewall is **on**, with **default Allow in and out** on every profile, plus one rule group `dstack155` bound only to the Dante leg: block in and out for every non-Dante program on the PC (OBS, Resolume Arena, Bonjour `mDNSResponder.exe`, `svchost.exe`, remote-access tools, scripting runtimes) and all TCP, **except inbound TCP 22 (ssh) and 8092 (the remoteos MCP)**: these stay reachable on the Dante address from anywhere, like on the LAN (owner, 13.11.2026, #239: they are not Dante traffic, and remoteos has that access anyway). An explicit Block rule beats every Allow rule on Windows, so the exception is made by narrowing the Block rules, never by an Allow rule:
- the TCP-in rule blocks local ports `1-21`, `23-8091` and `8093-65535` only (`Set-NetFirewallRule -LocalPort '1-21','23-8091','8093-65535'`); the TCP-out rule stays whole (a reply on an accepted connection is not filtered by it, the firewall is stateful);
- the inbound rule of the program that serves the MCP (the remoteos server runs as `python.exe`, a scripting runtime) blocks UDP only (`-Protocol UDP`), since an all-protocol program rule would also block TCP 8092; its TCP in is the TCP-in rule's job. Its outbound rule stays. `sshd.exe` is its own process and is in no program rule.

A script that recreates the group keeps both exemptions (dstack #254, remoteos-mcp#96: remoteos set them on the SongPlayer PC). An Allow rule for 22 or 8092 on the Dante leg (remoteos's `OpenSSH on the Dante leg`) is harmless but opens nothing by itself: the default is Allow, and only the narrowed Block rules make the ports reachable.

The Dante devices' processes (an app with dlink or dstack inside, dasio) are never in the group. No other enabled Block rule exists. This replaces the earlier "all profiles off" rule (songplayer #233): on 10.11.2026 an hour with the firewall on against an hour off showed no change in the sender's pump or lateness and none at three receivers (owner's decision, #155). Firewall rules bound to a removed NIC stay in the store and do nothing; remove them. A PC's setup script may recreate the group from this list, never turn the profiles off.

## 4. Rules for our applications

These hold in every project's code, so that the PC's layout is a second line of defence, not the only one.

- **A Dante device binds every socket to its Dante address** (dstack, dlink apps, dasio, iemmixer do). It needs no gateway and no DNS on that leg. It is configured by its address or its interface. Started before that address or interface is there (a boot or logon before DHCP or the VLAN is up), dstack-dev, dsend, dlisten, dasio and iemmixer (`embed::Embedded`) wait for it, looking it up every second with no time limit, and log "waiting for the device's address to appear on an interface" at the start and once a minute (dstack #225). dsend and dlisten given no `--ip` or `--iface` wait the same way for the default route's address ("waiting for the default route's address to appear", dstack #230). A dlink app chooses: `Link::start` fails at once on a place not there ("no interface of this box holds the address"), for an app that retries on its own; `Link::start_until(.., stop)` waits until the app sets `stop`, then returns "stopped while waiting for the device's address" within a second (dstack #230).
- **Dante's own mDNS** is direct queries answered unicast; a filter on the Dante leg must pass UDP from source port 5353 (above).
- **A non-Dante sender (NDI, web servers, OSC, Bonjour services) binds and advertises the LAN address only.** NDI: `adapters.allowed` in `ndi-config.v1.json` lists only the LAN address. The NDI 6.3 runtime does not hold to it: its sender listens on all addresses and can announce the Dante address too (seen 10.11.2026 on a SongPlayer started long after the Dante leg), and an app that registers through Bonjour announces on every adapter. So the guard is the Dante leg's firewall group: all TCP in on the Dante leg but ssh (22) and the MCP (8092) is blocked whatever the program (that closes NDI's data path there, and the group needs no exception for an app that is also a Dante device), and Bonjour does not answer there; a live mDNS query from the Dante VLAN then gets no NDI answer. What remains is an announcement a receiver may have cached, which the receiver rules below make harmless. The in-process NDI runtime also sends a periodic unsolicited announcement out of every interface (measured 12.11.2026, songplayer#309: one 188-byte packet every 10 s from the Dante address, port 5353). It cannot be filtered without blocking the same program's Dante mDNS. This is an allowed exception: about 19 B/s, ignored by Dante devices and PTP, never answered on the Dante leg, with its TCP blocked there. An app does not need to move its NDI sender into a separate process for it.
- **An app that is both a Dante device and a LAN service** (SongPlayer: Dante output in process, web UI and remote control on the LAN) keeps its LAN services off the Dante leg in its own code: it either binds them to the LAN address and 127.0.0.1, or (robust to a LAN address that changes) listens on all addresses and closes at once any connection whose local address is inside `DANTE_SUBNET`; any mDNS it does itself is on the LAN only, and its Dante device binds the Dante address. Its LAN address is the default-route interface's, never "the lowest private address". The firewall group never carries per-port exceptions for such an app.
- **A receiver never picks a peer's address by chance.** It dials the peer's LAN address it has verified (presenter#925: NDI by URL address on the LAN), never "whatever the name resolves to first", and it never uses an address in `<dante>` for non-Dante media or control.
- **Discovery that lists several addresses for one name is filtered by subnet** before use: `<lan>` for everything except Dante.
- **Every app knows `<dante>` from its configuration**, not only from its own interfaces: a PC without a Dante leg (a dev box, another site) cannot tell `<dante>` from any routed subnet, and the router would carry the connection. The setting is one value per app, `DANTE_SUBNET` (environment) or `dante_subnet` (config file), holding the Dante VLAN's subnet in CIDR; addresses in it are never used for non-Dante traffic.
- **dsync** is pinned to the Dante leg by name (`system.interface`, dsync 1.27 or newer) on a PC where the grandmaster is also seen on the LAN; without the pin it may follow PTP over the LAN.

## 5. Order of a change on one PC

1. dstack plans the PC on #155, the owner approves that PC, and dstack tells, before anything changes: the PC's owning project, presenter (for any PC that sends NDI), and every project with a device or receiver on that PC.
2. The network project adds the VLAN to the PC's port (the LAN stays as it is, so the PC keeps its connection).
3. The PC side as in section 3, by the safe procedure below.
4. Disable the other NICs; the owner removes the hardware.
5. Restart every app that binds or advertises on the network (NDI senders, Dante apps), so they see the final layout.
6. Verify (section 6), then record the PC's result on #155.

### The safe procedure for a VLAN change on Windows (never cut a PC off)

Two PCs on 10.11.2026 show what goes wrong: on resolume-snv (USB RTL8156, Cx driver) the base adapter kept its untagged leg after a VLAN adapter was added; on iem-snv (PCIe RTL8125, NDIS 10.80) the same step **unbound TCP/IP from the base adapter**, its untagged Dante leg died, the PC answered only on the new VLAN's DHCP address, and a watchdog that pinged the router "over any route" saw it answer through the new leg and never restored anything. Launching the Realtek utility's window from a remote shell also took the remote control away for 4 hours. So:

1. **First on a non-production PC with hands or a console at it**, never first on a production PC; a new NIC model or driver is tried there before any other PC.
2. **Know a second way in before the change**: the PC's remote control must reach it on the address it will keep, and someone must be able to sit at it (or a console path) for the case where both legs fail.
3. **A rollback that runs by itself, from the PC's own scheduler**: a scheduled task (not a process started from the remote shell, which can die with it) that, unless a "keep" file appears within a fixed time, deletes the VLAN adapter and restores the saved adapter settings and bindings.
4. **The rollback tests the exact leg, by its own address**: the untagged leg's address is still on the base adapter, TCP/IP is bound there, and a ping **from that address** to a host on the same subnet answers; and the same for the new leg. A ping "to the router" over any route proves nothing.
5. **Create the VLAN in the utility through the desktop** (the remote-control GUI tools), never by starting the utility from the remote shell, and close the utility right after (it never stays running, section 3).
6. **Right after the adapter appears, re-check the base adapter's bindings** (`Get-NetAdapterBinding -ComponentID ms_tcpip`): if the driver unbound them, bind them again at once, or make both legs virtual adapters (an untagged one and the tagged one) and move the base's address onto the untagged adapter.
7. **Only then write "keep"**; a failure at any step lets the scheduled rollback run.
8. **On a one-NIC PC, install the LAN self-heal task** (section 3) once both legs work, and check its log reads "gateway ok".

## 6. Checking a PC (the rig/E2E gates assert this)

Only on a PC with a Dante leg; a PC without one skips every item (scope above).

1. Exactly one physical NIC up; every other NIC disabled.
2. Exactly one default route, through the LAN leg.
3. The Dante leg: its address, no gateway, metric 900, no DNS registration, NetBIOS off, sharing/client unbound (Windows); `never-default`, policy table, no `<dante>` route in the main table (Linux).
4. `<host>.lan` resolves to the LAN address only.
5. Nothing non-Dante answers on the Dante address but ssh and the MCP: a TCP connect from another host's Dante leg to the PC's TCP 22 and 8092 succeeds, to its NDI and web ports (and any other port, e.g. 445) times out, and the same connect to its LAN address succeeds.
6. A passive mDNS listen on the Dante VLAN shows the PC's Dante devices and none of its NDI or other services.
7. The PC's Dante devices have live flows, and its receivers' late counters stay at zero over a soak.

## Single-NIC PCs that are still on Dante only

Until such a PC has its LAN leg, all its traffic shares the Dante queue. Keep bulk traffic small, turn large send offload off and set the transmit buffers to the minimum (`docs/windows-box.md`) on the NIC that carries Dante, and only where its driver offers the setting: the keyword and its range belong to the driver, not the chip (two RTL8156 adapters on different drivers differ), so a driver without it passes, and run the Dante devices at their priority. Our device warns once a minute when other traffic on its NIC exceeds 100 kB/s, and at its start when its Dante interface holds a default route.

# The two-network standard: Dante and the LAN on one PC (dstack #155)

This is the one procedure for every production PC that touches the Dante network. The owner ruled on 10.11.2026 that no project invents its own: the OS, the VLANs, the firewall and the behaviour of our applications follow this page, and every project points here instead of writing its own variant. dstack owns this page. A project that needs something it does not cover asks dstack, and dstack changes the page.

The goal: nothing reaches anything by chance. Which address a library picks after a restart, which NIC enumerates first, or which cable is in which port must never decide whether audio, video or control works.

The site's own values (subnets, VLAN numbers, host names, addresses, switch ports) are not in this public repo; they are in `~/.config/dstack/SITE.md` on dev1. Below, `<dante>` is the Dante VLAN's subnet and `<lan>` is the LAN's.

## 1. Network layout

- Two networks: the Dante VLAN (audio, PTP, Dante control) and the LAN (everything else: NDI, web UIs, remote access, updates, file copies).
- The router routes between them with no filter. So both addresses of a PC are reachable from the other side, and only the PC and the apps decide which one is used. That is why the rules below exist.
- The switches give DSCP 56 (PTP) and 46 (audio) strict priority on egress. That cannot help inside a PC whose single queue carries everything, which is why Dante gets its own leg.

## 2. One cable per PC

- **Every PC has exactly one physical NIC in use.** A second NIC only with the owner's explicit approval for that PC (owner, 9.11.2026).
- **Port:** the LAN untagged and the Dante VLAN tagged on the same cable (the switch port carries both). A PC that is on Dante only today gets the other network as a second VLAN on its cable.
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
  - in: `ct state established,related`; IGMP; multicast UDP (PTP, Dante info and heartbeat groups, multicast audio); the Dante ports of the PC's devices (ARC 4440+, flow control 4455+, info 8700/8702/8708, CMC 8800+, audio 30000+/30100+); PTP 319/320; mDNS 5353 and **`udp sport 5353 accept`**; a controller's own receive ports (dcontroller's meter ports 8810–8825); DHCP 68 only on a leg that uses DHCP; IPv6 dropped; everything else dropped;
  - out: only the PC's Dante programs (by user or program) and PTP 319/320, IGMP, DHCP 67/68, and only to `<dante>` or the groups 224.0.0.231/.233/.251 and the PTP/audio multicast groups; everything else dropped;
  - a guard that allows only one program out must also allow every Dante device on the PC (a dlink app under another user, for example), with its ports in.

### Windows

- **The NIC must do VLANs in its driver.** Intel's PROSet/ANS VLANs do not exist on Windows 11 and Windows 11 client editions have no LBFO, so an Intel I225/I226 on Windows 11 cannot carry two VLANs without Hyper-V. Use a Realtek NIC (the site's USB 2.5G adapter, RTL8156) with Realtek's **Ethernet Diagnostic Utility** (2.0.8.3 or newer, signed by Realtek), which creates a "Realtek Virtual Adapter" per VLAN. No Hyper-V on production PCs.
- The base adapter keeps the LAN (untagged); the virtual adapter is the Dante leg. Name it after the VLAN (`Dante70`), not `Dante`: an old adapter may still hold that name.
- The virtual adapter appears only after one restart of its base adapter, and **it takes a new random MAC at every restart of the base adapter**, so a DHCP reservation cannot follow it: give the Dante leg a **static** address, no gateway, and keep the router's lease for that address only as a reservation so the pool never hands it out.
- On the Dante leg: interface metric 900, "Register this connection's addresses in DNS" off, NetBIOS off (`NetbiosOptions=2`), File and Printer Sharing, Client for Microsoft Networks, IPv6, LLTD unbound.
- Set the base NIC's power and offload values with the app's own setup (EEE, green Ethernet, selective suspend, flow control off); they do not apply to the virtual adapter.
- Changing a static address with `New-NetIPAddress -DefaultGateway` fails on a stored route ("Instance DefaultGateway already exists"); `netsh interface ipv4 set address` changes it in one step.

### Firewall (Windows)

The Windows firewall is **on**, with **default Allow in and out** on every profile, plus one rule group `dstack155` bound only to the Dante leg: block in and out for every non-Dante program on the PC (OBS, Resolume Arena, Bonjour `mDNSResponder.exe`, `svchost.exe`, remote-access tools, scripting runtimes) and all TCP. The Dante devices' processes (an app with dlink or dstack inside, dasio) are never in the group. No other enabled Block rule exists. This replaces the earlier "all profiles off" rule (songplayer #233): on 10.11.2026 an hour with the firewall on against an hour off showed no change in the sender's pump or lateness and none at three receivers (owner's decision, #155). Firewall rules bound to a removed NIC stay in the store and do nothing; remove them. A PC's setup script may recreate the group from this list, never turn the profiles off.

## 4. Rules for our applications

These hold in every project's code, so that the PC's layout is a second line of defence, not the only one.

- **A Dante device binds every socket to its Dante address** (dstack, dlink apps, dasio, iemmixer do). It needs no gateway and no DNS on that leg. It is configured by its address or its interface, and it reports clearly when no interface holds that address ("no interface of this box holds the address"), retrying by itself.
- **Dante's own mDNS** is direct queries answered unicast; a filter on the Dante leg must pass UDP from source port 5353 (above).
- **A non-Dante sender (NDI, web servers, OSC, Bonjour services) binds and advertises the LAN address only.** NDI: `adapters.allowed` in `ndi-config.v1.json` lists only the LAN address. The NDI 6.3 runtime does not hold to it: its sender listens on all addresses and can announce the Dante address too (seen 10.11.2026 on a SongPlayer started long after the Dante leg), and an app that registers through Bonjour announces on every adapter. So the guard is the Dante leg's firewall group: all TCP in on the Dante leg is blocked whatever the program (that closes NDI's data path there, and the group needs no exception for an app that is also a Dante device), and Bonjour does not answer there; a live mDNS query from the Dante VLAN then gets no NDI answer. What remains is an announcement a receiver may have cached, which the receiver rules below make harmless.
- **An app that is both a Dante device and a LAN service** (SongPlayer: Dante output in process, web UI and remote control on the LAN) binds its LAN services, and any mDNS it does itself, to the LAN address and 127.0.0.1 only, and its Dante device to the Dante address. Its LAN address is the default-route interface's, never "the lowest private address". The firewall group never carries per-port exceptions for such an app.
- **A receiver never picks a peer's address by chance.** It dials the peer's LAN address it has verified (presenter#925: NDI by URL address on the LAN), never "whatever the name resolves to first", and it never uses an address in `<dante>` for non-Dante media or control.
- **Discovery that lists several addresses for one name is filtered by subnet** before use: `<lan>` for everything except Dante.
- **Every app knows `<dante>` from its configuration**, not only from its own interfaces: a PC without a Dante leg (a dev box, another site) cannot tell `<dante>` from any routed subnet, and the router would carry the connection. The setting is one value per app, `DANTE_SUBNET` (environment) or `dante_subnet` (config file), holding the Dante VLAN's subnet in CIDR; addresses in it are never used for non-Dante traffic.
- **dsync** is pinned to the Dante leg by name (`system.interface`, dsync 1.27 or newer) on a PC where the grandmaster is also seen on the LAN; without the pin it may follow PTP over the LAN.

## 5. Order of a change on one PC

1. dstack plans the PC on #155 and tells, before anything changes: the PC's owning project, presenter (for any PC that sends NDI), and every project with a device or receiver on that PC.
2. The network project adds the VLAN to the PC's port (the LAN stays as it is, so the PC keeps its connection).
3. The PC side as in section 3; an unreachable PC needs a watchdog script that restores the LAN binding before anything that can drop it.
4. Disable the other NICs; the owner removes the hardware.
5. Restart every app that binds or advertises on the network (NDI senders, Dante apps), so they see the final layout.
6. Verify (section 6), then record the PC's result on #155.

## 6. Checking a PC (the rig/E2E gates assert this)

1. Exactly one physical NIC up; every other NIC disabled.
2. Exactly one default route, through the LAN leg.
3. The Dante leg: its address, no gateway, metric 900, no DNS registration, NetBIOS off, sharing/client unbound (Windows); `never-default`, policy table, no `<dante>` route in the main table (Linux).
4. `<host>.lan` resolves to the LAN address only.
5. Nothing non-Dante answers on the Dante address: a TCP connect from another host's Dante leg to the PC's NDI and web ports times out, and the same connect to its LAN address succeeds.
6. A passive mDNS listen on the Dante VLAN shows the PC's Dante devices and none of its NDI or other services.
7. The PC's Dante devices have live flows, and its receivers' late counters stay at zero over a soak.

## Single-NIC PCs that are still on Dante only

Until such a PC has its LAN leg, all its traffic shares the Dante queue. Keep bulk traffic small, turn large send offload off and set the transmit buffers to the minimum (`docs/windows-box.md`), and run the Dante devices at their priority. Our device warns once a minute when other traffic on its NIC exceeds 100 kB/s, and at its start when its Dante interface holds a default route.

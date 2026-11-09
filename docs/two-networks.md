# Two networks on one PC: Dante and the LAN (dstack #155)

Every production host that sees both the Dante network and the normal LAN keeps them apart in one fixed way. Then no app reaches anything by chance, and nothing depends on which address a library picks after a restart.

## The site's facts

- Dante is VLAN 70, `10.77.7.0/24`, with the router at `10.77.7.1`. DHCP hands out `.100`–`.200` there. There is no `/23` on it.
- The LAN is `10.77.8.0/23`, with the router at `10.77.8.1`.
- The router routes between the two networks with no filter. So an address on either side is reachable from the other, and only the host decides which one is used.
- The switches give DSCP 56 (PTP) and 46 (audio) strict priority on their way out. They cannot help inside a PC whose one NIC carries everything.

## The rule for a two-network host

| | LAN NIC (or the untagged port) | Dante NIC (or the VLAN 70 leg) |
|---|---|---|
| Default route | yes, the only one | none |
| DNS servers, DNS registration, NetBIOS | yes | none |
| Route to `10.77.7.0/24` | none in the main table (Linux) | only for sockets bound to its address |
| What may pass | everything else | Dante, PTP and mDNS-for-Dante only (the ports below), filtered in and out |
| Discovery (NDI, Bonjour, web UIs) | here only | never |
| `.lan` name | its LAN address | none |

**A host with an input filter on its Dante leg must accept UDP from source port 5353.** Our devices and the network listing ask mDNS by direct queries sent to the group (224.0.0.251:5353), and each device answers unicast from its own address and port 5353 to the asker's port. Conntrack does not tie that answer to the multicast query, so a filter that accepts only `ct state established,related` drops every answer. dcontroller's guard did that on 9.11.2026 (3351 packets dropped), and its listing then lost every device after 120 s. In nftables: `udp sport 5353 accept` on the Dante interface. Windows Firewall allows the unicast answer to a multicast query by default.

The Dante devices (dstack, dasio, dlink apps, iemmixer) bind every socket to the Dante address. That includes their direct mDNS queries since dstack v0.6.56 (no browse) and the network listing since v0.6.58. They need no gateway and no DNS on that leg.

### Linux: the model, strih-snv

- The LAN NIC holds the only default route and the DNS.
- The Dante leg is `enp7s0.70`, VLAN 70 tagged on the same cable. It is `never-default`, and its connected route lives only in policy table 70 (`ip rule add from <dante ip> lookup 70`). The main table has no route to `10.77.7.0/24`, so only sockets bound to the Dante address reach VLAN 70.
- nftables on the leg:
  - in: the Dante ports of the host's devices (ARC 4440+, flow control 4455+, info 8700/8702/8708, CMC 8800+, audio 30000+/30100+, mDNS 5353), PTP 224.0.1.129 319/320, and anything else the host needs on purpose;
  - out: from the Dante address to `10.77.7.0/24` and to 224.0.0.231/.233/.251;
  - everything else on the leg is dropped.
- NDI is limited to the LAN in `/etc/ndi` and `~/.ndi` (`networks.ips`, the LAN boxes).

### Windows

- On the Dante NIC: no default gateway, interface metric 900 (fixed), "Register this connection's addresses in DNS" off, NetBIOS over TCP/IP off, and File and Printer Sharing / Client for Microsoft Networks unbound.
- Firewall: the Dante NIC's network profile is Public. One rule set admits the Dante ports and PTP on it and blocks every other program's inbound there. An app's outbound on it is blocked unless it is a Dante device.
- NDI: `C:\ProgramData\NDI\ndi-config.v1.json` lists only the LAN adapter (`adapters.allowed`). Check that the app really loads that runtime: SongPlayer's NDI sender still advertised its Dante address on 9.11.2026 while the file limited it to the LAN.

## Single-NIC hosts

A PC whose only NIC is on VLAN 70 (FOH, mbc, iem-snv, the iem-mixer box, lv1, stream_audio-snv, moderatori-snv, companion-snv in November 2026) carries all its traffic on the Dante network: AbleSet, remote access, updates. Its Dante audio waits in the same NIC queue as that traffic. Only a LAN leg fixes this, either a tagged VLAN on the existing cable or a second NIC; the owner chooses which (dstack #155). Until then, on such a host:

- keep bulk traffic small;
- turn large send offload off and set the transmit buffers to the minimum (`docs/windows-box.md`);
- run the Dante devices at their priority.

Our device warns once a minute when other traffic on its NIC exceeds 100 kB/s.

## Checking a host

1. List the addresses, gateways and metrics (`ip -4 addr`, `ip route`, `ip rule` on Linux; `Get-NetIPConfiguration`, `Get-NetIPInterface` on Windows). There must be exactly one default route, through the LAN.
2. DNS: `<host>.lan` resolves to the LAN address only. Remove a static record into `10.77.7.x` at the router, unless the host has no LAN leg yet.
3. Listening sockets: no non-Dante service is reachable on the Dante address (`ss -lntup`, `Get-NetTCPConnection -State Listen` with the firewall rules).
4. Discovery: NDI and Bonjour advertise the LAN address only, which a passive mDNS listen on the Dante network confirms.

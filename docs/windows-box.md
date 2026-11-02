# A Windows box for low latency

What a Windows PC needs before a dstack device on it (dasio) meets a
receiver's latency. Each item was found on the FOH PC (Windows 10,
Realtek 2.5GbE, Ableton and other apps running), judged by a Yamaha card's
latency records and a second receiver's (dstack #1, 1.11.2026). Our own
counters were clean in every case.

| Need | Why | Check |
|---|---|---|
| The device's spinning threads on cores of their own, apart from the NIC's RSS CPUs and dsync | Low_latency spins (yielding) the pump and the TX/RX threads at priority 31. On a CPU in the NIC's RSS set, the NIC's transmit work waits behind them, and on dsync's cores the clock suffers: the card saw our flow 4 to 5.5 ms late, about 170 late packets a second. Own cores (pump on 31, TX/RX on 29, apart from RSS and dsync) brought the peak to 1.2 to 2.4 ms. A WPR trace does not show it: it measures how long a DPC runs, not how long it waited. | `pump_cpu` / `io_cpu` in the config, Process Lasso or affinity so nothing else shares them; `Get-NetAdapterRss` for the NIC's CPUs. |
| Other traffic on the Dante NIC kept short | Every UDP sender on the FOH PC, ours or a plain test program, saw 1 to 2.5 ms gaps (once 5 to 8 ms) every ~285 ms, the period of a web server's 0.5 to 1.6 MB TCP bursts on the same NIC (AbleSet; proven by stopping and starting it). Windows has no priority between packets in one NIC queue without DCB (which consumer NICs lack), so audio waits behind whatever the NIC holds. The packets leave our stack on time (pktmon: ~0.1 ms after their stamp). The other apps stay as they are (owner): the NIC's queue is what is tuned. With Large Send Offload off the gaps fell from 184 to 117 per 30 s; next, the Transmit Buffers at the driver's minimum, so little sits ahead of a packet. | The device's start line warns when large send offload is on (v0.5.3, `lso`), and a warning once a minute names other traffic over 100 kB/s on the NIC (v0.5.5, `traffic`); `Get-NetAdapterAdvancedProperty`: "Large Send Offload v2" disabled, "Transmit Buffers" at its minimum; dasio's UDP stall test with the other apps running. |
| A Realtek 2.5GbE driver without the long receive DPC: rt640x64 10.39.212.2020 | With the Realtek drivers 10.72 and 10.79, a 20 to 30 ms NIC receive DPC (WFP flow-hash maintenance, `NETIO!NetiopUpdateFlowHashSignature`, run on mDNS receive) held every send completion about every 96 to 130 s. Hundreds of packets reached the card 20 to 30 ms late, and sends failed with WSAENOBUFS. Fewer sockets (v0.5.14 `flow_slots`) and no IPv6 mDNS socket (v0.5.15) did not change it; the driver 10.39.212.2020 did: late 0, max 3.07 ms, DPCs at most 2 to 4 ms, with every service and app running (dasio#1, 2.11.2026). | The device's start line warns on a measured-bad Realtek driver and names an unverified one (v0.5.16, `lso::driver`); Device Manager or `Get-NetAdapter \| Select DriverVersion` on the Dante NIC; an xperf trace with `ndis` DPC durations over 10 minutes. |
| DSCP EF for the audio, if the switches rank it | Windows ignores `IP_TOS` from a program without a QoS policy, so our audio leaves unmarked (DSCP 0) while Dante hardware marks EF (46). It does not help inside one NIC queue (above), only at the switches. | `Get-NetQosPolicy`; a policy for the device's executable with `-DSCPAction 46` (PC-wide: the owner's call). |

The gate is the same as on Linux: a real receiver's latency records for
the device's flows, for hours, with no late packet and every peak under
the flow's latency. Two receivers that agree place a fault on the sending
PC's side; a capture at the receiver's NIC (pktmon) shows whether the
packets arrive in bursts.

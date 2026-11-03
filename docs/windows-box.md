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
| DSCP EF for the audio | Windows ignores `IP_TOS` from a program. Since v0.5.18 the device marks each TX flow EF (46) itself through QoS2 (`QOSAddSocketToFlow`, `QOSSetOutgoingDSCPValue`), which Windows allows only an administrator process (a service is one). Unmarked audio is ranked behind TCP by switches and NIC filters that rank traffic (3.11.2026: dasio's NDIS filter on the FOH PC deferred a DSCP 0 flow, peaks 4–7.5 ms at the receiver). | The log line "TX audio marked DSCP 46 (EF) through QoS2", or the warning "TX audio leaves unmarked". A process that is not admin needs a QoS policy for its executable with `-DSCPAction 46` (`New-NetQosPolicy`). |

The gate is the same as on Linux: a real receiver's latency records for
the device's flows, for hours, with no late packet and every peak under
the flow's latency. Two receivers that agree place a fault on the sending
PC's side; a capture at the receiver's NIC (pktmon) shows whether the
packets arrive in bursts.

## The low-latency wait (`spin_tail_ns`)

The low-latency profile's media threads sleep on the high-resolution waitable timer and spin, yielding, for the last `spin_tail_ns` before each deadline. The default of 50 ms spins every wait at 1 ms: on the FOH PC in October 2026 the timer woke up to 2.7 ms late, measured with the Realtek driver that had a 20–30 ms DPC. Two dasio devices then held 4–6 CPUs at 100 %.

Where the timer wakes on time (the good driver, nothing else on the NIC's DPC CPUs), a tail of 100–300 µs frees those CPUs. Measure before you keep it: TX-late warnings and a receiver's lateness (rxlat) against the default.

Measured on the FOH PC (November 2026, dstack 0.6.6): one dasio device, block 256, 1 ms, its threads pinned on three cores of one CCD. The receiver was the hardware card, its per-second lateness on the device's five flow slots.

| `spin_tail_ns` | Run | Receiver p99 | Max | Seconds over 1 ms | TX-late warnings | Process CPU |
|---|---|---|---|---|---|---|
| default (50 ms) | 60 min | 0.47–0.65 ms (p99.9 2.8 ms) | 3.9 ms | 12 | 30 | 2.74 |
| 300 µs | 15 min | 0.42–0.60 ms | 3.0 ms | 6 | 0 | 1.20–1.45 |
| 150 µs | 15 min | 0.36–0.48 ms | 2.4 ms | 1 | 0 | 1.43 |

A short tail saves about 1.3 CPU per device and the timing gets better, not worse: the spin no longer takes the cores the device shares.

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

## The normal profile (dlink apps)

dlink apps (SongPlayer, OBS, the intercom) run the normal timing profile: a 10 ms latency budget for the whole path each way, the pump's work and the Dante latency together (since v0.6.22, dstack #68: the Dante latency is what the block, the pump margin and the rate converter leave, about 4 to 8.7 ms; the table is in the dlink crate docs), and no real-time setup asked of the app. Since v0.6.20 (dstack #90) the stack sets two things for it on a busy Windows PC:

- **Thread priority:** the pump, TX and RX threads ask `THREAD_PRIORITY_HIGHEST` within the process's normal priority class (`clock::thread_priority_for`). It needs no privilege and changes nothing process-wide; the realtime class stays the low-latency profile's. The start log says "runs at THREAD_PRIORITY_HIGHEST" per thread, or warns that the priority was refused (the thread then runs on at normal priority). Linux keeps normal priority.
- **Block:** dlink's pump runs 128 frames a block (1.33 ms at 96 kHz) instead of the stack's 32 (333 µs): a quarter of the wakes, and four times the frames for each rate-conversion call. `dlink::Config::block` sets another of 16, 32, 64, 128 or 256. The pump margin W, how late the pump may run before it starts again and loses blocks, is 2/5 of the budget, 4 ms at 10 ms, at least the block and 2 ms; with a rate converter on a device with TX it gives way to keep the Dante latency at its 4 ms floor (since v0.6.24, dstack #1: the block and 2 ms of v0.6.22 restarted the pump on a loaded PC; it was half the latency, 5 ms, before). A larger block costs Dante latency, not path: the budget stays 10 ms.

Why: SongPlayer's Dante output on a busy video PC (video playback running; dlink at block 32, normal priority, 96 kHz device; 3.11.2026, 12 h ledger) lost blocks in bursts (`pump_missed`, the last 17 blocks with the pump 5.85 ms late against W 5 ms) and had overload and overrun pairs every few minutes. The pairs rose from 0–4 to 8–28 per 12 min once the app ran at 48 kHz through `Rated`, whose sinc then ran on the pump thread every 333 µs. The check after the change is the same ledger (`pump_missed`, overruns per 12 min) over a few hours.

## A send the NIC refuses for a moment (WSAENOBUFS)

On a NIC that carries other traffic beside Dante (the FOH PC in November 2026: a Realtek 2.5GbE NIC with 10 to 19 MB/s of other traffic), Windows refuses a send now and then with WSAENOBUFS (10055): the NIC's transmit ring is full for a moment, for every program on the PC. dasio's logs on dstack 0.6.18 showed about 32 bursts a day, 1 to 14 refused packets each, on both of the PC's devices in the same second. Before v0.6.21 each refused packet was lost, and every receiver of the flow saw a hole (16 to 112 frames at a receiver of both devices). The socket's send buffer (1 MiB) does not help: the refusal comes from below it.

Since v0.6.21 (dstack #1) the TX thread sends a refused packet again:

- only on a transient refusal: WSAENOBUFS and WSAEWOULDBLOCK (10035) on Windows; ENOBUFS, EAGAIN and ENOMEM on Linux. Any other error fails at once;
- at most 3 times, 100 µs apart (a yielding spin on the TX thread: up to 300 µs plus the sends per packet; the other flows' sends would fail meanwhile anyway);
- only while the packet's age after the wait stays at least 100 µs under the flow's latency: a packet that would arrive late is not held (`media::tx::send_retry`).

The status counts a packet that went out on a retry in its TX lane's `send_retries` (recovered, not lost); a packet still refused after its retries counts in `send_errors`, as before. The log says the same once a second per flow: a warning "TX sends failed after their retries" for lost packets, an info line "TX sends refused for a moment went out on a retry" when none was lost. dtray turns a device orange while its `send_errors` grow ("send errors +N" on its line). Retries are not shown there: they are the healthy case.

The retry covers short refusals only. Whether to keep bulk traffic (file copies, web servers, other audio protocols) off the Dante NIC is the box owner's call; a second NIC for that traffic removes the refusals at their source. The check is the device's `send_errors` and `send_retries` over a day, and a receiver's holes at the same instants.

## A host's own send path (a frame sender)

The retries cover a short refusal, not the wait below Winsock: on the FOH PC (November 2026, dstack 0.6.21) sends that left on time still waited up to 6 ms below the socket, and after their retries band lost 9 and master 8 packets in one burst, while dasio's kernel test stream (frames built in its NDIS filter and handed straight to the miniport) lost none on the same driver. So since v0.6.22 a host application may hand its device its own kernel send path.

- **Who provides it:** the application, never dstack. dstack defines the interface (`media::bypass::FrameSender`: a batch of whole Ethernet II frames, and which of them the host took) and nothing driver-specific. dasio's NDIS filter is the first; another Windows host with a kernel path can plug in the same way.
- **How it is set:** before the device's threads start, `Media::set_frame_sender` (dlink: `Config::frame_sender`, embed: `Embedded::start_with_sender`). It is used on Windows only; on Linux the device keeps AF_PACKET past the qdisc (or the UDP socket).
- **What dstack does with it:** the TX thread writes each packet as a whole frame (our NIC's MAC from GetAdaptersAddresses, the receiver's MAC from the neighbour table, IPv4 with DSCP EF, don't fragment, TTL 64, and UDP from the flow's own port) and hands every frame due in a pass to the sender in one call. The neighbour table (GetIpNetTable2) is read once a second, off the TX thread.
- **What stays on the UDP socket:** multicast flows, a receiver the neighbour table has not resolved (the socket lets Windows resolve it), and every frame the sender did not take, sent at once with the usual retries. A batch the sender refused whole goes the same way.
- **What to read:** the device's start line names the path ("TX path: host frame sender on <adapter>", "AF_PACKET past the qdisc" or "UDP socket"). A TX lane's `direct` counts the frames the sender took; the rest of its `packets` went through the socket. A sender that refuses whole batches is warned once a second ("TX host frame sender refused whole batches", with the count and the OS error), never per packet. The verdict on latency stays a real receiver's records, as everywhere in this document.

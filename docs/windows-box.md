# A Windows box for low latency

What a Windows PC needs before a dstack device on it (dasio) meets a
receiver's latency. Each item was found on the FOH PC (Windows 10,
Realtek 2.5GbE, Ableton and other apps running), judged by a Yamaha card's
latency records and a second receiver's (dstack #1, 1.11.2026). Our own
counters were clean in every case.

| Need | Why | Check |
|---|---|---|
| The device's spinning threads on cores of their own, apart from the NIC's RSS CPUs and dsync | Low_latency spins (yielding) the pump and the TX/RX threads at priority 31. On a CPU in the NIC's RSS set, the NIC's transmit work waits behind them, and on dsync's cores the clock suffers: the card saw our flow 4 to 5.5 ms late, about 170 late packets a second. Own cores (pump on 31, TX/RX on 29, apart from RSS and dsync) brought the peak to 1.2 to 2.4 ms. A WPR trace does not show it: it measures how long a DPC runs, not how long it waited. | `pump_cpu` / `io_cpu` in the config, Process Lasso or affinity so nothing else shares them; `Get-NetAdapterRss` for the NIC's CPUs. |
| Other traffic on the Dante NIC kept short | Every UDP sender on the FOH PC, ours or a plain test program, saw 1 to 2.5 ms gaps (once 5 to 8 ms) every ~285 ms, the period of a web server's 0.5 to 1.6 MB TCP bursts on the same NIC (AbleSet; proven by stopping and starting it). Windows has no priority between packets in one NIC queue without DCB (which consumer NICs lack), so audio waits behind whatever the NIC holds. The packets leave our stack on time (pktmon: ~0.1 ms after their stamp). The other apps stay as they are (owner): the NIC's queue is what is tuned. With Large Send Offload off the gaps fell from 184 to 117 per 30 s; next, the Transmit Buffers at the driver's minimum, so little sits ahead of a packet. | The device's start line warns when large send offload is on (v0.5.3, `lso`), and a warning once a minute names other traffic over 100 kB/s on the NIC (v0.5.5, `traffic`); `Get-NetAdapterAdvancedProperty`: "Large Send Offload v2" disabled, "Transmit Buffers" at its minimum; dasio's UDP stall test with the other apps running. Only on the NIC that carries Dante, and only where its driver offers these keywords; a PC or driver without them passes (`docs/two-networks.md`, scope). |
| A Realtek 2.5GbE driver without the long receive DPC: rt640x64 10.39.212.2020 | With the Realtek drivers 10.72 and 10.79, a 20 to 30 ms NIC receive DPC (WFP flow-hash maintenance, `NETIO!NetiopUpdateFlowHashSignature`, run on mDNS receive) held every send completion about every 96 to 130 s. Hundreds of packets reached the card 20 to 30 ms late, and sends failed with WSAENOBUFS. Fewer sockets (v0.5.14 `flow_slots`) and no IPv6 mDNS socket (v0.5.15) did not change it; the driver 10.39.212.2020 did: late 0, max 3.07 ms, DPCs at most 2 to 4 ms, with every service and app running (dasio#1, 2.11.2026). | The device's start line warns on a measured-bad Realtek driver and names an unverified one (v0.5.16, `lso::driver`); Device Manager or `Get-NetAdapter \| Select DriverVersion` on the Dante NIC; an xperf trace with `ndis` DPC durations over 10 minutes. |
| DSCP EF for the audio | Windows ignores `IP_TOS` from a program. Since v0.5.18 the device marks each TX flow EF (46) itself through QoS2 (`QOSAddSocketToFlow`, `QOSSetOutgoingDSCPValue`), which Windows allows only an administrator process (a service is one). Unmarked audio is ranked behind TCP by switches and NIC filters that rank traffic (3.11.2026: dasio's NDIS filter on the FOH PC deferred a DSCP 0 flow, peaks 4–7.5 ms at the receiver). | The log line "TX audio marked DSCP 46 (EF) through QoS2", or the warning "TX audio leaves unmarked". A process that is not admin needs a QoS policy for its executable with `-DSCPAction 46` (`New-NetQosPolicy`). |

The gate is the same as on Linux: a real receiver's latency records for
the device's flows, for hours, with no late packet and every peak under
the flow's latency. Two receivers that agree place a fault on the sending
PC's side; a capture at the receiver's NIC (pktmon) shows whether the
packets arrive in bursts.

## The low-latency wait (`spin_tail_ns`)

The low-latency profile's media threads sleep on the high-resolution waitable timer and spin for the last `spin_tail_ns` before each deadline. The default of 50 ms spins every wait at 1 ms: on the FOH PC in October 2026 the timer woke up to 2.7 ms late, measured with the Realtek driver that had a 20–30 ms DPC. Two dasio devices then held 4–6 CPUs at 100 %. On 5.11.2026, with the driver fixed, it woke at most one tick + 140 µs late (1.1 ms at the 1 ms resolution, 0.64 ms at 0.5 ms).

The spin yields (SwitchToThread), on every thread. A yield hands the CPU to any ready thread, a lower priority included, for the rest of that thread's slice: dasio's CSwitch trace on the FOH PC (5.11.2026) caught FanControl (priority 10) holding the band TX thread's CPU (priority 31) for 1.66 ms, and the TX schedule started again. That is the default, and the safe one.

Every spin that did not yield to lower priorities froze a Windows PC (dstack #1):

- Sleep(0) (0.6.28): it yields only to equal or higher priority, so the spin never blocked. FOH and mbc stopped answering.
- The no-yield spin (0.6.34, every thread): two unpinned devices starved dasio's CI runner, and the pump overran hundreds of blocks a second.
- The no-yield spin of pinned threads only (0.6.35), with each spinner alone on its CPU and only blocking threads on its SMT sibling: FOH froze minutes after the start. Some per-CPU system work must run on exactly that CPU.

Never ship a spin that starves lower priorities; a change to this wait is tried on a non-production Windows box before FOH.

### The bounded escape (`spin_escape_ms`, a trial, off by default)

Since 0.6.38 (dstack #1) a device may set `spin_escape_ms` (100 to 10000; left out: off, the yielding spin above; process-wide, the last device set wins). It is built for a trial on dasio's test PC (iem-snv, no live receivers) before any production PC. When set, on Windows in the low-latency profile:

- The spin does not yield between the blocks, so no lower priority takes the CPU close to a send. Sleep(0) is no shelter on current Windows: on windows-2025 a lower-priority thread pinned to the same CPU ran 21 million turns in 0.3 s of a Sleep(0) spin, and the waits went 3 ms late at the median.
- At least once every `spin_escape_ms`, each spinning media thread blocks on its high-resolution timer for one tick (`clock::ESCAPE_BLOCK_NS`, a 1 ms relative due time). The timer's expiry preempts a lower priority at once, so per-CPU system work and per-CPU programs (FanControl's sensor walk) get the CPU in each block. This is what the Sleep(0) spin of 0.6.28 lacked.
- The engine's pump blocks when its escape is due only if its margin covers the block: W + one block of at least `clock::ESCAPE_MAX_NS` (2.14 ms; FOH's W 4 and 5.3 ms do). A pump with less (the iem-snv trial: block 64 and W 64 at 96 kHz lost 2 blocks at every escape) stays out of the escape and spins yielding; the start log says so. Since 0.6.39.
- The TX thread escapes right after a pass, and a relay thread sends its packets meanwhile (since 0.6.53, dstack #141). The TX thread makes a last pass at a fresh clock read (only the packets due by then), hands its `Transmitter` to the relay thread (`media::tx::relay`), blocks its tick, and takes it back. The relay thread waits blocked on a condition variable and spins without yielding only while it covers, at most `clock::ESCAPE_MAX_NS` (2.14 ms: the 1 ms block + the timer's late wake of one tick + 140 µs at the 1 ms resolution) once a period. No packet leaves before its due time.
- Why not send ahead (0.6.38 to 0.6.52 did): an UltimoX2 card (ESI planet 22) reads a packet that arrives before its media time as latency plus that early time. band-asio-dante's send ahead of up to 2.14 ms once a second read 3.5 ms there; the same device without it read 0.77 ms (dstack #141, 7.11.2026).
- A relay that does not take over within `media::tx::RELAY_TAKE_MAX` (200 µs; the packets due meanwhile wait that long), or a re-anchor since the pass, puts the escape off to a later pass, at most `clock::ESCAPE_DEFER_MAX_NS` (20 ms) past the period; after that it escapes anyway, uncovered. A miss is warned at the first and every 100th. The start log says the mode is on, with its period, and that the relay thread started.

On Linux the field is accepted and ignored (the start log says so): the wait sleeps on `clock_nanosleep` and never spins.

Open before it can leave the trial: whether the PC stays responsive with a FanControl-like per-CPU worker on the spinners' CPUs.

Where the timer wakes on time (the good driver, nothing else on the NIC's DPC CPUs), a tail of 100–300 µs frees those CPUs. Measure before you keep it: TX-late warnings and a receiver's lateness (rxlat) against the default.

Measured on the FOH PC (November 2026, dstack 0.6.6): one dasio device, block 256, 1 ms, its threads pinned on three cores of one CCD. The receiver was the hardware card, its per-second lateness on the device's five flow slots.

| `spin_tail_ns` | Run | Receiver p99 | Max | Seconds over 1 ms | TX-late warnings | Process CPU |
|---|---|---|---|---|---|---|
| default (50 ms) | 60 min | 0.47–0.65 ms (p99.9 2.8 ms) | 3.9 ms | 12 | 30 | 2.74 |
| 300 µs | 15 min | 0.42–0.60 ms | 3.0 ms | 6 | 0 | 1.20–1.45 |
| 150 µs | 15 min | 0.36–0.48 ms | 2.4 ms | 1 | 0 | 1.43 |

A short tail saves about 1.3 CPU per device and the timing gets better, not worse: the spin no longer takes the cores the device shares.

## The normal profile (dlink apps)

dlink apps (SongPlayer, OBS, the intercom) run the normal timing profile: a 15 ms latency budget for the whole path each way, the pump's work and the Dante latency together (10 ms from v0.6.22, dstack #68; 15 ms since v0.6.28, dstack #1: the Dante latency is what the block, the pump margin and the rate converter leave, about 5.9 to 9 ms with both directions; the table is in the dlink crate docs), and no real-time setup and no CPU pinning asked of the app (`dlink::Config::pump_cpu`/`io_cpu`/`rx_cpu` were removed in v0.6.28: nobody guards which core is free on every PC). Since v0.6.20 (dstack #90) the stack sets two things for it on a busy Windows PC:

- **Thread priority:** the pump, TX and RX threads ask `THREAD_PRIORITY_HIGHEST` within the process's normal priority class (`clock::thread_priority_for`). It needs no privilege and changes nothing process-wide; the realtime class stays the low-latency profile's. The start log says "runs at THREAD_PRIORITY_HIGHEST" per thread, or warns that the priority was refused (the thread then runs on at normal priority). Linux keeps normal priority.
- **Block:** dlink's pump runs 128 frames a block (1.33 ms at 96 kHz) instead of the stack's 32 (333 µs): a quarter of the wakes, and four times the frames for each rate-conversion call. `dlink::Config::block` sets another of 16, 32, 64, 128 or 256. The pump margin W, how late the pump may run before it starts again and loses blocks, is 2/5 of the budget, 6 ms at the default 15 ms (room for the 2.7 to 3.6 ms preemptions seen on loaded PCs), at least the block and 2 ms; on a device with TX it gives way, when a smaller budget leaves no room, to keep the Dante latency at the TX floor: a TX packet's own age when it leaves, 32 frames + 0.25 ms + 4 ms of lateness (4.58 ms at 96 kHz; since v0.6.28, dstack #1: songplayer's TX thread left packets up to 3.71 ms past due, and a receiver at the old 4 ms floor counted them late). A 48 kHz app on a 96 kHz network converts through a 123-tap half-band (0.64 ms) and keeps W's whole share. (W has given way since v0.6.24: the block and 2 ms of v0.6.22 restarted the pump on a loaded PC.) A larger block costs Dante latency, not path: the budget stays 15 ms.

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

## dsend's own output (drivers/dsend-audio)

dsend sends what Windows plays to Dante by capturing an output's loopback. On a PC where nothing should be heard locally (the FOH PC: Chrome into master Live, dstack #184), that output is dsend's own: a virtual render endpoint from our driver in `drivers/dsend-audio` (from Microsoft's simpleaudiosample, MS-PL for that directory only). It plays nowhere and consumes the audio at the real-time rate; Windows lists it as "Speakers (dsend)". A VB-Audio Matrix VAIO endpoint gave dsend's loopback zeros there (`input silent` at -20 dBFS on the endpoint), which is why we have our own.

- **Install:** the release zip `dsend-audio-<tag>-windows-x86_64.zip` (or the CI job `dsend-audio-driver`'s artifact), unpacked, then `.\install.ps1` as administrator. The package is test-signed with the build's own WDK test certificate, like dasio's NDIS filter: it needs test signing on (`bcdedit /set testsigning on`, a reboot, Secure Boot off). The script refuses and changes nothing when it is off; otherwise it trusts the certificate, creates the root device `ROOT\dsend_audio` once, installs the driver and prints the endpoint. Run it again to update; `.\uninstall.ps1` removes device and package.
- **Windows' default:** set "Speakers (dsend)" as the default output (Settings > System > Sound > Output, or `mmsys.cpl` > Playback > Set Default), so Windows and the browsers play into it.
- **dsend's choice:** with no `--output` and no output saved from dtray's menu, dsend captures the output whose name holds "dsend" when there is one (`dstack_endpoint::outputs::DSEND`), else the default output. `--output` and a saved choice still win. dlisten never takes it by default (it would play into nothing).
- **Formats:** 16-bit stereo at 48 kHz (the default) or 44.1 kHz in shared mode; the engine mixes in float at that rate and its loopback hands dsend the mix.
- **The check:** a tone played on the endpoint at -20 dBFS reads -20 dBFS on dsend's TX L/R at a Dante receiver (dcontroller's meters), and dsend's log line shows the input peak, not `input silent`.

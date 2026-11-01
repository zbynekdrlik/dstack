# Changelog

## 0.4.17 (2026-11-01)

- Fix: a new device name in the config takes effect at start. Before, the state file's name won silently, so a rename through the config needed a restart to discover it (iemmixer, 1.11). The state now records the config's name it was made with (`config_name`). A config whose name changed since then renames the device, and our own subscriptions to our own TX follow, as with a Dante Controller rename. The state is saved at once, with one info line. A Dante Controller rename under an unchanged config stays across restarts, with one info line at start naming both names. A state file written before the record keeps its name and warns at start when it is not the config's (dstack #1).

## 0.4.16 (2026-11-01)

- Fix (Windows): dantesync's nightly date step no longer moves PTP time. The "monotonic" clock was the system time, which dantesync steps. At 02:00:11Z on the SongPlayer PC the wall stepped +1.6 s; the TX schedule started again at once (lag 153609 samples), the engine pump skipped 1200 blocks, and PTP time ran 1.6 s ahead until the re-anchor 4.2 s later. Now it is QPC steered to the system time's rate by a model the clock poller updates every second (`clock::steer`). A wall jump over 1 ms plus 100 ppm of the time since the last poll is a date step and goes into an offset; smaller differences are slewed out within 1000 ppm. Linux's `CLOCK_MONOTONIC` already behaved this way (dstack #1).
- Linux: the qdisc warning comes only when audio TX cannot bypass the qdisc (no `CAP_NET_RAW`); it now names that remedy first. With the bypass, the qdisc carries only other traffic and shaping it (prio + tbf) is right (iemmixer, dstack #1).

## 0.4.15 (2026-11-01)

- Linux: audio TX bypasses the qdisc. An `AF_PACKET` socket with `PACKET_QDISC_BYPASS` hands each packet to the driver; we write the IPv4 header (DSCP EF, DF) and the UDP header in front of the media packet, and the kernel writes the Ethernet header. Only unicast flows to a neighbour with a complete ARP entry go direct. The control tick reads `/proc/net/arp` every 100 ms. Multicast and unresolved destinations keep the UDP socket. The socket needs `CAP_NET_RAW`; without it, the device warns at start and nothing changes. Each TX flow in the status gains `direct`, the packets sent past the qdisc. Cause, found by iemmixer on the gen3 box (one TX queue, PREEMPT_RT, TSO/GSO off): a TCP burst or an ssh login on the box held our packets in the qdisc behind a preempted sender. The IEM card read peaks of 250 to 1800 µs and late packets at 250 µs, while every send returned within 116 µs of its stamp (dstack #1).
- `docs/linux-box.md`: TSO and GSO off on the Dante NIC (peak 750 to 198-260 µs in an hour on the gen3 box), and `CAP_NET_RAW` for the direct path.

## 0.4.14 (2026-10-31)

- Fix (Linux): every TX flow socket marks its packets DSCP EF (46), as Dante hardware marks its audio, and takes `SO_PRIORITY` 6 (pfifo_fast's first band). Windows ignores a program's `IP_TOS`; there DSCP comes from a QoS policy on the box. Before, our audio went out unmarked: below marked traffic on a switch with QoS, and in the kernel's default band behind bulk data (gen3 box, dstack #1).

## 0.4.13 (2026-10-31)

- Linux: at start the device reads the Dante NIC's root qdisc (`tc -j qdisc show`) and warns unless it is `noqueue` or lockless `pfifo_fast`. On the gen3 box, fq_codel held our packets behind a preempted low-priority sender: the IEM card read peak 812 µs and late packets at 250 µs while our counters were clean; with `noqueue`, peak 229 µs and none late (iemmixer, dstack #1).
- New `docs/linux-box.md`: what a Linux box needs for low latency (real-time priority, no deep idle states, `noqueue`, RX apart from TX, a small margin), each with how to check it.

## 0.4.12 (2026-10-31)

- New `rx_cpu`: the CPU the RX thread runs on (`io_cpu` when left out, as before; `io_cpu` stays the TX thread's). On the gen3 box a received flood on the TX core delayed sends by up to 247 µs; iemmixer can now put RX on the core the NIC's interrupts land on (dstack #1).

## 0.4.11 (2026-10-31)

- Linux: the RX thread reads in passes at least 50 µs apart (from each pass's start) instead of waking per datagram: what arrives meanwhile waits in its socket and the next pass reads it. On the gen3 box, 7 stage flows at fpp 4 woke the thread 168 k times a second on the core it shares with TX. A datagram is read at most 50 µs later, a fifth of the lowest latency we receive at. Windows keeps a wake per datagram (its sleep is coarse) (dstack #1).

## 0.4.10 (2026-10-31)

- Fix: the TX thread runs at `SCHED_FIFO` 47, above the RX thread's 44 (both were 45). On the gen3 box the two threads share the io core, and with 7 stage flows received (168 k packets/s at fpp 4) the TX thread waited for the RX thread to yield: age_max 239/247 µs at 250 µs and card peaks over 400 µs (iemmixer, v0.4.9). A send now preempts a receive; both stay below dantesync's 50, the engine pump stays at 40 (dstack #1).

## 0.4.9 (2026-10-31)

- Linux: the TX thread holds a PM QoS request (`/dev/cpu_dma_latency` = 0 µs) while it runs, in the low-latency profile. Cause, found with iemmixer on the N100 box at 250 µs: with the deep idle states (C6, C8, C10) on, the IEM card read peak 490 µs and late packets; with them off, peak 240 µs and none in 300 s. The time comes after `send_to`, where no counter of ours sees it. The device is root's by default: refused, the device logs "CPU wake latency not held" and runs without it, so provisioning grants the device (a udev rule) or disables the idle states (dstack #1).

## 0.4.8 (2026-10-31)

- Fix: the TX counters now measure what a receiver measures. Each TX flow in the status gains `age_max_us` (the oldest a packet was, since its stamp, when its send returned) and `over_latency` (packets older than the advertised latency, which a receiver at that latency counts late). Before, `late_bins` and `late_max_us` measured from the due time (last sample plus margin) with a first edge of 250 µs: gen3 at 250 µs read late_max 204 µs while the FOH console read 770 µs and the PA popped (dstack #1).

## 0.4.7 (2026-10-31)

- Withdrawn: `timing = "normal"` is refused at setup. On the Resolume PC (v0.4.2), its Windows timer-only waits made SongPlayer's TX and pump late: the FOH console's Dante Controller Latency tab showed songplayer at peak 32.1 ms, average 14.7 ms, with 2562 late packets, and the PA popped. It stays refused until it is fixed and proven on a receiver's latency records (dstack #1).
- Fix (dasio): the start line "listening on the interface's address" named the default ports; it now names the device's own (`arc`, `cmc`, `flows`), its `process_id`, and `info_shared` (a member of the address's info port).

## 0.4.6 (2026-10-31)

- New: several of our devices on one address (owner, 31.10.2026: no extra IPs; dstack #1, the per-PC manager). UDP 8700 is one per address, and Dante Controller reads a device's ARC only after an 0x0061 reply carrying that device's id (dev1: a second device whose 8700 was the first's was never read; 0x0061 asked 38 times). The process holding `<ip>`:8700 is the address's manager. A device that finds it held (`host::info_port_held`, bind refused with AddrInUse) joins as a member over loopback (`info::share`, `share_port(ip)` = 18700 + the address's low 10 bits) and renews its lease every second (3 s lease, at most 32 members). The manager relays every info request to each live member; the member answers with its own identity and hands back the packet, which the manager sends from 8700, as it does the member's board and product info, heartbeats and notifications. When the manager ends, a member binds 8700 within a second and becomes the manager (`info::serve_shared`, `serve_member`, `serve_info_with`). Each device needs its own ports and process id (0.4.5). On dev1, two device processes on one address were both read in full by Dante Controller, and the handover took 0.25 s. Consumer-visible: `daemon::Sockets::info` is an `Option`.

## 0.4.5 (2026-10-31)

- New, the first step of the per-PC manager (owner, 31.10.2026: several of our devices on a PC's one address, no extra IPs; dstack #1): per-device ports and process id in the device config. `arc_port`, `cmc_port` and `flows_port` (defaults 4440, 8800 and 4455) are bound on the device's address and advertised in its records (the ARC and CMC SRV, the channel records). `process_id` (default 0) goes into the device id (`cmc::device_id(ip, process)`, the CMC advertisement, the info packets), the mDNS `process`, and our TX and RX flow names. On dev1 (31.10), Dante Controller read a device with ARC on 4441 and CMC on 8801 in full, so it uses the SRV ports. Consumer-visible: `cmc::device_id`, `cmc::handle_cmc` and `cmc::serve_cmc` take the process id, and `info::Identity` has `process`. The info port 8700 stays one per address; the shared responder for several devices is next.

## 0.4.4 (2026-10-31)

- Fix (iemmixer, dstack #1): on the box (29 subscriptions) the mic's record still never won. The query thread sent its due queries in one burst (one per subscribed name), and the wireless mic receiver answers none of a burst; at every re-query the burst came again, because the Yamaha card's 18 channels never answer. Reproduced from dev1 with the box's 29 names (the mic unresolved, ARC at 8 s). Queries are now spaced 20 ms apart (`instance::PACE`), with answers read in between: from dev1 the same 29 names give the mic's record in 0.5 s (1 ms, fpp 32). The stage rack answers direct queries too (250 µs, fpp 2..4).

## 0.4.3 (2026-10-31)

- Fix (iemmixer, dstack #1 problem C): the wireless mic's flow still went up from its ARC read (fpp 4, then 0x0314 steps, no latency) and its channel record never replaced it, so the approved override did not show. Three changes:
  - A direct instance query not answered is asked again after 1, 2 and 4 s, then every 10 s (`subscribe::instance::QUICK_NS`, `requery_after`). A first query at the device's start can be lost among its own announcements.
  - The network resolver keeps a name from ARC while its direct query is younger than 8 s and has no answer (`instance::GRACE_NS`, `Instances::young`). ARC's settle is 5 s, so the record usually wins.
  - The manager re-resolves its routed channels every tick. When a channel's record changes (flow control, latency, fpp range or slots), the channel leaves its flow, the flow ends (stopped if granted, given up if still asked), and it is asked for again from the new record.

## 0.4.2 (2026-10-31)

- Fix (iemmixer, dstack #1): a per-flow delay below one pump block did not delay a packet's first frame, because the pump reads whole blocks. At a block of 16, the mic's delay of 8 (playout 32 at P 24) played like 0, with 200-235 holes per 45 s, while a delay of 16 played clean. The delay is now rounded up to whole blocks, and the status's `playout` is what that gives (the mic at override 32: delay 16, playout 40). `below_advertised` compares that value.
- New: a timing profile per device (owner, 31.10.2026; dstack #1 problem A): `timing = "low_latency"` (the default, as before: real-time priority for the media threads and, on Windows, a yielding spin through the last 50 ms of a wait) or `timing = "normal"` for an application that needs no lowest latency (SongPlayer): no real-time priority, and on Windows the high-resolution waitable timer for the whole wait, so no core is spun. Why: on the Resolume PC, SongPlayer's in-process device spun two cores, even with no receiver (the TX thread waits 33 ms between plan reads, the pump one block; every wait under 50 ms was spun, the TX thread at TIME_CRITICAL), and the LED wall's frame boundaries went from 2-3 to about 540 per 40 s. One profile per process (`clock::set_profile`; a second device with another profile is refused). The start line logs `timing` and `windows_wait`. The full fix for low_latency follows.
- Fix (dstack #1 problem E): on Windows, a device on an extra address that is SkipAsSource (dasio's and SongPlayer's, beside the PC's primary address) answered mDNS from the primary address, because mdns-sd binds 0.0.0.0:5353 and only names the interface. Dante Controller then read nothing of it (no ARC, no 8700); on its own address on dev1 it read it in full. On Windows the device now also answers its own records from a socket bound to `<device address>:5353` and joined to 224.0.0.251 there (`announce::responder`, PTR/SRV/TXT/A/ANY via simple-dns, from the set the announcer published last, `Announcer::published`). mdns-sd keeps announcing and browsing. A failed start is a warning. Tests: unit tests of the replies and of the responder thread on loopback; the Windows build in CI. A CI reproduction (a SkipAsSource address added to the hosted Windows runner) cut the runner off the network, so it was removed. The check that Dante Controller reads such a device is done on a real Windows PC.

## 0.4.1 (2026-10-31)

- New: direct instance queries of channel records (`subscribe::instance`, dstack #1 problem C). A wireless mic receiver answers no browse (PTR) query for `_netaudio-chan` or `_netaudio-arc`, but does answer a query for its instance `<channel>@<device>._netaudio-chan._udp.local.` (tested from dev1's Dante NIC: no reply to PTR in any form; the instance answered within 0.1 s). The stack read it through ARC, with no latency and the config's fpp. Now, for a subscribed name whose device no browsed record names, the network resolver asks the instance directly (SRV + TXT, then the target's A if missing). It sends a legacy unicast query from a socket of its own, every 10 s while unanswered. Answers are used for 2 min and re-asked from 1 min while looked up, for at most 64 instances. The answer goes through the same channel-record parser, so the advertised latency and fpp come through. Order: browsed records, then the instance query, then ARC. DNS packets via simple-dns 0.12.0 (MIT). Logs carry counts only. A failed start of the query thread is a warning; records and ARC still resolve. Live on dev1: the mic's record read with latency_ns 1000000, fpp 32..32, nchan 2.
- New: `playout_override` in the device config, a per-source playout in samples by TX device name (case aside), e.g. `playout_override = { "<device>" = 32 }`. Owner's rule (31.10.2026): by default an advertised latency is always honoured; going below it needs the owner's approval per device. The override never goes below the flow's fpp or our latency. Status per RX flow: new `advertised` (samples) and `below_advertised`; `FlowView` has `advertised_ns` and `below_advertised`.
- Fuzz: a new target `instance_answer` (8 targets).

## 0.4.0 (2026-10-31)

Consumer-visible: an RX channel whose flow needs a later playout is now later in the engine's mix by that much; every other channel stays at the device latency.

- New: per-flow playout (owner's ruling, relayed by iemmixer: a slow source must not raise the others' latency; dstack #1). A flow's playout point P_flow = max(its transmitter's latency, ours, its frames per packet), with no config. The pump reads that flow's channels P_flow − P samples earlier in stamps (`RxShared::delay`, one `Relaxed` load per channel per block, real-time contract unchanged). A packet counts `late` only past P_flow + B (the pump's block, its real read budget), so a healthy long-packet flow no longer counts every packet late. For iemmixer's wireless mic at fpp 32 and a device latency of 24 samples, that flow plays out at 32 (delay 8) while the stage rack stays at 24. A transmitter that advertises a latency above ours is now also read at that latency (it used to judge `late` only).
- `media::rx::RxPlan` has `delay` and `block`; `latency` is the playout point, kept in 16 bits.
- ctl status, per RX flow: `latency` is the negotiated latency (its transmitter's or ours) in samples, new `playout` (P_flow), `delay` (P_flow − P) and `late_after` (P_flow + B).
- Removed: `subscribe::fpp_misses` and its warning (0.3.7): the playout now covers the fpp. The status shows each flow's `playout` and `delay`.

## 0.3.7 (2026-10-31)

- New: a warning when an RX flow's frames per packet reach its latency (`subscribe::fpp_misses`). A packet leaves its transmitter only after its last frame, so its first frame is at least fpp samples old when it arrives. At fpp >= latency every packet is late. On iemmixer's box the wireless mic accepted only fpp 32 (after the 0.3.6 step) at a device latency of 24 samples: every packet was late, with 13 holes in 10 s. The line names `fpp` and `latency_samples` and says to raise `latency_ns` above fpp plus the network's lateness.

## 0.3.6 (2026-10-31)

- Fix (iemmixer, dstack #1 problem C): a transmitter that refuses our flow request with status 0x0314 (`flows::control::status::FPP_REFUSED`) is asked again with twice the frames per packet: 8, 16, then 32 (`subscribe::FPP_STEP_MAX`, the most Dante devices advertise). A 2-channel wireless mic with a free flow answered 0x0314 to our fpp-4 request every 72 s; ARC names no fpp, so the ARC directory offers the config's `arc_rx_fpp` to every device. The step is per transmitter, kept while the device runs (at most 64 transmitters), applied only where the flow still fits a datagram, and other transmitters keep their fpp. A step counts no failure, so the next request comes after 9 s. A refusal at 32 counts as a failure with the usual backoff. Logs: "RX flow refused (0x0314, its fpp): the next request asks a larger fpp" with `fpp` and `next`; the accepted fpp is the one on the "RX flow requested" line before "RX flow granted".

## 0.3.5 (2026-10-31)

- Fix (dstack #1, quirk catalog): Dante Controller's ARC 0x2204 got no reply, and Dante Controller asked it, and 0x2200 with it, 3 times per read. It now gets status 0x0030 and no body, as the IEM card (a Yamaha Dante card) answers it in the 30.10 capture; replay test `dante_controller_s_0x2204_is_answered_as_the_iem_card_answers_it`. This is a card's answer, not the 0xffff refusal that once caused a request storm. On dev1, 2 minutes after the start, each of the two Dante Controllers on the network asked 0x2204 and 0x2200 once, with no warning.
- Finding (dev1, 31.10): on its own Dante address our device is read by Dante Controller in full (ARC 0x1000 to 0x3300 and 8700 0x13, 0x21, 0x61, 0xc1). On a Windows PC's SkipAsSource extra address it got nothing, because its mDNS answers leave from the PC's primary address. The fix for that is the next step.

## 0.3.4 (2026-10-31)

- Fix (iemmixer, dstack #1 problem C): an ARC-resolved transmitter with fewer than 4 TX channels was asked for a flow of 4 slots. A 2-channel wireless mic refused that with flow-control status 0x0311 every 72 s (captured: n = 4, fpp 4, 96 kHz, 24 bits), and the channel showed 0x15 TxFail. The directory now offers `min(NCHAN, the device's TX channels)` slots (`subscribe::directory::offer_in`), as Dante's own records count nchan and as our announcement does. Test: devices of 1, 2, 3, 4 and 16 channels get 1, 2, 3, 4 and 4 slots. If the mic still refuses with 2 slots, the next step is an fpp step on 0x0311, from a new capture.

## 0.3.3 (2026-10-31)

- Fix (iemmixer, dstack #1): a device with more mDNS records than mdns-sd's command queue holds (100; iemmixer's has about 200) could end at start with "announcing the device: mDNS record 100 (_netaudio-chan._udp.local.): try again". mdns-sd answers `Error::Again` while its queue is full. The announcer now waits that out, 1 ms between tries and at most `announce::AGAIN_TRIES` (2000) tries a call (2 s at least), for registrations and withdrawals. A queue still full after that is a daemon that does not run, and stays an error. `Announcer::waited()` counts the waits, and the "mDNS records announced" lines log them as `queue_waits`. Tests: a fake daemon whose queue is full for some calls, one that stays full, the wait count, and a real mdns-sd daemon taking 300 records at once.

## 0.3.2 (2026-10-31)

- New: `arc::control::subscribe(c, rx, tx_channel, tx_device)` and `arc::control::unsubscribe(c, rx)` (dstack #16 step 3, asked by dcontroller). One explicit ARC request each, 0x3010 and 0x3014, byte-exact with netaudio's `add_subscription`/`remove_subscription` (`subscribe_request`, `unsubscribe_request`). A refusal is `ReadError::Refused` with the device's status. A device answers OK also when it changed nothing, so read the channel back with `rx_channels`. Tests: the frames byte for byte, our own server in process, and in `tests/system_clock.rs` our running device subscribed over the network (its RX 2 to its own TX 1), read back, then unsubscribed. The stack itself never calls them: whether a device may be changed is the application's call (the owner's rule).

## 0.3.1 (2026-10-31)

- New: `arc::control::rx_channels(c)` (dstack #16 step 2, asked by dcontroller). Every RX channel of any device as `RxChannel { channel, label, tx_channel, tx_device, status }`, read from 0x1000's RX count and 0x3000 page by page. `tx_channel`/`tx_device` are `None` when the channel is not subscribed. `status` is the descriptor's 32-bit code (bytes 12 to 15; 0 when not subscribed), and `RxChannel::sub_status()` names it when it is a known code. `labels` keeps its output and reads through the same parser.
- New: `subscribe::SubStatus::from_code(u32)`, the inverse of `SubStatus::code`.
- Tests: a test that captures logs with a scoped subscriber sometimes captured nothing (seen 1 run in 4 in `device::tests::a_fitted_state_is_logged_once_by_its_counts_only` once more tests ran). Cause: tracing caches a callsite's interest, and a callsite first hit on another thread with no subscriber kept "never". A test-only global subscriber answers "sometimes" for every callsite (`test_log`), so no interest is cached as never (0 failures in 60 runs). No change to the stack's code.

## 0.3.0 (2026-10-31)

- New: `discover`, device discovery for a controller (dstack #16 step 1, asked by dcontroller). `Discovery::start(ip)` runs an mDNS daemon of its own on the interface with that address only and browses `_netaudio-arc` and `_netaudio-dbc`. `Discovery::devices()` gives the list now, and the returned `Receiver<Change>` gives each `Added`/`Changed`/`Removed` for a live list. `Discovery::stop()` waits at most 3 s. A `Device` is listed once its `_netaudio-arc` record resolves: `name` (its case kept), `arc` (lowest IPv4 and port), `dbc` (when published) and `txt` (the ARC record's TXT strings: `mf`, `model`, `router_info`, ...). The pure table `Devices::apply` is public too. At most 256 devices and 32 TXT strings each. It sends nothing to any device. `Discovery::start_with` takes any `subscribe::resolve::Browse`, for tests. Tests: the table, live discovery through a fake daemon, and in `tests/system_clock.rs` our own running device discovered with its ARC port and TXT, then removed by its goodbyes.

## 0.2.8 (2026-10-31)

- The project is renamed `dstack` (repo `zbynekdrlik/dstack`; GitHub redirects the old URL). The crate names stay: `dante-stack` (lib `dante_stack`), `dante-audio-io`, the binary `dante-stack-dev`. They name what the crates are, and a consumer changes only its `git =` URL, not its `package =` or its code. No change to the stack's code.

## 0.2.7 (2026-10-30)

- The project playbook: `.claude/rules/release.md` (bump, merge, tag, changelog, dependencies) and `.claude/rules/tests.md` (whole-device tests, timing on shared runners, Windows paths, the mutation gate). No change to the stack's code.

## 0.2.6 (2026-10-30)

- CI: a diff-scoped mutation gate on every PR (`mutation`, cargo-mutants 27.1.0 with nextest, 4 shards of at most 20 minutes, the unit tests and tests/hot_path.rs per mutant): a mutant of the PR's diff that the tests miss fails the PR. A full-tree sweep (`mutation-full.yml`) runs on demand only and files survivors as test-quality issues. No change to the stack's code.

## 0.2.5 (2026-10-30)

- New (request #2): Dante Controller can rename our device. ARC 0x1001 (netaudio's `set_name`, body = the name NUL-ended) sets the device's name when Dante holds it (1 to 31 letters, digits and hyphens, no hyphen at either end): saved in the state file, the records announced under the new name, our flow requests carry it (`Manager::set_host`, `Flows::renamed`, `Change::device_renamed`), and our own RX subscriptions to our own TX move to the new name. A foreign receiver subscribed to the old name loses us, as with any renamed Dante device (its subscriptions are its owner's to change). The same name changes nothing; anything else is refused. Inferno leaves 0x1001 unanswered; Dante Controller's own frame is still to be captured (#1).
- API: `arc::Flows` gains `renamed(&self, name)`; `arc::Change` gains `device_renamed`.

## 0.2.4 (2026-10-30)

- New: `arc::control`: read another device's name (0x1002), counts (0x1000) and every channel's label (TX: the 0x2010 own name else the 0x2000 default; RX: 0x3000), set or reset a channel's label (0x2013 / 0x3001) and set the device's name (0x1001), with netaudio v0.0.10's requests byte for byte, over the new `Call` trait (`arc::client::Udp` implements it). Every request is tested against netaudio's frames and round-tripped through our own ARC server.
- New: `card read|write|rename <ip> <expected-name> ...` on the device CLI (`daemon::card`, `Command::Card`), iemmixer's `dante_card.py` contract (JSON lines, the flag file, the read back, exit 0/1/2), so iemmixer can drop netaudio. One difference: a write the device refuses or leaves unanswered is `failed` and stops the writes (netaudio counted an unanswered write as sent). Tested in process and over the network against our own running device; a real card's replies to 0x3001/0x2013/0x1001 and its 0x2010 paging past 32 channels are still to be captured (#1).

## 0.2.3 (2026-10-30)

- Proven: a TX-only device (an empty RX list) sets up, starts, grants a receiver's flow of two channels and sends them stamped in order (`tests/system_clock.rs` `a_tx_only_device_serves_a_receiver`, Linux and Windows), for songplayer. No code change was needed. The whole-device tests now run one device at a time (they bind the same ports).

## 0.2.2 (2026-10-30)

- New, TEST ONLY: `host::ClockSource { Dantesync, System }` and `host::prepare_with(opts, engine_tx, source)`; `host::prepare` is unchanged (dantesync). `System` takes PTP time from the machine's own clock, ready at once, no dantesync poller, a warning at start, and the status's `clock.source` reads `system` (else `dantesync`; added field): for a whole device process on a box with no PTP network (CI). CLI: `--clock dantesync|system` on the run command (`daemon::Command::RunOnSystemClock`). Requested by dante-asio for its whole-process E2E. Test `tests/system_clock.rs` (Linux and Windows): the real host at a 10 ms latency, RX 1 subscribed to our own TX 1; after the start, a clean second of the counter, no TX schedule restart, every packet within the latency (the lateness bins are printed; real-time lateness is the hardware gate's).

## 0.2.1 (2026-10-30)

- New: coverage-guided fuzzing in CI (`fuzz/`, cargo-fuzz): `arc_server`, `flows_server`, `cmc`, `info`, `dsyx`, `media_packet` and `replies` (ARC and flow-control replies, a grant's handle, a string at an offset), 30 s each on every push. No change to the stack's code.

## 0.2.0 (2026-10-30)

Breaking: the box health moves to the application (iemmixer, agreed on #1).

- Kept in `health`: `Clock`, `Pump`, `Status`, `parse_status`, `read_status` (Linux). New: `DeviceReason` and `device_reasons(first, second, engine)`, the device's own reasons from two status reads a second apart: `NoStatus`, `ClockNotReady`, `Holdover`, and with an engine `PumpFree`, `PumpStalled`, `PumpRebootstrap` (the rules the box verdict used for these).
- Removed from `health`: `UNITS`, `PROPERTIES`, `SERVER_PORT`, `CLOCK_GRACE_US`, `Options`, `Active`, `UnitResult`, `Unit`, `Units`, `parse_units`, `ServerAnswer`, `server_answer`, `ask_server`, `State`, `Reason`, `Build` (the enum), `Verdict`, `verdict`, `Verdict::line`, `check`; internal to it: the glue constants `READ_APART`, `STATUS_WAIT`, `SERVER_WAIT`, `VERSION_REQUEST` (`MAX_READ` stays internal to `read_status`).
- Removed from `daemon`: `Command::Health` and the `health` subcommand (`parse_args` reads `health` as an unknown argument); `daemon::cli` no longer serves it.
- To port (iemmixer): the code and tests as of `fdfc321` (v0.1.2): `crates/dante-stack/src/health.rs`, `crates/dante-stack/src/health/tests.rs`, `Command::Health`, `parse_health` and its test `health_takes_a_socket_and_a_server_port` in `crates/dante-stack/src/daemon.rs` / `daemon/tests.rs`, and `health_line` in `crates/dante-stack/src/daemon/cli.rs`; `ask_server` needs its own copy of `MAX_READ` (1 MiB).

## 0.1.2 (2026-10-30)

- New: each TX lane counts how late its packets left past due, in bins < 0.25, < 0.5, < 1, < 2, < 5 and >= 5 ms (`LaneCounts::late_bins`), with the largest lateness (`late_max_ns`) and the longest single send (`send_max_ns`) since the device started; the control socket's status shows `late_bins`, `late_max_us` and `send_max_us` per TX flow (added fields). A stall inside the latency, which sends every packet late, used to read 0 in every counter (problem A, the 5.4 ms stall of 30.10 on the FOH PC); catalog test `a_stall_within_the_latency_is_counted_as_late_packets`.
- Removed: the hot-path log of a send blocking over 200 us (Windows experiment); `send_max_us` replaces it, and the hot path logs nothing again.

## 0.1.1 (2026-10-30)

- Fixed: `dante-audio-io` did not build with `--cfg iem_rtsan` (E0658: its crate root lacked `feature(sanitize)` for `hist::PeriodHist::record`), so an application's rtsan job failed (iemmixer). New CI job `rtsan` runs `tests/rtsan.rs`: the pump block and its histograms under RealtimeSanitizer, plus a self-test violation.

## 0.1.0 (2026-10-30)

First release: the Dante stack moves out of its two application copies.

- `dante-stack` (lib `dante_stack`): iemmixer's `iem-dante` (iemmixer `a510531`, frozen there at `131f44a`, no later change) with dante-asio's Windows port (dante-asio `21e79fa`): ARC, announce, CMC, flows, media TX/RX, the clock client of dantesync, health, the engine's pump, the device daemon.
- `dante-audio-io`: the callback contract (`Process`, `Block`, `StreamStats`) and the stream histograms, from iemmixer's `iem-audio-io`, which re-exports them.
- The device CLI is a library function, `daemon::cli::main(Build)`: each application keeps a thin binary with its own name, version and build (`dante-stack-dev` here, `iem-dante-dev` in iemmixer). `health::check` takes the build to compare.
- Changed against iemmixer's copy, by the Windows port (dante-asio), and visible to an application:
  - `stream::BLOCKS` and the device config's `block` take 16, 32, 64, 128 or 256 frames (was 16 or 32). An application whose own block contract is narrower checks it itself.
  - Windows only: the media threads run at `THREAD_PRIORITY_TIME_CRITICAL` (31 in a REALTIME_PRIORITY_CLASS process) instead of MMCSS "Pro Audio"; their waits are a yielding spin, never the timer.
  - The TX thread logs why its schedule starts again (the lag or a jump); on Windows it also logs a send that blocks over 200 us.
  - `daemon::version_line()` is gone (`Build::line`), `health::check` takes the binary's `Build`, the help text is `daemon::usage(name)`.
- Fixed quirks: none yet (the quirk catalog starts with the open TX/RX problems).

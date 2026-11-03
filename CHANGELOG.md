# Changelog

## 0.6.8 (2026-11-03)

- Fix (obs, camera-box#1485): an RX ring now counts a sample late when the reader passed its position while the write ran.
  - Before, `Ring::write_at` checked the reader once before its loop. With the RX thread and the reader on one CPU, the reader read holes and the writer then stored those samples as `written`: silence reached the app with `late` at 0.
  - Now a read that finds a hole marks the slot by compare-and-swap, and the writer stores by compare-and-swap, so that sample is `late`. Ring `holes` at written positions now equal `late`.
  - New: `ring::READ_EMPTY`. The ring's lap is 31 bits (positions 2^47 apart, 46 years at 96 kHz, share a slot; was 2^48).
- Docs (dstack #1): the FOH measurement of `spin_tail_ns` in `docs/windows-box.md`: 150 µs saved about 1.3 CPU per dasio device and lowered the receiver's lateness.

## 0.6.7 (2026-11-03)

- Changed (dstack #68): dsync-ptp v1.27.2 for the in-process PTP follower.
  - It fixes the follower's boot fit, which re-seeded the frequency wrong after the first windows and left a fresh follower 20-40 us off for minutes (mbc, Windows).
  - It holds a late measurement window instead of steering on it, the same rule as the dsync service.
  - The follower logs its engagement and boot fit, so a lock problem is readable from the device log.
  - No API change for consumers.

## 0.6.6 (2026-11-03)

- New (dstack #1, dasio FOH data): `spin_tail_ns` in the device config (Windows, low-latency profile).
  - It sets how long before each deadline the media threads stop sleeping on the high-resolution timer and spin. The range is 0 to 50 ms, and the default stays 50 ms, so nothing changes without it.
  - On FOH two dasio devices at 1 ms spin 4–6 CPUs at 100 %. A tail of 100–300 µs frees them where the timer wakes on time; measure it first (`docs/windows-box.md`).
  - New: `clock::{SPIN_TAIL_NS, SPIN_TAIL_MAX_NS, spin_tail_of, spin_tail_for, set_spin_tail_ns}`.
  - New public field: `DeviceConfig::spin_tail_ns`. Struct literals without `..` must set it.

## 0.6.5 (2026-11-03)

- New (dstack #68): dsync's in-process PTP follower runs on Windows too (dsync-ptp v1.27.1, which loads Npcap's `wpcap.dll` at run time).
  - A dlink device on Windows is now locked to PTP on its own when Npcap is installed, with or without the dsync service.
  - Without Npcap, or if the start fails, it logs the reason in a word (`Npcap is not installed`, `port 319 needs CAP_NET_BIND_SERVICE or root`, `no such interface, or no IPv4 on it`) and runs on the service's clock, as before.
  - dsync-ptp v1.27.1 also interpolates its wall-to-raw stamp conversion, so a read delay under host slew no longer skews stamps (dsync#136).
  - New: `clock::linked::follower_failure`.

## 0.6.4 (2026-11-03)

- New (dstack #90, vstack#8): two additions to `asrc::Controller` for vstack rig-core's genlock pairing.
  - `advance(ours_s, other_s)` takes a rate point as two advances over the same stretch (`audio_asrc_rate_point`'s master stamp advance and raw duration), summed into the fit.
  - `saturated()` says the correction sits at ±300 ppm (`asrc_saturated`).

## 0.6.3 (2026-11-03)

- New (dstack #20, intercom#5): an app reads each RX channel's state. `dlink::Link::rx_channels()` and `Media::rx_channels()` give each channel its subscription, status (`SubStatus`, with `label()` and `state()`), the flow it rides (from 1), that flow's packets and the channel's written samples. A `written` count that stops while the status reads receiving is a lost flow; a growing one with silence is a quiet source. The intercom had no way to tell the two apart. New: `daemon::media::RxChannel`; dlink re-exports `RxChannel`, `SubStatus` and `SubState`.

## 0.6.2 (2026-11-03)

- Fix (dstack #20, FOH 3.11.2026): two Dante devices on one PC address (dasio's master-asio-dante on 4455, band-asio-dante on 4456) were one transmitter to our subscriber.
  - Their channels went into one request to one of them, and the other's were asked again every 250 ms. The intercom heard the wrong channels, then lost the flow.
  - Requests, flows, failures and fpp floors are now per flow-control address (address and port).
  - Port: `Manager::peers` (a public field) is keyed by `SocketAddrV4`, no longer `Ipv4Addr`.
- New (dstack #90, phase 2): the ASRC in `dstack-resample`.
  - `asrc::Controller` holds a buffer between two clocks at its target. Its laws are SongPlayer's ASIO servo (#233) made generic:
    - a least-squares rate fit (600 s, locked after 30 points over 60 s, a 10 ms step re-bases it);
    - the level loop (P 2 ppm/ms on a 10 s EMA, ±50; I, ±3, frozen at saturation);
    - an offset drained by the ratio, never skipped;
    - at most ±300 ppm, moved at most 5 ppm/s.
  - A re-centre (insert or skip under fades) is asked for only past the floor or the excess, and counted.
  - The outside input from vstack's genlock pairing: a `Basis`, a placement error and a bias of at most ±10 ppm.
  - `Converter::drifting` keeps the sinc stage at equal rates. `Converter::set_ppm` steers it.
  - New: `dstack_resample::asrc::{Controller, Step, Basis, RateFit, LevelLoop, Offered, slew, braking_ppm}` and its constants, plus `Converter::{drifting, set_ppm}`.
- Correction to 0.6.1's notes: publishing `_netaudio-dbc` is right, because every Dante device does it, but it did not cause DVS's status 21 that night. DVS reaches a flows port of 4456 without it. The causes were the fpp 60 refusal (fixed in 0.6.0) and nothing listening on 4455 while instance 1 was stopped.

## 0.6.1 (2026-11-03)

- Fix (dstack #1): the device now announces `_netaudio-dbc._udp` on its flow-control port, with an empty TXT, as every Dante device on the site does. Without it, DVS sent its flow requests to 4455 whatever our chan records said. A second instance on one address (4456) was unreachable from DVS: the FOH gate's status 21 before the move to the default ports.
- Fix (dstack #16, dcontroller): `meter::parse_frame` reads version 3 frames. A 128-channel Yamaha card (AIC128-D, Dante PCIe IF 4.0.13.3) sends them, and every one was dropped before. In version 3, byte 25 is 0, the counts are big-endian u16s at 26 and 28, and the levels start at 30. New: `meter::FRAME_VERSION_3`.

## 0.6.0 (2026-11-03)

- Fix (dstack #1, FOH 3.11.2026): we refused Dante Virtual Soundcard's flow requests. DVS asks fpp 60 whatever we advertise, and our device advertised fpp 8,2 at 1 ms. Every request got 0x0302, and DVS showed status 21 on all its channels.
  - A request is now granted while its packet, our `tx_margin_ns` and the 50 µs send slack fit our advertised latency. This is the config's own rule for `tx_fpp_max`, which stays what we advertise. At 1 ms, 96 kHz and the default margin, that is up to 67 frames.
  - Each refusal is warned (`flow request refused`) with the request's fields and reason, once a minute per requester and reason. Before, it was logged at debug only.
  - New public field: `TxTable::warned`.
- New (dstack #90, owner 3.11.2026): the crate `dstack-resample`, one rate converter for our apps, with no Dante code in it.
  - The core is rubato's band-limited sinc (MIT OR Apache-2.0) with SongPlayer's measured settings: 256 taps, a 256× table, BlackmanHarris², cubic interpolation, automatic cutoff.
  - Equal rates are a plain copy. Tests hold THD+N above 120 dB at 96↔48 and 44.1↔48 kHz, and the folded band below −120 dBFS.
  - New: `dstack_resample::{Converter, Fixed, ConvertError, PARAMS}`.
  - The ASRC between two clocks follows in phase 2 (#90).
- Changed (dstack #90): dlink converts any app rate (8 to 384 kHz) to the network's, so an app keeps its own rate.
  - `Config::network_rate` sets the device's rate, and `Config::sample_rate` stays the app's.
  - `dlink::Rated` replaces v0.5.20's 2:1 `Halve`. RX is converted from the device's block. The app gets about `block × ratio` frames, which varies by a frame for a ratio that does not divide the block (44.1 kHz). TX takes the app's frames through a small FIFO into exactly the device's block.
  - Port: `Halve` and `Config::halved` are gone (use `Config::converts`). Apps at 48 kHz on 96 kHz keep working unchanged.

## 0.5.20 (2026-11-03)

- New (dstack #20, intercom#5): dlink runs a 48 kHz app on a 96 kHz network. `Config::network_rate(hz)` sets the device's rate (the network's); `Config::sample_rate` stays the app's. With the network at twice the app's rate, a 2:1 step (`dlink::Halve`) sits between them. RX is low-passed and every second sample kept; TX is zero-stuffed and low-passed. One half-band FIR (63 taps, Kaiser) is flat to about 20 kHz, holds the band that would fold back down by at least 60 dB, and delays 0.16 ms each way. Same clock, so no drift. The app's blocks have half the frames and half the media time. Any other ratio is refused at start. New: `Config::{network_rate, device_rate, halved}`, `dlink::Halve`.
- Fix (dstack #20): an RX channel subscribed to a transmitter at another sample rate now reads CHANNEL_FORMAT (16), as Dante Controller names it, instead of TX_FAIL (21). The device logs one warning per subscription naming both rates. It never asks such a transmitter: a Dante flow needs one rate at both ends.

## 0.5.19 (2026-11-03)

- Log (dstack #1, dasio#1): a TX flow to a destination on the same PC (another device of ours on the same address) is logged at info, "not marked", instead of a warning. QoS2 has no route to it (`QOSAddSocketToFlow` 1168, `ERROR_NOT_FOUND`), and its packets never leave the NIC. New: `media::dscp::{local_destination, ERROR_NOT_FOUND}`.

## 0.5.18 (2026-11-03)

- Fix (dstack #1, dasio#1): on Windows our TX audio now leaves marked DSCP 46 (EF), as Dante hardware marks it.
  - Windows ignores `IP_TOS` from a program, so without a QoS policy on the PC our audio went out with DSCP 0. dasio's NDIS filter on the FOH PC deferred such a flow behind TCP: peaks of 4–7.5 ms and ~350 late packets/s at the receiver, while the card's EF flow into the same receiver was clean.
  - The control side adds each TX flow socket to a QoS2 flow per destination and sets its DSCP (qwave.dll). Windows allows this only for an administrator process (a service is one). Another process gets one warning naming the QoS policy recipe (`docs/windows-box.md`). Linux is unchanged (`IP_TOS` when the socket opens).
  - New: `media::dscp::{Marks, changes, verdict, Verdict}`.

## 0.5.17 (2026-11-03)

- New (dstack #16, dcontroller): `latency::Listener::start(our) -> io::Result<(Listener, mpsc::Receiver<(Ipv4Addr, Beat)>)>`.
  - It keeps one socket on the heartbeat group and hands on every parsed heartbeat with its sender's address, for as long as it runs. A recorder of every receiver's latency keeps one listener per network instead of a `listen` per device and window, which left gaps.
  - Dropping the `Listener` stops its thread (within about 0.2 s) and closes the channel. The thread also ends when the receiver is dropped, or on a socket error (logged).
  - `latency::listen` is unchanged.

## 0.5.16 (2026-11-02)

- New (dstack #1, dasio#1): the NIC check at start (Windows) reads the Dante NIC's driver.
  - On the FOH PC, the Realtek drivers `10.79.20.1003` and `10.72.524.2024` bring a 20–30 ms receive DPC about every 100 s: late packets at receivers, failed sends. `10.39.212.2020` is clean.
  - A measured-bad Realtek driver gets a warning naming the good one. Another Realtek version gets an info line saying it was never measured (two points, no range is called bad). Other vendors get no line.
  - New: `lso::{driver, Driver, REALTEK_BAD, REALTEK_GOOD}`, `lso::check_driver` (Windows).
- Docs: `docs/windows-box.md` lists the driver.

## 0.5.15 (2026-11-02)

- Fix (dstack #1, dasio#1): our mDNS daemon opens no IPv6 socket.
  - mdns-sd always bound `[::]:5353`, which received every interface's mDNS. On the FOH PC (Windows 10) it is the next suspect for the 16–32 ms WFP receive DPC behind dasio's late packets. `flow_slots` ruled out the idle slot sockets.
  - dstack now uses a vendored mdns-sd 0.21.6 (`vendor/mdns-sd`, Apache-2.0 OR MIT) with that one change (see `vendor/mdns-sd/DSTACK.md`). It is a path dependency inside the repo, so consumers get it through the tag and need no change.
  - Test: `tests/mdns_ipv4_only.rs` (Linux).

## 0.5.14 (2026-11-02)

- New (dstack #1, dasio#1): `flow_slots` in the device config (1–32, default 32). Only that many TX and RX flow sockets are opened, and at most that many flows go each way.
  - Before, every device opened 32 TX and 32 RX sockets at start, most of them idle.
  - On a Windows 10 PC they are suspected of lengthening a WFP receive DPC that stalls the NIC's sends. This is the knob for the A/B on the FOH PC.
  - dlink: `Config::flow_slots(n)`.
  - New: `RxShared::with_slots`, `TxTable::with_slots`, `DeviceConfig::flow_slots`. A config that builds `DeviceConfig` without `..` must set it.

## 0.5.13 (2026-11-02)

- Fix (dasio#1, the FOH PC's send failures every ~96 s): each TX flow socket asks for a 1 MiB send buffer (`media::tx::SEND_BUFFER_BYTES`). Before, it kept the OS default.
  - The cause on that PC: a 16–32 ms NIC receive DPC holds the send completions. The default buffer filled within it, and every send failed with WSAENOBUFS.
  - A packet held through the stall goes out late. Within the receiver's latency it still plays.
  - The TX start logs "TX flow sockets' send buffer" with what the OS gave. `TxShared::send_buffer` is new.

## 0.5.12 (2026-11-02)

- Fix (intercom's report on 0.5.11): declared RX subscriptions are kept out of the state file.
  - In 0.5.11, the first save after any other change (a Dante Controller subscription, a latency drop) wrote them too. From then on, they beat a later config edit.
  - Now `DeviceState::declared` (never serialized) marks the channels the config filled, and `save` leaves them out. A Dante Controller change of that channel, or a rename that moves it, clears the mark.
  - New public field: `DeviceState::declared`. Code that builds `DeviceState` without `..` must set it.
- New (dsync 1.26, dsync#150): the clock stats name dsync's master. This is the source of the Sync it follows: the grandmaster unless a boundary clock relays it.
  - It goes in byte 0x14; the grandmaster stays in 0x1c.
  - `clock::Reading::master` reads DSYX bytes 112–117 (all zeros from an older dsync). `ClockStats::{set_master, master}`, `info::Follower::master`.
  - `info::server::follower_of` takes the master; the grandmaster stands in when dsync names none.
- Changed: dsync-ptp v1.26.0 (was v1.24.0). It fixes a seqlock read that could go back in time.

## 0.5.11 (2026-11-02)

- New (#20, intercom#5): an app declares its RX subscriptions.
  - dlink: `Config::subscribe(rx, tx_channel, tx_device)`, where `rx` is the RX channel number from 1.
  - Device config TOML: `[[subscribe]]` with `rx`, `tx_channel`, `tx_device`, checked with the rest of the config (a channel that exists, each `rx` once, Dante names).
  - At start, an RX channel the state file holds no subscription for takes its declared one. One Dante Controller set is saved in the state file and wins. Declared ones are not written into the file. To drop a declared one, remove it from the config.
  - New public items: `device::Declared`, `DeviceConfig::subscribe`, `DeviceState::declare`. A config that builds `DeviceConfig` without `..` must set `subscribe`.

## 0.5.10 (2026-11-02)

- New (#1): our board info claims the pull-up, the encoding and the sample rate (feature flags `00 00 12 18`, was `00 00 10 00`). Dante Controller now shows 96k / NONE / PCM 24 in Device Config, each marked as not configurable. Reboot and factory reset stay off.
  - Info 0x83 (encoding) and 0x85 (pull-up) are answered in a Tascam's layout, with our one value as the only option (`info::encoding_body`, `info::PULLUP_BODY`, `op::ENCODING`, `op::PULLUP`, `Kind::Encoding`, `Kind::Pullup`). A write gets the same answer, unchanged.
  - Both replies count in the reply bound (`limit::BOUNDED` has 8 kinds).
- Changed: `info::Identity` has a new field `encoding` (from the config). Code that builds an `Identity` literally must set it; `Identity::new` sets it.

## 0.5.9 (2026-11-02)

- New (dasio#1, FOH send errors every ~96 s): a failed media send keeps its OS error.
  - `LaneCounts::last_os_error` (the raw errno or WSA code, -1 without one) is in the status too.
  - Once a second, a TX flow whose failed sends rose gets one warning line, "TX sends failed", with `slot`, `errors` and `os_error` (`media::late::SendWatch`).
  - A failed packet is never sent again; it is a hole at the receiver.

## 0.5.8 (2026-11-02)

- New: our devices answer clock stats (8700 op 0x21) as a PTP follower of dsync's grandmaster.
  - The reply uses a hardware follower's layout: clock and servo state 3, one port following, our MAC as our clock id, the grandmaster's UUID from DSYX bytes 42–47 as master and grandmaster (dsync 1.17 on; dsync 1.26 will add the master's own), and dsync's frequency adjustment.
  - No reply while the media clock is not ready or no grandmaster is known.
  - Before this, Dante Controller kept the device's config half-loaded ("Retrieving model information"); now it loads the clocking section.
- New: `clock::Reading::grandmaster`, `ClockStats::{set_grandmaster, grandmaster}`, `info::{Follower, clock_stats_body, follower_of}`, `Beat::clock`, `Multicaster::set_clock`.

## 0.5.7 (2026-11-02)

- Fix: Dante Controller shows our device's latency. Its settings read (ARC 0x1100 with start code 0x27ff and 19 keys) got zeros, so Device Config showed 0.0 ms, greyed out (seen on mbc). It now gets the keyed answer the hardware gives, as netaudio's read already did.
- The settings now also state the latency default (Dante's 1 ms, fixed: it never moves with a write) and the range Dante Controller may set: min `0x8306` (250 µs, or 4 ms under the normal profile) and max `0x8302` (half the rings, 296 ms at 96 kHz). `DeviceConfig::latency_range`.
- Changed: a latency write outside that range is refused (`DeviceConfig::with_latency`).

## 0.5.6 (2026-11-02)

- Breaking, names (#68, owner: no "dante" in our names; dsync is dantesync's new name, dsync#147):
  - `--clock dantesync` is now `--clock dsync` (left out, it is the default, as before).
  - `host::ClockSource::Dantesync` is `ClockSource::Dsync`.
  - `clock::poll_dantesync` is `poll_dsync`, and `clock::DANTESYNC` is `DSYNC`.
  - The control socket's status says clock source `"dsync"` where it said `"dantesync"`.
  - Docs and logs say dsync. The DSYX protocol on UDP 31900 is unchanged.
- Breaking, the control socket's default path is `/run/dstack/control.sock` (was `/run/iem-dante/control.sock`). iemmixer passes its own path and is not affected.
- Catalog test: a dsync boot with a large phase error is ready only at the lock (the iem-mixer to FOH episodes of 1.11).

## 0.5.5 (2026-11-02)

- New (#1): once a minute the device compares its NIC's own sent-bytes counter with its audio's wire bytes. It warns ("the Dante NIC carries other traffic") when the rest averages over 100 kB/s, because our audio waits behind it in the NIC's queue. Linux reads `/sys/class/net/<nic>/statistics/tx_bytes`; Windows reads `GetIfEntry2`'s `OutOctets`.
- New: `media::tx::LaneCounts::bytes` and `TxShared::bytes_sent` (wire bytes: the datagram plus 42 header bytes).

## 0.5.4 (2026-11-02)

- New: dlink's media clock is dsync's in-process PTP follower (dsync-ptp, dsync v1.24.0) by default (#68, owner 2.11.2026).
  - `host::ClockSource::Linked` and `clock::linked::Linked`: the follower when it is locked, the dsync service (DSYX) otherwise, the follower's holdover after a lock when neither.
  - A selector picks the source once a second and logs every change; a change moves the epoch, so the media re-aligns.
  - The device has a clock whether the dsync service runs or not, and is never worse than the service: when the follower cannot start, it runs on the service as before.
  - Linux only for now: the follower needs port 319 (CAP_NET_BIND_SERVICE or root); without it the start logs why. Windows follows when dsync loads wpcap.dll at run time.
  - `dlink::Link::start` uses it; the daemon and `embed::Embedded` keep the service (`Dantesync`) unless asked.
- Dependencies: `dsync-ptp` (git tag v1.24.0, Linux only). `cc` is pinned to 1.6.0 and `find-msvc-tools` to 0.1.14 (versions under two weeks old are refused).

## 0.5.3 (2026-11-02)

- New on Windows (#1): the start line checks the Dante NIC's large send offload (the adapter's `*LsoV2IPv4` / `*LsoV1IPv4` settings, read by its GUID) and warns when it is on. With LSO on, another sender's large sends delay our packets. The device never changes the NIC; turning LSO off is the PC owner's step. `lso::verdict` is the decision.

## 0.5.2 (2026-11-02)

- New in dlink (intercom#5: the intercom hub and OBS as two dlink devices on one PC):
  - `Config::ports(Ports { arc, cmc, flows })` sets a second device's control ports.
  - `Config::process_id(id)` gives each device on an address its own id.
  - `Config::rx_port_base(base)` fixes the RX flow ports for a firewall.
  - Unset, they keep Dante's defaults and OS-picked RX ports.
- Fix: under the normal profile (dlink) a media thread at normal priority logs at info ("runs at normal priority (the normal timing profile)"). A warning now means only that real-time priority was asked and refused. The lines name the thread in a `thread` field.

## 0.5.1 (2026-11-02)

- New: the `dlink` crate (#68, owner 2.11.2026), the Dante device an application embeds (OBS, SongPlayer, the intercom).
  - `dlink::Config::new(name, state_file).rx([...]).tx([...])`, then `dlink::Link::start(&config, &place, map, processor)` and `link.stop()`.
  - Its defaults: 10 ms device latency, 48 kHz, 24-bit, the `normal` timing profile, and a pump margin of half the latency.
  - It asks the app for no real-time setup: no SCHED_FIFO request, no CPU pinning, no spinning core.
  - It re-exports `Block`, `Process`, `Map` and `Place`.
- Changed: the `normal` timing profile is taken again (it was withdrawn on 31.10).
  - It now needs `latency_ns` of 4 ms or more and `work_margin` of 2 ms or more; the config refuses less. `device::normal_work_margin` gives half the latency.
  - On Windows its media threads opt out of power throttling (our threads only).
  - A thread without the high-resolution waitable timer logs a warning once instead of silently waiting on the 15.6 ms system tick.
  - Before an app uses it in production, a run judged on a real receiver's latency records is required (#68).

## 0.5.0 (2026-11-02)

- **Breaking, the rename (#68, owner 2.11.2026: no "dante" in our names).** Port it by search and replace; nothing else changes.
  - In `Cargo.toml`: `dante-stack = { git = "https://github.com/zbynekdrlik/dstack", tag = "v0.5.0" }` becomes `dstack = { git = …, tag = "v0.5.0" }`, and `dante-audio-io` becomes `dstack-audio-io`.
  - In Rust: `dante_stack::` becomes `dstack::`, and `dante_audio_io::` becomes `dstack_audio_io::`.
  - The binary `dante-stack-dev` is `dstack-dev`. The release assets are `dstack-dev-<tag>-<suffix>`. `daemon::NAME` and the version line say `dstack-dev`.
  - The rtsan probe's environment variable `DANTE_RTSAN_PROBE` is `DSTACK_RTSAN_PROBE`.
  - Thread names: `dante-tx`, `dante-rx`, `dante-engine`, `dante-meter`, `dante-device`, `dante-arc-read`, `dante-discover-*` and `dante-mdns-*` are now `dstack-…`. Anything that pins or watches a thread by name moves with them.
  - The test config is `config/test-device.toml`.
  - Unchanged in this release: the control socket's default `/run/iem-dante/control.sock`, the state path a config names, and every protocol field (`dante_name` in the status is the Dante channel name).

## 0.4.39 (2026-11-02)

- Internal: `info.rs` is split into the pure packet builder (`info`) and the socket loops and record source (`info/server.rs`). Every public path stays: `info::{Sources, Levels, Multicast, serve_info, serve_info_with, serve_member, serve_shared}` are re-exported from `info`. No behavior change; nothing for a consumer to port.
- Fix: an `rx_port_base` too high for the 32 RX flow ports is refused before any port is bound, with `rx_port_base: past port 65535`. Before, the ports were bound in order and a held or reserved port below 65535 (Windows' excluded ranges) gave `RX flow port N` instead.

## 0.4.38 (2026-11-02)

- New: `dante_audio_io::Block::start()`, the media sample time of the block's first input sample (`None` when the engine runs without a media clock). The embedded pump sets it on every block (`Block::at`), and its outputs go out at `start + D` (`PumpView::offset`). Consecutive blocks are `frames` apart. A re-bootstrap's skipped blocks, an epoch change or a latency retime show as a jump in it, so an app can number its blocks by media time across a skip (dasio asked: its DAW timeline lost the blocks of a 49.7 ms VM stall). Free pacing gives `None`. `Block::new` is unchanged.

## 0.4.37 (2026-11-02)

- Fix: an embedded app's pump plays at the device latency Dante Controller set. In v0.4.36 it kept the config's latency, both after a restart (its `Timing` was taken before the state file was read) and live, so its playout sat off the RX's and it read the RX rings at the wrong point. The pump now takes its timing from the device as it runs (`host::Prepared::config`) and follows a live change: the manager publishes the device latency with the RX delays (`RxShared::playout`), and the pump re-bootstraps once at the new playout, with a discontinuity, as at a new epoch.
- `stream::PumpView` shows the pump's current D and P after such a change.
- Tests: the two device tests in `tests/embed.rs` take one lock, because they bind the same ports.

## 0.4.36 (2026-11-02)

- New: our device takes a latency write from Dante Controller or netaudio (ARC 0x1101, `arc::control::set_latency`'s form), as hardware does (dante-asio #2, owner requirement). A latency the config's own rules allow (`DeviceConfig::with_latency`) is answered OK, saved in the state file and taken live. The manager plans every RX flow with it, our TX channels advertise it (unless the config sets `tx_latency_ns`) and the TX thread's lag bound and late count follow it. The mDNS records are announced again with it, and the settings read shows it configured and in effect. A latency the rules refuse is refused; a 0x1101 in another form stays unanswered, as before.
- The Controller's latency survives a restart while the config keeps the latency it was set against; a config that says another latency since wins and the state file drops the Controller's value (`DeviceState::latency_for`), as a Controller rename does.
- Not yet live: an embedded app's pump keeps the `stream::Timing` it started with until it restarts (fixed in 0.4.37).
- Port: `arc::Change` gains `latency`, `DeviceState` gains `latency_ns` and `config_latency_ns` (struct literals need them), `arc::Flows` gains `set_latency`. New: `subscribe::Manager::set_latency`/`latency_ns`, `TxShared::set_device_latency`/`advertised_latency_ns`, `arc::control::parse_latency_body`, `DeviceState::set_latency`.

## 0.4.35 (2026-11-02)

- New: our device answers a meter subscription (CMC 0x3010, Dante Controller's and `meter::Meter`'s). It replies with the request's own bytes and result OK. While renewed (`cmc::METER_LEASE`, 10 s after the last renewal) the destination gets a frame once a second (`cmc::METER_EVERY`) with each TX and RX channel's level, at most `cmc::METER_MAX` (16) destinations at once. A zero destination stops it. The levels are the heartbeat's own peaks (`info::Levels`, `meter::level_of_peak`: a heartbeat byte is -2 dBFS a step, so the meter byte is that byte + 1, the floor 253), so the heartbeat's 0x8002 peaks stay whole. dcontroller asked for it, for its own-device E2E.
- New: `meter::frame` builds a meter frame, `meter::destination` reads a subscription's destination.
- `info::Sources` gains `levels`; `cmc::serve_cmc` takes the levels; `daemon::media::Media` gains `levels`.

## 0.4.34 (2026-11-02)

- `dante-stack-dev` runs on Windows too: `clock`, `card` and the device. `ctl` refuses there, because Windows has no control socket. v0.4.33's Windows release binary was a stub that printed "runs on Linux only"; `daemon::cli` now builds on Linux and Windows.
- `CLAUDE.md`: all Dante PTP code is dsync's, one maintainer (owner, 2.11.2026). The stack writes no PTP of its own; its media clock comes from the dantesync service today, and from dsync's embeddable library (dsync#136) in the app's own process when that exists. No code change.

## 0.4.33 (2026-11-02)

- CI: every tag gets a GitHub release with prebuilt `dante-stack-dev` binaries for Linux (x86_64) and Windows (x86_64), each with a `.sha256` file (`.github/workflows/release.yml`). dsync's stability gate pins an exact tag and checks the sha256 (dsync#98). Its part 1 runs `dante-stack-dev clock --seconds N` against the local dantesync.

## 0.4.32 (2026-11-02)

- Fix: `meter::Meter` renews its subscription every 3 s (`meter::RENEW`) until `stop()`. A device drops a meter subscription that is not renewed (netaudio's daemon renews every 3 s); v0.4.31 sent the start once, so frames stopped (dcontroller asked).
- New: `meter::Meter::start_at(bind, device, name, mac)` binds the meter's socket where the caller says (a fixed port for a firewall). The devices send their frames from another source port than 8800, so a guard that admits only established flows dropped them on dcontroller's box. `start` is `start_at` on port 0.
- New: our device answers the 8700 sample-rate read (0x81) with the hardware's reply (0x80) in a Brooklyn card's layout: our rate in effect, none pending, our rate as the only one supported (`info::sample_rate_body`). A write gets the same answer, unchanged: we run the config's rate. dcontroller's own-device E2E reads it. `info::Identity` gains `sample_rate`. The reply is bounded like the other info replies (`limit::BOUNDED` gains it).

## 0.4.31 (2026-11-02)

- New (dstack #16 step 6): `meter`, any device's meters over CMC (port 8800).
  - `Meter::start(iface, device, name, mac)` subscribes as the controller `name`, keeps the latest frame and sends every frame on a channel. `Meter::stop()` sends the stop.
  - `start_request` / `stop_request` are byte for byte Dante Controller's captured subscription (netaudio's fixture). The one difference: a zero pad byte where Dante Controller leaves garbage.
  - `parse_frame` gives the device id, the TX levels and the RX levels.
  - `level` / `dbfs` read netaudio's scale: 0 clipping, 1-253 = -(raw-1)/2 dBFS, 254 muted, 255 marker.
- Windows: the TX thread runs above the RX thread and the engine's pump, as on Linux (TX 47 > RX 44 > pump 40). TX keeps THREAD_PRIORITY_TIME_CRITICAL; RX and the pump take THREAD_PRIORITY_HIGHEST (`media::tx::windows_thread_priority`). Before, all three were TIME_CRITICAL. dasio asked for it after the owner's gen3 list. `rx_cpu` already puts RX on a CPU of its own apart from TX's `io_cpu`.
- Fix: `SampleRate::pending` is `None` when the device's word equals the running rate. Both Ultimo devices (DT_20, planet 22) carry their running rate there while nothing is pending; the Brooklyn cards send 0 (dcontroller on the site).
- Docs: `docs/windows-box.md` now names the NIC queue tuning (Large Send Offload off, Transmit Buffers at the minimum) for other traffic on the Dante NIC, with the other apps left as they are.

## 0.4.30 (2026-11-02)

- New (dstack #16 step 5, the writes; dcontroller calls each one only after the owner confirms it). The requests are byte for byte netaudio 0.4.2's, built offline with its own encoder; nothing was written to any device to learn them.
  - `arc::control::set_latency(c, ns) -> Result<Settings, WriteError>` sends ARC 0x1101. It reads netaudio's 23 settings keys (`WATCHED_KEYS`) before and after, and returns `Ok` only when the configured latency reads `ns` and no other key changed. The write carries pairs whose meaning is not known (0x0211, 0x0310), so a changed key comes back as `WriteError::OthersChanged` with its values. `latency_request_body`, `changed_besides_latency`.
  - `info::client::sample_rate(iface, device, id, wait) -> SampleRate` reads the current rate, a pending one, and the supported rates.
  - `info::client::set_sample_rate(..., rate, wait)` writes once, then reads back until the rate shows in effect or pending, else `InfoError::NotConfirmed`.
  - `info::client::set_encoding(iface, device, bits)` sends the write. No reply to it is known, so confirm with `arc::client::read_directory(..).encoding()`.
- Our own device leaves a latency write unanswered, as before and as inferno (its latency is the config's): `set_latency` to it ends in `ReadError::Timeout`. A refusal is not sent, because a refused opcode once made Dante Controller ask again thousands of times a second.

## 0.4.29 (2026-11-01)

- Fix: every 8700 info request now carries a sequence of its own, resends included (it was always 1). With the 0.4.27 resend in place, dcontroller still found the Brooklyn cards (two Yamaha AIC128-D, a Tascam ML-32D) leaving the board info unanswered after the product info, and the Tascam its clock status. netaudio's trace shows one sequence on all its requests and the card's two replies as consecutive packets, the board info only after the third request: a card drops a request whose sequence it has just answered.
- New: `info::client::network_info(iface, device, id, wait) -> Vec<NetInterface>`: each interface's link speed (Mbit/s), flags, MAC, address, netmask, gateway and DNS server (8700 network info). The layout matched netaudio's decode on three devices, including a two-port card and a secondary without a link. dcontroller's per-device network status.
- Docs: `docs/windows-box.md`, what a Windows PC needs for low latency, from the FOH PC (dstack #1):
  - the device's spinning threads on their own cores, apart from the NIC's RSS CPUs and dantesync;
  - a NIC for Dante only;
  - DSCP needs a QoS policy on Windows.

## 0.4.28 (2026-11-01)

- New: `embed::Embedded`, a Dante device inside an app's own process (the owner's connection model: every app carries its own connector and appears as its own device; dstack #20, for the intercom and strih-obs).
  - `Embedded::start(cfg, place, map, processor)` takes a config built in code. It sets up the device with no control socket, serves it, and runs the app's `dante_audio_io::Process` every block on the RX channels in `map.rx`, writing into the TX channels in `map.tx`. Those TX channels are set to send the app's output (`Source::Engine`), so the app does not set sources itself.
  - `Embedded::stop(within)` ends the pump, then the device and its goodbyes. Start and stop can repeat in one process.
  - `Embedded::media()` gives the status; `ended()` says why serving ended by itself.
- New: `host::Signals::Host` registers no signal handler; the app owns its signals. `Signals::Stop` and `Signals::Report` still register SIGTERM/SIGINT through tokio, and that handler stays for the process's life, which an app that embeds a device must avoid (inside OBS, SIGTERM would stop ending OBS).
- New: `host::prepare_config(cfg, place, control, engine_tx, source)`, the setup from a config value with the control socket optional. `prepare_with` reads the file and calls it.
- New: `stream::Timing::of(&cfg)`, the pump's timing (block, the latency in nearest samples, the work margin) in one place. A host that built `Timing` itself should move to it, so its playout point matches the RX's since 0.4.26.
- Embedded apps run on the `low_latency` profile; `timing = "normal"` stays refused, as withdrawn on 31.10.

## 0.4.27 (2026-11-01)

- New: `info::client::clock_status(iface, device, id, wait)` reads any device's clock status over UDP 8700 (Dante's clock stats, netaudio's request), read-only (dstack #16, for dcontroller's clock column). `ClockStatus`:
  - the clock state (2 undisciplined, 3 disciplined) and the servo state (3 synchronized, 6 none);
  - the stratum and the frequency offset (ppb);
  - the device's, master's and grandmaster's clock ids;
  - each PTP port's state (`port_state_name`: 3 disabled, 6 leader, 9 follower);
  - the capabilities, the maximum drift and the word clock state;
  - `is_leader()` and `is_grandmaster()`.
  - Codes are kept raw; only codes seen in captures have names. The layout matched netaudio's decode on three devices: the leader card and two followers.
- Our own device answers no clock stats (it has no PTP of its own), as before.
- Fix: the 8700 client sends its request again every 250 ms (`info::client::RESEND`) until the reply comes or the wait ends. dcontroller found the Brooklyn cards (two Yamaha AIC128-D, a Tascam ML-32D) leaving a board-info request asked right after the product info unanswered within 1 s; netaudio sends it three times, and the reply came after the third.

## 0.4.26 (2026-11-01)

- Fix: a latency set in ns now reads as its nearest whole samples, not cut down. dasio's RX latency of 333 333 ns at 96 kHz read as 31 samples, so a 1 ms source needed a delay of 65 and two blocks of 64: a round trip of 351 samples where 32 gives 288 (dstack #1). New `clock::latency_samples`. It applies to our device's latency, a flow's latency and advertised latency (the playout point, the block delay, `below_advertised`), the TX lag bound, the 0x3200 stated latency, and the ctl status's `latency` and `advertised` samples. `clock::to_samples` (time to a sample index) still rounds down. A latency that was a whole number of samples reads as before.
- New: `info::client` reads any device's product and board info over UDP 8700, read-only (dstack #16 step 4, for dcontroller):
  - `product_info(iface, device, id, wait)`: manufacturer, model, product version, manufacturer software and firmware versions, manufacturer and product ids.
  - `board_info(iface, device, id, wait)`: board name, platform model, platform software and hardware versions with their builds, API and ROM boot versions.
  - The reply comes to the info group (224.0.0.231:8702), not to the asking socket. The client joins the group on the interface and takes the reply matching the device's address, its device id (several devices answer on one address) and the opcode. Errors are typed (`InfoError`: socket, timeout, malformed).
  - The offsets come from captures of four makes. Each value matched `netaudio device show`.
- New: `arc::client::Directory::encoding()`, the bits per sample all of a device's TX channels share (from `read_directory`).
- New: our device answers the keyed settings read (ARC 0x1100 with start code 0x2809, netaudio's `config latency` and `arc::control::settings`) with its own values, in the hardware's layout: the sample rate, pull-up 0, the latency as configured and in effect (`latency_ns`). It keeps no default latency and no latency range, so those keys are answered as absent. Before, the table was empty. Dante Controller's full-table read (start code 0x27ff) and 0x1102 stay zeros, as inferno answers them, until a Dante Controller check.
- Our product info also carries our version at 0x12c, where netaudio and Dante Controller read the product version (0 before).

## 0.4.25 (2026-11-01)

- RX loss check (iemmixer: a single packet now and then missing from a flow, with no late packets):
  - Each RX flow slot in the status gains `gaps` and `gap_frames`. A packet that began past the end of the one before is a gap, and its skipped frames are counted. Dante packets carry no sequence number; their stamps serve as one.
  - On Linux, `socket_drops` shows the kernel's drops at the slot's socket, from `/proc/net/udp`.
  - Once a second, a flow whose gaps or drops rose logs one warning with the slot, the gaps, the frames and the socket drops.
  - A gap with no socket drops means the packet never reached our socket: the sender, the switch or the NIC (dstack #1).

## 0.4.24 (2026-11-01)

- Fix (Windows): a device that is a member of the address's info manager no longer stops when the managing device stops. Its sends to the stopped manager drew ICMP port-unreachable, which Windows reported on the next receive as WSAECONNRESET (10054). The member's info server ended, its device exited, and the service restarted it with an audio gap. Found by dasio with two instances on one PC. The member's socket, the info port it takes over, and the manager's loopback share socket now turn that report off (`SIO_UDP_CONNRESET`), as the device's other sockets already did. The member keeps serving and takes the port when it frees (dstack #1).
- New: `arc::control::settings(c)` reads any device's settings, read-only (dstack #16 step 4, asked for by dcontroller). It returns the sample rate, the pull-up, and the latency (configured, active, default, minimum, maximum), with every key answered in `Settings::all`.
  - It asks ARC 0x1100 with start code 0x2809 and the wanted keys, as netaudio 0.4.2 does, and every hardware device on the site answers.
  - The key meanings were confirmed against netaudio's own readings on six devices: two Yamaha cards, a Tascam, a wireless receiver, an ESI interface and DVS.
  - `Call::call_as` sends another start code.
- Still to come: encoding, model and firmware.
- Our own device still answers 0x1100 with an empty table.
- `CLAUDE.md`: the connection model (one embedded device per app; dasio for ASIO hosts; iemmixer the lowest-latency tier).

## 0.4.23 (2026-11-01)

- New config key `rx_port_base`: the RX flow sockets take fixed ports, slot `n` on `rx_port_base + n` (32 ports), so a firewall can name them. camera-box needs this for its nft table on the editing PCs (dstack #20).
  - Left out, the OS chooses the ports, as before.
  - The config refuses a base under 1024, a range past 65535, and a range holding the ARC, CMC or flow-control port.
  - A port already taken at start is an error naming it (`RxShared::new_at`).

## 0.4.22 (2026-11-01)

- Discovery also browses `_netaudio-cmc` and gives each device its device id (`discover::Device::device_id`), read from the record's `id`. It is the id its heartbeat carries, so a controller can match `latency::listen`'s entries to devices when several share an address. On the site, ten devices' CMC ids were checked, ours and the hardware alike. `discover::parse_device_id` reads the 16 hex digits (dcontroller, dstack #16).

## 0.4.21 (2026-11-01)

- Fix: `latency::listen` now returns one `Latency` per device heard at the address, told apart by the heartbeat header's device id (`Latency::device_id`, `Beat::device_id`). Before, it filtered by source IP only, so two devices on one address on their own ports (dasio's two instances on iem-snv) mixed into one result. `latency::Heard` gathers several devices (dstack #47).
- `CLAUDE.md`: dsync, formerly dantesync, is its own project, and its only peer is dstack. Every request to dsync from another project comes through dstack (airuleset#1280).

## 0.4.20 (2026-11-01)

- New `latency` module: what Dante Controller's Latency tab shows, for any receiver, read-only (dstack #47).
  - Every receiver sends this data in its heartbeat once a second (224.0.0.233:8708): record 0x8003 is each RX flow slot's largest lateness that second, record 0x8004 its cumulative late packets.
  - `latency::listen(our, device, window)` gathers one device's records. Per slot it gives the per-second peaks (µs), the late packets in the window and the total.
  - `latency::histogram` bins the peaks. No device sends a histogram: on the site, 8 devices send records 0x8000 to 0x8005 and 0x8008, and none of them is one.
- `arc::control::rx_flows` reads a device's RX flows (ARC 0x3200): slot, rate, latency in ns, and the RX channels fed. This is what the lateness is measured against.
- Our device's heartbeat now also sends 0x8004, each RX flow slot's late packets since start, as the hardware receivers do. Dante Controller can then show our RX's late packets.

## 0.4.19 (2026-11-01)

- The late watch: once a second, each TX flow whose packets left later than the advertised latency since the last look logs one warning line. The line gives the slot, the count and the oldest packet's age (`age_peak_us`). A receiver at that latency counts each such packet late.
- So the log now says when our side was late. A receiver that counts late packets while the log stays quiet points past our send: the OS send path or the network.
- Before, only the status's cumulative `over_latency` held this; it had no time and was lost at a restart. SongPlayer's flow read 24 late at the FOH card at 12:08:53 on 1.11, and nothing on our side could say whose they were (dstack #1).

## 0.4.18 (2026-11-01)

- `SubStatus` names every subscription status Dante Controller shows: netaudio 0.4.2's table of 47, plus inferno's `1` (unresolved). Each status has a label as Dante Controller words it (`label`), Dante's name (`dante_name`) and a state for a controller (`SubState`: idle, pending, connected, error).
- The RX channel's status word has two halves. The low 16 bits are the subscription status (`SubStatus::from_code`), the high 16 bits a receiver detail (`RxChannel::receiver_detail`).
- On the site network, dcontroller saw `0x03010009` on 42 channels from hardware transmitters. It is status 9 ("Subscribed (unicast)", audio flowing) with receiver detail `0x0301`, so it now reads as `Receiving`.
- `0x4` reads as `SubscribedSelf` ("Subscribed (self)").
- The words our own server lists do not change (dstack #44).

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

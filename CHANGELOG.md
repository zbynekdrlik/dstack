# Changelog

## 0.6.115

- Fix (dstack #239, `tools/windows/lan-selfheal.ps1`): the LAN self-heal never disables, enables or restarts a virtual VLAN adapter. `Disable-NetAdapter` on a Realtek Virtual Adapter (driver 10.4.1117.2020) bugchecked dasio's test PC, and the repair's `Restart-NetAdapter` (a disable plus an enable) would have done the same on a layout-B PC, whose LAN adapter is that virtual adapter. A pure `Test-VirtualVlanAdapter` reads the LAN adapter's `InterfaceDescription` ("Realtek Virtual Adapter", or one naming a VLAN or a vendor's virtual adapter); for such an adapter the repair is the VLAN ID (`-NoRestart`), the IPv4 binding, DHCP, then `RenewDHCPLease` on that adapter (by interface index), and the log says the restart was skipped. A disabled virtual LAN adapter is a WARN and exit 1, never enabled. A VLAN ID the repair had to write to a virtual adapter (`-NoRestart`) may apply only when the adapter re-initializes: a WARN (reboot the PC if the LAN stays down) and exit 1. A LAN adapter whose description cannot be read fails closed: repaired like a virtual one, with a WARN. The Enable and Restart steps also refuse a virtual or undescribed adapter themselves. Layout A (the LAN on the physical base adapter) keeps its restart, as tested on the SongPlayer PC.
- Change (lan-selfheal): the state file is written to a `.tmp` file with write-through and swapped in (`File.Replace`), so a crash leaves the old or the new file; one that is unreadable all the same (NUL bytes) is still logged and counted from 0, now tested. The install's log line names the kind of LAN adapter.
- Installed PCs: re-run `-Mode install` with the new script; the config is unchanged (no new key). dasio reinstalls it on its test PC (layout B).
- No crate change: no API change, no latency change for any app.

## 0.6.114

- New crate `dstack-asio` (dstack #243, the owner's ROZHODNUTÉ of 13.11.2026): the ASIO host for our apps, lifted from SongPlayer's ASIO output (songplayer #233, #229) so SongPlayer and dasio's coming bridge instance run one host. A local ASIO driver is opened as a host with azo 0.4.0 (pure-Rust COM ASIO host, MIT, no Steinberg SDK; SongPlayer's pin), its rate, buffer and sample types read and never set, and every buffer switch runs the app's `dstack_audio_io::Process` (the pump's contract): the inputs read decoded to channel-major `f64`, the app's outputs encoded into the chosen driver outputs, every other output zeroed. The encode is SongPlayer's formula (an `f32` widened to `f64` writes SongPlayer's bytes, pinned against its constants written out). A panicking `Process` plays silence from then on, is counted once (`faults`) and closes the run.
  - Modules: `format` (`Sample`: Int16/24/32LSB, Float32LSB, Int32LSB16-24), `state` (SongPlayer's decisions in ns: backoff 2/10/30/60 s, close on reset/rate change/stall, the calm no-clock wait reopened every 60 s, `RateWatch` for a card more than 1 % off its rate, the asioMessage replies; `Reason` and `DeviceEvents` serialize with the `serde` feature), `hold` (one holder per driver per process, parking), `device` (`Device`, `Channels`, `Inputs::{Exact, FirstReadable}`, `Opened`, `Started`, `Unsupported` off Windows; a driver with no output is refused, as in SongPlayer), `engine` (`Engine`, `Counters`: the buffer switch, pure), `runner` (`Runner`, `Status`: open/start/poll/close for an app with no queue; an off-rate card is proven silently and reopened every 60 s, songplayer #229; ticks count from a run's first step), `win` (Windows: `WinDevice`, the static callback slots, the COM STA, `drivers()`), `bridge` (below). A process that loads an ASIO driver must spawn no child process (SoundGrid resyncs, songplayer #233).
  - `bridge`: the two sides of a Dante <-> ASIO bridge on `dlink::bridge`: `DanteSide` (the pump's `Process`: RX into the to-ASIO FIFO, the from-ASIO player into TX) and `AsioSide` (the driver's `Process`: the to-ASIO player into its outputs, its inputs into the from-ASIO FIFO), made by `Bridge::new(Spec)`, `Bridge::dante_side()` once and `Bridge::asio_side(&Opened)` per open (refused when the open's channel counts are not the spec's). Each direction's FIFO target is #247's `app_buffer_ns`: by default the ASIO buffer + one pump block (256 frames at 96 kHz + block 128: 4.0 ms), a set value verbatim.
  - Measured in the simulated-clock tests (256-frame buffers at 96 kHz, both directions): at the default 4 ms a card ±50 ppm off Dante holds with no dropout; at ±100 ppm the default drops out twice in its first 80 s (at about 27 s and 67 s) in the direction whose consumer is the faster clock, then holds (the correction slews 5 ppm/s, the rate fit locks after 60 s); a set 5 ms holds ±100 ppm from the start.
  - `asio-probe` (Windows release binary): lists the drivers, or runs one (`--driver <name> --seconds <s> --outputs 1,2 --inputs 1,2`) and prints its status once a second.
- New (dlink): `bridge::switch::{Switch, Taker, switch, NOT_TAKEN, GONE, RETIRED}`, the handover of a player to a running audio thread, moved from dsend (dstack #164) and made generic; dsend's `source::Switch` is now `dlink::bridge::Switch<Player>` (same behaviour, same errors; `pending` is a method).
- No change to dstack's stream, dlink's device or dstack-resample; no latency change for any existing app.
- Consumer migration (from each consumer's pin):
  - songplayer (pins v0.6.99): move dlink, dstack and dstack-resample to v0.6.115 (0.6.114 was released with it, never tagged alone) and add `dstack-asio` (same tag, `features = ["serde"]`). From the pin: v0.6.106 removed `dlink::BUDGET_NS` (`dante_device.rs`: use `dlink::REFERENCE_PATH_NS`; no latency change at the defaults); v0.6.101 added `RxCounts` fields (a struct literal adds `..Default::default()`); v0.6.103 and v0.6.112 do not touch it (no `dlink::bridge`). No latency change for its Dante output. The ASIO move (its own ticket): `asio_win.rs`'s device becomes `dstack_asio::win::WinDevice`; its ring copy becomes a small `Process` (pop the rtrb ring into its two `Opened::outputs`, count underruns once primed, the loop check's taps from the block's inputs with `Inputs::FirstReadable { max: 8 }`, which taps or plays unchecked as before); `asio_format`'s interleaved `f32` helpers (`encode(stereo, ch, ..)`, `fill_channel`, `read_channel`, `source_of`) are rewritten on `Sample::encode`/`decode` of one channel (same bytes); `asio_hold` and the generic part of `asio_state` come from `dstack_asio::{hold, state}`, its `Reason` wrapping `dstack_asio::Reason` beside its own `HostDown`; times move from 100 ns to ns (the late/fast bounds are the same decisions within one 100 ns tick of rounding). Its worker, servo adapter, splice, queue, host pipe and status stay. Its own tests stay the proof.
  - dasio (pins v0.6.110): from the pin, v0.6.111 to v0.6.113 change no API it uses (v0.6.113 drops the false "DSCP 0" warning when a QoS policy marks its service; no latency change). The bridge instance kind on `dstack_asio::bridge` (the pump runs `DanteSide` instead of the DAW `Bridge`; a `Runner` over `WinDevice` runs `AsioSide` on its own thread, its `MakeProcess` calling `Bridge::asio_side`), set in dtray's dasio settings window.
  - dsend: in this repo, moved (above).
  - camera-box, obs, intercom, presenter, iemmixer: nothing.

## 0.6.113

- Fix (dstack #256): on Windows the warning "TX audio leaves unmarked (DSCP 0)" (no QoS2 handle, or a process that is not an administrator) no longer fires when a Policy-based QoS rule marks the program's audio. The device reads the machine's policies from the registry (`HKLM\SOFTWARE\Policies\Microsoft\Windows\QoS\<name>`, where `New-NetQosPolicy` and the group policy editor write them and `Get-NetQosPolicy` lists them), never through PowerShell. A policy counts when its application is this executable (`AppName` or `Application Name`: the file name, or its full path, in any case), its DSCP is 46 (`DSCP` or `DSCP Value`), its protocol any or UDP, and no other condition narrows it (a port, an address or a network profile other than all; the group policy editor's `*` does not narrow). Then the device logs the info line `TX audio marked DSCP 46 by QoS policy '<name>'`. A policy for another program, another DSCP, TCP only, a narrowing condition, or all programs still warns, and so does a per-user policy (HKCU, not read). Applies to every dstack device on Windows (dsend, dlisten, dasio's service).
- New: `media::dscp::{Policy, PolicyValue, marking_policy, unmarked_note, POLICY_KEY}`. The Windows registry reads of `lso` and `media::dscp` share one internal module.
- No dlink API change, no latency change.

## 0.6.112

- Change (dstack #247, the owner's ROZHODNUTÉ of 13.11.2026): the bridge FIFO's target (`dlink::bridge`) is a fixed device term, `app_buffer_ns`, no longer a fixed 40 ms. Its default is a constant per app: the app's known fixed chunk plus one pump block at the device's rate. A value the app configures wins, is used as given (1 to 150 ms) and never changes by itself. The device logs it at its start beside dstack #244's terms: `dlink latency: app buffer (bridge FIFO target) 22.666666 ms = app chunk 21.333333 ms + pump block 1.333333 ms (default)`, or `(set; default ...)`.
- Removed (dstack #247): the automatic cushion (up to +40 ms after dropouts, in 10 ms steps, stepping down a minute later). A dropout primes back to the same target and is counted (`Stats::underruns`), never absorbed by growing the latency. The floor and the hard excess follow the target: the floor is 5 ms at most and a quarter of the target, an excess of twice the target is skipped back to it.
- Fix (dstack #247): a chunked producer (dsend's 10 ms loopback packets, OBS's 1024-frame ticks) primes half a chunk higher, so its sawtooth centres on the target. At a target of one chunk and a block, the old priming left a 48 kHz capture on a 96 kHz network dropping out every 20 ms.
- **Latency change, dsend:** its FIFO drops from 40 ms (up to 80 ms after dropouts) to WASAPI's 10 ms period + one pump block: 11.333 ms at the default 96 kHz network (12.667 ms at 48 kHz). dsend's TX path shrinks by about 28.7 ms. Restore a larger buffer with `--app-buffer-ms <ms>` (`--app-buffer-ms 40` gives the old target, without the cushion).
- **Latency change, dlisten:** the same, its output's 10 ms WASAPI period + one pump block: 12.667 ms at its default 48 kHz network (was 40 ms, up to 80 ms). `--app-buffer-ms <ms>` sets it.
- Limit found in the simulated-clock tests: the correction moves 5 ppm a second at most (inaudible), so a sound card 200 ppm off the network costs about 4 to 6 ms of depth in its first 40 s or so. At the default target ±100 ppm (and +150 ppm) holds with no dropout. At 200 ppm one sign drops out once or twice at the start, counted in the status: a dlisten output 200 ppm fast, a dsend capture 200 ppm slow; the other sign holds. `--app-buffer-ms 20` holds ±200 ppm.
- A configured app buffer under half the app's chunk plus one pump block (dsend and dlisten: under 6.333 ms at 96 kHz) drops out every chunk; the device warns at its start (`AppBuffer::short`). The default chunk is WASAPI's 10 ms shared-mode period, assumed, not measured (cpal exposes no period); an output or ALSA device with a longer period needs `--app-buffer-ms` of at least half that period plus a block.
- The status line reads `depth 11.9 ms of 11.3 ms, ...`: the live depth of its target, then the underruns (the dropouts). New: `Stats::target_ms`, `Player::target_s`, `status::Snapshot::target_ms`.
- **Breaking API (dlink):** `bridge::Player::new(fifo, net_rate, out_rate, buffer, stats)` takes the target as a `bridge::AppBuffer`. Removed: `bridge::player::TARGET_S`, `HARD_EXCESS_S`, `CUSHION_STEP_S`, `CUSHION_STEPS`, `CUSHION_MAX_S`, `CUSHION_DECAY_S`. New: `bridge::AppBuffer` (with `short`, `clamped_ns`, `chunk_s`), `bridge::chunk_ns`, `bridge::MIN_APP_BUFFER_NS`, `bridge::MAX_APP_BUFFER_NS`, `bridge::player::floor_s`, `bridge::player::hard_excess_s(target_s, chunk_s)` (twice the target, at least twice the chunk), `Config::app_buffer(chunk_ns, set_ns)`, `Config::bridge_buffer()`. `Config::device` refuses an app buffer outside 1 to 150 ms. dstack-endpoint: `cli::Latency::app_buffer_ns`, `cli::Shared::AppBufferNs`, `cli::WASAPI_PERIOD_NS`; `with_shared` always states the WASAPI chunk.
- Consumer migration:
  - obs (pins v0.6.106; `plugins/dante-input/ffi/src/tx.rs` uses `dlink::bridge`): in `Config::dlink` add `.app_buffer(dlink::bridge::chunk_ns(1024, obs_rate), <configured ns or None>)` (an optional config key such as `app_buffer_ms`); build the TX with `cfg.dlink(obs_rate).bridge_buffer()` and pass it as the fourth argument of `Player::new`; `tx::target_ms()` reads it (`buffer.target_ns() as f64 / 1e6`) instead of `player::TARGET_S`, and the module doc drops `player::CUSHION_MAX_S`. The program TX's FIFO becomes 22.667 ms on a 96 kHz device (21.333 ms + 1.333 ms; 24 ms on a 48 kHz device) instead of 40 to 80 ms, so its TX path is (c) + P + 22.667 ms. The test `the_fifo_target_is_dstack_s_40_ms` pins the new value.
  - songplayer: nothing; it does not use `dlink::bridge` (its Dante output is a `dlink::Process` on the pump).
  - camera-box, intercom, presenter, dasio, iemmixer: nothing; none uses `dlink::bridge`.

## 0.6.111

- Change (dstack #239 and #254, owner's decision 13.11.2026): the two-network standard (docs/two-networks.md) allows inbound TCP 22 (ssh) and 8092 (the remoteos MCP) on the Dante leg, from anywhere, on Windows and Linux; everything else non-Dante stays blocked there. On Windows the `dstack155` TCP-in rule blocks every TCP port but 22 and 8092, and the MCP runtime's program rule (`python.exe`) blocks UDP in only. Check 6.5 now expects 22 and 8092 to answer on the Dante address and every other port not to. PCs to change: the TCP-in rule's `LocalPort` and the `python.exe` inbound rule's protocol; Linux PCs: accept TCP 22 and 8092 in on the Dante leg (and their replies out).
- Removed (dstack #239): lan-selfheal's emergency remote path (0.6.109, `emergencySource`, `emergencyPort`, `danteBlockRule`, group `dstack emergency`). The LAN self-heal itself is unchanged. A config that still has those keys is accepted and the keys are logged as ignored; remove them. Every run (and `-Mode uninstall`) enables a `dstack155` rule the old path left disabled, then removes the two `dstack emergency` rules; if the rule does not come back on it keeps them, logs an ERROR and exits 1. Install the new script over 0.6.109 with `-Mode install`.
- No dstack or dlink API change, no latency change.

## 0.6.110

- Fix (dstack #246, field test on a dasio PC): the settings window's interface list no longer offers adapters that cannot carry Dante: Windows' loopback pseudo-interface (127.0.0.1) and an adapter with only a link-local (169.254/16) or unspecified address. An instance's configured interface stays in the list when the live list lacks it, marked "(no Dante address)" for an adapter left out and "(not found)" when the PC has no adapter of that name, so a rename never changes the setting silently. An adapter's text shows its first Dante-capable address.
- Fix (dstack #246): during an apply's restart (about 20 s) the window's status line says "Restarting…" instead of "Not running (not found on this PC)": from the elevated helper's start until the device answers again after it went away, at most `dasio::RESTART_WAIT` (60 s) from when it went away; then the real reason shows. It needs the device seen up during the apply and then missed: an instance that was not running before the apply shows its real status, and an apply that ends with the device never gone shows the status at once.
- New: `dtray::dasio::Restart::restarting`.
- New: `dtray::dasio::choices::carries_dante`, `dtray::dasio::Restart`, `dtray::dasio::shown_status`, `dtray::dasio::RESTART_WAIT`, `dtray::dasio::apply::RESTART_STEP`.
- No dstack or dlink API change, no latency change.

## 0.6.109

- New (dstack #239): an emergency remote path in the Windows LAN self-heal task (`tools/windows/lan-selfheal.ps1`, docs/two-networks.md section 3). With the optional config key `emergencySource` (one IPv4 host, the admin machine's Dante address), a LAN leg down for the threshold also opens TCP `emergencyPort` (default 22) on the Dante adapter for that host only: the task enables two Block rules of its own (group `dstack emergency`, created on first use: TCP in on the Dante adapter from every other address, and from `emergencySource` on every other port), and only then disables the `dstack155` rule named by `danteBlockRule`. When the gateway answers again it enables that rule first, then disables the emergency rules, and every run with the LAN up restores that state. `danteBlockRule` is required with `emergencySource`; a name that is not exactly one inbound Block rule is refused and logged. Without `emergencySource` nothing changes and no firewall rule is read.
- No Rust API change, no latency change.

## 0.6.108

- Fix (dstack #246): the settings window's banner area showed its orange background with no text (seen on Windows with v0.6.107). It is the window's background until a change needs a restart, as in the mockup.
- No API change, no latency change.

## 0.6.107

- New (dstack #246, owner-approved mockup v1, `docs/mockups/dtray-dasio-settings-v1.html`): dtray's dasio settings window on Windows. One window, a tab per dasio instance found on the PC (`%ProgramData%\dasio\instance-N\device.toml`, service `dasio-N`), a live status line on top (from the device's status: the app's line and the clock). Opened from the tray menu's new "dasio settings…" (shown when the PC had an instance at the tray's start) or alone with `dtray --settings`.
  - Fields: Ethernet interface (the PC's adapters; written as `iface = "<alias>"`, dasio#101), buffer (`block`), Dante RX latency (`latency_ns` through the device's ARC, at once, as `dtray --set-latency` writes it), TX latency (`tx_latency_ns`), sample rate (read only), inputs and outputs 2 to 64 (the `rx`/`tx` name lists: existing names kept, new channels named by their number), three CPU pickers (`pump_cpu`, `io_cpu`, `rx_cpu`; the other instance's CPUs greyed and refused).
  - ● marks the RX latency (applies at once), ↻ the fields that need a restart of the service. The TX latency is ↻: dstack has no live path for a fixed `tx_latency_ns` (receivers read it when they subscribe, dasio reports it to the DAW at its start); asked on #246.
  - A banner counts the restart changes. When the buffer or a channel count changes (the shared memory's layout), it says the DAW must release the driver, and the apply waits until the device's status says `daw_attached: false`. Only that reading lets it go on: a DAW still attached or a status that does not say keeps it waiting, at most 10 minutes, after which the file is not written (a live RX latency change of the same apply is already set, and the message says so). A restart of the same shape does not wait.
  - "Apply and restart" writes the config through an elevated helper (Windows asks once: UAC), `dtray-cli --dasio-apply <instance> <file>`. The helper rebuilds the edit from the instance's current file and the staged file's fields and refuses a staged file that changes anything else (so a swapped staged copy cannot move the state file or any other key). It then writes `device.toml` atomically (a staged file renamed over it), restarts `dasio-N` through the Service Control Manager, and leaves its verdict in the instance's own folder (`dtray-apply.result`) for the window. Only the changed keys are rewritten (`toml_edit`): the file's comments, order and other keys stay. The window starts the helper only when dtray runs from under `%ProgramFiles%` (dasio's installer puts it in `%ProgramFiles%\dasio`): a copy in a folder the user can write could be replaced and run elevated. The window then reads the device back (block from the status, channel counts and RX latency from its ARC, the adapter holding its address) and shows the verdict or each mismatch. The CPUs and the TX latency are written but not reported by the device.
  - A CPU picker is checked only when it changed: a pin the file already has does not block another change. While an apply runs, the tabs, the lists, the window's close and the tray's Exit are held, and a second apply is refused until the first ends (or, when the helper did not end within 180 s, until dtray restarts). The helper's verdict counts only with its exit code 0; the ProgramData and Program Files folders are the system's known folders, not the environment's, and the helper refuses an instance folder that is a link.
- New: `dtray::dasio` (the window's logic, pure and tested: `Fields`, `edit`, `resize`, `changes`, `Field::kind`, `banner`, `busy_cpus`, `cpu_problems`, `plan`, `changed_cpus`, `may_elevate`, `status_line`, `daw_attached`, `verify`, `install`, `apply_staged`, `result_path`; `dasio::choices`, `dasio::apply`), `dtray::Mode::Settings` and `Mode::DasioApply`, `dtray::cli::verdict`, `dtray::latency::label_wide` (now public).
- New dependency: `toml_edit` 0.25.17 (dtray only). The lockfile moves `toml_parser` to 1.1.5 and `toml_writer` to 1.1.3, which dstack's `toml` also uses; `toml_datetime` 1.1.2 is added.
- No change for other consumers: no dstack or dlink API change, no latency change.

## 0.6.106

- Breaking (dstack #244, owner ROZHODNUTÉ 12.11.2026): a dlink device's latency is three named terms, not one budget. Each term is logged at the device's start, one line each.
  - (a) the Dante network latency, `dlink::Config::network_latency_ns` (per device; 1 ms unless set, `dlink::NETWORK_LATENCY_NS`).
  - (b) the receive buffer, `dlink::Config::receive_buffer_ns`: this PC's room for the RX thread's wake and pass, sized from measurement.
  - (c) the app hand-off, computed and stated: RX = block + the converter's RX delay; TX = W + the converter's TX delay.
  - The device's Dante latency P (what ARC advertises and Dante Controller shows) = (a) + (b), rounded up to whole samples. Its floors stay: `dlink::tx_floor_ns` with TX, 4 ms RX only; a sum under the floor is refused.
  - W, the pump's work margin, is a term of its own: `dlink::Config::work_margin_ns`, 6 ms with TX and 15 ms RX only unless set (`WORK_MARGIN_NS`, `RX_WORK_MARGIN_NS`). Unset on a device with TX, it still gives way to P's TX floor where the old 15 ms left no room for both (a long converter TX delay), exactly as before. A W the app sets never gives way.
- Removed: `dlink::Config::budget_ns`, `dlink::BUDGET_NS`, `dlink::work_margin`, `dlink::rx_work_margin`, `dlink::WORK_SHARE_NUM` and `WORK_SHARE_DEN`.
- Changed: `dlink::split` takes `dlink::Terms` (the three optional terms) in place of the budget. Its refusal now starts `network_latency_ns + receive_buffer_ns:` or `work_margin_ns:` (was `budget_ns:`).
- New: `dlink::Split` names every term: `network_latency_ns`, `receive_buffer_ns`, `latency_ns` (P), `block_ns`, `rx_convert_ns`, `tx_convert_ns`, `rx_handoff_ns`, `tx_handoff_ns`, `margin`, `work_margin_ns`, `tx_path_ns`, `rx_path_ns`.
- New: `Split::lines`, `Config::terms`, `dlink::default_work_margin`, `dlink::REFERENCE_PATH_NS`, `dlink::ms`, `Directions::has_rx`/`has_tx`.
- No latency change for an app that sets no receive buffer: the defaults keep P, W and both paths exactly as in 0.6.105 for every config 0.6.105 accepted (the old 15 ms split; `REFERENCE_PATH_NS` sizes only this default), with (a) 1 ms. A unit test runs 0.6.105's split verbatim against the new one over 4 device rates, 12 app rates (8 to 192 kHz), every block and all three directions. OBS on the editing PC (RX only, 96 kHz network, 48 kHz app, block 128) stays P 13.041666 ms, now stated as (a) 1 ms + (b) 12.041666 ms, (c) 1.333333 + 0.625 ms, RX path 14.999999 ms. A device with both directions at 48/48 kHz stays P 9 ms = 1 + 8 ms.
- Latency change, for configs 0.6.105 refused only: a config the old 15 ms could not hold now starts at P's floor (4.583333 ms with TX at 96 kHz, 4.916666 ms at 48 kHz; 4 ms RX only) instead of being refused. These are the 8, 11.025 and 16 kHz apps (up to about 19 ms of filters), and 22.05 or 32 kHz apps on a 44.1 or 48 kHz device.
- Setting only (a) moves P by its difference from 1 ms, and never under P's floor (the default (b) grows there).
- New status fields (dstack #244, Linux): `network_latency_min_us`, `network_latency_p99_us` and `network_latency_max_us` per `rx` flow slot, next to `wake_delay_us`.
  - What they measure: each packet's arrival (the kernel's receive time on our PTP clock) less its sender's media-clock stamp (its first frame), over the heartbeat's last second. That is the transit plus the sender's own part (the packet's frames and how late it sent them): an upper bound on the transit.
  - How: p99 in 20 µs steps, min and max exact (each read in µs, at most 2.1 s), from a fixed histogram per slot with no allocation on the RX thread. The three are stored as one word, so a read never mixes two seconds.
  - They are absent for a second without a stamped packet, and on Windows (no kernel receive stamp). `dstack::media::rx::RxShared::last_network_latency`, `NetLatency`.
- dsend and dlisten: `--budget-ms` is gone; `--network-latency-ms <ms>` and `--receive-buffer-ms <ms>` take ms with up to 6 decimals (0.25). `dstack_endpoint::cli::with_shared` takes a `Latency` (the two terms) in place of the budget.
- What each consumer changes:
  - camera-box (OBS dante-input on the editing PC): its dante-input TOML sets no latency, so nothing changes and P stays 13.041666 ms. To size (b) from measurement, set the receive buffer once obs's dante-input takes the new keys.
  - obs (`plugins/dante-input/ffi/src/config.rs`): replace `latency_ms` (fed to `Config::budget_ns`) with `network_latency_ms` → `Config::network_latency_ns` and `receive_buffer_ms` → `Config::receive_buffer_ns`, both optional. Drop `DEFAULT_LATENCY_MS = dlink::BUDGET_NS / 1_000_000` and the 4..=100 check of the budget. `InputConfig::split` maps the refusal prefix `network_latency_ns + receive_buffer_ns:` instead of `budget_ns:`.
  - intercom (`hub/src/dlink_leg.rs`): replace `[dlink] latency_ms` (fed to `Config::budget_ns`, default `dlink::BUDGET_NS`) with optional `network_latency_ms` and `receive_buffer_ms` → `Config::network_latency_ns` and `Config::receive_buffer_ns`. `/api/state`'s `latency_ms` (the budget) becomes the split's terms, and the tests that compare with `dlink::BUDGET_NS` read `Config::split` or `dlink::REFERENCE_PATH_NS`.
  - songplayer (`crates/sp-server/src/playback/dante_device.rs`): `.split().map_or(dlink::BUDGET_NS, |s| s.tx_path_ns)` loses its fallback constant: use `dlink::REFERENCE_PATH_NS` (or handle the error). No latency change: the TX path stays as before at the defaults.
  - presenter (`crates/presenter-server/src/listen/dante/tests.rs`): `split.rx_path_ns <= dlink::BUDGET_NS` becomes `<= dlink::REFERENCE_PATH_NS`.
  - dsend and dlisten users: `--budget-ms N` becomes `--network-latency-ms` and `--receive-buffer-ms`.

## 0.6.105

- Fix (dstack #239): `tools/windows/lan-selfheal.ps1` assumed the LAN is untagged on the physical adapter and its repair set the LAN adapter's VLAN ID to 0. On a PC with the reverse layout (Dante untagged on the physical adapter, the LAN on a Realtek Virtual Adapter on a VLAN, as on dasio's test PC) that repair would cut the LAN. The config's new required key `lanVlanId` states the LAN adapter's VLAN: `0` for a LAN untagged on the parent adapter, `N` for a LAN on a VLAN adapter. The repair sets exactly that ID, only through the LAN driver's own "VLAN ID" property; a LAN adapter without the property is not changed, and the run logs a "VLAN mismatch" WARN with the VLAN it reads. The Dante adapter is never touched, as before. The repair step is renamed `VlanIdEmpty` to `VlanId` in the log.
- Breaking for an installed config: a config without `lanVlanId` is refused (`ERROR config: 'lanVlanId' is missing ...: nothing done`, exit 1). Add `"lanVlanId": 0` to the config of a PC whose LAN is the untagged physical adapter before installing 0.6.105's script.
- Docs: docs/two-networks.md, the self-heal task: the two layouts and their configs.
- No crate change: no API change, no latency change.

## 0.6.104

- New tool (dstack #239): `tools/windows/lan-selfheal.ps1`, the LAN self-heal for a one-NIC Windows PC with a Dante VLAN. A scheduled task ("dstack LAN self-heal", SYSTEM, every minute and at startup, no window) checks the LAN leg from the PC's own config (`C:\ProgramData\dstack\lan-selfheal.json`): a usable IPv4 address on the LAN adapter, the gateway's route through it, the gateway's answer. After 3 consecutive failed minutes it puts the LAN adapter back (enabled, VLAN ID 0, IPv4 bound, DHCP on, restart); the restart, which also drops Dante for a few seconds, runs when a step changed something or the adapter has no address, else at most once per 30 min. It never touches the Dante adapter (exact-name resolution, fail closed, no wildcard names) and never acts without its config; it logs every run to `lan-selfheal.log` (rotated at 1 MB). `-Mode install` (admin-only directory ACL, read back; task read back) / `-Mode uninstall`. Tested under Windows PowerShell 5.1 in CI's new `windows-tools` job, with fake adapters for the repair path.
- Docs: docs/two-networks.md, Windows: the vendor VLAN tool never stays running; the self-heal task and its install; a Realtek VLAN adapter's MAC changes on a USB replug; the 12.11.2026 incident. The safe VLAN procedure closes the utility after setup and ends with the self-heal install.
- No crate change: no API change, no latency change.

## 0.6.103

- New API (dstack #241, additive): `dlink::bridge`, the bridge between an app's own clock and the device's, moved from dstack-endpoint: `dlink::bridge::{Fifo, Player, Stats, fifo_for}` and the modules `dlink::bridge::{fifo, player, map}` (the player's constants such as `player::TARGET_S`, the channel map). An app on its own clock (OBS's mix) writes the `Fifo` on its audio thread and calls `Player::fill_with` in its `Process`, without dstack-endpoint's cpal (ALSA/WASAPI); `fifo_for` and `Player::new`'s first rate are then the app's (the FIFO's producer), and the FIFO's depth (about 40 ms, up to 80 ms after dropouts) adds to the device's path, as in dsend. dlink gains no dependency (dstack-resample was one already); its tree still holds tokio and tracing-subscriber through dstack itself, as before.
- Not changed: dstack-endpoint keeps every path as a re-export (`dstack_endpoint::fifo`, `::player`, `::map`), so dsend, dlisten and their re-exports change nothing; the code and its 31 tests moved unchanged.
- Docs: dlink's crate docs gain "An app on its own clock"; a unit test pins the split of a device with TX and RX at a 20 ms budget, a 96 kHz network, a 48 kHz app and block 128: W 768 samples (8 ms), P 11.364583 ms, RX path 13.322916 ms, TX path 19.999999 ms.
- No latency change.

## 0.6.102

- Fix (dstack #237): netaudio reads our ARC 2.8.9 channel lists (0x2400 TX, 0x3400 RX) in full. netaudio asks the first page with media selector 1 and every following page with the media type of the last record it read (3, audio); we answered selector 3 with an empty page, so netaudio 0.4.2 refused the whole list ("tx/rx channel page rejected ... channel page made no progress") on every device whose list takes more than one page (16 channels already). Now a page request with selector 3 is answered as with selector 1, for 0x2400, 0x3400 and 0x3600, as real 2.8.9 devices answer it; any other selector still gets an empty page. Dante Controller asks with selector 1: its replies are byte-identical. Catalog fixture: `crates/dstack/tests/fixtures/arc_netaudio_289_pages.json` (netaudio's own requests, encoded offline).
- Not changed (dstack #237): `netaudio device list`'s "Could not reach ... execute channel_count" timeouts. Our devices answer netaudio's channel count (0x1000); the timed-out set changes between runs and includes a real Yamaha card, and each such device answered a direct 0x1000 at once.
- No API change; no latency change.

## 0.6.101

- Feature (dstack #234): the RX thread tells a packet late on the wire from a late wake of its own. On Linux every RX flow socket (the slot sockets and each multicast group's socket) asks the kernel for each datagram's receive time (`SO_TIMESTAMPNS`), and the thread reads it with the datagram (`recvmsg`, one call per datagram as before). A packet's wake delay is the RX pass's time less the kernel's receive time; its lateness on the wire is its lateness at the pass less that delay. New per flow slot: `wire_late`, the packets already later than `late`'s limit (playout + block) when the kernel received them, and the largest wake delay in µs, of the heartbeat's last second and since the device started. A `late_at_read` or `late` with `wire_late` flat and a large wake delay is our RX thread's late wake on a loaded box; a rising `wire_late` is lateness that came over the network. The kernel stamps in the wall clock (`CLOCK_REALTIME`) only: a step of it between a receive and the pass reads as a false wake delay (forward) or as none (backward).
- New status JSON keys (each `rx` flow, after `late_at_read`, Linux only): `wire_late`, `wake_delay_us` (the heartbeat's last second), `wake_delay_peak_us` (since the start). Windows reads its flows with `recv_from` as before, with no receive stamp (and its passes read no wall clock): the keys are absent there (as `socket_drops`), and the counts read 0.
- New API (additive): `media::rx::RxCounts::{wire_late, wake_delay_peak_us}`, `RxShared::last_wake_delay_us`, `RxShared::stamps` (true when every flow socket stamps its receive times). A consumer that builds an `RxCounts` with a struct literal adds the two fields (or `..Default::default()`). `media::rx::join` and `GROUP_BOUND_TO_PORT` (moved to the private `media/rx/socket.rs`) and `RxPlan`, `RxRoute` (moved to `media/rx/plan.rs`) keep their paths, re-exported; `rx.rs` stays under 1000 lines.
- No latency change.

## 0.6.100

- Docs (dstack #217): docs/windows-box.md records the spin_escape trial's result. It stays an opt-in; a stress walker froze the test PC with and without it.
- Internal (dstack #231): `arc.rs` is split. The ARC settings server (0x1100, 0x1102) moved to `arc/settings.rs`, the counts and names replies (0x1000, 0x1003) to `arc/counts.rs`, and the page writer with the channel lists (0x2000, 0x2010, 0x3000) to `arc/channels.rs`. Every reply is byte-identical; no item moved was public, so no path changes. No API change, no latency change.
- No API change; no latency change.

## 0.6.99

- Dependency (dstack #226, dsync#166): dsync-ptp v1.32.0 -> v1.33.1. The in-process follower keeps its settled history across a continued grandmaster change: a same-line handover no longer reads settled=0 for ~15 s. dsync's new replay test of a 7 ppm frequency step at a GM change (the field handback from iemmixer) keeps the lock, with worst |e| 64 µs and settled again in ~31-35 s. No API change; no latency change.
- Fix (dstack #227): 0x1000 states the TX and RX flows the device has, its config's `flow_slots` (at most 32), under every start code (2.7's counts and 2.8.9's media record); it stated 32 whatever `flow_slots` was. Dante Controller creates a multicast flow in the highest flow 0x1000 states, which a device with fewer slots refused ("a slot past the device's flow slots"), so its Add failed there. Our 0x2600 table states the same slots (a controller's "flows N/slots"). Checked in Dante Controller on mbc (12.11.2026): on our test device with `flow_slots = 4`, Add made "Multicast Flow 4: TX 1, TX 2" and Delete ended it.
- Fix (dstack #227): our ARC 2.8.9 TX flow list (0x2600) is paged. Each page lists the flows from the one the request names (body bytes 10-11, as `list_tx_flows` asks; a request without it, or with 0, from flow 1) as far as one datagram (1472 bytes) holds them, status 0x8112 when flows are left, in #148's records and table as before. It was one reply of every flow, past a datagram from 22 to 27 flows, by their channels (32 flows of 8 channels: 2194 bytes, over the readers' 2048-byte receive buffer). Our `list_tx_flows` (since v0.6.98) follows the pages. A reader up to v0.6.97 asks once, from flow 1, and takes status OK only: it reads a transmitter whose flows all fit page one (21 flows of 8 channels, 24 of 4, 26 of 1), and above that gets a refusal (it keeps its last list; with none it finds no multicast flow and takes unicast). Before, it read up to 29 flows of 8 channels from the one oversize reply (IP fragments into its 2048-byte buffer), so 22 to 29 such flows move from read to refused for it. Receivers should move before transmitters that serve more flows than that.
- Fix (dstack #227): the reply to a 2.8.9 multicast create (0x2601) states the same flow slots in its table, `flow_slots`, as 0x1000 and 0x2600 do; it stated 32.
- New API: `arc::flows::list_reply_page`.
- Fix (dstack #230, songplayer#313, the intercom): `dlink::Link::start` (and `start_on`) fails at once again on an address or interface not there, as up to v0.6.97, with the same errors (`HostError::Run`, "no interface of this box holds the address" or "no such interface, or it has no IPv4 address"; an interface list that cannot be read now reads "the box's interfaces: <error>", as the wait's has since v0.6.98). v0.6.98 made it wait with no time limit and no way out, which hung the apps that call it on their own thread with their own retry and stop flag (their rebuild and shutdown, killed by systemd with no goodbyes). **dlink apps: skip v0.6.98**; move from v0.6.97 straight to this release.
- Feature (dstack #230): `dlink::Link::start_until(config, place, map, processor, stop: &AtomicBool)`, a start that waits for the place as v0.6.98's did (looked up every second, logged at its start, once a minute and when it is there) until the app sets `stop`, then returns `HostError::Run` "stopped while waiting for the device's address" (or "... interface") within one second. A place already there starts the device even with `stop` set. Call it on the app's own thread, never a UI thread.
- Fix (dstack #230): dsend and dlisten started with no `--ip` or `--iface` wait for the default route's address. At boot or logon before the network is up there is no default route, and the start failed at once ("Network is unreachable"); now it looks the route up every second with no time limit (while the network is unreachable or down, or no address is given yet) and logs "waiting for the default route's address to appear" at the start and once a minute.
- Not changed: dstack-dev, dsend, dlisten and `embed::Embedded` (dasio, iemmixer) keep #225's wait for their `--ip` address or `--iface` interface.
- New API (dstack): `daemon::PlaceWait { Now, Forever, Until(&AtomicBool) }`, `daemon::wait_for_route`, `host::default_route`, `embed::Embedded::start_waiting`. dlink: `Link::start_until`, `default_route_place`. `daemon::wait_for_interface`, `PLACE_RETRY` and `PLACE_LOG_EVERY` keep their paths (moved to the private `daemon/place.rs`, re-exported); their log lines' target is now `dstack::daemon::place` (a `RUST_LOG` filter on `dstack::daemon` still covers it).
- No latency change.

## 0.6.98

- Feature (dstack #222): our devices answer Dante Controller's ARC 2.7 multicast create and delete (0x2201, 0x2202), under any start code (Dante Controller sends them under 0x2809, netaudio's legacy encoder under 0x2729). The create makes the multicast flow in the slot its flow id names, of the TX channels it lists, through the same path as 2.8.9's 0x2601 (#193): sent to its group at our `tx_fpp_max` frames per packet and our encoding, saved to the state file's `[[multicast]]` before the OK (a restart makes it again, #207), listed in 0x2200 (2.7) and 0x2600 (2.8.9); the answer is OK with no body. Refused, making nothing: another type word than multicast's 0x0002 (netaudio's legacy encoder writes 0x0010 for a flow that is neither persistent nor advertised; ours are both), a rate or an encoding other than ours (0 asks ours), flow 0, a slot past ours or in use, a channel we do not have, a body that does not read. The delete ends every flow it names (its body: the count, 0, the flow ids; one named twice ends once), all or none, OK with no body. `arc::handle` alone (no TX table) still refuses both, as inferno does. Checked in Dante Controller on mbc (12.11.2026): Add on the Transmit tab makes "Multicast Flow 32: TX 1, TX 2" on our test device, Delete ends it, and a flow made survives the device's restart. Catalog fixture (name-free): `crates/dstack/tests/fixtures/arc_dc_multicast_27.json` (Dante Controller's own 0x2201 and 0x2202). New API: `arc::flows::{CREATE_27, DELETE_27, TYPE_MULTICAST_27, Create27, parse_create_27, parse_delete_27}`.
- Change (dstack #222): 0x1000's first word states multicast TX (0x0020) under both start codes, as inferno does (word 0 reads 0x0030 under 2.7, was 0x0010): Dante Controller offers "Add" on our devices' Transmit tab.
- Not changed (dstack #223): 0x1000's bit 0x1000 (segmented flows) stays clear and our 0x2600 keeps its records. With the bit Dante Controller reads 0x2600, and loads our device once only when it holds the 2.8.9 card's records (checked on mbc, 12.11.2026); those records would make every `list_tx_flows` reader up to v0.6.97 (dstack receivers, dcontroller) misread the bits and drop a new transmitter's multicast flows for unicast. Dante Controller works on the 2.7 lists with the bit clear. Our reader (`arc::flows::list_tx_flows`) now reads a real 2.8.9 device's records right: the bits from the record's format block (it read the format's offset, right only when the format sits at 0x18, a 1-2 flow list), and a list in several pages (STATUS_MORE, the next page from the flow after the last; a page's flows before its first left out); its first request is unchanged. New API: `arc::flows::list_body_from`.
- Fix (dstack #225, dasio): a device started before its address exists waits for it instead of failing. At FOH's logon dsend started in the same second the PC's DHCP client gave the Dante NIC its address, and failed with "no interface of this box holds the address". Now every start (dstack-dev, dlink's `Link::start`, `embed::Embedded`, so dsend, dlisten and dasio) looks its `--ip` address (or `--iface` interface with its one IPv4 address) up again every second, with no time limit, until it is there. It logs a WARN at the start and once a minute ("waiting for the device's address to appear on an interface", or "waiting for the device's interface to appear with an IPv4 address", with `minutes=N`) and an INFO when the place is there ("the device's address is there now: the device starts", `seconds=N`); the lines never name the address or the interface. What a consumer sees: `dlink::Link::start`, `host::prepare`/`prepare_with`/`prepare_config` and `Embedded::start*` no longer return an error for an address or interface that is not there yet; they return when it is, so call them on a thread that may wait. dstack-dev under systemd no longer exits 1 and restarts in that case. Still at once: an address the Dante network cannot reach (loopback, multicast, broadcast, unspecified), an interface with several IPv4 addresses or a loopback one, an interface list that cannot be read, and every config, state-file, port and engine error. dsend and dlisten wait the same way before they pick free control ports (no `--ports`), so the ports are probed on the address itself, not before it exists (where every slot failed and the device kept Dante's defaults beside another device of the PC). The wait cannot be cancelled: an app that must stay responsive calls `start` on a thread of its own. New API: `daemon::wait_for_interface`, `daemon::PLACE_RETRY` (1 s), `daemon::PLACE_LOG_EVERY` (60), `dlink::wait_for_place` (the same wait for an app that uses the address before `Link::start`), `dstack_endpoint::slots::pick_when_there`. Changed: `daemon::pick_interface` refuses an unreachable address (loopback, multicast, broadcast, unspecified) before it looks it up, so such an address no interface holds now gets "the address is not one the Dante network reaches" instead of "no interface of this box holds the address"; its other answers are unchanged.
- Fix (dstack #225): dsend shows no modal box when its run fails. A logon task starts dsend with no console and no stderr, and a failed start stayed on the desktop as a message box until someone clicked OK. A run's failure now goes to stderr and its log (`<state file>.log`) only; the box stays for a run whose log file could not be opened (else no trace would be left), and for `--list-outputs` started from Explorer. New API (dsend): `cli::shows_box(run, logged, attached, has_stderr)`.
- No latency change.

## 0.6.97

- Feature (dstack #220): our devices answer netaudio's ARC 2.8.9 writes. netaudio picks its write opcodes from our `arcp_vers` (2.8.9 since v0.6.96) and wrote with 2.8.9's own, which our devices left unanswered; now, under start code 0x2809: 0x3410 (a subscription page: each audio record subscribes or unsubscribes its RX channel, as 0x3010), 0x3401 (RX channel rename, as 0x3001) and 0x2438 (TX channel names, as 0x2013; answered with the records and the names the channels now carry, the layout netaudio's parser reads). Each makes the change through the same function as the 2.7 write of its kind, is saved before its OK and is all or nothing as before; a body of another record size, or cut short, is refused and changes nothing; a subscription record of another media than audio is passed over; a name offset of 0 is the channel's default name (Dante Controller's form for a TX channel in 0x2438, taken alike in 0x3401); a 0x2438 of more than 38 records is refused, so its answer fits one datagram. netaudio 0.4.2 over mDNS now subscribes, renames RX and TX channels and resets them on our devices (checked live on dev1's test device, 12.11.2026). Dante Controller keeps 2.7's writes (0x3010, 0x3001) for our devices while 0x1000's bit 0x1000 stays clear; with the bit, it sends the same 2.8.9 writes (checked on mbc). Catalog fixtures (name-free): `crates/dstack/tests/fixtures/arc_netaudio_289_writes.json` (netaudio 0.4.2's frames, encoded offline) and `arc_dc_289_writes.json` (Dante Controller's frames and our answers).
- Change (dstack #220): our RX channel records state netaudio's can-subscribe-self bit. The flags word of every RX record, 2.7's 0x3000 (its second word) and 2.8.9's 0x3400, reads 0x000e (was 6): netaudio refused a subscription of our RX channel to our own TX without the bit ("does not advertise self-connection support") before it sent one. Dante Controller still lists our devices and subscribes them as before (checked on mbc). A reader that compared the word with 6 sees 0x000e.
- Fix (dstack #220): netaudio 0.4.2's channel-name reset (`netaudio channel name <ch> ""`, 0x2013 and 0x3001 under 0x27ff) resets the name. netaudio 0.4.2 points the name's offset at the datagram's end and sends no byte after it (older netaudio sent a NUL); our devices read no name there and refused it. An offset at the datagram's end is now the empty name, the channel's default; one past it is still refused.
- Not changed (dstack #220, checked in Dante Controller on mbc, 12.11.2026): 0x1000's bits 0x0020 (multicast TX) and 0x1000 (segmented flows) stay clear. With 0x0020 alone, Dante Controller offers "Add" for a multicast flow but creates it with 2.7's 0x2201, which our devices refuse (they create with 2.8.9's 0x2601, #193): no flow, no error shown. With 0x1000, Dante Controller lists our device, reads the 2.8.9 lists and writes with the 2.8.9 opcodes above, but as soon as our device lists a TX flow in 0x2600 (any receiver's unicast flow, or a multicast flow) it cannot read that page and re-reads the device without end: our 0x2600 records (#148's layout) are not the 2.8.9 card's. Both wait for that work.
- No API change, no latency change.

## 0.6.96

- Change (dstack #216, dasio#2): our devices speak ARC 2.8.9 to Dante Controller. The ARC TXT announces `arcp_vers=2.8.9` and `router_vers=4.3.0` (was 2.7.41 and 4.0.2, inferno's); Dante Controller picks a device's protocol from them, and offers the device rename only to a 2.8.9 device (a real card's full exchange, #216 Finding 16). Under 2.8.9's start code 0x2809 our devices answer every read Dante Controller sends a 2.8.9 device at its start, in a 2.8.9 card's layouts with our own values: 0x1000 (the 2.7 counts with word 0's bit 0x0001 set, which makes Dante Controller offer the device rename, then a media record of TX/RX pairs: channels, flows, channels in a flow; bit 0x1000, segmented flows, stays clear: with it Dante Controller read the 2.8.9 lists below and left our device out of its list, cause not found yet), 0x1003 (name and factory name in 32-byte fields, a versions record: router 4.3, ARC 2.8.9, ARC min 0.2.4), 0x2010 (the TX channels whose name is not the default, as the card lists them), 0x2204 (TX flow labels: none, under start code 0x2801 as both 2.8.9 devices on the site answer), and the new lists 0x2400 (TX channels: number, current and default name, format), 0x3400 (RX channels: the same, and the subscription with its status and receiver detail) and 0x3600 (RX flows: number, format, latency, our address and port, which RX channel each slot feeds). These pages take the request's media selector (another media than audio gets an empty page), first and last item, and page as 2.7's do. 0x1100, 0x1102, 0x2600 (the TX flow list), 0x3300, 0x2320 and every write answer as before, under either start code. A 2.7 request (start code 0x2729 or 0x27ff: older controllers, netaudio when it has no TXT, e.g. by IP, our own `arc::client` reads) gets exactly the 2.7 answers as before; 0x2400, 0x3400 and 0x3600 stay unanswered under 2.7. Checked in Dante Controller on mbc (12.11.2026, #216 Findings 17 and 18) against a dev1 device built from this change: Dante Controller talks 0x2809 to it, lists it, reads the 2.7 lists (0x2000, 0x3000, 0x2200, 0x3200) under 0x2809 while 0x1000's bit 0x1000 stays clear, and its Rename field is editable: a rename from Device Config reaches our device as 0x1001 (netaudio's form, which our devices take), is saved and announced, and Dante Controller shows the new name; renamed back the same way. Routing and an RX channel rename from Dante Controller still arrive as 2.7's 0x3010 and 0x3001 and work; Device Config's latency control is enabled (its write was checked in v0.6.95). netaudio, when it reads our TXT (by mDNS), now takes our devices for 2.8.9 and writes with 2.8.9's opcodes (subscribe 0x3410, RX rename 0x3401, TX names 0x2438), which our devices do not answer yet: use netaudio by IP, or Dante Controller, dtray or `arc::control` for those writes until they are answered. New API: `arc::ARCP_VERSION`, `arc::ROUTER_VERSION`. Catalog fixtures (name-free): `crates/dstack/tests/fixtures/arc_289_card.json` (the card's full 2.8.9 exchange with Dante Controller, and a second 2.8.9 device's 0x1000 and 0x2204) and `arc_dc_rename.json` (Dante Controller's rename of our device and our answer). No latency change.

## 0.6.95

- Fix (dstack #216, dasio#2): our devices take Dante Controller's own latency write. Our devices answer ARC 0x1102, the settings key table Dante Controller reads to know which settings it may write, as a real card does: a count, then `key, access` for each key the settings read (0x1100) holds (sample rate 0x8020, pull-up 0x8021, the latency's default 0x8204, as configured 0x8205, in effect 0x8301, maximum 0x8302, minimum 0x8306), access 0x0003 (read and write) for 0x8205 and 0x8301 when a latency write can move the latency (`DeviceConfig::latency_range` holds another value), 0x0001 (read only) for the rest. Before, the reply was 94 zero bytes (inferno's), which marked every setting read only, and Dante Controller showed a latency change as made without ever sending the write (0x1101). With the table it sends the write (seen on mbc, 12.11.2026) in its own keyed form: `05, n`, then `n` pairs `key, offset` (the offset from the frame's start), the values after the pairs. Our devices now take that form as a latency write when its keys are only 0x8205 and 0x8301, its values are equal, and every offset lies at or past the pairs' end, on the frame's 4-byte grid, with its value inside the frame: saved to the state file, taken live, answered OK, as netaudio's write (which keeps working); a latency outside the range is refused, as netaudio's. Checked in Dante Controller on mbc (12.11.2026) against a dev1 device: a change from 10 to 5 ms is sent once, set, saved and shown without the "custom latency" warning. Still open on #216: Dante Controller's rename (its field stays greyed out) and its latency list, which after a change offers only up to 5 ms. Any other settings write stays unanswered, as before. `arc::control::parse_latency_body` reads both forms (it read netaudio's only). Catalog fixtures (name-free): `crates/dstack/tests/fixtures/arc_settings_card.json` (a real card's 0x1102 and 0x1100 replies) and `arc_dc_latency_write.json` (Dante Controller's write). No other API change; no latency change by itself.
- Feature (dstack #218, dasio): `dtray-cli`, dtray's command-line modes as a console program for scripts and automation. `dtray.exe` is a GUI-subsystem program, so PowerShell's `&` without capturing the output neither waited for it nor set `$LASTEXITCODE`, and in a shell whose output is a pipe (a remote shell, CI) its lines were lost (#213's console attach reaches an interactive console only); a refused `--set-latency` looked silent there. `dtray-cli` is a console program on every platform with the same modes (`--print`, `--version`, `--set-latency <device> <ms>`, `--switch <label> <source>`, `--autostart on|off`), run by the same code as dtray's, with the same lines and exit codes (0 done, 1 a failure, 2 a usage error). It never starts a tray: no arguments is a usage error (`usage: dtray-cli --print | ...` on stderr, exit 2). Its `--autostart` runs the tray program beside it (`dtray.exe`, `dtray` on Linux) with the same arguments and exits with its code, so the start at logon names the tray; without the tray program in the same folder it exits 1. On Linux it links no GTK. Released as `dtray-cli-<tag>-windows-x86_64.exe` and `dtray-cli-<tag>-linux-x86_64` with their `.sha256`; install both under their plain names in one folder (`dtray.exe`, `dtray-cli.exe`; `--autostart` looks for exactly `dtray.exe`) and call `& dtray-cli.exe --set-latency <device> 2` (docs/windows-box.md). `dtray.exe` is unchanged: the tray, and its command-line modes with #213's console attach for an interactive console. New API (dtray): `cli` (`run`, `usage_error`, `tray_program`, `autostart_args`, `CLI_USAGE`, `PRINT_WAIT`, `APP_WAIT`). Tested from the outside through pipes (`crates/dtray/tests/dtray_cli.rs`; on Windows CI also the console PE subsystem and PowerShell's `&`, uncaptured in a PowerShell whose output is a pipe as dasio calls it, and captured, each with `$LASTEXITCODE`). No latency change.
- Removed (dstack #215): the device CLI's `card read|write|rename` subcommand (`dstack-dev card`, v0.2.4), with its module `dstack::daemon::card` (`Card`, `Verb`, `Change`, `Refused`, `ReadOut`, `WriteOut`, `RenameOut`, `Mismatch`, `MAX_CHANNEL`, `USAGE`, `parse`, `parse_changes`, `read`, `write`, `rename`) and the variant `daemon::Command::Card` (an exhaustive `match` on `Command` drops its arm). It carried iemmixer's `dante_card.py` contract, and iemmixer deleted that script (iemmixer 14ef1093); no other caller exists. `dstack-dev card ...` is now a usage error (exit 2). To read or write another device's labels and name, use `dstack::arc::control` over an `arc::client::Udp` exchange: `device_name`, `labels`, `rename_channel` (`None` resets a label to its default), `rename_device`; they stay as they are. They do not carry card's guards: a caller that wants them checks the name (`device_name`) and a complete read (`labels`) before it writes, and stops on its own condition. No latency change.
- Internal (dstack #211): the status writer moved from `ctl.rs` to `ctl/status.rs`, its tests to `ctl/status/tests.rs`. `dstack::ctl::{status, app_object, APP_MAX}` keep their paths, and the status JSON is unchanged. No API change, no latency change.

## 0.6.94

- Fix (dstack #213, dasio#35): `dtray --set-latency <device> <ms>` takes any latency in ms inside the range the device states (ARC `latency_min_ns` to `latency_max_ns`, both included), not only Dante's 0.25/0.5/1/2/5/10 ms: digits with an optional point and decimals, up to six of them (whole ns: `0.333` writes 333000 ns, `0.333334` writes 333334 ns), more rounded half up at the seventh. A device configured at a value off the menu (a dasio test device at 333334 ns, which it also states as its minimum) can now be set back to it exactly from dtray. A value already configured writes nothing, a non-preset one too. A value past a `u32` of ns (`5000`) is outside the range, not junk. A value outside the range is refused with the range ("0.2 ms is outside the latency range the device states (0.25 ms to 296 ms); nothing written"), junk with what is taken; both exit 1. The tray menu still lists Dante's values only. On Windows, every command-line mode of dtray (`--set-latency`, `--switch`, `--print`, `--autostart`, `--version`, a usage error; not the tray) now attaches to the console of the shell that started it, so its lines show there without a `Start-Process` redirect (before, a refusal printed nowhere and looked silent); an interactive cmd or PowerShell prompt does not wait for a GUI-subsystem program (its lines may come after the prompt), so read the exit code with `Start-Process ... -NoNewWindow -Wait -PassThru` (docs/windows-box.md). Changed API (dtray): `latency::pick(choices, ms)` is now `pick(settings, ms)` (the device's ARC `Settings`; port: pass the settings read, not the menu's choices); `latency::ns_of_ms` reads any decimal ms and returns `Option<u64>` (was `Option<u32>`); `latency::offered` is gone. New: `dtray::wants_console`. The Windows console attach is built and tested on Windows CI through pipes (`crates/dtray/tests/cli.rs`); the interactive prompt was not run on a Windows box. No latency change of any device by itself.

## 0.6.93

- Dependency (dstack #1): dsync-ptp v1.30.1 -> v1.32.0. v1.31.0 (dsync#166): faster PI gains for a few minutes after a grandmaster change, so a follower no longer drifts out of lock ~2 min after a handover. v1.32.0 (dsync#170): two in-process followers on one PC take their first phase lock in the same window, so their times agree within 1 µs (was up to 2.5 µs); a seeded follower now locks at ~11 s instead of ~4 s, a cold one without a drift file at ~51-61 s. Until then the media clock runs on the dsync service when one is ready, as before. No API change; no dlink latency change.
- Fix (dstack #209, part of #203): the traffic watch counts a neighbour's audio only when its status names the device it was found as. The status JSON gains `name` (the device's name as it is now: the config's, or a controller's rename since; a JSON string, right after `tx_bytes`), so a local reader can tell which device answered; `dlink::Link::status` carries it too. A neighbour whose reply names another name (a stale mDNS record: the device renamed since, whose record under the new name counts, or a 127.0.0.1 port another of our devices holds now) or no name (a stack before this one) is left out of our devices, its audio counted as other traffic, and warned about once ("could not be read", with the reason) until it answers under its name; names compare with ASCII case aside. A dstack 0.6.92 neighbour therefore counts as other traffic until it moves to this release. The status is no longer numbers and fixed words only: it carries the device's own name (never an address), so a status pasted into a public issue needs the name redacted. dtray and `health::parse_status` read the status by key and ignore the field. New: `traffic::{status_name, answered_as, NO_NAME}`, `subscribe::Manager::host`. Changed: `traffic::reply_bytes(read)` is now `reply_bytes(name, read)` (port: pass the name the neighbour is listed under). No latency change.
- Fix (dstack #206): `discover::Discovery` reads the mDNS group as fast as it comes, so a device's goodbye takes it off the list at once on a busy network. Its thread read one datagram per socket a pass and waited 50 ms on the idle sweep socket each pass, about 20 datagrams a second; dev1's LAN carries about 30, so the group socket's queue filled (189 KB) and the kernel dropped datagrams: goodbyes (a device stayed listed until its record aged out), announcements and answers (a device listed late or not at all). Now each pass reads a socket until nothing more arrives within its read's 50 ms wait, within the 250 ms round. Seen in the whole-device test `discovery_lists_our_running_device_and_its_withdrawal`, which missed the goodbye 7 runs in 20 on a loaded dev1. A controller listing devices (dcontroller, dtray, the traffic watch's neighbours) gets the same fix. No API change, no latency change.
- Feature (dstack #210, dasio#31): dtray's source switches. A file on the PC (`switches.toml` beside the expected devices' folder, `%ProgramData%\dstack\switches.toml` on Windows, `/etc/dstack/switches.toml` on Linux; `DSTACK_SWITCHES` names another) names a receiver anywhere on the network, its RX channels and the sources they move between. dtray shows each switch as a submenu "<label>: <current source>" (read from the receiver every 10 s; "other" or "unread" otherwise), and a choice subscribes only the listed RX channels to that source, then reads them back and shows an error box when the read-back differs or a subscription was refused. `dtray --switch <label> <source>` makes the same choice from the command line (exit 0 when it reads back on that source or already was, 1 otherwise), and `dtray --print` also prints each switch's line (a refused switch file: on stderr, exit 1). Format and behaviour: `docs/windows-box.md`, "dtray's source switches". New API (dtray): `switches` (`Switch`, `Source`, `Current`, `Step`, `Item`, `MAX_NAME`, `parse`, `load`, `path_for`, `path`, `find`, `find_source`, `matches`, `current`, `state`, `line`, `plan`, `verdict`, `set`, `due`, `items`, `read_live`, `set_live`), `arc::call_from` (an ARC exchange from a given address of the PC), `discover::{Remote, remote}` and `Scanner::remote` (a device anywhere on the network by name), `Mode::Switch` (a new variant: an exhaustive `match` on `dtray::Mode` needs an arm for it). The usage line now lists `--switch <label> <source>`. No latency change.

## 0.6.92

- Fix (dstack #203, FOH PC 11.11.2026): the "other traffic on the Dante NIC" warning no longer counts our other dstack devices on the same PC (another dasio, dsend, a dlink app) as other traffic. Once a minute's look first finds the NIC's bytes past this device's audio over 100 kB/s, the device starts a discovery on its own address (`traffic::neighbours`), lists our devices there by their `dstack_ctl` port, reads each one's new status field `tx_bytes` from its local endpoint at every look (all within 200 ms, each in its share of the time left so a stuck one never keeps the others unread; off the media threads, and only once the NIC's own counter was read), and takes their audio off the rest. The discovery's first list (within the minute after its start, usually seconds) starts the watch's minute again with our devices read, so the warning after the first one names them; a device listed only later counts from its second look. The warning now carries `other_bytes_per_s` (what is left), `our_devices_bytes_per_s` and `our_devices` ("this device N, <name> N, ..." in bytes per second; names and a neighbour's own refusal text logged escaped, they come from mDNS and the neighbour). A neighbour's counter is its own reply: the sums saturate. The first warning of a loud NIC names no neighbour; a PC whose NIC stays under the threshold never starts the discovery, and it stops after an hour without a loud look. A neighbour seen at only one end of a window (it started, stopped or restarted) counts as other traffic for that window; one that cannot be read, or runs a stack without `tx_bytes`, is left out and warned about once. New: the status JSON field `tx_bytes` (every TX slot's wire bytes, as `TxShared::bytes_sent`), `dstack::ctl::probe` (dtray's local-endpoint client, moved; `dtray::probe` re-exports it), `discover::Device::status_port` (moved from `dtray::discover::status_port`, which is gone), `traffic::{Traffic, Peer, Readings, PEERS_WAIT, NO_TX_BYTES, on_address, deltas, our_devices, tx_bytes_of, reply_bytes, read_peers}`, `TrafficWatch::{look_with, due, take, restart}`, `traffic::neighbours::{Neighbours, Lazy, Step, Finder, DiscoveryFinder, QUIET_NS, rest_past_ours}` (`Neighbours::tick` is the watch's step at a control tick), `Driver::set_status_port`. Changed: `TrafficWatch::watch` was `watch(now_ns, ip, &tx)`; it is now `watch(now_ns, nic, ours, peers)`, the NIC's counter and the neighbour list as closures and this device's bytes, and returns the window's `Traffic` (port: `watch(now, || traffic::nic_sent_bytes(ip), tx.bytes_sent(), Vec::new)`). The warning's message reads "the Dante NIC carries other traffic besides our devices' audio: ...". No latency change.

## 0.6.91

- Fix (dstack #207, part of #193): a multicast TX flow a controller created on our device (ARC 2.8.9's 0x2601) now survives the device's restart, as on an UltimoX2. The state file keeps each one as `[[multicast]]` with `slot` (from 0, the media-local id - 1) and `channels`; a state without any writes no key, so older files load and are written unchanged. The create and delete save the state before they answer (`ArcOutcome::Changed` with the new `Change::TX_FLOWS_ONLY`, `tx_flows` set alone, which the server signals to no one: the mDNS records and the channel-change notification carry no TX flow); a change the state file cannot take is undone, on the TX table too, and refused (status error). At start `Media::new` makes each saved flow again before any thread sends, in its slot, to the same group (derived from the address, process id and slot, never stored), at the config's `tx_fpp_max` and encoding; a saved flow the config no longer allows (a TX channel or slot it lost) is left out with a warning at each start and stays in the file (a create in its slot replaces it; one past the config's `flow_slots` stays until the file is edited). A downgrade to a dstack without this reads a state file that holds `[[multicast]]` as invalid: delete those flows first (or the lines). New API: `device::SavedMulticast`, `DeviceState::{multicast, set_multicast, remove_multicast}` (a new public field: a `DeviceState { .. }` literal needs it), `TxTable::add_own_multicast` (the create's channel check, group, fpp and encoding in one place), `TxShared::recreate_multicast`, `TxTable` is `Clone`, `arc::Change::tx_flows` and `Change::TX_FLOWS_ONLY` (a new public field: a `Change { .. }` literal needs it; one with `..Change::default()` does not). No latency change.
- Feature (dstack #193, dcontroller#63): our own devices answer ARC 2.8.9's multicast create and delete (0x2601, 0x2602 under start code 0x2809), as an UltimoX2 does, so `arc::flows::create_multicast` / `delete_multicast` work against a dstack device too (dcontroller's E2E). A create (Dante Controller's form) makes a multicast flow of our TX channels (1 to 8, each once) in the asked media-local slot, at our `tx_fpp_max` frames a packet and our encoding, answered with its record and no destination; a slot in use or past our flow slots, a channel we do not have, or anything but one multicast record is refused (status error), creating nothing. The flow is a real entry in the TX table, sent by the transmitter to `media::tx::multicast_group` (239.255.<our address's last byte>.<process id mod 8 x 32 + the slot from 0, the media-local id - 1>, port 4321, TTL 32, out of the device's address); it never expires, only its delete (OK, empty body) ends it, and no receiver's flow handle reaches it. The 0x2600 list states it with flags multicast and its group, the 0x2200 page with type 0x02 and its group. New API: `arc::answer` (the server's entry, `respond` stays for everything else), `Flows::change_tx` (an own `Flows` implementation must add it), `media::tx::{multicast_group, MULTICAST_PORT, MULTICAST_TTL}`, `TxTable::{add_multicast, remove_multicast}`, `TxShared::ip`, the field `media::tx::TxFlow::multicast`. A dstack receiver (#148) of a channel such a flow carries moves to its group at its next read of our list, and back to a unicast flow once the flow is deleted and falls silent; Audinate receivers on it are not checked yet. Multicast flows are not kept across a restart. The ARC fuzz target now also runs `arc::answer`. No latency change.
- Feature (dstack #187, dcontroller#46): dstack-dev's control socket takes `tx-end <flow>` (the status's `tx` flow number, from 1), for a controller's E2E: it ends that live TX flow as if its receiver had gone quiet (`TxTable::end`, `TxShared::end_flow`), so the flow sends nothing more and its slot is free; its receiver stops receiving, then asks for the flow again and receives by itself, on a new flow that often has another number (our own RX subscribed to our own TX: stopped within 5 s, receiving again within 30 s). `dstack-dev ctl --socket <path> tx-end 1` answers `ok`, `error: tx-end: no live TX flow there` for an empty or expired slot. Only dstack's own binary answers it (`Build::dev_requests`, `Media::allow_dev_requests`); every other device, an app's `daemon::cli::main` binary (iem-dante-dev) included, answers `error: tx-end is dstack-dev's only`, dtray's local endpoint refuses it, and an embedded device (dlink, dasio) has no control socket. The control socket's answer to an unknown request now reads `error: unknown request: status, source, check, app or tx-end`, and `ctl` without a request `ctl: the request is missing (status, source, check, app or tx-end)`. API: `Build::grant_dev_requests`, `TxTable::end`, `TxShared::end_flow`, `Media::allow_dev_requests`/`dev_requests`, `Build::dev_requests`, `ctl::Request::TxEnd` (a new variant: an exhaustive `match` on `ctl::Request` needs an arm for it), `ctl::TX_END_DEV_ONLY`, `ctl::NO_LIVE_FLOW`. No latency change.

## 0.6.90

- Fix (dstack #196): a device states as its minimum latency (ARC 0x8306, `DeviceConfig::latency_range`) the lowest value its own write accepts. With `tx_latency_ns` left out, the TX channels advertise `latency_ns`, so the minimum is at least the TX floor `tx_fpp_max` frames + `tx_margin_ns` + 50 µs (0.633 ms for the test device at 96 kHz, fpp 32, 250 µs margin); before, 0.25 ms was stated and refused (dtray and Dante Controller offered it). New: `DeviceConfig::tx_floor_ns`, also used by the config check. A controller now offers 1 ms and up on such a device. No latency change of a running device.
- Docs (dstack #155): docs/two-networks.md §3 Linux: every Dante app on a guarded PC runs as its own service user.

## 0.6.89

- Fix (dstack #200, mbc PC 11.11.2026: Dante Controller there read no device): the slot choice (`dstack_endpoint::slots::pick`) takes a port as used also when another program holds it on the wildcard address, as Audinate's ConMon service (Dante Controller, DVS) holds 0.0.0.0:8700/8800. On Windows a socket on the device's own address took ConMon's unicast replies ahead of its wildcard socket; now each slot port is probed on the device's address and on 0.0.0.0, so a device beside ConMon takes slot 1 (4441/8801/4456, process id 1). With nothing else on the PC a device still takes slot 0, and our devices beside each other 0, 1, 2. Only apps that choose their slot with `pick` (dlisten, dsend, an app that calls it) get it; an app that passes fixed ports does not. New: `slots::binds_with` (the probe with an injected bind). No latency change.

## 0.6.88

- Fix (dstack #198, FOH PC 11.11.2026: ~0.5 s dropouts in browser music through dsend while a browser started and dsend ran at BelowNormal): dsend's loopback capture keeps up on a busy desktop whatever its process class. Its capture thread joins MMCSS "Pro Audio" on its first callback (logged once with the task index or the error; the pump, TX and RX threads already did); the loopback buffer asks WASAPI for 250 ms of the mix rate (12000 frames at 48 kHz), so a 200 ms stall loses nothing inside WASAPI; a capture discontinuity is logged with the time since the last good callback and the frames lost in ms. The FIFO and its priming level are unchanged. API: `dstack::media::tx::join_mmcss` is public and returns the MMCSS task index or the OS error; `dstack_endpoint::stream::is_discontinuity`. No dlink latency change.

## 0.6.87

- Feature (dstack #104): `dtray --set-latency <device> <ms>` runs the tray menu's own latency write without a desktop: the same discovery as `--print`, the device by name (case aside), one of the values it offers (`latency::choices`), `menu::write_needed`, `control::set_latency`, then a read-back; exit 0 only when the configured latency reads back as asked (stdout), errors on stderr with exit 1, usage exit 2. The tray and its ARC call now share `dtray::arc::call`; the tray and `--print` behave as before. No latency change for any device.

## 0.6.86

- Feature (dstack #184, owner 11.11.2026): dsend's own Windows render endpoint "dsend", a virtual audio driver in `drivers/dsend-audio/` (derived from Microsoft's simpleaudiosample, MS-PL for that directory only, like dasio's NDIS filter): one speaker endpoint, no capture, the buffer consumed at the real-time rate, 48 or 44.1 kHz 16-bit stereo. Test-signed (WDK test certificate per build) for a PC with test signing on; `scripts/install.ps1` refuses and changes nothing otherwise. Built in CI on windows-2025 (WDK 10.0.28000 from NuGet); release asset `dsend-audio-<tag>-windows-x86_64.zip`. Windows plays into it, dsend captures its loopback and sends it to Dante: the VB-Audio VAIO endpoint dsend used on the FOH PC gave silence.
- dsend picks an output whose name holds "dsend" when no `--output` or saved choice names one (else the Windows default); dlisten never does. API (dstack-endpoint): `outputs::choose` takes a `Fallback` (`Fallback::Default` or a name, `outputs::DSEND`). No latency change.

## 0.6.85

- Feature (dstack #148, dcontroller#46): `arc::flows::delete_multicast(call, local_id)` deletes another device's TX flow by its media-local slot (ARC 2.8.9, op 0x2602, `arc::flows::DELETE`, body `delete_body`), so a controller can create, list and delete a shared multicast flow on a transmitter with few slots. Verified live on a UltimoX2 (DT_20, 4.2.5.1, 11.11.2026, owner's OK): a test flow made with `create_multicast` was deleted (status OK, empty reply) and gone from the next list; the flow in use was untouched. No latency change.

## 0.6.84

- Fix (dstack #188, dsync#166, iemmixer 6.11.2026): the media clock stays on the in-process follower while it is in use, alive and anchored but unlocked; before, a ready dsync service on the PC took over at once. After a GM handover the follower's core keeps steering on PTP through its few unlocked minutes, while the swap to the service and back cost two re-aligns and took the service's drift (9 ppm there). At a start an unlocked follower still yields to a ready service; a follower that is dead or loses its anchor still hands over. No API change; no latency change for a dlink app.
- Feature (dstack #190, dcontroller#62): our devices' ARC record carries `dstack_vers=<version>` (`announce::VERSION_KEY`), and `discover::Device::states_flow_slots` says whether a device's 0x2600 list states its real slots: false only for one of ours from before this version (inferno's model id, no version key), whose table echoed the flow count (its limit is 32). A controller then shows "N TX flows" with no max for those.

## 0.6.83

- Diagnostics (dstack #184, FOH PC 11.11.2026: dsend sent -126 dBFS while its output played a -20 dBFS tone): dsend's and dlisten's 10 s log line ends with `input peak <x> dBFS` (or `input silent`), the largest magnitude their FIFO's producer stored since the last line: for dsend the WASAPI loopback capture, for dlisten the Dante RX. So the log tells whether the capture hands over silence or the level is lost after it. API (dstack-endpoint): `Fifo::take_peak`; `status::Snapshot` has `input_peak: Option<f32>` (`None` from `Snapshot::read`, set by `Snapshot::with_peak`). No latency or behaviour change.
- Feature (dstack #185, dcontroller#62): `arc::flows::Flows` has `slots: u16`, the transmitter's TX flow slots from the 0x2600 reply's `[slots, count]` (an UltimoX2 states 2), so a controller can show "flows N/max". Our devices' own list reply states their 32 slots (`arc::MAX_FLOWS`) where it stated the count before. API: code that builds a `Flows` sets `slots` (or uses `..Default::default()`).

## 0.6.82

- Dependency (dstack #1): dsync-ptp v1.29.0 -> v1.30.1 (dsync#160): the follower's late sample is clipped, not held, so two in-process followers on one PC stay within 1 µs of each other (v1.30.0 failed its release on this; its tag has no assets and is skipped). The service-side changes of v1.30.1 (#164: a stale date offset refused at load, READY=1 once the wall clock is placed; the service servo on the delay floor) are dsync's service, not the library. No API change; no latency change for a dlink app.

## 0.6.81

- Fix (dstack #180, dcontroller#65, 11.11.2026): `discover::Device` has `cmc: Option<SocketAddrV4>`, the address and port of the device's `_netaudio-cmc` record. A device beside another on one PC takes CMC 8800+k (since v0.6.77) and advertises it there, but discovery kept only the record's id, so a controller sent its meter subscription to the PC's 8800, the first device's port: the FOH PC's band and dsend gave no meter frames in dcontroller. A changed CMC port is a `Change::Changed`. API: a code that builds a `discover::Device` sets `cmc` (`None` when no record is read); a meter client starts at `device.cmc`, 8800 only without a record. No latency or behaviour change for a device.

## 0.6.80

- Docs (dstack #155, owner 11.11.2026): `docs/two-networks.md` applies only on a PC with a Dante leg (a PC that runs a Dante device). Its layout, NIC and driver values, firewall group, dsync pin and §6 checks do not apply to a PC without one, which passes; a project gates every requirement it takes over on the same condition. The NIC values in `docs/windows-box.md` and the single-NIC section ("Transmit Buffers" at the minimum, Large Send Offload off) apply only on the NIC that carries Dante and only where its driver offers the keyword: the setting belongs to the driver, not the chip. No code, API or latency change.

## 0.6.79

- Diagnostics (dstack #177, FOH PC 10./11.11.2026): the late watch's line ("TX packets older than the latency their receiver plays at") carries `relayed`, the late packets the relay thread sent while the TX thread blocked its bounded escape, and `since_escape_us`, the shortest time from the last escape's start to a late packet that second (absent before any escape). A pass ~1 ms late on every slot at once shows 2-3 times a day there with `spin_escape_ms` on; the two fields say whether it was the relay's cover or the TX thread's own time. API: `media::late::Late` has the fields `relayed` and `since_escape_ns`; `LateWatch::look` takes one closure returning the new `LateCounts` (was two); new `TxShared::over_latency_relayed` and `TxShared::take_since_escape`. No latency or behaviour change.

## 0.6.78 (unreleased)

- Dependency (dstack #1): dsync-ptp v1.28.1 -> v1.29.0 (dsync#160): the in-process PTP follower measures phase on the delay floor (the Theil-Sen line through the last 10 window minima), not the window median, so a one-sided receive delay (a USB NIC such as mbc's ASIX, late kernel stamps under CPU load) no longer moves it; D is anchored only once the floor holds 3 windows (about 2 more windows at a cold start). No API change; the media clock follows dsync as before.

## 0.6.77

- Fix (dstack #164, dasio on FOH, 10.11.2026): dsend and dlisten pick free control ports themselves. Started on an address where another Dante device holds Dante's defaults, they take the first free slot: ARC 4440+k, CMC 8800+k, flows 4455+k, process id k (`dstack_endpoint::slots`); dsend on FOH had stopped at "ARC on UDP 4440: Only one usage of each socket address" beside dlisten (slot 0) and band (slot 1). `--ports`/`--process-id` still win; a device named by interface keeps the defaults.
- dsend's default network rate is 96 kHz (was 48 kHz), the rate of every Dante device on the site's network; `--rate` sets another.

- Dependency (dstack #1): dsync-ptp v1.28.0 -> v1.28.1 (dsync#161): the in-process PTP follower no longer floods the log when its capture adapter disappears (a NIC driver swap or USB re-plug logged ~100 lines a second on iem-snv, 10.11.2026): one WARN, a count at most every 10 s, one INFO on recovery; reads back off 10 ms -> 1 s. No API or latency change.

- Docs (dstack #164): the test playbook records how to check a Windows binary from a release on an MCP box, why Windows-only glue is built `cfg(windows)` (the Linux coverage floor), and that a WASAPI loopback stream reports an xrun when it starts. No code change.

## 0.6.76 (unreleased)

- Fix (dsend, dstack #164): a loopback xrun no longer reopens the captured output. Seen live with 0.6.75 on a Windows PC: a fresh WASAPI loopback stream reports an xrun (the data-discontinuity flag) on its first packets; dsend took it for a dead stream and reopened the output every 5 s, each new stream reporting another. Only a fault (the device gone, the stream invalidated, a backend error) opens it again now; an xrun, a followed reroute or a refused real-time promotion is logged only (`dstack_endpoint::stream::needs_reopen`, used by dlisten too). No latency change.

## 0.6.75 (unreleased)

- New: `dsend` (dstack #164), the inverse of dlisten: a small app with no window that sends a Windows PC's sound into Dante. It captures an output (render endpoint) by WASAPI loopback (cpal 0.18.2, at the output's own mix format) and sends it as the TX channels L and R of a dlink device `<host>-dsend` (TX only; `--output <text>`, `--list-outputs`, `--name`, `--rate`, `--budget-ms`, `--ip`/`--iface`, `--ports`/`--process-id`, `--state`). The capture writes dlisten's lock-free FIFO; the device's pump pulls each block through the same ASRC player (drifting converter steered on the FIFO's depth, target 40 ms), so a capture ±200 ppm off the network holds the target with no underrun or overflow (tested). L/R come from the output's first two channels (a 5.1/7.1 output's front pair), a mono output goes to both. A silent keep-alive stream on the captured output keeps the loopback running while nothing plays, so an idle output is sent as silence. dtray shows its status (the output and its rate, whether the capture runs, depth, ppm, underruns) and its Output menu chooses the output to capture (`app output <n>`, kept in `<state file>.settings`, restored at start; `--output` wins). Linux has no output loopback in cpal's ALSA host: `dsend` exits 1 there with the reason. The release ships `dsend-<tag>-windows-x86_64.exe` with its sha256. No latency change for any existing app (dsend is a new dlink device at dlink's default 15 ms budget).
- New crate `dstack-endpoint` (dstack #164): dlisten's FIFO, ASRC player, channel map, output choice, settings and alert moved here (dlisten re-exports them under `dlisten::{fifo, player, map, outputs, settings, alert}`, unchanged), with the status parts both apps share (`status::{Snapshot, line, state, status_name, with_outputs, parse_command}`), the command line helpers (`cli::{number, parse_ports, default_name, state_path}`, each taking the app's suffix or name) and the process glue (`endpoints`, `host`, `win`). dlisten's behaviour does not change.

## 0.6.74 (unreleased)

- Docs (dstack #155, owner 10.11.2026): the two-network standard gets the safe procedure for a VLAN change on Windows, from iem-snv's cut-off (the Realtek PCIe driver unbound the base adapter; the watchdog tested the wrong path): non-production PC first, a second way in, a rollback from the scheduler that tests each leg by its own address, the VLAN made through the desktop, the base bindings re-checked. No code change.

## 0.6.73

- Docs (dstack #155): the standard's NDI rule corrected: the NDI 6.3 runtime announces on the Dante leg whatever `adapters.allowed` says; the guard is the leg's TCP block and receivers excluding the Dante subnet, not restarting NDI apps. No code change.

## 0.6.72

- New (dstack #155): on Windows the device tells a VLAN virtual adapter (Realtek's "Realtek Virtual Adapter", the Dante leg on a one-NIC PC) from a NIC in its driver check and says so at start, instead of calling the VLAN layer's driver (10.4.1117.2020) an unmeasured NIC driver (songplayer, resolume-snv, 10.11.2026). `lso::virtual_adapter`, `lso::Driver::Virtual`. No latency change.

## 0.6.71

- Docs (dstack #155): the Windows firewall rule of the two-network standard is final: profiles on with default Allow, the Dante-leg group `dstack155` (after a measured hour with no change, 10.11.2026). No code change.

## 0.6.70

- Fix (dstack #20, iemmixer, 10.11.2026): a round of flow requests to one transmitter counts one failure, however many of its flows fail. Each flow counted its own, so band's two flows to iem-mixer doubled the backoff twice per restart: after band restarted at 11:12Z the re-request waited 72 s and the band was silent in the in-ears for 73 s (with v0.6.65; v0.6.67/0.6.68's quick retry covers the no-answer part). No latency change.

## 0.6.69

- Docs (dstack #155, owner 10.11.2026): `docs/two-networks.md` is now the one standard for every PC with a Dante leg: one cable with the LAN untagged and Dante as a tagged VLAN, the legs' settings on Linux (NetworkManager) and Windows (Realtek VLAN driver; Intel has no VLANs on Windows 11), the Dante-leg firewall group, the rules our applications follow (bind and advertise the LAN for non-Dante traffic, dial verified LAN addresses, pin dsync), the order of a change and the checks the rig gates assert. Site values moved to the private site file. No code change.

## 0.6.68

- Fix (dstack #20, dasio and camera-box, 10.11.2026): a transmitter that answers no flow request is asked again every 6 s (`QUICK_RETRY_NS`) for as long as it stays silent, not three times and then the 18/36/72 s backoff. A starting transmitter answers nothing until its media clock is ready: mbc-snv's took 37 s after a reboot, and stream-obs stayed silent 2 min 6 s. `QUICK_TRIES` is removed. A refusal still backs off at once. No latency change.

## 0.6.67

- Fix (dstack #20, camera-box and dasio, 10.11.2026): a flow request the flow client gets no reply to (`Answer::Failed`) now counts as unanswered and is asked again 6 s after it (`QUICK_RETRY_NS`, up to `QUICK_TRIES`), like a request given up on. It reset the quick retries and took the full backoff: when mbc-snv's NIC restarted, stream-obs asked at 08:32:04Z, mbc was back at 08:32:10Z, and the next request went out only at 08:32:22Z (about 10 s of extra silence). No latency change.

## 0.6.66

- New (dstack #155): the device warns at its start when its Dante interface holds a default route (the PC's other traffic then rides the Dante network and waits in its NIC's queue with our audio): `daemon::gateway_warning`. On Windows the gateway is now read (`GetAdaptersAddresses` with its gateways); before it was always 0.0.0.0, also in our CMC network info. The owner's rule (9.11.2026): one NIC per PC, Dante and the LAN as two VLANs, the LAN holding the only default route (`docs/two-networks.md`). No latency change.

## 0.6.65

- Fix (dstack #148): our devices answer ARC 2.8.9's TX flow list (0x2600, start 0x2809) with their flows: each a unicast flow (flags 0x11) to its receiver, with its slot as both ids, its channels, fpp, bits and destination. Since v0.6.54 every dstack receiver reads its transmitters' list once a minute to find multicast flows. Our devices left it unanswered, so each read of one of them waited 1 s and counted as a failed read, and the device asked logged "0x2600 unanswered" about 49 times an hour (band, 9.11.2026). New: `arc::flows::list_reply_body`, `arc::flows::UNICAST`. No latency change.

## 0.6.64

- Fix (dstack #20, iemmixer and dasio, 9.11.2026): after band's restart, iem-mixer had 15 return channels off for 37 s. Two causes in our RX, and one in our TX:
  - RX: a transmitter's flows are now requested together, one request per flow the waiting channels need, not one per transmitter per 9 s. The second of band's two flows used to come 9-10 s after the first on every re-make.
  - RX: a request with no answer is asked again 6 s after it (2 s after it is given up), up to three times in a row (`QUICK_RETRY_NS`, `QUICK_TRIES`), before the 9-72 s backoff. A refusal still backs off at once.
  - TX: a new-flow request that comes before our TX clock is ready is held for up to 3 s and granted as soon as the clock is ready (`flows::server::Held`, `HOLD`, `MAX_HELD`), so a receiver whose request came a moment early needs no retry. The holding, answering and dropping of a request are logged.
  No latency change.

## 0.6.63

- Fix (dstack #20, 9.11.2026): a transmitter grants no flow before its media clock is ready. After mbc-snv's restart its clock was ready only after ~80 s (waiting for dsync). Our TX sends nothing before that, yet the flow server granted stream-obs's requests: two silent flows, captured on both sides. The receiver gave each up after 6 s, its retries backed off to 72 s, and the stream program was silent for 2-4 min after every mbc restart. Now a flow request (0x0100) before the TX clock is ready gets no answer, as from a device not up yet, and the receiver asks again. Stops and updates are answered as before. New: `TxShared::clock_ready`, `TxShared::set_clock_ready` (a host or test that steps the TX itself); `flows::server::answer` takes `ready`. No latency change.

## 0.6.62

- Fix (dstack #148, 9.11.2026, a v0.6.56 regression): a receiver joined no multicast flow of a transmitter that answers no query for its `_netaudio-arc` record. The DT_20 is one: it answers only for its channels. Since v0.6.56 our devices keep no browse, so the ARC directory never had that record and never read the DT_20's 0x2600 list. band and iem-mixer then asked the DT_20 for unicast flows beside its multicast flow, and band got "No more flows (Tx)" on HAND 1 and PETA. Now, past the record's 2 s grace (`RECORD_GRACE_NS`), the flow list of a device with no record is read at its channel's flow-control address on ARC's port 4440. Read on dev1: the DT_20 answers 0x2600 at 10.77.7.102:4440 with its 2 flows. No API change, no latency change.

## 0.6.61

- Docs (dstack #141, #155): `docs/two-networks.md` says that a host with an input filter on its Dante leg must accept UDP from source port 5353. Direct mDNS queries are answered unicast from the device's port 5353, which conntrack does not tie to the multicast query. dcontroller's guard dropped them, and its listing lost every device after 120 s.

- dtray: `dtray --version` prints `dtray <version>` (the dstack release it was built from), and `--print` starts with that line. The tray's menu shows it already (`dstack <version>`). Each PC's dtray can be checked in one command. New: `Mode::Version`, `version_line`.

## 0.6.60

- Fix (dstack #155): on Linux our mDNS daemon's legacy unicast replies (the answers to direct queries, which every device and the network listing now use) leave from the interface's address. mdns-sd sent them with `send_to` on its socket bound to 0.0.0.0:5353, so the kernel picked the source by the main routing table: strih-obs, whose Dante leg is routed only for its own address, answered from its LAN address through the router. The vendored mdns-sd now sends them with IP_PKTINFO's source (`mdns_sd::send_from_v4`, `vendor/mdns-sd/DSTACK.md`). Windows answers from our own responder bound to the Dante address already. No API change of ours, no latency change.
- Test: the failed-join test of the multicast RX owns its group port (main's CI failed it with EADDRINUSE when a parallel test took a port picked free).

## 0.6.59

- Fix (dstack #141): v0.6.58's network listing (`discover::Discovery`) removed and added devices that stayed online and reported changes that were none: dcontroller saw devices flap and went back to v0.6.31; dev1 counted 94 changes and one removal in 3 min. Three causes in `subscribe::instance`, each with a test: another host's query was read as an answer (its known answers; `read` now takes responses only); an answer without TXT emptied a record's TXT (a device id gone; the last TXT is kept); and a refresh whose answer lacked the target's A asked the instance again until the record aged out (it now asks the A). A record's addresses are those answered within its age (`Srv::ips`), so one datagram carrying only some of them changes nothing. A device the sweeps still name stays listed while its record's queries catch up (`Names::named_lately`). After the fix dev1 listed all 15 devices for 10 min with no change after the start. The plain-record line logs at debug.

- Docs (dstack #155): `docs/two-networks.md`, the rule for a host on both the Dante network and the LAN: the LAN holds the only default route and DNS, the Dante leg is reached only by sockets bound to its address and filtered to Dante, PTP and mDNS-for-Dante; Linux and Windows recipes, the site's facts, and the single-NIC hosts.

## 0.6.58

- Fix (dstack #141): `discover::Discovery` (dcontroller, dtray) keeps no standing mDNS browse. Its three browses (`_netaudio-arc`, `_netaudio-dbc`, `_netaudio-cmc`) made mdns-sd refresh every device's records from port 5353 at 80 % of their TTL, every 96 s, so every device answered by multicast to the whole network, in step with every other browsing host (dcontroller's capture on companion-snv: 20 refreshes 96 s apart). Now it sends one PTR sweep of the three services every 30-60 s (jittered, `SWEEP`, `SWEEP_JITTER`, `sweep_after`) from a port of its own, so the answers come back to it alone; it listens to the mDNS group as a passive member (announcements, goodbyes, others' answers: a device that answers no PTR query is still heard); and it reads each instance's SRV, TXT and A by direct legacy unicast queries (`subscribe::instance`). A device leaves the list at its goodbye, or when its `_netaudio-arc` record has gone unanswered for 120 s; a name heard of nowhere for 5 min is no longer asked (`FORGET_NS`). API: `Device` and `Change` unchanged, `Discovery::start(ip)` unchanged; `Discovery::start_with` (a `Browse`) is replaced by `Discovery::start_to(ip, mdns, listener)`; `DiscoverError` is now `Io` or `StopTimeout`; `Devices::apply` (a browse event) is replaced by `Devices::set(service, name, record)`; new: `Service`, `record_of`, `Heard`, `heard`, `sweep_query`, `sweep_after`, `Names`, `SERVICES`, `ROUND`, `FORGET_NS`. `subscribe::resolve::{Browse, BrowseError}` are removed (nothing browses). `instance::Srv` has the TXT strings (`txt`), `Instances::with_max`, `answer` and `forget` are new. No latency change.

## 0.6.57

- dlink on Windows (the normal timing profile, dstack #1, songplayer #285): the media threads (the pump, TX, RX) also join MMCSS "Pro Audio" at AVRT_PRIORITY_HIGH. Before, they ran at THREAD_PRIORITY_HIGHEST in the app's normal class (base priority 10), below an app's own MMCSS audio threads (the realtime band). Under SongPlayer's load on resolume-snv the pump stayed off the CPU for 22 to 41 ms: 6 to 13 blocks skipped (`pump_missed`), 1 to 2 times a day, each one heard as silence in the in-ears. MMCSS needs no privilege, keeps its share of the CPU for the rest of the system, and is joined per thread, for the thread's life. When MMCSS refuses (its service stopped), the thread keeps HIGHEST and logs a warning; the log line says which it got. The real-time profile (dasio, iemmixer) is unchanged. Linux is unchanged. No API change, no latency change.

## 0.6.56

- Fix (dstack #141, band 9.11.2026): a device keeps no browse of a whole mDNS service any more. A browse's PTR query made every Dante device answer with all its records at once, and mdns-sd refreshes each learned record at 80 % of its TTL on every host that heard the multicast answer, together: the channel records' TTL is 120 s, so every 96 s our devices (FOH, mbc, iem-mixer, dasio-test) asked for the same devices' records in the same second, and 77 to 306 datagrams were flooded to every port. zvukar's UltimoX2 read a late second 1 s after each such burst (14 in 10 min). Audinate devices never browse a whole service. Now a device asks only the names it needs, by a legacy unicast query from its own port (the answer comes back to it alone, so nothing synchronises): each subscribed channel's record (`subscribe::instance`, as before), and the `_netaudio-arc`/`_netaudio-dbc` records of the devices it looks up (`Instances::record`, `instance::Srv`: SRV and the target's A, no TXT needed), answered for 120 s and asked again from 60 s. A channel with no record of its own (a Yamaha card) goes to its device's ARC after its query's 8 s grace, as before. The `_netaudio-chan` browse is gone (`resolve::Mdns`, `Adverts::names_device` and the `Network::chan` field are removed); `ArcDirectory::start(ip, mdns, requests)` takes the query target instead of an mDNS daemon, `ArcDirectory::tried` is new, and a failed start is a warning (channels with records of their own still resolve), not the device's end. `info::server::Multicast` has a new field `mdns` (where the direct queries go, mDNS's group in `DANTE`). `daemon::serve` no longer needs `Browse` of its registry; `ServeError::Browse` is removed. `Book::set_record` is new. `discover` (the network listing) still browses. No latency change.
- Fix (dstack #141, band 9.11.2026): a TX flow made between two control ticks went out on the UDP socket until the next tick (up to 100 ms) read the neighbour table, even when its receiver's MAC was in it already. band's re-made 8-channel flow to iem-mixer sent its first 984 packets (82 ms) that way, and the 250 µs receiver read 32 late. The tables as last read (`/proc/net/arp`, the Windows neighbour rows) are kept, and `TxShared::publish` sets each slot's MAC from them, so a new flow to a known neighbour takes the direct (or host sender) path from its first packet. No API change, no latency change.

## 0.6.55

- Fix (dstack #141, FOH): our own mDNS responder (Windows, the answers from the device's address) answered a query for a service type with every instance in ONE datagram: band-asio-dante's 64 channels went out as 9 kB+, dasio-test's and mbc's 16 as 5 kB. Each became 4 to 7 IP fragments flooded to every port (224.0.0.251), and an UltimoX2 receiver (zvukar) read a latency spike of 1.5 to 2.9 ms at each burst while 1 Gbit receivers of the same flow read none. Audinate devices never send an mDNS datagram over ~1450 B. Now a reply is split into datagrams of at most 1452 B (`announce::responder::MAX_DATAGRAM`, mdns-sd's own limit), names compressed; each PTR travels with its instance's SRV and TXT, each datagram carries the host's A, so each reads on its own. 64 channels now go in at most 10 datagrams. New: `announce::responder::answers` (every datagram); `answer` returns the first. No latency change.

## 0.6.54

- New (dstack #148): our RX receives a transmitter's multicast flow. When a subscribed channel's transmitter (any device with a `_netaudio-arc` record) lists a multicast flow that carries the channel at our bits (`arc::flows::list_tx_flows`, read by the ARC directory's reader, kept 60 s), a flow slot joins its group on our address (`media::rx::join`: Linux binds the group, Windows the port, `SO_REUSEADDR` so two of our devices on one PC both receive it). No flow request, no stop, no keepalive is sent, and the channel shows Subscribed (multicast). While the transmitter's list is being read (the first read: one exchange, right after its `_netaudio-arc` record is seen; a device whose record has not come is waited for up to 2 s after the first lookup, `directory::RECORD_GRACE_NS`) the channel waits instead of asking for a unicast flow, so no unicast flow takes a transmitter's last slot first. Measured: a test device joined a UltimoX2's multicast flow 0.75 s after its start, 87 695 packets, 0 gaps, 0 late, least lateness 394 µs. A channel on a unicast flow moves to a multicast flow that appears: its unicast flow is stopped, so the transmitter's slot is free. A multicast flow that is gone, moved or silent is left, and the channel is resolved again (unicast when none carries it). A failed list read keeps the last list. On Windows, where a group's socket is bound to the port only (`media::rx::GROUP_BOUND_TO_PORT`), a second group of the same transmitter on the same port is received as unicast, so the two groups never mix in one slot. A failed join is tried again with the slot's next flow. `create_multicast` returns only the record of the slot and channels it asked for. Every device that subscribes now browses `_netaudio-arc`/`_netaudio-dbc` once and reads each subscribed transmitter's 0x2600 list once a minute (a device that answers none, as ours, costs one 1 s wait and backs off as a failed read). API: `RxPlan::group` (new field; `RxPlan::words`/`from_words` take three words), `RxCounts::joins`/`join_errors`, `FlowView::group`, `Resolver::multicast` (default: none, so an application's own resolver compiles unchanged), `resolve::{Multicast, MulticastLookup}`, `directory::{Book::multicast, Book::finished_flows, Kind, carried, RECORD_GRACE_NS}`, `Job::kind`, `media::rx::{join, GROUP_BOUND_TO_PORT}`. Status: an RX flow has `multicast`, `joins` and `join_errors`. No latency change: a multicast flow plays out as a unicast one (the larger of its transmitter's latency and ours, at least its fpp).
- New (dstack #148): `arc::flows`, another device's TX flows over ARC 2.8.9. `list_tx_flows` (0x2600) reads every flow with its id, media-local id, flags (`TxFlow::multicast`), channels, fpp, bits and destinations (a multicast flow's group). `create_multicast` (0x2601) makes a multicast flow of the channels named in a free media-local slot. It sends Dante Controller's own captured request (options 0x0001 or 0x0071, no name, fpp 0, device-allocated group) and reads the reply's flow. A reply with status OK and no flow is `CreateError::NotCreated`: that is what a UltimoX2 answered to netaudio's request with a name and options 0. Verified on a UltimoX2 (DT_20, 4.2.5.1): the flow was made and an Audinate receiver subscribed to it as multicast. No delete yet (no captured exchange). New: `arc::flows::{TxFlow, Flows, CreateError, list_tx_flows, create_multicast, list_body, create_body, parse_flows, START_2809, LIST, CREATE, MULTICAST, MAX_FLOW_CHANNELS, OPTIONS}`. No latency change.

## 0.6.53

- TX sends no packet before its due time around a bounded escape (`spin_escape_ms`, Windows, dstack #141). The send ahead (up to 2.14 ms before the packets' media time, once a period) is gone. While the TX thread blocks its escape tick, a relay thread (`dstack-tx-relay`, `media::tx::relay`) runs its passes. An UltimoX2 card (ESI planet 22) read band-asio-dante's flow at 3.5 ms because it counts an early packet as latency; the same device without the send ahead read 0.77 ms. API: the TX lane counter `LaneCounts::early` and the status's TX `early` are removed, as are `Transmitter::presend`/`presend_all`, `Schedule::due_early`/`due_by`, `Presend`, `PRESEND_ROUNDS`, `Source::ready` and `Ring::holds`. `clock::PRESEND_NS` is renamed `clock::ESCAPE_MAX_NS` (same value). New: `clock::block_escape`, `clock::relay_spin`, `media::tx::RELAY_TAKE_MAX`, `EscapeState::relay`.

## 0.6.52

- `dstack-audio-io`: `StreamStats::parked` is removed. No backend ever set it (always false), and no consumer reads it any more (iemmixer dropped its reader). A consumer that names the field in a `StreamStats` literal or reads it drops that line.

## 0.6.51

- Media on the direct TX paths (Windows host frame sender, Linux AF_PACKET) goes out with no UDP checksum (0, RFC 768), as every Audinate sender sends it (dstack #141). An UltimoX2 receiver (ESI planet 22) read band-asio-dante's flow ~3 ms late while it read an Audinate sender at 0.4 ms on the same card. Our packets leave on time, with the same media header; the UDP checksum is the one wire difference a capture on FOH shows. No API change; the UDP socket path is unchanged.

## 0.6.50

- dlisten (dstack #139): the status line (log and dtray) names the mix: `, mono` under `--mono`. On FOH (dasio) it could not be seen. New: `Stats::mono`, `Snapshot::mono`.

## 0.6.49

- New (dlisten, dstack #139): `--mono` sums every RX channel at 0 dB onto both speakers, clamped at full scale (`map::source_mono`, `map::Source::Sum`, `Player::with_mono`). The FOH monitor speaker's old VB-Matrix route summed its two sources (an alert and a stage feed) this way.

## 0.6.48

- Fix (iemmixer, SongPlayer, 6.11.2026; dstack #1): after dsync's coordinated date step the media clock re-anchored, every TX flow started again on its fpp grid after now (a receiver saw 2 packets, 64 frames, missing from SongPlayer's flow) and the engine's pump re-bootstrapped (`pump_missed`). The clock now reports each re-anchor's time step. On the new epoch the device takes one shift from it, the step rounded to whole engine blocks (`block`), and every TX flow and the pump move their schedules by that shift instead of starting again: a coordinated step goes on at the old stamps + the step, a step under half a block (a local correction of the clock) changes no stamp, and no packet or block is lost. The pump and TX keep the engine rings on one line of positions across a shift (position less the engine shift), so the samples in flight across the step are sent once, not silence, at an unchanged work margin. Still started again, counted as before: a re-anchor whose step is unknown (a source switch of the linked clock, two re-anchors between two reads), and a flow whose fpp does not divide the shift (fpp 64 on block 32 for a one-block shift), which keeps its stamps on its fpp grid. A step off the block grid also calls the processor's `discontinuity` (its inputs jump by the rest). A pass that reads the new time with the old epoch restarts once on the jump and is not shifted again (the media threads now read the epoch before the time). Each re-anchor logs one info line per thread ("TX flows across a clock re-anchor", "engine pump across a clock re-anchor") with the step, the shift and the outcome.
  - API (additive): `PtpClock::last_re_anchor` (default `None`: an application's own clock needs no change, its re-anchors keep starting the schedules again), `clock::ReAnchor`, `clock::Anchor` (`Anchor::of(clock)`, `step_from`, `From<u32>`), `clock::grid_shift`, `clock::linked::Follow::last_re_anchor` (default `None`), `Absorb::step`, `media::tx::shift` (`EngineLine`, `Shift`, `source_position`), `TxShared::engine_line`, `Schedule::re_anchor`, `stream::Blocks::re_anchor`, `stream::inputs_jump`. `Transmitter::step` and `stream::Pump::media` take `impl Into<Anchor>`: a `u32` epoch still compiles (no step known). No latency change: dlink's budget, split, block and converter are as before.

## 0.6.47

- Fix (dlisten, dstack #139): a failure shows its message box only when nothing reads stderr (a start from Explorer or the Run value). Started from a command line or a script, it prints the error and exits. Seen live: `dlisten --list-outputs` on a Windows PC with no output printed its error, then held the exit on a modal box. New: `dlisten::alert::shows_box`.

## 0.6.46

- New: `dlisten` (dstack #139), a small app with no window (Windows; it also runs on Linux) that plays Dante audio on a local output: PC or notebook speakers, a monitor's audio. A dlink device `<host>-dlisten`, RX only, 2 channels L/R by default (`--channels`, `--name`, `--subscribe <rx>=<ch>@<dev>` for our own device only, `--rate`, `--budget-ms`, `--ip`/`--iface`, `--ports`/`--process-id`). The output plays at its own default config (WASAPI shared on Windows, ALSA on Linux, through cpal 0.18.2), through `dstack-resample`'s drifting converter, its ratio steered by the ASRC controller on the FIFO's depth (target 40 ms; each underrun primes 10 ms more, up to 40 ms, kept as the controller's cushion). L/R go to the output's first two channels, a mono source to both, a mono output gets L and R at half each. dtray shows it: its status line (output, rate, audio flowing, depth, underruns) and an Output menu. The chosen output is kept in `<state file>.settings` and restored at start (`--output <text>` wins; a saved output that is gone falls back to the default, logged). `--list-outputs`, `--autostart on|off` (with the other options given). The status is logged every 10 s to stderr and `<state file>.log`. The release ships `dlisten-<tag>-<suffix>` for Linux and Windows with its sha256, like dtray.
  - Measured in its tests (the output a simulated clock): an output 200 ppm fast or slow over 4 minutes ends at 40.9 / 41.8 ms mean depth, the correction at +200.9 / -201.0 ppm, no underrun, no overflow; 48 to 44.1 kHz and 96 to 48 kHz take the network's frames within 0.01 % and pass a 1 kHz sine at 1000.0 Hz and 0.0 dB. A cushion steps down 10 ms after each minute with no dropout.
- New: `Media::set_app_command` and `Link::set_app_command(f: Fn(&str) -> Result<String, String>)`: an app's own command hook. A control request `app <words>` (the Unix socket, and the local endpoint dtray reads) calls it and answers `ok`, the app's one-line text, or `error: <reason>`; without a hook `error: no app command`. The local endpoint now answers `status` and `app <words>` (was `status` only; its refusal reads "this endpoint answers status and app only", `ctl::local::ONLY_STATUS` is now `ONLY_STATUS_AND_APP`). The test requests (`source`, `check`) stay on the Unix socket. New: `ctl::app_words`, `ctl::app_answer`, `ctl::NO_APP_COMMAND`, `ctl::APP_ANSWER_MAX`, `Request::App`.
- dtray: a device whose app part carries `outputs` (names) and `output` (the index in use) gets an Output list in its submenu, the one in use checked; choosing one sends `app output <n> <name>` to the device (`dtray::output`, `probe::ask`); dlisten refuses it when its list changed since, never playing another output. `menu::Shape` has `outputs`, and `menu::shapes_of` takes the readings. This is dtray's first write besides a latency, limited to an app's own setting.
- `dstack_resample::asrc::RateFit` makes the room for its points at once (`REGRESSION_CAP` + 1), so a controller on an audio callback (dlisten) never allocates as its fit fills; SongPlayer's servo gains the same.
  - API: `ctl::local::ONLY_STATUS` renamed (above); `Request` has a new variant `App` (a match on it needs the arm). No latency change: dlink's budget, split, block and converter are as before.

## 0.6.45

- Fix (camera-box, strih-snv OBS, 5.11, v0.6.33): after a restart the device did not start ("RX flow port 47217: Address already in use"), and the strih program had no audio for 70 min. The TX side bound its 32 sockets on ports the OS chose before the RX side bound its fixed `rx_port_base` ports. The base 47200 lies in Linux's ephemeral range, so a TX socket could take one of them: in the test, about one start in 28. The RX side now binds first. A base inside the ephemeral range can still collide with another process's socket, so the `rx_port_base` doc now says to pick one outside it.
  - No API change, no latency change.

## 0.6.44

- Fix (iemmixer, 5.11, v0.6.43; dstack #1): a channel counted 11531 holes and 6982 late samples, in bursts every 15-20 s, while its flow counted 0 late packets. A flow judged a packet late only once its first frame passed playout + block, the most a block's read can wait. The pump reads a block at each wake, so a packet that passes playout within the block is behind the reader or not depending on the pump's phase. Its channels counted it; the flow never did. The status's `lateness` holds only the last second's peak, so the bursts did not show there either. Each RX flow now also counts `late_at_read`: packets with a frame that a ring refused as already read, the same packets its channels count. It also keeps `lateness_peak`, the largest lateness since start. `late` keeps its meaning.
  - Additive status fields only; no API change, no latency change.

## 0.6.43

- Fix (dasio, FOH 5.11, v0.6.39; dstack #1): every 96 s each device process sent about 60 mDNS queries in one second, and the answers made a burst of over 500 packets. FOH's audio TX stalled in that second on both instances (TX schedule restarts, send error 10055, late packets at the receivers), 60 times in 2 h. The vendored mdns-sd queried each cached instance and host in its own message at exactly 80 % of the record's TTL (120 s), so records learned together re-queried together forever. Now a refresh pass asks every due question in one message, split only where a packet is full, and a pass with a record due takes along those due within 2 % of their TTL. Each responder answers once for all its records. On the wire (`tests/mdns_refresh.rs`): 4 instances over 4 refresh stages went from 16 query messages to 4.
  - No API change, no latency change.

## 0.6.42 (unreleased)

- dsync v1.28.0 (dsync#158, asked by dstack #1 after iemmixer's GM loss at the FOH reboot of 5.11): the in-process follower keeps its time base over a grandmaster change within 1 ms of it. The residual is retired along dsync's pull (τ 20 s, at most 50 ppm), so no step reaches the media clock and no re-align follows. dsync logs it as `[FOLLOWER] grandmaster change in the same time base`. A change over 1 ms still re-anchors as before. After every grandmaster change dsync fits the rate afresh once, so the follower settles again within about a minute (8-10 ppm off: 48-50 s in its simulation).
- New: the clock status's `follower` object carries `pull_ns`, the phase dsync still retires (0 when none).
  - No API removed (new field: `FollowerCounts::pull_ns`), no latency change.

## 0.6.41 (unreleased)

- Fix (v0.6.40's main CI on windows-2025; dstack #1): a handover wait's round counts as progress only when it comes `clock::linked::CONVERGE_NS` = 5 µs nearer than the mark (was 1 µs). Measured offsets jitter by a few µs from read to read. At 1 µs a steady disagreement could read as progress now and then, and the wait went on past `HANDOVER_WAIT`. The test with a constant 1 ms offset failed that way on main. A converging follower still hands over seamlessly: iemmixer's -61 µs to -31.75 µs over about 3 minutes makes progress every 20-30 rounds.
  - No API change, no latency change.

## 0.6.40 (unreleased)

- Fix (iemmixer, 5.11, v0.6.37, after a hard reset of its box; dstack #1): a linked clock's handover wait keeps waiting while the two sources keep closing in. Before, a switch from a working service to the follower waited a fixed 60 rounds for them to agree within `clock::linked::HANDOVER_NS` (30 µs), then re-aligned. After the reset the follower closed on the service slowly (-61 µs, -43 µs, -31.75 µs), and the wait ran out 1.75 µs outside the bound. The result was a re-align and one pump re-bootstrap, about 5 minutes after boot, with the band possibly playing. Now a waiting round that comes `clock::linked::CONVERGE_NS` (1 µs) nearer than the last progress starts the count again (`clock::linked::wait_round`). So a converging follower hands over seamlessly once within 30 µs. A steady disagreement still re-aligns after `HANDOVER_WAIT` (60) rounds without progress, for example strih's 0.48 ms before dsync 1.27.4.
  - No API removed (new: `wait_round`, `CONVERGE_NS`), no latency change.

## 0.6.39 (unreleased)

- Fix (dasio's iem-snv trial of v0.6.38, 5.11; dstack #1): with `spin_escape_ms` set, an engine pump takes the escape's one-tick block only when its work margin covers it (`clock::pump_may_escape`: W + one block >= `clock::PRESEND_NS`, 2.14 ms). In the trial a pump at block 64 with W 64 (96 kHz, 0.67 ms each) re-bootstrapped and lost 2 blocks at every escape (152 re-bootstraps, 304 missed blocks in 7 min). A pump out of the escape spins yielding, as without it, and says so at its start. FOH's pumps (W 384 and 512 at 96 kHz) still take it.
  - Only the threads that opt in make the escape (`clock::allow_escape`): the TX thread, which sends ahead first, and a pump whose margin covers the block. Every other thread spins yielding.
  - The trial's other results: TX made no restarts and sent about 74 k packets ahead per flow, and the receiver at 250 µs had 0 gaps and 0 late; the PC stayed responsive, and the per-CPU walker waited at most one period on the spinning CPUs.
  - Still OFF by default; unset, nothing changes.

## 0.6.38 (unreleased)

- New, OFF by default, for a trial only (dstack #1, agreed with dasio 5.11): `DeviceConfig::spin_escape_ms`, a bounded escape for the Windows low-latency wait. Left out, nothing changes: the spin yields with SwitchToThread as in v0.6.36. Set (100 to 10000 ms), on Windows:
  - The spin does not yield between the blocks, so no lower priority takes a media thread's CPU near a send (the FanControl steal of 5.11, 1.66 ms, a band TX restart). Sleep(0) was tried first and is no shelter: on windows-2025 a lower-priority thread on the same CPU ran 21 million turns in 0.3 s of a Sleep(0) spin, and the waits went 3 ms late at the median.
  - At least once a period each spinning media thread blocks one timer tick (`clock::ESCAPE_BLOCK_NS`, 1 ms). The timer's expiry preempts lower priorities at once, so per-CPU system work runs on every CPU; its absence froze FOH on v0.6.28 and v0.6.35.
  - The pump just blocks (W absorbs it). The TX thread blocks right after a pass, and first sends every packet due within `clock::PRESEND_NS` (2.14 ms = the block + the timer's late wake of one tick + 140 µs) whose samples the engine ring (or the echo's RX ring) already holds. It repeats that from a fresh clock read while it sends anything (at most 4 rounds), so the window also covers its own send time (all but the last round's when every round sent).
  - When a packet in the window has no samples yet, TX puts the escape off to a later pass instead of blocking: held through the block, that packet would pass a short latency (`max_lag`, 96 samples at 1 ms) and restart its flow. It puts it off at most 20 ms past the period (`clock::ESCAPE_DEFER_MAX_NS`); then it escapes anyway, and such a packet may restart its flow. A re-anchor between the pass and any round of the send ahead (each round reads the clock's time and epoch) sends nothing more and puts it off.
  - Status: each TX lane gains `early` (packets sent ahead); `media::tx::LaneCounts::early`. The start log names the mode and its period.
  - Linux: the field is accepted and ignored, said in the start log.
  - The period is process-wide: the last device a process sets wins, as with `spin_tail_ns`.
  - New API: `clock::escape` (re-exported in `clock`: `SPIN_ESCAPE_MIN_MS`, `SPIN_ESCAPE_MAX_MS`, `ESCAPE_BLOCK_NS`, `TIMER_LATE_NS`, `PRESEND_NS`, `ESCAPE_DEFER_MAX_NS`, `EscapeState`, `spin_escape_of`, `spin_escape_line`), `clock::{set_spin_escape_ms, ask_escape, defer_escape}`, `media::tx::Schedule::{due_early, due_by}`, `media::tx::Transmitter::{presend, presend_all, before_wait}`, `media::tx::{Presend, PRESEND_ROUNDS}`, `media::ring::Ring::holds`, `media::source::Source::ready`.
  - First trial on dasio's test PC (iem-snv), never on FOH before it: a real receiver must accept the early packets within its buffer.
  - No latency change; no behaviour change without the field.

## 0.6.37 (unreleased)

- New (iemmixer, 5.11; dstack #1): the status's `clock.follower` object carries what the linked clock's selector decides on: `locked` (dsync's lock gate, |e| within 50 µs for 3 windows), `settled` (locked, and |pull| and the recent |e| p90 within 30 µs; only a settled follower takes over a ready service), `offset_ns` (the last window's error) and `recent_abs_e_ns` (the recent |e| p90; `null` until there are enough windows). `clock::FollowerCounts` gains the same four fields.
  - Why: the FOH freeze (5.11, 06:01 CET) changed the grandmaster under iemmixer. Its follower lost its lock, the clock handed over to the service seamlessly, and the follower relocked at -39.8 µs. It took over again only 6 min 41 s later, once settled, and the status could not say which condition it was waiting on. The audio was unaffected throughout (late, missed, rebootstraps and TX holes 0).
  - No behaviour change, no latency change.

## 0.6.36 (unreleased)

- HOTFIX (FOH froze, 5.11 ~04:48Z, minutes after dasio rolled v0.6.35; dstack #1): every Windows low-latency spin yields again, as in v0.6.33. v0.6.35 kept a spin that never yields on threads pinned to a CPU of its own. With FOH's pinned TX and engine threads (each alone on its CPU, only blocking threads on the SMT sibling), the PC stopped answering, the same freeze as v0.6.28's Sleep(0): some per-CPU system work must run on exactly that CPU. **Do not run v0.6.34 or v0.6.35 on Windows.**
  - Removed: `clock::set_pinned`, `clock::pinned` (v0.6.35).
  - The yield's cost comes back: a lower-priority thread can keep a media thread's CPU for the rest of its slice (FanControl, 1.66 ms, a TX restart on FOH). A bounded escape is dstack #1's open item, to be tried on a non-production Windows box before FOH.
  - Regression test: `a_spun_wait_lets_a_lower_priority_thread_on_its_cpu_run` (pinned to CPU 0 with a busy lower-priority thread); it failed on v0.6.35. The v0.6.34 test that required no yield is removed: it required the behaviour that froze FOH.
  - No latency change.

## 0.6.35 (unreleased)

- Fix (dasio, 5.11, v0.6.34 in its CI; dstack #1): a media thread without a CPU of its own yields in its spin again. In v0.6.34 every low-latency spin on Windows stopped yielding. dasio's Windows e2e runs two unpinned devices on one runner, and there the spinners starved the box: the pump overran 67-398 blocks a second, re-bootstrapped (missed 16), and the device drove the host 0 times in 4 s. Now:
  - A thread the stack pinned to a CPU of its own (`pump_cpu`, `io_cpu`, `rx_cpu`, the pin taken; `clock::set_pinned`, cleared when a re-pin is refused) never yields. That is v0.6.34's fix for the FOH TX restarts, kept for dasio's pinned threads.
  - Any other thread yields as in v0.6.33, and so does the TX retry wait.
  - The default spin tail is back to 50 ms (`clock::SPIN_TAIL_NS`; it was 2 ms in v0.6.34). So an unpinned device waits exactly as in v0.6.33.
  - Regression test: `an_unpinned_spun_wait_lets_a_lower_priority_thread_on_its_cpu_run`. On v0.6.34, on the Windows runner, the lower-priority thread never ran during 100 spun waits.
  - No API removed (new: `clock::set_pinned`, `clock::pinned`), no latency change.

## 0.6.34 (unreleased)

- Fix (dasio, FOH, 5.11; dstack #1): on Windows the low-latency wait no longer yields its CPU to a lower priority. Before, the media threads spun the last `spin_tail_ns` (50 ms by default) of every wait with SwitchToThread, which hands the CPU to any ready thread, a lower priority included, for the rest of that thread's slice. dasio's CSwitch trace caught FanControl (priority 10) holding the band TX thread's CPU (priority 31) for 1.66 ms; the TX schedule started again (lag 156 samples > max_lag 96), and the receivers lost those packets. Now:
  - The spin never yields (`spin_loop`), and the TX retry wait does the same (`media::tx::retry`). A thread gives its CPU away only on the high-resolution timer, whose expiry preempts a lower priority at once.
  - The default spin tail is 2 ms (`clock::SPIN_TAIL_NS`, was 50 ms). It covers the timer's late wake measured on the FOH PC: one tick + 140 µs, 1.1 ms at the 1 ms resolution. A wait longer than the tail sleeps on the timer until the tail. A device's own `spin_tail_ns` is unchanged.
  - A thread whose deadlines come closer than the tail spins all the time: TX at 8 frames a packet (every 83 µs at 96 kHz), or a pump whose block is shorter than the tail. Such a thread needs its CPU alone, and the other CPU of its core must run only threads that block. Windows pins a priority-0 System thread to each core; with both CPUs spun it never runs, which is how 0.6.28's Sleep(0) froze two PCs (`docs/windows-box.md`). dasio's FOH layout meets this.
  - Regression test: `a_spun_wait_never_yields_its_cpu_to_a_lower_priority_thread`, with a lower-priority busy thread on the same CPU. Before the fix, on the Windows runner: median 2.49 ms late, max 32.7 ms.
  - The normal profile (dlink) waits on the timer only and is unchanged. No latency change.

## 0.6.33 (unreleased)

- Fix (strih, 5.11, v0.6.32 live; dstack #1, dsync#136): dsync-ptp v1.27.3 -> v1.27.4. On a PC that hears both ports of a dual-port Dante grandmaster, the in-process follower ran about 0.48 ms behind the dsync service. Such a grandmaster sends the same clock id and sequence numbers on its primary (VLAN) and secondary (untagged) port, about 470 µs apart. The follower paired the Sync from one port with the Follow_Up from the other. So every start re-aligned the media threads at the Service -> Internal switch (strih: offset -471..-495 µs, over `HANDOVER_NS`), one pump re-bootstrap and every TX flow started again.
  - dsync v1.27.4 follows one grandmaster port (the first heard; it switches only after 2 s of silence) and pairs a Follow_Up only with that port's Sync. On Linux its sockets set `IP_MULTICAST_ALL=0`, so a follower hears only the NIC it joined on.
  - The follower stays on the device's media interface (the Dante primary). On strih the service follows the secondary port; the ports' own difference, measured there, is about 20 µs (median over 24 sequence numbers), under `HANDOVER_NS`.
  - No API change, no latency change.

## 0.6.32 (unreleased)

- Fix (camera-box, 4.11, v0.6.31 live; dstack #1): an RX-only dlink device's pump margin W is the whole budget, so a pump preempted several ms no longer restarts and loses blocks. OBS dante-input (RX only, a 48 kHz app on a 96 kHz network, the default 15 ms budget, block 128, unpinned on a loaded PC) was preempted about 6 ms, right at its W of 6 ms (576 samples), and re-bootstrapped 19 -> 30 in about 15 minutes, each one an audible gap. An RX-only device's W is in none of its paths: its RX path is P + B + the converter's RX delay (block k reads the RX rings at its own stamps whenever the pump runs), and D = B + P + W only stamps the engine's TX rings, which it does not have. W only says how late its pump may run before it starts again; a block that late reaches the app that much later, so the app's budget bounds it. The stack holds any W up to `stream::MAX_WORK_MARGIN` (4096 samples) beside any P (`device::MAX_LATENCY_SAMPLES` keeps both inside half the RX rings).
  - New: `dlink::rx_work_margin(budget_ns, rate, block)`: the budget in samples, at most `MAX_WORK_MARGIN`, at least the block and 2 ms. `dlink::split` uses it for `Directions::Rx`; a device with TX keeps `work_margin` (2/5 of the budget, given way to the TX floor), unchanged.
  - **No latency change**: P, B, the converter delays, the RX path and the TX path of every device with TX are unchanged. RX-only W at the default 15 ms, block 128: a 96 kHz device 576 -> 1440 samples (6 -> 15 ms; a 48 kHz app on a 96 kHz network, OBS, included), a 48 kHz device 288 -> 720 samples (6 -> 15 ms), a 44.1 kHz device 265 -> 662 samples. At another budget it is that budget (10 ms: 960 samples at 96 kHz, was 384).
  - What an app can notice on an RX-only device: the device config's `work_margin`, `Split::margin`, the pump view's `offset` (B + P + W) and `margin` in the pump warning grow by those numbers; `Split::tx_path_ns` (no TX path on such a device) grows with W. After a preemption longer than the old W and within the new one, the pump runs the blocks it owes back to back (up to W / B, about 11 at 96 kHz and block 128), so the processor sees a burst of calls and the pump's `late` count grows where `rebootstraps` and `missed` grew before.
- Fix (dstack #1): the once-a-second warning `engine pump lost or overran blocks this second` prints that second's longest block as `max_process_us`. Before, it printed the longest since the pump started, so one long block repeated in every later line. The longest since the start stays in the status (`engine.max_process_us`, `StreamStats::max_process_ns`). No latency change.

## 0.6.31 (unreleased)

- Fix (iemmixer, 4.11, v0.6.29 live; dstack #1): a linked clock's switch between dsync's in-process follower and the dsync service no longer re-aligns the media threads when the two clocks agree. Before, every engine start broke the audio once, at the Service -> Internal switch about 35 s in (the epoch's high half moved, 65536 -> 131072; the pump re-bootstrapped and every TX flow started again). Both sources trail the grandmaster by the same receive latency, so now:
  - When the new source's time is within `clock::linked::HANDOVER_NS` (30 µs, dsync's settled bound) of the old one's, the switch keeps the epoch. Media time takes one step of at most 30 µs (6 samples at 192 kHz), which the pump's margin (at least the block, 16 samples) and the TX schedules absorb: nothing is skipped and nothing starts again. The offset is measured with the service (or follower) read before and after the other source, and the farther of the two differences counts, so a selector preempted between its reads waits instead of handing over; a source that re-aligns during the reads gives no agreement. At the tightest pump schedule (192 kHz, block 16, W 16) a forward step leaves 10 samples (52 µs) of the margin for that one round's wake jitter; a step back sends the next TX packets up to 30 µs later once, inside the 50 µs send slack every advertised latency covers (`device::TX_SEND_SLACK_NS`).
  - When they disagree and the source in use still works (a ready service while the follower wants to take over), the switch waits up to `HANDOVER_WAIT` rounds (60, one a second) for them to agree, then re-aligns as before. A switch away from a source that is gone (the follower lost its lock) never waits: seamless when they agree, a re-align otherwise. So a disagreement costs no more breaks than before.
  - Each change is logged, a handover with the offset measured in ns: `SEAMLESS_LINE` ("media clock source handed over, no re-align"), `RE_ALIGN_LINE` ("media clock source changed with a re-align"), the first round of a wait `WAIT_LINE` ("media clock: the new source waits to agree with the one in use"), a wait that ended without a switch `WAIT_ENDED_LINE` ("media clock: the wait for a handover ended, no switch"), the first source after none `FIRST_SOURCE_LINE` ("media clock: a first source is ready"; was the INFO "media clock source"), and no source left `NO_SOURCE_LINE` (the WARN "media clock: no source ready", unchanged). A log reader matching "media clock source" moves to these texts.
  - A consumer that counted an epoch change at the switch (a test expecting one pump re-bootstrap per start) now sees none when the clocks agree. A linked clock's epoch keeps its shape: re-aligns in the high half, the re-anchors since the last re-align in the low half (a seamless handover carries them over to the new source).
- Fix (iemmixer, 4.11, v0.6.29 live, a grandmaster reboot): a re-anchor of dsync's in-process follower no longer re-aligns the media threads when its step is within `HANDOVER_NS` (30 µs). The follower re-anchored by +2557 ns ("new time base", epoch 1) and then by -5710 ns, and each one re-bootstrapped the pump and every TX flow, for steps under one sample at 96 kHz. The step is exact: dsync-ptp publishes its model (`ClockModel`: `m0`, `ptp0`, `rate_ppm`), so the step is the new model's time at its anchor less the previous model's at that instant (`clock::linked::re_anchor_step`). A re-anchor within the bound is absorbed (media time takes that one small step, the epoch stays); a bigger one re-aligns as before, and so does a change after a model with no anchor (the first anchor itself is no re-anchor: the switch from no source is the break). The service path already behaved this way: its tracker holds k for steps under 20 µs (`clock::TOLERANCE_NS`). The follower is read through `clock::linked::Absorbing` (over the `Model` trait, dsync's `PtpClock` implements it); whichever thread reads first feeds the tracker behind `try_lock`, reading the model under the lock, so the pump and TX threads never wait for it. A thread that finds it taken reads the counts last published: on a re-align in that very step it can see the new time under the old epoch for one step (one more pump re-bootstrap on a step that re-aligns anyway), never on a step that is absorbed.
- Fix (iemmixer, 4.11): `ctl status` `clock` counts describe the source in use. The top-level `polls`, `re_anchors`, `holdovers` and `holdover` are the follower's while `source` is `internal` (its phase-lock windows in `polls`), and the dsync service poller's otherwise. Before, they were always the poller's: stopping the dsync service for 30 s showed holdover 1 and re_anchors 1 while the device ran on the follower. A linked clock adds two objects after `source`: `service` {`polls`, `re_anchors`, `holdovers`, `holdover`} (the poller) and, when the follower started, `follower` {`windows`, `re_anchors`, `holdovers`, `holdover`, `absorbed`} (dsync-ptp's `windows`, `reanchors`, `holdovers`, `holdover`, and the re-anchors absorbed without a re-align; several dsync re-anchors between two reads count once, as their sum, so `absorbed` plus the re-aligns can be under `re_anchors`). New words in the status: `follower`, `windows`, `absorbed`. A clock that is not linked (`dsync`, `system`) shows no new object. A reader that reads the top level as the dsync service's moves to `clock.service`.
- New: `clock::linked::{HANDOVER_NS, HANDOVER_WAIT, Handover, handover, WAIT_LINE, WAIT_ENDED_LINE, SEAMLESS_LINE, RE_ALIGN_LINE, FIRST_SOURCE_LINE, NO_SOURCE_LINE}`, `clock::linked::{re_anchor_step, Absorb, Absorbing, Model}`, `clock::FollowerCounts`, `Linked::follower_counts`, `ClockStats::set_follower` / `follower`. Changed: the `clock::linked::Follow` trait gains `counts() -> FollowerCounts` (an app's own follower stand-in implements it); `start_internal` returns the follower wrapped in `Absorbing`, and dsync's `PtpClock` no longer implements `Follow` itself (it implements `Model`). No latency change.

## 0.6.30 (unreleased)

- **Fix, urgent** (dstack #1, dasio 4.11): revert v0.6.28's `Sleep(0)` in the Windows spin tail back to `SwitchToThread`. Sleep(0) yields only to threads of equal or higher priority. A media thread at real-time priority (timing = low latency, windows_wait = spin) spinning through its wait then starved every lower-priority thread on its CPU, and two Windows PCs running dasio on v0.6.28 stopped answering anything else. SwitchToThread lets lower-priority threads run again. The 10–23 ms waits it can cause on a busy CPU (dasio's FOH trace) are left to the per-thread pins (re-applied once a second since v0.6.28) and to a separate fix. No latency change.

## 0.6.29 (unreleased)

- Fix (iemmixer, 4.11): `ctl status` `clock.source` names what a linked clock uses now: `internal` (dsync's follower in process), `service` (the dsync service), or `none` (nothing ready yet). Before, it said `dsync` whichever was in use. A clock that isn't linked still says `dsync`, and the test clock `system`. The host's once-a-second source selection stores it (`ClockStats::set_linked` / `linked`). New: `ctl::clock_source_word`. A reader matching `"source":"dsync"` on a linked device moves to the new words. No latency change.

## 0.6.28 (unreleased)

- **Latency change** for every dlink app (dstack #1; songplayer's report against v0.6.25, and the owner's ROZHODNUTÉ of 4.11.2026). songplayer's TX-only 48 kHz device on a 96 kHz network ran P 4.01 ms, its TX thread left packets up to 3.71 ms past due, and a receiver at that latency counted 137 late in 1 h 50 min. Three changes:
  - **The default budget is 15 ms** (`dlink::BUDGET_NS`, was 10 ms since v0.6.22). The owner: 10 ms was never meant to push dlink apps low. W stays 2/5 of the budget, so it is 6 ms at the default (was 4 ms): room for the 2.7 to 3.6 ms preemptions seen on loaded PCs (intercom, camera-box), where a dlink app's threads run unpinned and without real time. `Config::budget_ns` still sets another.
  - **A TX floor for P:** a device with TX channels gets P at least `dlink::tx_floor_ns(device_rate)`, a TX packet's own age when it leaves = 32 frames (`dstack::announce::FPP_MAX`, the most a receiver asks in a packet) + the TX margin (0.25 ms, `dstack::media::tx::TX_MARGIN_NS`) + 4 ms of lateness (`dlink::TX_LATE_NS`; the measured 3.71 ms rounded up, the 5 ms bin was empty): 4583333 ns at 96 kHz, 4916666 ns at 48 kHz, 4975623 ns at 44.1 kHz. W gives way to it, down to its least (the block and 2 ms), only when a budget leaves no room for both; at the default 15 ms it never has to. A budget that cannot hold it is refused with the floor named. An RX-only device keeps the 4 ms profile floor (`MIN_LATENCY_NS`).
  - **A short converter for an exact 2:1 or 1:2:** dlink's `Rated` (and `rated::delay_ns`, so `dlink::split`) use the new `dstack_resample::Converter::low_delay`, which steps a 2:1 or 1:2 ratio with an even block through a 123-tap linear-phase half-band FIR (Kaiser beta 15.57, polyphase, `dstack_resample::halfband`), with no TX FIFO prime. Its delay is 61 samples at 96 kHz: RX 625000 ns, TX 635416 ns for a 48 kHz app on a 96 kHz device (the 256-tap sinc took 1.33 / 2.83 ms with its prime). Measured by its tests: passband ripple 0.0000006 dB to 20 kHz at 96 kHz; stopband at least 149.0 dB down from 28 kHz (under a 24-bit LSB, -144.5 dBFS, for a full-scale tone); THD+N of a 1 kHz tone at -1 dBFS 252.4 dB at 96 -> 48 kHz and 161.4 dB at 48 -> 96 (the sinc: 252.0 / 230.7 dB); a 30 kHz tone folded at 96 -> 48 at -157.6 dBFS (sinc -209.1); a 20 kHz tone's 28 kHz image at 48 -> 96 at -150.2 dBFS (sinc -214.9); 1 kHz and 20 kHz at their level within 0.0001 dB. 44.1 kHz pairs stay on the sinc. `Converter::new` and `drifting` (SongPlayer's ASRC) are unchanged.
  - Before (v0.6.24 to v0.6.27, the 10 ms default) and after (the 15 ms default), block 128. A TX-only device splits as one with both directions in every row:

    | Device | App | Directions | W before | W after | Converter RX / TX before | Converter RX / TX after | P before | P after | TX path before -> after | RX path before -> after |
    |---|---|---|---|---|---|---|---|---|---|---|
    | 48 kHz | 48 kHz | both, TX only | 4 ms | 6 ms | none | none | 6 ms | 9 ms | 10 -> 15 ms | 8.67 -> 11.67 ms |
    | 48 kHz | 48 kHz | RX only | 4 ms | 6 ms | none | none | 7.33 ms | 12.33 ms | none | 10 -> 15 ms |
    | 96 kHz | 96 kHz | both, TX only | 4 ms | 6 ms | none | none | 6 ms | 9 ms | 10 -> 15 ms | 7.33 -> 10.33 ms |
    | 96 kHz | 96 kHz | RX only | 4 ms | 6 ms | none | none | 8.67 ms | 13.67 ms | none | 10 -> 15 ms |
    | 96 kHz | 48 kHz | both, TX only | 3.16 ms | 6 ms | 1.33 / 2.83 ms | 0.63 / 0.64 ms | 4.01 ms | 8.36 ms | 10 -> 15 ms | 6.68 -> 10.32 ms |
    | 96 kHz | 48 kHz | RX only | 4 ms | 6 ms | 1.33 / 2.83 ms | 0.63 / 0.64 ms | 7.33 ms | 13.04 ms | none | 10 -> 15 ms |
    | 96 kHz | 44.1 kHz | both, TX only | 2.91 ms | 6 ms | 1.32 / 3.08 ms | 1.32 / 3.08 ms | 4.01 ms | 5.92 ms | 9.99 -> 14.99 ms | 6.66 -> 8.57 ms |
    | 96 kHz | 44.1 kHz | RX only | 4 ms | 6 ms | 1.32 / 3.08 ms | 1.32 / 3.08 ms | 7.34 ms | 12.34 ms | none | 9.99 -> 14.99 ms |
    | 48 kHz | 44.1 kHz | both, TX only | 2.90 ms | 6 ms | 2.65 / 3.08 ms | 2.65 / 3.08 ms | 4.02 ms | 5.92 ms | 9.99 -> 14.99 ms | 9.34 -> 11.24 ms |
    | 48 kHz | 44.1 kHz | RX only | 4 ms | 6 ms | 2.65 / 3.08 ms | 2.65 / 3.08 ms | 4.67 ms | 9.67 ms | none | 9.99 -> 14.99 ms |
    | 48 kHz | 96 kHz | both, TX only | 4 ms | 6 ms | 2.67 / 1.42 ms | 0.64 / 0.63 ms | 4.58 ms | 8.38 ms | 10 -> 15 ms | 9.92 -> 11.68 ms |
    | 48 kHz | 96 kHz | RX only | 4 ms | 6 ms | 2.67 / 1.42 ms | 0.64 / 0.63 ms | 4.67 ms | 11.69 ms | none | 10 -> 14.99 ms |

    Exact P after, ns, both directions and TX only: 9000000 (48/48), 9000000 (96/96), 8364583 (96/48), 5916666 (96/44.1), 5916666 (48/44.1), 8375000 (48/96), 6167800 (44.1/48), 8979591 (44.1/44.1). RX only: 12333333 (48/48), 13666666 (96/96), 13041666 (96/48), 12343750 (96/44.1), 9666666 (48/44.1), 11687500 (48/96), 9183673 (44.1/48), 12086167 (44.1/44.1). W after, samples: 288 at 48 kHz, 576 at 96 kHz, 265 at 44.1 kHz (6 ms) in every row. Converter delays after, ns: 96/48 RX 625000, TX 635416; 48/96 RX 635416, TX 625000 (no FIFO prime); 96/44.1 RX 1315192, TX 3077238 and 48/44.1 RX 2653061, TX 3077238 (the sinc, unchanged).
  - Block 256 at the default changes only the RX path (W stays 6 ms, over the block; P as at block 128). Every pair of 44.1, 48 and 96 kHz fits 15 ms at every block of the stack: none of these is refused at the default (an 8 kHz app on a 96 kHz network still needs a larger budget).
  - Lateness room at a receiver playing at P, songplayer's device (96 kHz, 48 kHz app, TX only): P less 32 frames and the TX margin = 7.78 ms against the measured 3.71 ms (was 3.43 ms).
  - An app that sets a smaller budget now gets the TX floor: at 10 ms (`budget_ns(10_000_000)`) a 48 kHz app on a 96 kHz device gets W 4 ms and P 5364583 ns (was W 3.16 ms, P 4010416), a 44.1 kHz app W 2.32 ms and P 4593750 ns, and these configs with TX are refused by `Config::device` (and `dlink::split`) with the floor named ("at least 4916666 (a TX packet's own age, dlink::tx_floor_ns)"): a 48 kHz device with a 44.1 kHz app at block 128 (block 64 fits); a 44.1 kHz device with a 48 kHz app at block 128 (block 64 fits); block 256 on a 48 kHz or 44.1 kHz device; a 96 kHz device with a 44.1 kHz app at block 256. They ran at 10 ms before with P at about 4 ms, under a TX packet's age.
  - At a 20 ms budget: a 48 kHz app on a 96 kHz device P 9.17 -> 11.36 ms (RX only 17.33 -> 18.04 ms), W 8 ms as before.
  - What a consumer re-measures: an app that aligns by the device latency (SongPlayer's ledger, camera-box's A/V) moves to the P above; an app that set no budget gets 5 ms more path each way. Pinned test numbers move (the default config): 48/48 P 288 -> 432 samples, W 192 -> 288, pump offset 608 -> 848; a 48 kHz app on a 96 kHz device with both directions P 385 -> 803, W 303 -> 576, offset 816 -> 1507; RX only P 704 -> 1252, W 384 -> 576, offset 1216 -> 1956. An app that needs the old 10 ms sets `budget_ns(10_000_000)`.
  - Problem text: `dlink::split`'s refusal ends with "(the normal profile's floor)" for an RX-only device and "(a TX packet's own age, dlink::tx_floor_ns)" with TX, after "at least <ns>"; a test matching the old text moves.
  - New: `dlink::tx_floor_ns`, `dlink::TX_LATE_NS`; `dstack_resample::Converter::low_delay`, `Converter::is_half_band`, `dstack_resample::halfband` (`HalfBand`, `Step`, `taps`, `TAPS`, `DELAY_HIGH`, `BETA`, `STOP_DB`, `RIPPLE_DB`). `Converter::set_ppm` refuses a half-band step as it refuses a copy.
- **Removed** (the owner's ROZHODNUTÉ of 4.11.2026, dstack #1): `dlink::Config::pump_cpu`, `io_cpu` and `rx_cpu` (added in v0.6.26). CPU pinning is out for dlink apps: nobody guards which core is free on every PC. A dlink device's threads stay where the OS puts them; the 6 ms W of the 15 ms default is what keeps a preempted pump whole. Port: delete the calls (camera-box's hub and OBS pinner). dstack's own `DeviceConfig` `pump_cpu`/`io_cpu`/`rx_cpu` stay (dasio, iemmixer).
- Fix (dstack #1, dasio's FOH trace): on Windows the spin tail of a media thread's wait yields with `Sleep(0)` instead of `SwitchToThread` (`std::thread::yield_now`). SwitchToThread gave the CPU to any ready thread on it, a lower-priority one included (System workers, a fan tool at priority 10), for that thread's whole quantum: dasio's pumps sat Ready for 10–23 ms (`WrYieldExecution`) and ran late on both FOH instances together. Sleep(0) runs only a ready thread of equal or higher priority, so our RX/TX threads sharing a core still get it, and returns at once otherwise. No latency change.
- Fix (dstack #1, dasio's FOH PC): a pinned media thread (dstack `DeviceConfig`'s `pump_cpu`, `io_cpu`, `rx_cpu`) re-applies its pin about once a second on Windows. Windows resets every thread's affinity to the process mask when a program sets the process's (`SetProcessAffinityMask`); Process Lasso's process-affinity rule (and a catch-all `*` rule) re-applies one after start, so the pins set once at start were lost and two media threads ended up on one CPU. The re-pin is one `SetThreadAffinityMask` call on the thread's own loop, no allocation; the TX thread and the pump read one `Instant` more per pass for its schedule. The daemon's tick tells what the re-pins found, once per device for resets anywhere in the process: one INFO line when a pin was found lost (`dstack::media::repin::RESET_LINE`, "a media thread's CPU pin was found lost"), one WARN line when the OS refused the re-pin because the CPU is outside the process affinity another program set (`REFUSED_LINE`; the thread then runs unpinned). `pin_resets()` and `pin_refusals()` count them for the process. Linux is unchanged (nothing there resets a thread's pin). New: `dstack::media::repin` (`RePin`, `PinOutcome`, `outcome`, `one_cpu_mask`, `PinWatch`, `pin_resets`, `pin_refusals`, `REPIN_NS`, `RESET_LINE`, `REFUSED_LINE`). No latency change.

## 0.6.27 (unreleased)

- Fix (obs's review of v0.6.26): on Windows a config's `pump_cpu`, `io_cpu` or `rx_cpu` from 64 on is refused ("pump_cpu: 0 to 63"); Windows pins a thread only within its first processor group. Before, it passed the check and the thread was then never pinned. New: `dstack::device::max_cpu_for(Os)`. Linux keeps 0 to 1023. No latency change.

## 0.6.26 (unreleased)

- New (dstack #1, camera-box): `dlink::Config::pump_cpu(cpu)`, `io_cpu(cpu)` and `rx_cpu(cpu)` place the device's pump (`dstack-engine`), TX (`dstack-tx`, and RX unless `rx_cpu` is set) and RX (`dstack-rx`) threads on CPUs the app chooses, the way a dstack config's `pump_cpu`/`io_cpu`/`rx_cpu` do. Unset, they stay anywhere, as before. A CPU at or past `dstack::device::MAX_CPU` is refused by `Config::device`. Why: on a loaded PC (load 11 on 20 CPUs), a hub's unpinned pump was preempted for about 3.5 ms, past its margin, and lost blocks. No latency change.

## 0.6.25 (unreleased)

- New log line (dstack #1; intercom's CI, where a 96 kHz TX into a 96 kHz RX with a 48 kHz app breaks the tone about once a second): once a second, when an engine pump re-bootstrapped, skipped blocks or overran in that second, one WARN `engine pump lost or overran blocks this second` (grep for that text; `dstack::stream::watch::PUMP_WARNING`), with `device`, the growth that second (`rebootstraps`, `missed`, `overruns`), `max_process_us` (the longest block since the start), `media` (on the media clock or free pacing) and the pump's timing in samples: `block` (B), `margin` (W), `playout` (P), `offset` (D). It comes from the daemon's tick, never the pump thread, for every device with an engine: every dlink device and every `embed` engine. A gap in an app's RX in the same second as this line is the pump; a gap without it is the flow (the RX loss line, `RX packets missing from a flow`, or the sender). Quiet seconds log nothing.
- New: `dstack::stream::watch` (`PumpWatch`, `Grew`, `PUMP_WARNING`), `PumpStats::timing` (B, P and W now), `daemon::media::Media::engine_stats`.
- Docs, for every consumer of a converting dlink device (app rate unlike the network's): debug builds and tests must optimise the converter. Add to the consumer's own workspace `Cargo.toml` `[profile.dev.package.rubato] opt-level = 3` and `[profile.dev.package.dstack-resample] opt-level = 3` (Cargo reads profiles only from the top workspace; dstack's own carries both since v0.6.0). intercom's debug tests (a 48 kHz app on a 96 kHz device, block 1.33 ms) overran every block (1514 of 1514, longest 2839 µs) and broke the tone; with rubato at opt-level 3, 0 overruns and 141 µs at most (dstack #1). See dlink's crate docs, "Debug builds with a rate converter".
- No latency change.

## 0.6.24 (unreleased)

- Docs: `.claude/rules/release.md` notes how to recover a PR whose head did not move after a GitHub 500 on push.
- **Latency change** for every dlink app (dstack #1; intercom's report against v0.6.22, and the RX-only sizing of an obs input). Two changes to `dlink::split`, the budget's split:
  - The pump margin W takes a share of the budget: 2/5 of it (4 ms of 10 ms, 8 ms of 20 ms), at least the block and 2 ms. It was the block and at least 2 ms since v0.6.22 (half the latency, 5 ms, before), whatever the budget. W is how late the pump may run: a pump more than W behind starts its schedule again and the skipped blocks are lost, as TX holes on the sending side and as a gap in the app's RX on the receiving side. intercom saw those gaps on a loaded PC with v0.6.22 and v0.6.23 (a 96 kHz TX into a 96 kHz RX with a 48 kHz app), also at a 20 ms budget. On a device with TX, W is in the TX path, and with a rate converter it gives way to keep P at its 4 ms floor (down to its own least). An RX-only device keeps the whole share: W is not in its path.
  - P is sized by the directions the device has: RX channels only, the budget less the RX work (B + the converter's RX delay); TX channels only, less the TX work (W + the converter's TX delay); both (or no channels), the larger, as before. Before, a one-direction device paid for a path it never runs: an RX-only 48 kHz app on a 96 kHz device got P 5.17 ms from the converter's TX delay.
  - Before (v0.6.22 and v0.6.23) and after, at the default 10 ms and block 128. P is what the device's RX plays out at and its TX channels advertise; a TX-only device now splits as one with both directions in every row (its TX work is the larger), and so does a one-direction device before:

    | Device | App | Directions | W before | W after | P before | P after | TX path before -> after | RX path before -> after |
    |---|---|---|---|---|---|---|---|---|
    | 48 kHz | 48 kHz | both, TX only | 2.67 ms | 4 ms | 7.33 ms | 6 ms | 10 -> 10 ms | 10 -> 8.67 ms |
    | 48 kHz | 48 kHz | RX only | 2.67 ms | 4 ms | 7.33 ms | 7.33 ms | none | 10 -> 10 ms |
    | 96 kHz | 96 kHz | both, TX only | 2 ms | 4 ms | 8 ms | 6 ms | 10 -> 10 ms | 9.33 -> 7.33 ms |
    | 96 kHz | 96 kHz | RX only | 2 ms | 4 ms | 8 ms | 8.67 ms | none | 9.33 -> 10 ms |
    | 96 kHz | 48 kHz | both, TX only | 2 ms | 3.16 ms | 5.17 ms | 4.01 ms | 10 -> 10 ms | 7.83 -> 6.68 ms |
    | 96 kHz | 48 kHz | RX only | 2 ms | 4 ms | 5.17 ms | 7.33 ms | none | 7.83 -> 10 ms |
    | 96 kHz | 44.1 kHz | both, TX only | 2 ms | 2.91 ms | 4.92 ms | 4.01 ms | 9.99 -> 9.99 ms | 7.57 -> 6.66 ms |
    | 96 kHz | 44.1 kHz | RX only | 2 ms | 4 ms | 4.92 ms | 7.34 ms | none | 7.57 -> 9.99 ms |
    | 48 kHz | 44.1 kHz | both, TX only | 2.67 ms | 2.90 ms | 4.25 ms | 4.02 ms | 9.99 -> 9.99 ms | 9.57 -> 9.34 ms |
    | 48 kHz | 44.1 kHz | RX only | 2.67 ms | 4 ms | 4.25 ms | 4.67 ms | none | 9.57 -> 9.99 ms |
    | 48 kHz | 96 kHz | both, TX only | 2.67 ms | 4 ms | 4.67 ms | 4.58 ms | 8.75 -> 10 ms | 10 -> 9.92 ms |
    | 48 kHz | 96 kHz | RX only | 2.67 ms | 4 ms | 4.67 ms | 4.67 ms | none | 10 -> 10 ms |

    Exact P after, ns: 6000000 (48/48), 6000000 (96/96), 4010416 (96/48 and 96/44.1), 4020833 (48/44.1), 4583333 (48/96); RX only 7333333 (48/48, 96/48), 8666666 (96/96), 7343750 (96/44.1), 4666666 (48/44.1, 48/96).
  - At a 20 ms budget W grows to 8 ms: 48 kHz and 96 kHz devices go from W 2.67 / 2 ms and P 17.33 / 18 ms to W 8 ms and P 12 ms; a 48 kHz app on a 96 kHz device from W 2 ms and P 15.17 ms to W 8 ms and P 9.17 ms (RX only: P 17.33 ms).
  - Block 256 at 48 kHz (W is the block, 5.33 ms, over its share) is unchanged: P 4.67 ms. A budget that leaves P under 4 ms is refused as before; the floor (`MIN_LATENCY_NS`) and its problem text are unchanged. A latency Dante Controller set on the device still wins.
  - What a consumer re-measures: an app that aligns by the device latency (camera-box's A/V by measurement, SongPlayer's ledger) moves to the P above for its rates and directions. Pinned test numbers move: 48/48 P 352 -> 288 samples, W 128 -> 192 (pump offset 608 unchanged); 96/96 P 768 -> 576, W 192 -> 384 (offset 1088 unchanged); a 48 kHz app on a 96 kHz device with both directions P 496 -> 385, W 192 -> 303 (offset 816 unchanged), RX only P 496 -> 704, W 192 -> 384 (offset 816 -> 1216).
  - Port: `dlink::split(budget_ns, device_rate, app_rate, block)` takes a fifth argument, `directions: Directions` (`Directions::Both` is the old sizing of P). `dlink::work_margin(rate, block)` is `work_margin(budget_ns, rate, block)` (the share before the TX cap); `dlink::least_work_margin(rate, block)` is the old value (the block, at least 2 ms). New: `dlink::Directions` (`Both`, `Rx`, `Tx`; `Directions::of(rx, tx)`), `Config::directions`, `WORK_SHARE_NUM` / `WORK_SHARE_DEN`. `Config::split`, `Config::device` and `Link::start` use the config's own channels. The start log's split line has a new field, `directions`. `Split::tx_path_ns` / `rx_path_ns` of the direction a device lacks may pass the budget (that path carries no audio).

## 0.6.23 (unreleased)

- **Changed default** (dstack #1, the owner's ROZHODNUTÉ of 4.11.2026): every device of ours follows Dante PTP in its own process by default, the way Dante Virtual Soundcard does, so its audio does not hang on the PC's dsync service. The service serves the rest of the system on the same grandmaster and stays the follower's fallback.
  - `host::ClockSource` implements `Default` (`Linked`): `host::prepare`, `embed::Embedded::start` and the `dstack-dev` daemon take it (they took `Dsync`, the service only). dlink already did.
  - The daemon's `--clock` is `linked` (the default), `dsync` (the service only, `Command::RunOnService`) or `system` (tests).
  - What a consumer notices: the start line says `clock="linked (dsync's follower, else the service)"`. The follower needs PTP on UDP 319/320: on Linux the port-319 bind needs `CAP_NET_BIND_SERVICE` (or root), on Windows Npcap. Without it the device warns "dsync's in-process follower did not start" and runs on the service, as before. A host that wants the service only passes `ClockSource::Dsync` (`prepare_with`, `Embedded::start_with`) or `--clock dsync`.
  - No latency change.
- Fix (dstack #1, the owner's ruling): a TX packet is counted late only against the latency its receiver really plays at, never against a guess. Before, the TX side counted `over_latency` and warned "TX packets left later than the advertised latency: receivers count them late" against our advertised latency (or the packet's floor), the lowest a receiver could use: an ASIO host's flow to a Dante Virtual Soundcard playing at 10 ms ("High") was warned late for packets aged 1.0 to 1.15 ms, all on time for it.
  - How: each TX flow's receiver is asked its latency with the standard settings read (ARC 0x1100, `arc::control::settings`, as dcontroller reads a device's latency), at the ARC its `_netaudio-arc` record announces, only when that record announces the flow's address among its addresses (a PC with several network cards announces one per card); read-only. One reader thread, one exchange at a time, a device several flows go to read once, off the TX thread: when the flow starts, again every 30 s, a failed read after the usual backoff (9 to 72 s). The threshold is the receiver's latency, at least our advertised latency (a Dante receiver plays a flow at no less) and the packet's floor.
  - Unknown (no record, no reply, not read yet): no packet is counted late and the late watch stays quiet. Age stats (`age_max_us`, `late_bins`, `late_max_us`) are plain age, as before. The send retry window still uses the advertised floor.
  - What a consumer notices: a TX lane in the status has a new field `receiver_latency_us` after `over_latency` (0: unknown); a test that pins a TX lane's exact JSON adds it. The late warning is now "TX packets older than the latency their receiver plays at: it counts them late", with `receiver_latency_us`; a log reader matching the old text moves to the new one. A new info line "a TX flow's receiver states its latency" (slot, latency_us) when a receiver's latency is read or changes.
  - New: `flows::receivers` (`Reads`, `Job`, `Outcome`, `Unread`, `latency_of`, `read_latency`, `look`, `spawn`, `Reader`, `REREAD_NS`, `IDLE`); `TxShared::set_receiver_latency`, `receiver_latency_ns`, `receiver_flows`; `LaneCounts::receiver_latency_ns`; `subscribe::directory::ArcRecords` (`arc_at`), `ArcDirectory::records`, `Book::arc_at`, `ArcMiss`, `MAX_ARC_ADDRESSES`.
  - A TX-only device (no subscriptions) now starts its `_netaudio-arc` and `_netaudio-dbc` mDNS browses (and the directory reader, idle) once its first TX flow starts: that is where a receiver's ARC port comes from. It only listens, as for an RX lookup.
  - No latency change: nothing an application plays or sends moves (dlink's budget, split, block and converter delay are as in 0.6.22); only what is counted and logged as late.

## 0.6.22 (unreleased)

- **Latency change** (dstack #68, the owner's ROZHODNUTÉ of 3.11.2026): dlink's 10 ms is now the app's whole latency each way, the work (block, pump margin, rate converter) and the Dante latency together; dlink chooses the split. Every dlink app's path drops from about 15 to 19 ms to about 10 ms, and its advertised Dante latency (what its TX channels advertise and its RX plays out at) drops from 10 ms to the split's P: 7.33 ms on a 48 kHz device, 8 ms on a 96 kHz device, 5.17 ms for a 48 kHz app on a 96 kHz device, 4.92 ms for a 44.1 kHz app there (block 128). A receiver set to its own longer latency adds the difference, as before. An app that aligns its audio by the device latency (SongPlayer's ledger) moves its figure; a latency Dante Controller set on the device is kept in its state file and still wins.
  - How: P = the budget less the larger direction's work, in whole samples at the device's rate. TX = W + P + the converter's TX delay, RX = P + B + its RX delay (`dstack::stream`: block k runs at media time kB + B + P, its output goes out stamped W later). The pump margin W is the block and at least 2 ms, no longer half the latency (5 ms at 10 ms). P stays at least 4 ms (`MIN_LATENCY_NS`, the normal profile's floor); a budget that leaves less (block 256 with a conversion on a 48 kHz device; an 8 kHz app at 10 ms) is refused by `Config::device` with the other problems, first. The crate docs have the table of splits. A whole-device test sends a step through our own flow and checks the loop against the split.
  - Port: `Config::latency_ns(ns)` is gone; `Config::budget_ns(ns)` sets the budget (`BUDGET_NS`, 10 ms, replaces `LATENCY_NS`). An app that set a latency sets a budget instead; the same number now gives a shorter Dante latency. `work_margin(latency_ns, rate, block)` is `work_margin(rate, block)`. A test that pinned P 480 / W 240 at 48 kHz (offset 848) or P 960 / W 480 at 96 kHz (offset 1568) moves to 352 / 128 (608) and, for a 48 kHz app on a 96 kHz device, 496 / 192 (816).
  - New: `dlink::split`, `dlink::Split` (`latency_ns`, `margin`, `tx_path_ns`, `rx_path_ns`), `Config::split`, `MIN_LATENCY_NS`, `BUDGET_NS`; `dlink::rated::delay_ns(device_rate, app_rate, block)`, the converter's delay each way before a `Rated` exists (`Rated::delay_ns` uses the same formula). The start log has the split ("dlink: the latency budget's split").
- New (dstack #1, the FOH PC's sends waiting below Winsock): a host application can hand its device its own kernel send path on Windows. dstack writes whole Ethernet II frames (our NIC's MAC, the receiver's MAC from the Windows neighbour table, IPv4 with DSCP EF and UDP) and hands every frame due in a TX pass to the host's sender in one call. Without a sender, and on Linux, TX is as before.
  - API: `media::bypass::FrameSender` (`send(&self, frames: &[&[u8]], sent: &mut [bool]) -> io::Result<()>`: the host marks each frame it took in the caller's `sent`; `Err` refuses the whole batch). `Media::set_frame_sender(Arc<dyn FrameSender>) -> bool`, before the media threads start (Windows only; `false` elsewhere, with no adapter for the address, or when one is set already). dlink: `Config::frame_sender(Arc<dyn FrameSender>)`. embed: `Embedded::start_with_sender(.., Option<Arc<dyn FrameSender>>)`.
  - Which flows: a unicast receiver whose MAC the neighbour table holds (GetIpNetTable2, read once a second off the TX thread) goes to the sender; multicast and unresolved receivers stay on the UDP socket. A frame the sender did not take, and every frame of a batch it refused whole, goes out on the UDP socket at once, with the retries of v0.6.21 and their `send_retries`/`send_errors`.
  - What a consumer notices: a TX lane's `direct` counts the frames the sender took (as it counts AF_PACKET sends on Linux). The TX thread's start line names the path: "TX path: host frame sender on <adapter>", "AF_PACKET past the qdisc" or "UDP socket". A sender refusing whole batches is warned once a second, "TX host frame sender refused whole batches: their packets went through the UDP socket", with `batches` and `os_error`; never per packet.
  - Also new: `media::bypass::write_ethernet`, `ETHERNET_HEADER`, `FRAME_HEADROOM`, `Neighbour`, `neighbour_row`, `neighbour_mac`, `neighbour_mac_words`, `parse_mac`; `media::tx::host` (`HostNic`, `TxPath`, `BATCH_FRAMES`, `FRAME_MAX`); `TxShared::use_frame_sender`, `set_neighbours`, `tx_path`, `host_refusals`; `media::late::HostWatch`.
- Docs: `docs/windows-box.md` has the host's send path: who provides it, how it is set, what stays on the UDP socket, and what to read.

## 0.6.21 (2026-11-03)

- Fix (dstack #1, the FOH PC's WSAENOBUFS bursts): the TX thread sends again a packet the NIC refused for a moment, instead of losing it, when the refusal ends within the retry window (about 300 µs); a longer refusal still loses packets. The result on the FOH PC is still to be measured (dasio, after the owner's go-ahead). dasio's FOH logs (dstack 0.6.18) showed about 32 bursts a day of `TX sends failed` with os_error 10055 on both of the PC's devices at once, and their receivers saw holes of 16 to 112 frames then.
  - Retried: only a transient refusal, WSAENOBUFS (10055) and WSAEWOULDBLOCK (10035) on Windows, ENOBUFS (105), EAGAIN (11) and ENOMEM (12) on Linux; at most 3 times, 100 µs apart (a yielding spin on the TX thread); only while the packet's age after the wait stays 100 µs under its flow's latency floor (`flow_latency_ns`). Any other error, and a packet that would arrive late, fails at once as before.
  - What a consumer notices: a TX lane in the status has a new field `send_retries` after `send_errors` (packets that went out on a retry); `send_errors` now counts only the packets still refused after their retries. A test that pins a TX lane's exact JSON adds the field. A refused packet can hold the TX thread up to 300 µs plus its sends; a pass with several refused packets at most until its packets near their latency floor, which the age bound keeps (the other flows' sends would be refused meanwhile anyway). `send_max_us` stays the longest single try; the lateness and age count the retries.
  - The log: once a second, a flow that lost packets after their retries warns "TX sends failed after their retries" with `errors`, `retried` and `os_error` (was "TX sends failed"); a flow whose refused packets all went out on a retry logs at info "TX sends refused for a moment went out on a retry" with `retried` and `os_error`. A log reader matching the old warning text moves to the new one.
  - New: `media::tx::send_retry`, `transient_refusal`, `SEND_RETRIES`, `SEND_RETRY_WAIT_NS`, `SEND_RETRY_HEADROOM_NS`; `LaneCounts::send_retries`; `media::late::SendErrors::retried` and `SendErrors::lost`. Changed: `media::late::SendWatch::look`'s counts give `(errors, retries, last OS error)`.
- dtray: refused sends that grew since the last update (`tx[].send_errors`, summed) turn the device orange, "send errors +N" (after the late counts), and its line ends with `; send errors +N`. Retries are not shown. `model::Counts` has `send_errors`.
- Docs: `docs/windows-box.md` has the WSAENOBUFS bursts on a NIC that carries other traffic, the retry, and keeping bulk traffic off the Dante NIC as the box owner's call.

## 0.6.20 (2026-11-03)

- Change (dstack #90, the dlink pump on a busy Windows PC): dlink's default pump block is 128 frames (1.33 ms at 96 kHz, 2.67 ms at 48 kHz), no longer the stack's 32. `dlink::Config::device()` now writes `block`; the pump wakes a quarter as often and `Rated` converts four times the frames a call (it follows the device block). The path grows by 96 frames (+1 ms on a 96 kHz device, +2 ms on a 48 kHz one). What a consumer notices: an app's `process` gets 128 frames a block at the device's rate (64 at 48 kHz on a 96 kHz network through `Rated`), and the pump's offset is B + P + W with B 128. A test that pinned 32-frame blocks or their app frames sets `.block(32)` or moves its expectation.
- New (dlink): `Config::block(n)` sets the pump's block, one of `stream::BLOCKS` (16, 32, 64, 128, 256); another is refused by `Config::device()` with the other problems ("block: 16, 32, 64, 128 or 256 frames"). `dlink::BLOCK` (128) is the default, `dlink::work_margin` the margin rule: W stays half the latency, now at least the block (4 ms at 48 kHz gives 128, no longer 96).
- Change (dstack #90): under the normal timing profile on Windows the pump, TX and RX threads ask `THREAD_PRIORITY_HIGHEST` within the process's normal class (no privilege, nothing process-wide, nothing asked of the app); the log says so per thread, and a refusal is warned and the thread runs on at normal priority. Linux's normal profile and the low-latency profile are unchanged. New: `clock::thread_priority_for(profile, os)`, `clock::ThreadPriority` (`Realtime`, `Highest`, `Normal`), `clock::Os`, `media::tx::windows_priority_for`. Consumers port nothing.
- Docs (dtray): `dtray.exe` is a GUI-subsystem program, so an installer runs `Start-Process dtray.exe -ArgumentList '--autostart','on' -Wait -PassThru` and reads `ExitCode` (PowerShell's `&` neither waits nor sets `$LASTEXITCODE`; dasio's note).
- Docs: `docs/windows-box.md` has the normal profile's thread priority and block, from SongPlayer's Dante output ledger of 3.11.2026.

## 0.6.19 (2026-11-03)

- Change (dstack #104 step 5): the app's status hook (`Media::set_app_status`, `dlink::Link::set_app_status`) runs on a thread of its own (`dstack-app-status`, started at the first call, ended with the `Media`), never inside a status request. A slow or stalled hook no longer delays the device's ARC, CMC or status answers.
  - The thread calls the hook at most once a second, and only while the status is read (a read within 5 s): nobody reading means no calls (`daemon::media::app::REFRESH`, `READ_WINDOW`).
  - The status carries the last part the hook returned. What a consumer notices: the first status after the hook is set (or after a pause in reading) has no `app` yet, or a stale one; a part over 3 s old carries `"stale":true` (`app::STALE`, set over any `stale` key of the app's own), and a hook still running then is warned once. A test that set a hook and read the status once must now read until `app` appears.
  - `f` must not make a status request of its own (`ctl::status`, `Link::status`): each counts as a read and keeps `f` called for good.
  - A panic in `f` no longer reaches the status request: the last part stays (stale in time), the panic is warned once, and `f` is called again at the next refresh. A hook replaced while its call stalls takes over once that call returns.
  - New: `daemon::media::app` (`AppHook`, `next`, `Next`, `is_stale`, `mark_stale`); `AppStatus` moved there (still re-exported from `daemon::media`).
- dtray: a stale app part (`"stale":true`) turns the device orange, "app status stale" (after the app's own warning, before the counts); `model::App` has `stale`. `dtray --print` reads each device twice, 0.5 s apart, so the app part (answered on the hook's thread after the first read) is in its output.
- New (dtray, Linux, #104 step 5): the tray on Linux, the same icon and menu as on Windows (device submenus with the latency values, the expected devices, Copy status to the GTK clipboard, Exit), through `tray-icon`'s GTK backend (AppIndicator). `dtray` without `--print` starts it; one tray a user session (a lock on `$XDG_RUNTIME_DIR/dtray.lock`, a second start leaves at once). Without a desktop it exits 1 at once ("no desktop to show the tray on (GTK)"), no longer 2.
  - `dtray --autostart on|off` on Linux writes or removes the XDG autostart entry `~/.config/autostart/dtray.desktop` (`$XDG_CONFIG_HOME` when set), no longer a usage error.
  - dtray library: the `desktop` module (`lock_path`, `autostart_file`, `exec_arg`, `desktop_entry`); `menu` is the tray menu's decisions on both systems.
  - Building dtray on Linux needs `libgtk-3-dev` and `libayatana-appindicator3-dev` (CI installs them). The Linux `dtray` binary links GTK 3, so every mode (`--print` and `--autostart` too) needs `libgtk-3-0` installed; the tray itself also needs libayatana-appindicator3 (or libappindicator3) and a panel that shows AppIndicator icons.
- Dependencies (dtray, Linux): `gtk` 0.18.2 and `glib` 0.18.5 (the versions tray-icon 0.21.2 uses; already in the lockfile); `tray-icon` now without its default `libxdo` feature on both systems (unused; `libxdo`, `libxdo-sys` and `x11` leave the lockfile).

## 0.6.18 (2026-11-03)

- New (dstack #104 step 4): an app's own part of the status, for dtray.
  - `daemon::media::Media::set_app_status(f)` and `dlink::Link::set_app_status(f)`: each status request calls `f`; the status line (`ctl::status`, the local endpoint, `Link::status`) ends with `"app":<object>` when `f` returns one JSON object of at most 4096 bytes compact (`ctl::APP_MAX`; written compact, so the line stays one line). Anything else is left out, with one warning per hook. A later call replaces the hook. `f` runs inside the device's control task (no ARC or status answer while it runs): read values the app holds, never I/O, a wait or a status request. Without a hook the status is as before. `ctl::app_object` is the check.
  - The keys dtray reads: `line` (a short text after the device's line) and `state` (`"ok"`, `"warn"`: orange, `"error"`: red). Other keys are the app's own; dtray shows them in Copy status only. Unlike the rest of the status, this part may carry names.
- New (dtray): expected devices. An installer writes one empty file per device it installs, `<device name>.device`, in `%ProgramData%\dstack\devices\` (Windows) or `/etc/dstack/devices/` (Linux), and its uninstaller removes it (`DSTACK_DEVICES_DIR` names another folder). An expected device that discovery does not find shows as "<name>: expected, not running" and turns the icon red.
- New (dtray, Windows): each device's menu line is a submenu with its latency: Dante's 0.25/0.5/1/2/5/10 ms within the device's ARC range, the configured one checked; choosing one writes it over ARC (`arc::control::set_latency`). dtray now writes this one setting; it still never touches a device that is not ours on this PC.
- New (dtray, Windows): `dtray --autostart on|off` sets or removes the user's Run value `dtray` (the exe path, quoted). On Linux it is a usage error (exit 2).
- dtray library: `discover::Local` has the device's `arc` address; `discover::read`; `model::Status::app` (`App`, `AppState`, `app_of`), `Tracker::update_with`, `missing`; the `expected` and `latency` modules; `Mode::Autostart`, `run_value` (`None` for a path that is not Unicode); `menu` (the Windows menu's decisions: `Known`, `Shape`, `needs_rebuild`, `write_needed`); the app line is one line of at most 64 units (`model::app_line`).

## 0.6.17 (2026-11-03)

- New (dstack #104 step 3): the `dtray` crate, the PC's tray icon for its dstack devices (dlink apps, dasio, dstack-dev). No app changes for it: it finds the devices by mDNS on the PC's own addresses (the ARC records that carry `dstack_ctl`) and reads each one's local status endpoint once a second.
  - The icon is grey with no device, red when a status cannot be read or a clock is not ready, orange when a subscribed RX channel is not connected or a late, missed or overrun count grew since the last second, green otherwise. The tooltip names the worst device and why.
  - The menu (Windows): a line per device (clock, RX subscriptions connected, the counts' growth), Copy status (every device's status JSON), Exit (the tray only).
  - `dtray --print` on every platform: one 3 s discovery pass, the tooltip and the lines on stdout. On Linux the tray itself is not there yet (`dtray` alone exits 2).
  - The library is pure and tested: `discover` (the filter, `Scanner`), `probe`, `model` (`Tracker`, `View`, `Colour`), `icon`.
- Dependencies: `tray-icon` 0.21.2 (Windows only, dtray). Lockfile: `crc32fast` held at 1.5.2 (1.5.3 and 1.5.4 are under two weeks old).
- Release: the GitHub release also carries `dtray` for Linux and Windows (`dtray-<tag>-<os>`), with its sha256.

## 0.6.16 (2026-11-03)

- Fix (dasio, FOH 3.11.2026): a flow request whose packet is longer than our TX latency is granted (up to one datagram). master-asio-dante at a 0.5 ms TX latency refused DVS's fpp 60 ("more frames a packet than our latency covers"), and DVS showed Transmitter setup failed, though DVS plays at its own 10 ms. A receiver that asks past the advertised `tx_fpp_max` holds the packet in its own latency; one that plays at ours never asks it. The "TX flow started" line logs `receiver_latency_from_us`.
  - TX counts such a flow's packets as `over_latency` only past fpp + margin + the send slack (`media::tx::flow_latency_ns`), not against our advertised latency.

## 0.6.15 (2026-11-03)

- Fix (dstack #90, songplayer's port onto v0.6.14: 11 of its own tests failed on three laws that differed from its asrc_servo.rs):
  - Priming: with `with_priming`, ANY first observation primes (`Step::Prime`, an insert under the target, a skip over it, 0 at it), and it is no part of the first window. It primed only under the target before.
  - Stall: a window is judged by its OWN first and last rate points (more than `MAX_SANE_WINDOW_PPM` apart), so the first window can be one too. A stalled window no longer resets the slew's clock: the next window moves by the whole time since the last applied one. It compared consecutive window means before.
  - `played_silence`: only what passes the HIGHEST count seen folds into the cushion. A count that went back made its later rise count again before.

## 0.6.14 (2026-11-03)

- New (dstack #90 step 3, songplayer's servo rules): `asrc::Controller` takes SongPlayer's additions, all opt-in, so existing users (vstack) are unchanged:
  - the underrun cushion: `with_cushion_max_s`, `played_silence(total_s)`, `cushion_s()`. Silence the consumer played is kept as an excess up to the cap. The offset is braked against [target, target + cushion] (`band_error_ms`), while the level loop keeps the target. Each window the cushion follows the mean down, but a window that folded new silence keeps it; a re-centre or the priming drops it;
  - the calm zone as a parameter: `with_calm_zone_ms`, `calm_zone_ms_for(callback_s)` (half the callback period, at least 1 ms);
  - priming: `with_priming()`, `restart()`; the first observation returns `Step::Prime { by_s, ppm }`, not counted as a re-centre;
  - a stall: a window more than `MAX_SANE_WINDOW_PPM` (100 000 ppm) from the last flushes the rate fit and holds the correction (`stalls()`, `stalled(dx, dy)`);
  - a clock that goes back before the open window's start starts it over;
  - `offset_ms()`: the last window's mean depth outside the band.
  - Port: `Step` has a new variant, `Prime`; an exhaustive `match` on `Step` needs its arm (it only comes with `with_priming`).
- Changed: `Converter::set_ppm` moves the ratio within ±`ROOM_PPM` (1000 ppm, rubato's 1.001), no longer ±300; `asrc::Controller` still keeps its own correction within ±`asrc::MAX_PPM` (300).
- Docs: the crate's measured THD+N, image and fold figures, and its delays at 48↔96.

## 0.6.13 (2026-11-03)

- New (dstack #104, dtray): every device (dstack-dev, dlink, dasio's) opens a local status endpoint on 127.0.0.1 at a port the OS picks, and announces it in its ARC record as `dstack_ctl=<port>` (`announce::STATUS_PORT_KEY`). It answers `status` (the same JSON line as `ctl status`) and nothing else, on Windows too; the test requests stay on the Unix socket. New: `ctl::local`, `device::Device::status_port`, `announce::with_status_port`.
- Test: the RX flow list's byte test also reads its reply with `arc::control`'s parser (dcontroller's).

## 0.6.12 (2026-11-03)

- License (dstack #102, owner 3.11.2026: as permissive as possible): `dstack` and `dstack-dev` are MIT OR Apache-2.0 too, so every crate of the workspace is (`LICENSE.md`). The functions an audit found structured after inferno's code were rewritten in our own structure, with the bytes on the wire unchanged: ARC's paging, the flow request's build and parse, the info change and heartbeat bodies, the mDNS channel records and the flow-control client's exchange. Quoted comments were reworded.
  - Changed: the ARC TX and RX flow lists (0x2200, 0x3200) place their descriptors in another order behind the table; every offset is stated, so a reader that follows offsets (Dante Controller, `arc::control`) reads the same flows. Verified live with Dante Controller.
  - Renamed: `info::Multicaster` is `info::InfoBuilder`.
- New (dstack #102, #104, songplayer): `dlink::Link::status()`, the device's numbers as one JSON line (what `dstack-dev ctl status` prints). An app reads it from dlink instead of calling `dstack::ctl` itself.

## 0.6.11 (2026-11-03)

- License (owner 3.11.2026: as permissive as possible): `dlink`, `dstack-audio-io` and `dstack-resample` are MIT OR Apache-2.0 (`LICENSE-MIT`, `LICENSE-APACHE`, `LICENSE.md`). `dstack` stays GPL-3.0-or-later for now: some of its files began from inferno (GPL-3.0-or-later), and those must become our own implementation first. Its GPL text is now in `crates/dstack/COPYING`.

## 0.6.10 (2026-11-03)

- New (dstack #68, dsync 1.27.3): the in-process follower starts from the dsync service's frequency word, and the linked clock keeps a ready service until the follower settles.
  - dsync-ptp moves to v1.27.3 (frequency seed, settled state, continuous boot re-seed).
  - The seed is read once at the start from the service (DSYX bytes 36-39, ppb), only when the service is locked and settled (`clock::linked::seed_of`, `service_seed`). On mbc: settled in 37 s with the seed (|diff| p99 5.1 µs, max 31 µs) vs 70 s without (p99 51.4 µs).
  - The selector takes the follower over a ready service only once it is settled; once in use, losing `settled` alone does not switch back (a switch costs a re-align). With no service, a locked follower is taken as before.

## 0.6.9 (2026-11-03)

- New (dstack #90, obs): `dlink::Rated::delay_ns()`, the converters' delay each way (RX, TX with the FIFO prime). The rated.rs doc had RX and TX swapped; on a 96 kHz network: 48 kHz app RX 1.33 ms, TX 2.83 ms; 44.1 kHz RX 1.32 ms, TX 3.08 ms. v0.5.20's 2:1 half-band delayed about 0.3 ms each way.

## 0.6.8 (2026-11-03)

- Fix (obs, camera-box#1485): an RX ring now counts a sample late when the reader passed its position while the write ran.
  - Before, `Ring::write_at` checked the reader once before its loop. With the RX thread and the reader on one CPU, the reader read holes and the writer then stored those samples as `written`: silence reached the app with `late` at 0.
  - Now a read that finds a hole marks the slot by compare-and-swap, and the writer stores by compare-and-swap, so that sample is `late`. Ring `holes` at written positions now equal `late`.
  - New: `ring::READ_EMPTY`. The ring's lap is 31 bits (positions 2^47 apart, 46 years at 96 kHz, share a slot; was 2^48).
  - An unwritten slot is its own word, never a lap: a read marks it in every lap (laps 2^30 to 2^31 - 2 left it unmarked), and in the lap 2^31 - 1 it reads as a hole, not a sample 0.
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

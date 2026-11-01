---
paths:
  - "crates/dante-stack/tests/**"
  - "crates/**/tests.rs"
  - "crates/**/tests/**"
  - ".config/nextest.toml"
  - ".cargo/mutants.toml"
---

# Tests in dstack

- **Whole-device tests** (`tests/system_clock.rs`): the host refuses loopback (no MAC, unreachable for Dante), so run the device on the box's routed IPv4 (`UdpSocket::connect` to 192.0.2.1:9, then `local_addr`; sends nothing). Each test binds the Dante ports on that address: take the file's `ONE_DEVICE` lock.
- **Timing on CI and dev boxes:** a normal-priority TX thread wakes over 1 ms late now and then; at the test config's 1 ms latency that restarts the TX schedule (a 2 to 5 ms RX dropout). Whole-path tests use a 10 ms latency, take their counts after the start (0.1 s of RX read), and require no restart and every packet within the latency; real-time lateness is the hardware gate's, never a shared runner's.
- **Windows CI runs every test:** paths must be absolute on Windows too (`std::env::temp_dir()`), not `/run/...`.
- **Async inputs:** the directory's ARC and DBC records come from separate browse threads; wait for every record a lookup needs before it (a lookup without them registers no want).
- **Mutation gate:** per mutant only the unit tests and `tests/hot_path.rs` run (`.cargo/mutants.toml`); a whole-device test cannot run per mutant (ports). A test bound inside a step must carry the step's own time (`Instant` around `step`), not a fixed bound. Host glue that only calls `daemon::serve` (`host::serve_device`) is covered by the whole-device test alone: any edit inside it, a comment too, puts its `Ok(())` mutant in the diff and fails the gate. Leave such functions out of cosmetic changes.
- **Fake time vs real time:** TX lateness adds real time inside a step to the fake PTP time; bound it by the measured step duration.
- **Captured logs:** a test that captures logs with `tracing::subscriber::with_default` calls `crate::test_log::undecided()` first. Without it, tracing's callsite interest cache can keep "never" for a callsite another test thread hit first, and the capture comes back empty about 1 run in 4.
- **Equivalent mutants:** combine disjoint bit fields with `from_be_bytes`, not `(a << 16) | b`. cargo-mutants turns `|` into `^`, which gives the same value, so the mutant cannot be killed.
- **Live tests on dev1's Dante NIC** (`dante-net`; the address is in the private config): run the device with `--ip <that address>` and a private config, write its pid to a file, and stop it with `kill -INT <pid>`, which sends the goodbyes. Never use `pkill -f <pattern>`: it also matches the shell running the command and kills it. The ctl `status` request counts show which ARC and 8700 opcodes Dante Controller sends; the once-a-minute "not answered" warning lists the gaps.
- **Unreachable guards:** a run-time check that can never be false, given what the code before it already limits, is a mutant no test can kill. Prove the bound once with `const _: () = assert!(...)` and drop the check (example: the fpp step in `subscribe.rs`: 9 + 8 * 4 * 32 <= 1472).
- **Windows-only code and the gate:** the mutation gate runs on Linux, so mutants in `cfg(windows)` code can never be caught. `clock/sys_windows.rs` is excluded in `.cargo/mutants.toml`. Keep Windows logic that matters in cross-platform functions with tests (e.g. `clock::wait_mode_for`, `clock::realtime_for`), and leave the `cfg(windows)` part as thin glue.
- **Latency verdict = a real receiver's records, never our counters** (owner, 31.10.2026). Dante heartbeats (224.0.0.233:8708) carry per RX flow slot: record 0x8003 = that second's peak lateness in samples, 0x8004 = cumulative late count (header: count, 0, offset of the values). Map slot to transmitter with ARC 0x3000 + 0x3200 (page body `0,1,start,0,0`). Probe: `~/.claude/work-products/dstack/rxlat.py <our ip> <receiver ip> <s>` (exit 1 on late or peak over latency). Every gen3 fault tonight (deep C-states, TX/RX same FIFO priority on one core, fq_codel qdisc) was invisible to our counters; checklist in `docs/linux-box.md`; Windows (FOH PC, 1.11: spinning threads on the NIC's RSS CPUs, a NIC shared with bulk TCP) in `docs/windows-box.md`.
- **The IEM card is shared with iemmixer's box:** use only card RX 104/105, tell iemmixer before subscribing, unsubscribe and stop the dev1 device after; a second sender on the card spoils its measurements.
- **Constant bounds:** clippy refuses `assert!` on constants inside a test; put `const _: () = assert!(...)` next to the constant.

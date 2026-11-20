---
paths:
  - "crates/dstack/src/recv.rs"
  - "crates/dstack/src/recv/**"
  - "crates/dstack/tests/recv_under_load.rs"
  - "crates/dstack/src/discover.rs"
  - "crates/dstack/src/subscribe/instance.rs"
  - "crates/dstack/src/info/client.rs"
  - "crates/dstack/src/arc/client.rs"
  - "crates/dstack/src/latency.rs"
  - "crates/dstack/src/meter.rs"
  - "crates/dstack/src/announce/responder.rs"
  - "crates/dstack/src/health.rs"
  - "crates/dstack/src/ctl/probe.rs"
  - "crates/dstack/src/daemon/cli.rs"
---

# UDP reads with a wait

- **A UDP loop that waits reads through `dstack::recv`, never `set_read_timeout` + `recv_from`** (#454). `recv::within` / `recv_from_within` / `recv_within` give `Ok(None)` when nothing came within the wait (no error kind to classify) and on Windows wait with `WSAPoll` on a non-blocking socket (`recv/windows.rs`); elsewhere the wait is still `SO_RCVTIMEO`. Why: on Windows a blocking receive that ends on `SO_RCVTIMEO` can lose the datagram completing into it as it fires. Measured on windows-2025 with 8 busy threads (probe run 44661855528, 27 rounds of 300 datagrams): 0.5 % of the 10 ms timeouts and 6 % of the 50 ms ones lost a datagram, multicast and unicast alike; a 200 ms reader (3 timeouts a round) and `WSAPoll` readers lost none. Alone the timeout reads lost nothing, so a loss shows only under load.
- **Proving a Windows-only loss:** `tests/recv_under_load.rs` brings its own load (8 busy threads at normal priority reproduce it as well as #453's PowerShell jobs) and is its own binary, so cargo runs no other test beside it; it runs on every OS (on Linux it measures that the timeout read loses nothing). It failed 15 of 15 against the timeout read (run 44663445297) and passed 15 of 15 with `WSAPoll` (run 44664370634). Loop such a test on a throwaway `wip-<N>-<k>` branch with a workflow of its own (no existing workflow runs there; `$ErrorActionPreference = "Continue"` so a failing run does not end the loop), then delete the branch.
- **`WSAPoll` ends a wait early, by under a millisecond** (run 44666301837: a 10 ms wait with nothing after 9.28 ms at the shortest, a 50 ms one after 49.58 ms): `read_when_ready` waits again for what is left and gives `None` only once the time is over, so a request's wait or a heartbeat window never ends early; the under-load test asserts every empty read took its whole wait. Count an empty-wait rate against the run's real length: a Windows run under load is long (each 0-20 ms sender sleep takes a timer tick), and 1700 empty 10 ms waits in a run read like early returns when they were not.
- **On Windows the socket stays non-blocking** after its first read through the helper, its sends included: a send there can return would-block when the send buffer is full. The loops' sends are a few small datagrams a second and each result is handled; a new loop with bulk sends on the same socket must handle would-block or send from another socket.
- **The decisions are cross-platform** (`recv::read_of`, `result_of`, `poll_ms`, `read_when_ready`, tested on Linux with scripted reads and waits); `recv/windows.rs` is excluded from the mutation gate. A loop's own decision on the helper's result stays a pure function (`latency::hand_on` takes `io::Result<Option<..>>`).
- **TCP control reads** (`health`, `ctl/probe`, `daemon/cli`) keep their read timeouts: a stream read with a deadline per request, not probed for a loss (#454 step 3).

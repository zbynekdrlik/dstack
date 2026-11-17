---
paths:
  - "crates/dstack/src/media/tx.rs"
  - "crates/dstack/src/media/tx/*.rs"
---

# The TX side (media/tx)

- **Module layout** (#360, after #300's `subscribe` split): `tx.rs` keeps the constants, `TxShared`/`Control`, `Plan`, the seqlocked `Cell`, the per-slot counts (`LaneStats`, `LaneCounts`, `count_send`) and `nanos`; `table.rs` the control side's `TxTable`/`TxFlow` (`multicast.rs` adds the multicast flows to it); `schedule.rs` `Schedule` and the time helpers (`ns_of`, `send_clock`, `flow_latency_ns`, `over_latency`); `thread.rs` the TX thread (`Lane`, `Transmitter` and its pass, `send_one`, `spawn`, the loops and the relay escape); `priority.rs` the OS thread calls (`ask_priority`, the Windows priority mapping, `place`, `pin_to_cpu`) beside `take_priority` and the MMCSS record. Every public name is re-exported at `media::tx::`, so callers and siblings (`use super::ns_of`) never name the file. A `Transmitter` field a sibling's `impl Transmitter` reads (`split.rs`, `shift.rs`) is `pub(super)`. The tests read names through `use super::*`, so a name only they use is a `#[cfg(test)]` import in `tx.rs` (`escape_through`, `run_relay`, `run_with`, `send_one`, `pin_to_cpu`; `ask_priority` only under `cfg(all(test, windows))`). A function that moves files takes its `.cargo/mutants.toml` `exclude_re` path along (`tx/thread.rs.*send_one`, `tx/priority.rs.*unthrottle`).

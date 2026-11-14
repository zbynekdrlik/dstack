---
paths:
  - "crates/dstack/src/media/ring.rs"
  - "crates/dstack/src/media/ring/**"
  - "crates/dstack/src/media/rx.rs"
  - "crates/dstack/src/media/rx/**"
  - "crates/dstack/src/media/late.rs"
  - "crates/dstack/src/media/late/**"
  - "crates/dstack/src/ctl/status.rs"
  - "crates/dstack/src/ctl/status/**"
---

# RX counters, the ring and the minute lines

- **A ring's state reset from the control side races the RX thread** (#234, `Ring::restart`). `RxShared::publish` runs on the control thread. The RX thread keeps its old routes and plan until its next `Receiver::refresh`, so it can still write an old flow's packet after the publish's reset. Reset from the writer too: `refresh`/`retarget` restart before the pass writes. The writer only lowers the state, with `fetch_min`, never a compare-then-store (that leaves an unkillable `<`/`<=` mutant).
- **A per-flow minute rise is per ring, never a per-slot sum of lifetime counters.** A channel routed into a slot brings its whole history into the slot's sum. `RxMinute::holes_rose` rises each ring against its own last look, then adds the rise to the slot the ring feeds. It reads the routes under the publish's lock (`RxShared::routes_settled`), because a publish clears a leaving route before it stores the new one.
- **Changing what a ring counts ripples** (#234, `start_holes`). About 17 tests read a never-written ring and assert `holes`: ring, rx, tx engine ring, stream, status strings, `tests/hot_path.rs`. Find them first with `grep -rn '\.holes\b\|"holes"' crates/dstack`. Assert the exact split (`start_holes: N`), never a sum that passes either way. obs's `rx_holes` is the sum of the rings' `holes` (`obs/plugins/dante-input/ffi/src/status.rs`), so name the change in the CHANGELOG for obs and camera-box.
- **A new status key** goes into `ctl/status/tests.rs`'s `WORDS` list, or every status test fails on the unknown word.
- **A field on the minute timing line:** keep the message text (`RX flow timing in the last minute: ...`) unchanged. Consumers grep it, so add fields only.
- **A total split by cause** (#281, the pump's `rebootstraps_late/_clock_back/_reanchor/_retime`): keep only the parts as atomics and derive the total as their sum in the view (`PumpStats::view`). A separate total atomic lets a watch look between the two adds see a total without its cause (`cause=none`), and the cause's rise is lost the next second.

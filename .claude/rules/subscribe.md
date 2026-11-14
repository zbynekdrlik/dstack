---
paths:
  - "crates/dstack/src/subscribe.rs"
  - "crates/dstack/src/subscribe/coarse.rs"
  - "crates/dstack/src/subscribe/publish.rs"
  - "crates/dstack/src/subscribe/request.rs"
  - "crates/dstack/src/subscribe/slots.rs"
  - "crates/dstack/src/subscribe/tests.rs"
  - "crates/dstack/src/subscribe/tests/**"
  - "crates/dstack/src/subscribe/directory/flows.rs"
---

# RX subscriptions: flows, multicast and our own unicast flow

- **Own unicast over a coarser multicast is opt-in** (#297, v0.6.133): only a device whose config SETS `arc_rx_fpp` (`DeviceConfig::arc_rx_fpp: Option<u16>`, `Manager::own_unicast`) leaves a multicast flow with more frames a packet than its unicast flow would carry. A device on the default must see zero change: the band's FOH/PA feed (a dasio device on the default) takes the same transmitter's multicast flow. A test that needs the opt-in builds its manager with `arc_rx_fpp: Some(4)` (`subscribe/tests/coarse_multicast.rs`); `tests.rs::manager()` is the default device.
- **Owner's rules for that path** (14.11.2026, "extrémne opatrne"): never write to the transmitter's multicast flow; ask only into a free TX slot (`Multicast::free_slots` = the flow list's `slots` less its flows; unknown for an ARC 2.7 list or one with an unread record, and unknown asks nothing); a refusal, no answer or a silent grant keeps the multicast flow with no other side effect (no `failures`, `unanswered` or `last_request` change, no status change); make-before-break.
- **An `upgrade` flow carries no route until it delivers** (`Manager::promote`). Every helper that reads "a flow carries this TX channel" or "a flow nobody routes to" must treat it apart: `carrying()` skips it, `close()` counts it as used while young, `answered`/`give_up` send it to `upgrade_answered`/`upgrade_failed`, and `abandoned` keeps its flag so a late answer to a given-up request counts nothing.
- **Checks that `reresolve` already makes are unreachable later in the same tick**: a changed record or a moved group is unrouted before `upgrade()` runs, so re-checking them there only leaves unkillable mutants.
- **Free-slot order in tests:** a freed flow slot is taken last (`free_slot` picks the slot freed longest ago, never-used first), so after a refusal the multicast flow joins slot 1, not 0: read the slot with `group_slot(&m)`.
- **A new `DeviceConfig` field or type change breaks iemmixer's full struct literal** (`crates/iem-engine/tests/dante_loopback.rs`): name its migration line in the CHANGELOG.

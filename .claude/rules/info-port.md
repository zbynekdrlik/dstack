---
paths:
  - "crates/dstack/src/info.rs"
  - "crates/dstack/src/info/**"
  - "crates/dstack/tests/bridge_figures.rs"
  - "crates/dstack/tests/conformance/**"
---

# The info port (8700) and dstack-own requests

- **A dstack-own request answers UNICAST to the asker, never to the info group** (#391, the bridge figures, `info::bridge`): 224.0.0.231:8702 and the heartbeat group are read by Dante Controller and every Audinate device, so a vendor packet there risks every reader for one consumer. Pick its opcode outside Audinate's message types on 8700 (seen: 0x00xx, 0x01xx, 0x10xx, 0xffxx in inferno, netaudio-core `commands/*.rs` and our captures; dstack's are 0xd5xx), keep it out of the group path (`answer` returns `None`, `answer_within` skips the bound for it) and bound it as a kind of its own.
- **Several devices on one address:** a member never sees the asker's address, so the manager relays such a request as `share::Msg::Ask(asker, ..)` and sends the member's `Msg::SendTo` from 8700. Guard that path (`share::Asks`, `is_bridge_reply`, `is_unicast`): without it any local process that registers could send any bytes from the Dante info port to any address. An older manager or member drops the new messages; the CHANGELOG says so (every app on a dasio PC must take the release).
- **Read the figures after the bound:** `bridge_within` checks the slot's source, then the bound, then reads (the source takes the runner's status lock), so a flood reads at most `PER_SPAN` a second.
- **Tests:** a fixed-layout packet still gets an independent walker (`tests/conformance/bridge_figures.rs`, its own offset table, every field's bytes distinct so a swapped offset shows); prove it bites by swapping two offsets in the writer's `at` table (our own round trip still passes, the walker tests fail). A `match` arm guard (`if from == manager`) is mutated by cargo-mutants: test the arm with a stranger's message. The manager's `select!` picks a ready arm at random: a test that sends a share message and then a request waits for the first's effect (or its absence) before sending the second.

---
paths:
  - "crates/dstack/src/membership.rs"
  - "crates/dstack/src/membership/**"
  - "crates/dstack/src/announce/responder.rs"
  - "crates/dstack/src/discover.rs"
  - "crates/dstack/src/latency.rs"
  - "crates/dstack/src/media/rx/rejoin.rs"
  - "crates/dstack/src/media/rx/socket.rs"
  - "crates/dstack/src/info/client.rs"
---

# Multicast sockets and their group membership

- **A long-lived multicast socket keeps its group only with a watch** (#282). On Windows an address removed and added again (`netsh ... set address`), often with a new interface index, drops the membership; the socket keeps working for unicast, so nothing fails loudly. Every socket that joins once and lives on holds a `membership::Membership` and calls `keep` from its own loop: the responder reopens its socket and announces, discovery and the latency listener `rejoin` (leave + join) on the same socket, RX flows are asked from the daemon's tick (`RxShared::rejoin_groups`, never the RX hot path). A socket opened per request (`info::client::group_socket`) needs none. A new long-lived owner must do the same.
- **Test the comparison, not the OS event:** `Membership::look`/`poll` take fake `Link` lists and times; the wiring of a thread is tested with an injected reader (`responder::Watch::read`). Wait in a test for the watch to have READ the state you set (count the reads), never a fixed sleep: the responder's thread blocks up to 250 ms in its receive between looks.
- **Known limit:** a same-index re-add between two 2 s reads is not seen. Windows lists an address again only once its duplicate-address check is done (`if_addrs` 0.15 filters on `IpDadStatePreferred`).
- **A bit mask built in a fresh accumulator from distinct bits:** sum the bits (`.map(|s| 1u64 << s).sum()`), not `|=`: the `|=` to `^=` mutant gives the same mask and cannot be killed (`media/rx/rejoin.rs`). Merging that mask into another one with `|=` is fine when a test makes the two overlap (a plan change and a rejoin in one pass).

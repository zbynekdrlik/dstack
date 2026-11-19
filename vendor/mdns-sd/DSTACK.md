# mdns-sd 0.21.6, vendored by dstack

Upstream: https://crates.io/crates/mdns-sd/0.21.6 (Apache-2.0 OR MIT, see
LICENSE-APACHE and LICENSE-MIT), copied unchanged except:

- `src/service_daemon.rs`, the daemon's start: no IPv6 multicast socket
  (`ipv6_sock = None`). Dante is IPv4 only; the `[::]:5353` socket received
  every interface's mDNS (dstack #1, dasio#1).
- `src/service_daemon.rs` `refresh_active_services`, `src/dns_cache.rs`
  `refresh_due_*` and `src/dns_parser.rs` `refresh_due_soon` /
  `refresh_maybe`: a refresh pass asks every due question in one message,
  and a pass with a record due takes along those due within 2 % of their
  TTL. Upstream sent one query per instance and host at exactly 80 % of
  the TTL: every 96 s each of FOH's two device processes sent about 60
  queries in one second, and the answers made a burst of over 500 packets
  in which FOH's audio TX stalled (dstack #1, dasio, 5.11.2026). Since
  dstack #319 no dstack build reaches this pass (below); its test went
  with `browse`.

- `src/service_daemon.rs`, `src/dns_cache.rs`, `src/lib.rs`: the daemon
  answers and announces only. `browse`, `browse_cache`, `stop_browse`,
  `resolve_hostname`, `stop_resolve_hostname` and `verify`, their
  `Command` variants, handlers and the helpers only they reach
  (`query_cache_for_*`, `add_hostname_resolver`, `service_verify_queries`,
  `remove_service_type`, ...) are `#[cfg(test)]`: they exist for the
  crate's own unit tests and in no build of a dependent. So are the
  refresh passes themselves (`refresh_active_services`, the A/AAAA refresh
  in `run`, the cache's `refresh_due_*`, their counters), the browse
  queries `add_interface` sends on a new interface, and `send_query` /
  `send_query_on_intf`. With no browser the resolve retries
  (`Command::Resolve`, reached only through `service_queriers`) have
  nothing to ask, so the daemon sends no query of its own (probes for its
  own names aside, RFC 6762 §8.1). `crates/dstack/src/announce.rs` holds
  `compile_fail` doctests that lock the removed calls out. An old dtray with standing browses refreshed
  every learned `_netaudio-arc` record every 96 s (80 % of 120 s); every
  Dante host answered in the same second and receivers counted late
  packets (dstack #319, 15.11.2026; `crates/dstack/tests/mdns_answer_only.rs`).
  `VERIFY_TIMEOUT_DEFAULT` is gone. The upstream integration tests that
  drive a browse (`tests/mdns_test.rs`, `tests/shutdown_test.rs`) and the
  `query` example are removed; `tests/addr_parse.rs`, the `register`
  example and every unit test stay (4 unit tests that need a second
  interface address fail on a one-address box, as before).

- `src/service_daemon.rs` `unicast_on_intf`, `send_from_v4` (exported from
  `src/lib.rs`, whose `forbid(unsafe_code)` is `deny` so this one
  function may allow it), `Cargo.toml` (libc on Linux): on Linux a legacy unicast
  reply leaves from the interface's address (IP_PKTINFO `ipi_spec_dst`).
  Upstream used `send_to` on the socket bound to 0.0.0.0:5353, so the
  kernel picked the source by the main table: strih-obs (its Dante leg
  routed only for its own address) answered from its LAN address, through
  the router (dstack #155, 9.11.2026; `crates/dstack/tests/mdns_reply_source.rs`).

- `src/service_daemon.rs` `ServiceDaemon::new_tapped`, `Tap` (exported from
  `src/lib.rs`), `Zeroconf::set_tap` and one call in `handle_read`: the
  daemon's thread hands every datagram its IPv4 socket received on an
  enabled interface (after the interface check, before decoding) to the
  caller's tap, and on Linux that socket takes only its own memberships
  (`IP_MULTICAST_ALL` off). dstack's mDNS meter and Windows responder read
  the group from it instead of a 5353 socket each: a process then receives
  each datagram once per address, where up to dstack v0.6.192 a Windows
  device process received it three times (dsg, dasio#190: 0.1-0.8 ms of
  receive DPC a datagram; dstack #442, `crates/dstack/src/announce/mdns.rs`,
  `crates/dstack/tests/mdns_one_socket.rs`). `new`/`new_with_port` keep
  upstream's behaviour. The tap is exercised by dstack's tests (unit tests
  on loopback, the whole-device count); `IP_MULTICAST_ALL` off is in no
  gate (this crate is outside the mutation gate, its tests run by hand).

- `Cargo.toml`: an empty `[workspace]` table, so cargo never searches above
  this crate (a dstack checkout nested in another one, such as a git
  worktree, found the outer workspace and refused this excluded path; dstack
  #239). Build-neutral: the lockfile does not change.

Drop this copy when upstream offers all of these (an answer-only daemon: a responder feature without the querier).

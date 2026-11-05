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
  in which FOH's audio TX stalled (dstack #1, dasio, 5.11.2026;
  `crates/dstack/tests/mdns_refresh.rs`).

Drop this copy when upstream offers both.

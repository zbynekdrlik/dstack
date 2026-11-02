# mdns-sd 0.21.6, vendored by dstack

Upstream: https://crates.io/crates/mdns-sd/0.21.6 (Apache-2.0 OR MIT, see
LICENSE-APACHE and LICENSE-MIT), copied unchanged except:

- `src/service_daemon.rs`, the daemon's start: no IPv6 multicast socket
  (`ipv6_sock = None`). Dante is IPv4 only; the `[::]:5353` socket received
  every interface's mDNS (dstack #1, dasio#1).

Drop this copy when upstream offers an IPv4-only daemon.

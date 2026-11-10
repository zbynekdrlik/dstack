// SPDX-License-Identifier: MIT OR Apache-2.0

//! A free set of control ports for a device sharing its PC's address with
//! other Dante devices (dasio, 10.11.2026: dsend on FOH stopped at "ARC on
//! UDP 4440: Only one usage of each socket address" beside dlisten on 4440
//! and band on 4441). Slot `k` is ARC `4440 + k`, CMC `8800 + k`, flows
//! `4455 + k` and process id `k`, the numbering the site's devices use; the
//! first slot whose three ports bind on the address is taken.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};

use dlink::{Place, Ports};

/// How many slots are tried: ARC 4440-4454 never meets flows 4455 up.
pub const SLOTS: u16 = 15;

/// Slot `k`'s ports and process id.
pub fn slot(k: u16) -> (Ports, u16) {
    (
        Ports {
            arc: 4440 + k,
            cmc: 8800 + k,
            flows: 4455 + k,
        },
        k,
    )
}

/// The first of the [`SLOTS`] whose ports `free` says are free.
pub fn first_free(free: impl Fn(&Ports) -> bool) -> Option<(Ports, u16)> {
    (0..SLOTS).map(slot).find(|(p, _)| free(p))
}

/// Whether UDP `ports` all bind on `ip` now (the sockets close again).
pub fn binds(ip: Ipv4Addr, ports: &Ports) -> bool {
    [ports.arc, ports.cmc, ports.flows]
        .iter()
        .all(|&port| UdpSocket::bind(SocketAddrV4::new(ip, port)).is_ok())
}

/// The ports and process id a device on `place` starts with: the ones
/// given (`--ports`, `--process-id`) when either is, else on an address the
/// first free slot ([`first_free`], [`binds`]); `None`s keep the defaults
/// (an interface by name, or no free slot: the start then names the port).
pub fn pick(place: &Place, ports: Option<Ports>, id: Option<u16>) -> (Option<Ports>, Option<u16>) {
    if ports.is_some() || id.is_some() {
        return (ports, id);
    }
    let Place::Ip(ip) = place else {
        return (None, None);
    };
    match first_free(|p| binds(*ip, p)) {
        Some((p, k)) => {
            if k > 0 {
                tracing::info!(
                    arc = p.arc,
                    cmc = p.cmc,
                    flows = p.flows,
                    process_id = k,
                    "another Dante device on this address holds the default ports: this one takes the next free set"
                );
            }
            (Some(p), Some(k))
        }
        None => (None, None),
    }
}

#[cfg(test)]
mod tests;

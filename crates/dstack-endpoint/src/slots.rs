// SPDX-License-Identifier: MIT OR Apache-2.0

//! A free set of control ports for a device sharing its PC's address with
//! other Dante devices (dasio, 10.11.2026: dsend on FOH stopped at "ARC on
//! UDP 4440: Only one usage of each socket address" beside dlisten on 4440
//! and band on 4441). Slot `k` is ARC `4440 + k`, CMC `8800 + k`, flows
//! `4455 + k` and process id `k`, the numbering the site's devices use; the
//! first slot whose three ports are free ([`binds_with`]) is taken.
//!
//! A port held by another program on the wildcard address is used too
//! (dstack #200): Audinate's ConMon service (Dante Controller, DVS) holds
//! 0.0.0.0:8700 and 0.0.0.0:8800 on a Windows PC. Windows lets a socket
//! bind the PC's address beside it and hands that socket the unicast to
//! the address first, so a device on 8800 took ConMon's CMC replies and
//! Dante Controller on the PC read no device. Each port is therefore tried
//! on the address and then on the wildcard address (no options, the probe
//! closed before the next): Windows refuses that second bind while a
//! wildcard socket holds the port, Linux while any socket does.

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

/// Whether UDP `ports` are all free for a device on `ip` now
/// ([`binds_with`] on real sockets, which close again).
pub fn binds(ip: Ipv4Addr, ports: &Ports) -> bool {
    binds_with(ip, ports, |at| UdpSocket::bind(at).is_ok())
}

/// Whether `ports` are all free for a device on `ip`, `try_bind` saying
/// whether a UDP socket binds at an address and port now: each port binds
/// on `ip` (no other device of ours holds it) and on the wildcard address
/// (no other program holds it there, such as Audinate's ConMon; see the
/// module).
pub fn binds_with(ip: Ipv4Addr, ports: &Ports, try_bind: impl Fn(SocketAddrV4) -> bool) -> bool {
    [ports.arc, ports.cmc, ports.flows].iter().all(|&port| {
        try_bind(SocketAddrV4::new(ip, port))
            && try_bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port))
    })
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
            tracing::info!(
                arc = p.arc,
                cmc = p.cmc,
                flows = p.flows,
                process_id = k,
                "the device's control ports: the first set free on its address"
            );
            (Some(p), Some(k))
        }
        None => (None, None),
    }
}

#[cfg(test)]
mod tests;

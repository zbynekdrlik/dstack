// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Where our RX flows arrive (dstack #464, item 2): the (destination IPv4,
//! UDP port) pairs of the flow plan as last published, for a reader that
//! takes our flows' packets before the host's network stack (dasio's NDIS
//! filter programs its match table, `IOCTL_DASIO_RX_MATCH`, from them).
//!
//! [`super::RxShared::publish`] works the set out from the plans it hands
//! the RX thread ([`destinations`]) and keeps it with a generation that
//! moves only when the set changed ([`RxDestinations::update`]); a reader
//! polls [`super::RxShared::rx_destinations`] and acts when the generation
//! moved.

use std::net::{Ipv4Addr, SocketAddrV4};

use super::{MAX_FLOWS, RxPlan};

/// The (destination IPv4, UDP port) pairs our RX flows arrive on, as the
/// flow plan was last published (dstack #464).
///
/// - A multicast flow: its group and port (the slot's group socket joins
///   it on our address).
/// - A unicast flow (our own flow request's): our address and the flow
///   slot's port.
///
/// A slot with no flow gives no pair; a flow requested and not yet granted
/// gives its pair already, so a filter set from it sees the first packet.
/// The set is complete: up to [`super::MAX_FLOWS`] pairs, one per flow
/// slot. A consumer with a smaller table caps it itself (dasio's filter
/// holds 16; its 0.0.0.0 "any address" is never given here).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RxDestinations {
    /// Moves by one each time the set changes (0: the empty set before any
    /// flow was published). A plan change that keeps the set (a latency, a
    /// grant) leaves it, so a reader rewrites its filter only when needed.
    pub generation: u64,
    /// The pairs, sorted (by address, then port), each once.
    pub pairs: Vec<SocketAddrV4>,
}

impl RxDestinations {
    /// Takes `pairs` as the set: `true` and the generation moved on when
    /// they differ from the set held, else `false` and nothing changes
    /// (and nothing is allocated: a publish of the same set allocates
    /// nothing, `tests/hot_path.rs`).
    pub fn update(&mut self, pairs: &[SocketAddrV4]) -> bool {
        if pairs == self.pairs.as_slice() {
            return false;
        }
        self.pairs = pairs.to_vec();
        self.generation += 1;
        true
    }
}

/// At most [`MAX_FLOWS`] pairs, worked out without allocating
/// ([`destinations`]).
#[derive(Debug, Clone, Copy)]
pub struct Pairs {
    at: [SocketAddrV4; MAX_FLOWS],
    len: usize,
}

impl Pairs {
    /// The pairs, sorted, each once.
    pub fn as_slice(&self) -> &[SocketAddrV4] {
        &self.at[..self.len]
    }
}

/// The pairs flow slots `slots` (each its plan and its socket's port, 0
/// for a slot with no socket) receive on, for a device at `ip`: a
/// multicast flow's group, a unicast flow's (`ip`, port); sorted, each
/// once; the first [`MAX_FLOWS`] found (one per flow slot). Allocates
/// nothing (the publish runs it on every manager change).
pub fn destinations(ip: Ipv4Addr, slots: impl IntoIterator<Item = (RxPlan, u16)>) -> Pairs {
    let mut at = [SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0); MAX_FLOWS];
    let found = slots
        .into_iter()
        .filter(|(plan, _)| plan.src.is_some())
        .filter_map(|(plan, port)| match plan.group {
            Some(group) => Some(group),
            None => (port != 0).then(|| SocketAddrV4::new(ip, port)),
        });
    let mut n = 0;
    for (cell, pair) in at.iter_mut().zip(found) {
        *cell = pair;
        n += 1;
    }
    at[..n].sort_unstable();
    // Each once: keep a pair unlike the one kept before it.
    let mut len = 0;
    for i in 0..n {
        if len == 0 || at[i] != at[len - 1] {
            at[len] = at[i];
            len += 1;
        }
    }
    Pairs { at, len }
}

#[cfg(test)]
mod tests;

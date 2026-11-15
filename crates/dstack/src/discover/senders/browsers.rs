// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! Hosts that refresh Dante records on a ~96 s cycle (dstack #319).
//!
//! An mDNS cache asks again for a record it learned at 80 % of its TTL
//! (RFC 6762 §5.2): 96 s for Dante's 120 s records. A program that keeps a
//! standing browse of `_netaudio-*` (an old dtray, 15.11.2026) therefore
//! sends multicast queries from port 5353 every 96 s, every Dante host
//! answers in the same second, and a receiver on the network counts late
//! packets. Our own lookups never do it: they ask by legacy unicast from
//! a port of their own (`subscribe::instance`), and our daemon answers
//! only (`vendor/mdns-sd/DSTACK.md`).
//!
//! - [`netaudio_query`] (pure): a datagram is such a query: a query (no
//!   answer, no probe: no authority section) with a question under a
//!   `_netaudio-*` service type.
//! - [`cycle`] (pure): the times a host sent them end in a chain of
//!   [`CYCLES`] intervals each [`CYCLE_NS`] ± [`TOLERANCE_NS`], each point
//!   a refresh after a quiet time (other queries may come between).
//! - [`Browsers`] (pure, fake times): the times per host (a query within
//!   [`SAME_BURST_NS`] of the last is the same refresh), the hosts whose
//!   cycle was seen lately, and once an hour per host the warning (a host
//!   forgotten, past [`MAX_HOSTS`] or unheard for 4 cycles, starts again).
//!
//! A host with Bonjour browsing Dante (Dante Controller's PC) refreshes the
//! same way, but at 80-82 % of the TTL (RFC 6762 §5.2), 96-98.4 s: it is
//! named only while three of its intervals in a row fall within the
//! tolerance.

use std::collections::{HashMap, VecDeque};
use std::net::Ipv4Addr;

use simple_dns::{Packet, PacketFlag};

use crate::subscribe::instance::MDNS;

const S: u64 = 1_000_000_000;
/// The refresh cycle: 80 % of Dante's 120 s TTL.
pub const CYCLE_NS: u64 = 96 * S;
/// How far an interval may be off the cycle.
pub const TOLERANCE_NS: u64 = 2 * S;
/// The cycles in a row that name a host.
pub const CYCLES: usize = 3;
/// Queries this close to the host's last one are the same refresh.
pub const SAME_BURST_NS: u64 = S;
/// A named host is warned about at most once in this long.
pub const WARN_EVERY_NS: u64 = 3600 * S;
/// Hosts followed at most (the one heard longest ago leaves).
pub const MAX_HOSTS: usize = 64;
/// Times kept per host (the oldest leave): more than a whole chain's
/// window holds at one a [`SAME_BURST_NS`].
pub const MAX_TIMES: usize = 512;

/// A host that refreshes Dante records on the cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Browser {
    pub source: Ipv4Addr,
    /// Its cycle as measured: the mean of its last [`CYCLES`] intervals.
    pub period_ns: u64,
    /// This minute's report warns about it (the first time in
    /// [`WARN_EVERY_NS`]).
    pub warn: bool,
}

/// `bytes` is a query (not an answer or probe) with a question under a
/// `_netaudio-*` service type (any label of the name).
pub fn netaudio_query(bytes: &[u8]) -> bool {
    let Ok(p) = Packet::parse(bytes) else {
        return false;
    };
    if p.has_flags(PacketFlag::RESPONSE) || !p.name_servers.is_empty() {
        return false;
    }
    p.questions.iter().any(|q| {
        q.qname
            .get_labels()
            .iter()
            .any(|l| l.to_string().to_ascii_lowercase().starts_with("_netaudio-"))
    })
}

/// The time `times` (oldest first) started a chain of [`CYCLES`] intervals
/// of `cycle_ns` ± `tolerance_ns` that ends at its last time; `None`
/// without one. Each point of the chain starts a refresh: the host sent
/// nothing in the `cycle_ns /` [`QUIET_PARTS`] before it (the first time
/// kept counts as one). A host that queries every few seconds is no
/// browser on the cycle, though some of its times are 96 s apart.
pub fn cycle(times: &[u64], cycle_ns: u64, tolerance_ns: u64) -> Option<u64> {
    let last = times.len().checked_sub(1)?;
    chain(times, last, CYCLES, cycle_ns, tolerance_ns)
}

/// The part of a cycle a refresh is quiet before (12 s of 96 s).
pub const QUIET_PARTS: u64 = 8;

/// `times[at]` follows the time before it by at least `quiet_ns`.
fn quiet_before(times: &[u64], at: usize, quiet_ns: u64) -> bool {
    at.checked_sub(1)
        .is_none_or(|p| times[at] - times[p] >= quiet_ns)
}

/// The start of a chain of `left` intervals back from `times[at]`.
fn chain(times: &[u64], at: usize, left: usize, cycle_ns: u64, tolerance_ns: u64) -> Option<u64> {
    if !quiet_before(times, at, cycle_ns / QUIET_PARTS) {
        return None;
    }
    if left == 0 {
        return Some(times[at]);
    }
    let (lo, hi) = (cycle_ns - tolerance_ns, cycle_ns + tolerance_ns);
    // Oldest first: going back, the gap only grows.
    (0..at)
        .rev()
        .map(|j| (j, times[at] - times[j]))
        .take_while(|&(_, gap)| gap <= hi)
        .filter(|&(_, gap)| gap >= lo)
        .find_map(|(j, _)| chain(times, j, left - 1, cycle_ns, tolerance_ns))
}

/// One host's queries and what was seen of it.
#[derive(Debug, Default)]
struct Host {
    times: VecDeque<u64>,
    /// When its cycle was last seen, and the period then.
    seen: Option<(u64, u64)>,
    warned: Option<u64>,
}

/// The hosts' query times (see the module).
#[derive(Debug)]
pub struct Browsers {
    cycle_ns: u64,
    tolerance_ns: u64,
    same_burst_ns: u64,
    hosts: HashMap<Ipv4Addr, Host>,
}

impl Default for Browsers {
    fn default() -> Self {
        Browsers::with_cycle(CYCLE_NS, TOLERANCE_NS, SAME_BURST_NS)
    }
}

impl Browsers {
    /// Looking for `cycle_ns` ± `tolerance_ns`, queries within `same_burst_ns`
    /// of the last one being the same refresh (a test: short ones).
    pub fn with_cycle(cycle_ns: u64, tolerance_ns: u64, same_burst_ns: u64) -> Self {
        Browsers {
            cycle_ns,
            tolerance_ns,
            same_burst_ns,
            hosts: HashMap::new(),
        }
    }

    /// The datagram `bytes` from `source`:`port` at `now_ns`: counted when
    /// it is a Dante query from port 5353 ([`netaudio_query`]).
    pub fn heard(&mut self, source: Ipv4Addr, port: u16, bytes: &[u8], now_ns: u64) {
        if port == MDNS.port() && netaudio_query(bytes) {
            self.query(source, now_ns);
        }
    }

    /// How long a host's times are kept: a whole chain and one cycle more.
    fn window(&self) -> u64 {
        (CYCLES as u64 + 1) * (self.cycle_ns + self.tolerance_ns)
    }

    /// A Dante query from `source` at `now_ns`.
    pub fn query(&mut self, source: Ipv4Addr, now_ns: u64) {
        if !self.hosts.contains_key(&source) && self.hosts.len() == MAX_HOSTS {
            let oldest = self
                .hosts
                .iter()
                .min_by_key(|(a, h)| (h.times.back().copied(), **a))
                .map(|(a, _)| *a);
            if let Some(a) = oldest {
                self.hosts.remove(&a);
            }
        }
        let (cycle_ns, tolerance_ns, keep) = (self.cycle_ns, self.tolerance_ns, self.window());
        let same_burst_ns = self.same_burst_ns;
        let host = self.hosts.entry(source).or_default();
        if host
            .times
            .back()
            .is_some_and(|&t| now_ns.saturating_sub(t) < same_burst_ns)
        {
            return;
        }
        while host
            .times
            .front()
            .is_some_and(|&t| now_ns.saturating_sub(t) > keep)
            || host.times.len() == MAX_TIMES
        {
            host.times.pop_front();
        }
        host.times.push_back(now_ns);
        if let Some(start) = cycle(host.times.make_contiguous(), cycle_ns, tolerance_ns) {
            let period = (now_ns - start) / CYCLES as u64;
            host.seen = Some((now_ns, period));
        }
    }

    /// The hosts whose cycle was seen within two cycles of `now_ns`, by
    /// address, each marked to warn about once in [`WARN_EVERY_NS`]. A
    /// host not heard in 4 cycles (and their tolerance) is forgotten.
    pub fn take(&mut self, now_ns: u64) -> Vec<Browser> {
        let (recent, keep) = (2 * self.cycle_ns, self.window());
        self.hosts.retain(|_, h| {
            h.times
                .back()
                .is_some_and(|&t| now_ns.saturating_sub(t) <= keep)
        });
        let mut out = Vec::new();
        for (source, h) in &mut self.hosts {
            let Some((at, period_ns)) = h.seen else {
                continue;
            };
            if now_ns.saturating_sub(at) > recent {
                continue;
            }
            let warn = h
                .warned
                .is_none_or(|w| now_ns.saturating_sub(w) >= WARN_EVERY_NS);
            if warn {
                h.warned = Some(now_ns);
            }
            out.push(Browser {
                source: *source,
                period_ns,
                warn,
            });
        }
        out.sort_by_key(|b| b.source);
        out
    }

    /// The hosts followed now.
    pub fn hosts(&self) -> usize {
        self.hosts.len()
    }

    /// The times kept for `source` (a test).
    #[cfg(test)]
    fn times_of(&self, source: Ipv4Addr) -> Vec<u64> {
        self.hosts
            .get(&source)
            .map(|h| h.times.iter().copied().collect())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests;

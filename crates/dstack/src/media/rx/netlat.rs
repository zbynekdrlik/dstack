// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The measured network latency of an RX flow slot (dstack #244): each
//! packet's arrival (the kernel's receive time, read on our PTP clock as
//! the pass's PTP now less the packet's wake delay, `rx/socket.rs`) less
//! its sender's media-clock stamp (its first frame), as min, p99 and max
//! of each heartbeat second, next to the wake delay.
//!
//! The RX thread records into a fixed histogram ([`Window`]: [`BUCKETS`]
//! buckets of [`BUCKET_NS`] and one past them, plus the exact least and
//! largest), one `fetch_add` and a `fetch_min`/`fetch_max` per packet: no
//! allocation, no lock. The heartbeat's take ([`Window::take`]) zeroes each
//! bucket as it reads it; a packet recorded meanwhile counts in this window
//! or the next, never in both.
//!
//! The figure is the packet's age when the kernel received it, measured
//! from its first frame: the network's transit plus the sender's own part
//! (the packet's frames, which exist only once its last frame does, and
//! how late the sender sent it). It is an upper bound on the transit, the
//! figure to set (a) and (b) against. Only differences of one clock's
//! reads go into the arrival (as for `wire_late`), so the wall clock's
//! offset from PTP does not matter.
//! Windows has no receive stamp: nothing is recorded there.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// One bucket's width, ns: 20 µs, fine enough for a latency of about 1 ms.
pub const BUCKET_NS: u64 = 20_000;
/// The buckets under [`BUCKETS`] × [`BUCKET_NS`] (5.12 ms); one more holds
/// everything at or past it.
pub const BUCKETS: usize = 256;
/// A least that holds none: no packet recorded since the last take.
const NONE: u64 = u64::MAX;

/// A packet's network latency, ns: its arrival at our PTP time `now_ns`
/// less its wake delay `wake_ns` (the kernel received it that much before
/// the pass), less its own stamp `stamp_ns`; 0 for a packet stamped after
/// it arrived.
pub fn network_latency_ns(now_ns: u64, wake_ns: u64, stamp_ns: u64) -> u64 {
    now_ns.saturating_sub(wake_ns).saturating_sub(stamp_ns)
}

/// The bucket of a latency of `ns`: `ns / BUCKET_NS`, at most [`BUCKETS`]
/// (the one past them).
pub fn bucket(ns: u64) -> usize {
    usize::try_from(ns / BUCKET_NS).map_or(BUCKETS, |b| b.min(BUCKETS))
}

/// One window's least, 99th percentile and largest network latency, µs
/// (each at most 2.1 s, `MAX_US`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NetLatency {
    pub min_us: u64,
    pub p99_us: u64,
    pub max_us: u64,
}

/// The largest figure a window keeps, µs (21 bits, 2.1 s): three fit one
/// atomic, so a reader never mixes two windows' figures.
pub const MAX_US: u64 = (1 << 21) - 1;

impl NetLatency {
    /// The three figures of `ns` values as µs in one word: min in the low
    /// 21 bits, then p99, then max, each at most `MAX_US` (2.1 s).
    pub fn pack(min_ns: u64, p99_ns: u64, max_ns: u64) -> u64 {
        let us = |ns: u64| (ns / 1_000).min(MAX_US);
        // Disjoint fields, so `+` is the same as `|` (tests.md: `|` leaves
        // an equivalent `^` mutant); 63 bits, no overflow.
        us(min_ns) + (us(p99_ns) << 21) + (us(max_ns) << 42)
    }

    /// The figures `NetLatency::pack` put in `word`.
    pub fn unpack(word: u64) -> Self {
        NetLatency {
            min_us: word & MAX_US,
            p99_us: (word >> 21) & MAX_US,
            max_us: (word >> 42) & MAX_US,
        }
    }
}

/// The 99th percentile of a window's `counts` (one per bucket, the last
/// one past them) whose least is `min_ns` and largest `max_ns`: the upper
/// edge of the bucket that holds the packet at 99 % (by rank, rounded up),
/// within `min_ns..=max_ns`; `max_ns` when that packet is past the buckets.
/// `None` for no packet.
pub fn p99(counts: &[u32], min_ns: u64, max_ns: u64) -> Option<u64> {
    let total: u64 = counts.iter().map(|&c| u64::from(c)).sum();
    if total == 0 {
        return None;
    }
    let rank = (total * 99).div_ceil(100);
    let mut seen = 0;
    for (b, &c) in counts.iter().enumerate().take(BUCKETS) {
        seen += u64::from(c);
        if seen >= rank {
            let edge = (b as u64 + 1) * BUCKET_NS;
            return Some(edge.clamp(min_ns, max_ns.max(min_ns)));
        }
    }
    Some(max_ns)
}

/// One flow slot's window: written by the RX thread, taken once a second.
#[derive(Debug)]
pub struct Window {
    counts: [AtomicU32; BUCKETS + 1],
    min_ns: AtomicU64,
    max_ns: AtomicU64,
    /// The last take's figures in one word (`NetLatency::pack`); [`NONE`]
    /// after a window with no packet.
    last: AtomicU64,
}

impl Default for Window {
    fn default() -> Self {
        Window {
            counts: std::array::from_fn(|_| AtomicU32::new(0)),
            min_ns: AtomicU64::new(NONE),
            max_ns: AtomicU64::new(0),
            last: AtomicU64::new(NONE),
        }
    }
}

impl Window {
    /// Records one packet's latency (the hot path: three atomics).
    pub fn record(&self, ns: u64) {
        self.counts[bucket(ns)].fetch_add(1, Ordering::Relaxed);
        self.min_ns.fetch_min(ns, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// Ends the window: its figures become the last ones
    /// ([`Window::last`]) and the next window starts empty.
    pub fn take(&self) {
        let counts: [u32; BUCKETS + 1] =
            std::array::from_fn(|b| self.counts[b].swap(0, Ordering::Relaxed));
        let min = self.min_ns.swap(NONE, Ordering::Relaxed);
        let max = self.max_ns.swap(0, Ordering::Relaxed);
        let last = match p99(&counts, min, max) {
            Some(p) if min != NONE => NetLatency::pack(min, p, max),
            _ => NONE,
        };
        self.last.store(last, Ordering::Relaxed);
    }

    /// The last window's figures; `None` for a window without a packet.
    pub fn last(&self) -> Option<NetLatency> {
        let word = self.last.load(Ordering::Relaxed);
        (word != NONE).then(|| NetLatency::unpack(word))
    }
}

#[cfg(test)]
mod tests;

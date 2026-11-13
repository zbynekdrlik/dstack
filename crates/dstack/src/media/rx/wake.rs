// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The RX thread's wake delay of a flow slot over a minute (dstack #234):
//! each stamped packet's wake delay (the pass's wall clock less the
//! kernel's receive time, `rx/socket.rs`) in a fixed histogram, read as
//! the 50th, 99th and 99.9th percentile and the largest once a minute.
//!
//! A dlink app (OBS's dante-input) exposes no status socket, so the RX
//! minute watch logs these with the minute's network latency
//! (`media/late.rs`); the status shows the last minute's percentiles. The
//! RX thread records one `fetch_add` and one `fetch_max` per packet: no
//! allocation, no lock. A take ([`Window::take`]) zeroes each bucket as it
//! reads it; a packet recorded meanwhile counts in this window or the
//! next, never in both, though its largest may land in the other window
//! than its count (which can only lower this window's top figures by that
//! one packet's). Windows has no receive stamp: nothing is recorded
//! there.
//!
//! The same window holds each packet's write delay (dstack #234: the
//! kernel's receive time to the end of its ring writes), which also
//! catches a stall inside a pass, after its start.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::netlat;

/// One bucket's width, ns: 50 µs.
pub const BUCKET_NS: u64 = 50_000;
/// The buckets under [`BUCKETS`] × [`BUCKET_NS`] (25.6 ms, past the 15.5 ms
/// peak strih-obs showed); one more holds everything at or past it.
pub const BUCKETS: usize = 512;
/// A last window that held none.
const NONE: u64 = u64::MAX;

/// The bucket of a wake delay of `ns`: `ns / BUCKET_NS`, at most
/// [`BUCKETS`] (the one past them).
pub fn bucket(ns: u64) -> usize {
    usize::try_from(ns / BUCKET_NS).map_or(BUCKETS, |b| b.min(BUCKETS))
}

/// One window's wake delays, µs: the 50th, 99th and 99.9th percentile (the
/// upper edge of the 50 µs bucket holding that rank, at most the largest)
/// and the largest, and the packets they are of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WakeDelay {
    pub packets: u64,
    pub p50_us: u64,
    pub p99_us: u64,
    pub p999_us: u64,
    pub max_us: u64,
}

impl WakeDelay {
    /// The figures of a window's `counts` (one per bucket, the last past
    /// them) whose largest is `max_ns`; `None` for no packet.
    pub fn of(counts: &[u32], max_ns: u64) -> Option<Self> {
        let at = |per_mille| netlat::quantile(counts, BUCKET_NS, per_mille, 0, max_ns);
        Some(WakeDelay {
            packets: counts.iter().map(|&c| u64::from(c)).sum(),
            p50_us: at(500)? / 1_000,
            p99_us: at(990)? / 1_000,
            p999_us: at(999)? / 1_000,
            max_us: max_ns / 1_000,
        })
    }
}

/// One flow slot's minute: written by the RX thread, taken once a minute.
#[derive(Debug)]
pub struct Window {
    counts: [AtomicU32; BUCKETS + 1],
    max_ns: AtomicU64,
    /// The last take's percentiles in one word (`netlat::pack_us`: p50,
    /// p99, p99.9), [`NONE`] after a window with no packet.
    last: AtomicU64,
    /// The last take's largest, µs, [`NONE`] after a window with no packet.
    last_max_us: AtomicU64,
}

impl Default for Window {
    fn default() -> Self {
        Window {
            counts: std::array::from_fn(|_| AtomicU32::new(0)),
            max_ns: AtomicU64::new(0),
            last: AtomicU64::new(NONE),
            last_max_us: AtomicU64::new(NONE),
        }
    }
}

impl Window {
    /// Records one packet's wake delay (the hot path: two atomics).
    pub fn record(&self, ns: u64) {
        self.counts[bucket(ns)].fetch_add(1, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// Ends the window: its figures, kept as the last ones for
    /// [`Window::last`]; the next window starts empty. `None` for a window
    /// without a packet.
    pub fn take(&self) -> Option<WakeDelay> {
        let counts: [u32; BUCKETS + 1] =
            std::array::from_fn(|b| self.counts[b].swap(0, Ordering::Relaxed));
        let max = self.max_ns.swap(0, Ordering::Relaxed);
        let taken = WakeDelay::of(&counts, max);
        let last = taken.map_or(NONE, |w| {
            netlat::pack_us([w.p50_us, w.p99_us, w.p999_us].map(|us| us * 1_000))
        });
        self.last.store(last, Ordering::Relaxed);
        self.last_max_us
            .store(taken.map_or(NONE, |w| w.max_us), Ordering::Relaxed);
        taken
    }

    /// The last window's 50th, 99th and 99.9th percentile, µs; `None` for
    /// a window without a packet (or before the first take).
    pub fn last(&self) -> Option<[u64; 3]> {
        let word = self.last.load(Ordering::Relaxed);
        (word != NONE).then(|| netlat::unpack_us(word))
    }

    /// The last window's largest, µs; `None` for a window without a
    /// packet (or before the first take).
    pub fn last_max_us(&self) -> Option<u64> {
        Some(self.last_max_us.load(Ordering::Relaxed)).filter(|&us| us != NONE)
    }
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The placed packets' mean age of a flow slot (dstack #342): the sum and
//! the count of each placed packet's age (our PTP now less its own stamp,
//! ns: the judge's figure, `lateness_min_ns`; 0 for a packet stamped after
//! our now), as running totals since the device started.
//!
//! The ring writer adds to both in `intake::place`, the one place every
//! writer goes through (the RX thread's pass, the pump's drain, an app's
//! drain: dstack #326, #307): two `fetch_add`, no allocation, no lock. A
//! reader forms the mean of the packets between two of its own looks
//! ([`mean_between`]), so any number of readers never steal each other's
//! window (a swap to 0 would). The heartbeat's take keeps its second's
//! mean ([`Ages::take`]), next to the second's least and largest.
//!
//! The sum and the count are two atomics: a look between the two adds is
//! off by one packet, nothing over a second's thousands. Information
//! only: no latency is set from it.

use std::sync::atomic::{AtomicU64, Ordering};

/// The most one packet adds to a sum, ns (2^32, 4.29 s): a packet that
/// old is far past any latency. With it the sum wraps only after 2^32
/// packets at the cap (16 days at 3000 packets a second); a reader diffs
/// modulo 2^64 ([`mean_between`]).
pub(super) const AGE_CAP_NS: u64 = 1 << 32;
/// A mean that holds none: no packet placed in the last take's second.
const NONE: u64 = u64::MAX;

/// The mean age of the packets placed between two looks at the running
/// totals `(sum ns, count)`, `earlier` first, ns (rounded down); `None`
/// when none was placed.
pub(super) fn mean_between(earlier: (u64, u64), later: (u64, u64)) -> Option<u64> {
    let count = later.1.wrapping_sub(earlier.1);
    (count != 0).then(|| later.0.wrapping_sub(earlier.0) / count)
}

/// One flow slot's ages: the running totals the writer adds to, and the
/// heartbeat's last second.
#[derive(Debug)]
pub(super) struct Ages {
    sum_ns: AtomicU64,
    count: AtomicU64,
    /// The totals at the last take (only the heartbeat takes).
    taken_sum_ns: AtomicU64,
    taken_count: AtomicU64,
    /// The last take's mean, ns; [`NONE`] for a second without a packet.
    last_mean_ns: AtomicU64,
}

impl Default for Ages {
    fn default() -> Self {
        Ages {
            sum_ns: AtomicU64::new(0),
            count: AtomicU64::new(0),
            taken_sum_ns: AtomicU64::new(0),
            taken_count: AtomicU64::new(0),
            last_mean_ns: AtomicU64::new(NONE),
        }
    }
}

impl Ages {
    /// Adds one placed packet's age, ns, at most [`AGE_CAP_NS`] (the hot
    /// path: two atomics).
    pub(super) fn record(&self, ns: u64) {
        self.sum_ns.fetch_add(ns.min(AGE_CAP_NS), Ordering::Relaxed);
        self.count.fetch_add(1, Ordering::Relaxed);
    }

    /// The running totals since the device started: (sum ns, count).
    pub(super) fn totals(&self) -> (u64, u64) {
        (
            self.sum_ns.load(Ordering::Relaxed),
            self.count.load(Ordering::Relaxed),
        )
    }

    /// Ends the heartbeat's second: the mean of the packets placed since
    /// the last take becomes [`Ages::last_mean_ns`].
    pub(super) fn take(&self) {
        let (sum, count) = self.totals();
        let earlier = (
            self.taken_sum_ns.swap(sum, Ordering::Relaxed),
            self.taken_count.swap(count, Ordering::Relaxed),
        );
        let mean = mean_between(earlier, (sum, count)).unwrap_or(NONE);
        self.last_mean_ns.store(mean, Ordering::Relaxed);
    }

    /// The last take's mean age, ns; `None` for a second without a placed
    /// packet.
    pub(super) fn last_mean_ns(&self) -> Option<u64> {
        let mean = self.last_mean_ns.load(Ordering::Relaxed);
        (mean != NONE).then_some(mean)
    }
}

#[cfg(test)]
mod tests;

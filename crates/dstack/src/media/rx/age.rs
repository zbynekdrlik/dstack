// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The placed packets' mean age of a flow slot (dstack #342): the sum and
//! the count of each placed packet's age (its arrival on our PTP clock
//! less its own stamp, ns, [`arrival_age_ns`], dstack #350: the judge's
//! figure, `lateness_min_ns`; 0 for a packet stamped after it came), as
//! running totals since the device started. The wait from the arrival to
//! the writer's read is kept the same way, apart (`read_waits`).
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

/// A placed packet's age at its ARRIVAL, ns (dstack #350): what the
/// heartbeat, the judge and the mean report, never the wait to its read.
///
/// - `at_read`: its age when the writer read it (our PTP now less its own
///   stamp).
/// - `wake`: the OS's receive time to the read (Linux's `SO_TIMESTAMPNS`,
///   Windows' NIC stamp where the driver gives one, dstack #381): with it
///   the arrival is exact, `at_read` less `wake`; a wake past `at_read`
///   (a forward step of the wall clock the Linux stamp is on) is ignored.
/// - Without a stamp (a box or a datagram the OS stamps nothing on),
///   `since`: the time from the writer's last
///   read that left the socket empty to this one. The packet came after
///   that read, so its arrival age is at least `at_read - since`. The
///   packets of one read share one network delay, which `newest` (the
///   least `at_read` of the read: the newest packet, which came at most
///   one packet interval before the read) bounds from above: the
///   arrival age is the larger of the two, never above `at_read`
///   (`newest <= at_read` by its definition).
/// - With neither (the first read after the start, a flow change, a read
///   that left datagrams behind, the clock back): `at_read`.
pub(super) fn arrival_age_ns(
    at_read: u64,
    wake: Option<u64>,
    since: Option<u64>,
    newest: u64,
) -> u64 {
    // A wake past the age at the read is no arrival (the wall clock the
    // stamp is on stepped forward): the bracket then, as without a stamp.
    match (wake.filter(|&w| w <= at_read), since) {
        (Some(w), _) => at_read - w,
        (None, Some(s)) => at_read.saturating_sub(s).max(newest),
        (None, None) => at_read,
    }
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

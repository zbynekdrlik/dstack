// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! How late the engine's pump woke (dstack #281): at each wake on the media
//! clock, how far behind its due time the oldest block it found due was
//! (`behind`, the pump's lateness at that wake). The pump only records
//! ([`WakeLateness::record`]: atomics, no allocation, no lock; the wall
//! time is read only for a wake one block or more late); the control side
//! reads the largest since the start, that second's and the last minute's
//! (taken by the pump watch), the fixed buckets ([`BUCKET_EDGES_NS`]) and
//! the last [`RECENT`] wakes one block or more late, each with its UTC
//! time and whether the pump re-bootstrapped there.
//!
//! The recent wakes are a ring of slots, each guarded by a sequence word
//! (a seqlock with one writer, the pump): the word goes up by one before a
//! slot is written and by one after, so slot `i % RECENT` holds wake `i`
//! once its word reads `2 (i / RECENT + 1)` (`done_of`). A reader keeps
//! a slot only when the word is that before and after its reads, so a slot
//! being written or already overwritten by a newer wake is left out, never
//! torn.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering, fence};

/// The buckets' upper edges, ns: below 0.5, 1, 2, 5, 10, 20 and 50 ms, the
/// last bucket at or above 50 ms.
pub const BUCKET_EDGES_NS: [u64; 7] = [
    500_000, 1_000_000, 2_000_000, 5_000_000, 10_000_000, 20_000_000, 50_000_000,
];
/// The number of buckets: one per edge and one past the last.
pub const BUCKETS: usize = 8;
const _: () = assert!(BUCKETS == BUCKET_EDGES_NS.len() + 1);
/// The late wakes kept (the newest ones).
pub const RECENT: usize = 32;

/// The bucket of a wake `behind_ns` late: the number of edges at or below
/// it.
pub fn bucket(behind_ns: u64) -> usize {
    BUCKET_EDGES_NS
        .iter()
        .filter(|&&edge| edge <= behind_ns)
        .count()
}

/// One wake one block or more late.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LateWake {
    /// Its wall time, ms since the Unix epoch (UTC).
    pub unix_ms: u64,
    /// How far behind its due time the pump woke, whole µs.
    pub behind_us: u64,
    /// The pump was more than its margin behind there and re-bootstrapped.
    pub rebootstrap: bool,
}

/// One slot of the recent late wakes.
#[derive(Default)]
struct Slot {
    /// Odd while a wake is written; [`done_of`] its index once it is.
    seq: AtomicU64,
    unix_ms: AtomicU64,
    /// `behind_us << 1`, plus 1 when the pump re-bootstrapped.
    word: AtomicU64,
}

/// The pump's wake lateness (see the module).
pub struct WakeLateness {
    max_ns: AtomicU64,
    /// Since the pump watch's last look (once a second).
    second_max_ns: AtomicU64,
    /// Since the pump watch's last minute.
    minute_max_ns: AtomicU64,
    /// The last whole minute's, once the watch took one.
    last_minute_max_ns: AtomicU64,
    minute_taken: AtomicBool,
    buckets: [AtomicU64; BUCKETS],
    /// Wakes one block or more late since the start: the next one's index.
    late: AtomicU64,
    recent: [Slot; RECENT],
}

impl Default for WakeLateness {
    fn default() -> Self {
        WakeLateness::new()
    }
}

impl WakeLateness {
    pub fn new() -> Self {
        WakeLateness {
            max_ns: AtomicU64::new(0),
            second_max_ns: AtomicU64::new(0),
            minute_max_ns: AtomicU64::new(0),
            last_minute_max_ns: AtomicU64::new(0),
            minute_taken: AtomicBool::new(false),
            buckets: std::array::from_fn(|_| AtomicU64::new(0)),
            late: AtomicU64::new(0),
            recent: std::array::from_fn(|_| Slot::default()),
        }
    }

    /// A wake `behind_ns` behind its due time (the pump thread): the
    /// largest kept, its bucket counted; when it was `late` (one block or
    /// more), it goes into the recent ring with its wall time (`unix_ms`,
    /// read only then) and whether the pump re-bootstrapped there.
    pub fn record(
        &self,
        behind_ns: u64,
        late: bool,
        rebootstrap: bool,
        unix_ms: impl FnOnce() -> u64,
    ) {
        self.max_ns.fetch_max(behind_ns, Ordering::Relaxed);
        self.second_max_ns.fetch_max(behind_ns, Ordering::Relaxed);
        self.minute_max_ns.fetch_max(behind_ns, Ordering::Relaxed);
        self.buckets[bucket(behind_ns)].fetch_add(1, Ordering::Relaxed);
        if !late {
            return;
        }
        // One writer: the index is ours until it is published below.
        let i = self.late.load(Ordering::Relaxed);
        let slot = &self.recent[slot_of(i)];
        slot.seq.fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        slot.unix_ms.store(unix_ms(), Ordering::Relaxed);
        // Bit 0 is free after the shift: a sum, not an `|` (whose `^`
        // mutant gives the same word).
        slot.word.store(
            ((behind_ns / 1_000) << 1) + u64::from(rebootstrap),
            Ordering::Relaxed,
        );
        slot.seq.fetch_add(1, Ordering::Release);
        self.late.store(i + 1, Ordering::Release);
    }

    /// The latest wake since the start, whole µs.
    pub fn max_us(&self) -> u64 {
        self.max_ns.load(Ordering::Relaxed) / 1_000
    }

    /// The latest wake since the last call, whole µs, and a fresh start for
    /// the next (the pump watch, once a second).
    pub fn take_second_max_us(&self) -> u64 {
        self.second_max_ns.swap(0, Ordering::Relaxed) / 1_000
    }

    /// Ends a minute (the pump watch): its latest wake becomes the last
    /// minute's, and the next minute starts afresh.
    pub fn take_minute(&self) {
        let max = self.minute_max_ns.swap(0, Ordering::Relaxed);
        self.last_minute_max_ns.store(max, Ordering::Relaxed);
        self.minute_taken.store(true, Ordering::Release);
    }

    /// The last whole minute's latest wake, whole µs; `None` before the
    /// watch took a minute.
    pub fn minute_max_us(&self) -> Option<u64> {
        self.minute_taken
            .load(Ordering::Acquire)
            .then(|| self.last_minute_max_ns.load(Ordering::Relaxed) / 1_000)
    }

    /// The wakes in each bucket ([`BUCKET_EDGES_NS`]) since the start.
    pub fn buckets(&self) -> [u64; BUCKETS] {
        std::array::from_fn(|b| self.buckets[b].load(Ordering::Relaxed))
    }

    /// The wakes one block or more late since the start.
    pub fn late_wakes(&self) -> u64 {
        self.late.load(Ordering::Acquire)
    }

    /// The last [`RECENT`] late wakes, oldest first (a slot the pump is
    /// writing again is left out).
    pub fn recent(&self) -> Vec<LateWake> {
        let end = self.late.load(Ordering::Acquire);
        let from = end.saturating_sub(RECENT as u64);
        (from..end).filter_map(|i| self.read(i)).collect()
    }

    /// Wake `i` from its slot, `None` when the slot holds another one (or
    /// is being written).
    fn read(&self, i: u64) -> Option<LateWake> {
        let slot = &self.recent[slot_of(i)];
        let done = done_of(i);
        if slot.seq.load(Ordering::Acquire) != done {
            return None;
        }
        let unix_ms = slot.unix_ms.load(Ordering::Relaxed);
        let word = slot.word.load(Ordering::Relaxed);
        fence(Ordering::Acquire);
        (slot.seq.load(Ordering::Relaxed) == done).then_some(LateWake {
            unix_ms,
            behind_us: word >> 1,
            rebootstrap: word & 1 == 1,
        })
    }
}

/// The sequence word of late wake `i`'s slot once that wake is written:
/// the slot's `i / RECENT + 1`-th write, two steps each.
fn done_of(i: u64) -> u64 {
    2 * (i / RECENT as u64 + 1)
}

/// The slot of late wake `i`.
fn slot_of(i: u64) -> usize {
    (i % RECENT as u64) as usize
}

/// The wall time now, ms since the Unix epoch (0 before it).
pub(crate) fn unix_ms_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The jitter ring of one RX channel: samples placed by their position (the
//! sample index their packet's stamp names, [`crate::clock::from_stamp`])
//! and read by position, so packets may come in any order and a missing
//! one reads as a hole.
//!
//! [`SLOTS`] slots (0.68 s at 96 kHz), one writer (the RX thread) and one
//! reader (the TX thread's echo, phase C's engine), atomics only; an engine
//! output ring ([`Ring::put_at`]) has the engine's pump as its writer and
//! the TX thread as its reader. Each slot
//! is one `AtomicU64`: the sample in its low 32 bits and the position's
//! lap (`position >> 16`, mod 2³²) in its high 32, written and read in one
//! access, so a reader never sees a sample of another position: a slot of
//! an earlier lap, or one never written, reads as a hole (0), never stale.
//! inferno's `ring_buffer.rs` keeps a ready bit per item and a readable
//! position instead; this is our own design.
//!
//! - A write behind the reader (a position before the end of the furthest
//!   read) is late: not written, counted.
//! - A read that finds a hole marks the slot as read empty (the position's
//!   lap with [`READ_EMPTY`] set), by compare-and-swap. The writer stores
//!   by compare-and-swap too, so a sample whose hole the reader already
//!   read is late, not written, even when the reader passed it while the
//!   write ran (obs, camera-box#1485: the RX thread and the reader on one
//!   CPU; before, those samples counted written and the app got silence
//!   with late at 0).
//! - A write of a position the slot already holds is a duplicate: written
//!   again (the last copy wins), counted.
//! - A write [`SLOTS`] or more ahead of the reader would overwrite samples
//!   it has not read yet: early, not written, counted (a stamp far in the
//!   future). Before the first read nothing is late or early.
//! - The largest magnitude stored is kept until [`Ring::take_peak`] (the
//!   heartbeat's peaks, once a second): one atomic per write.
//!
//! Known limit: the lap is kept mod 2³¹, so positions 2⁴⁷ apart (46 years
//! at 96 kHz) share a slot and a lap.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

/// The ring's slots.
pub const SLOTS: usize = 1 << 16;

/// What one write did, in samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Write {
    /// Stored at a position the slot did not hold.
    pub written: usize,
    /// Behind the reader: not stored.
    pub late: usize,
    /// Stored again at a position the slot held.
    pub duplicate: usize,
    /// [`SLOTS`] or more ahead of the reader: not stored.
    pub early: usize,
}

/// What one read found, in samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Read {
    /// Positions no write stored: read as 0.
    pub holes: usize,
}

/// The ring's counts since it was built, in samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Counters {
    pub written: u64,
    pub holes: u64,
    pub late: u64,
    pub duplicate: u64,
    pub early: u64,
}

/// One RX channel's ring.
#[derive(Debug)]
pub struct Ring {
    slots: Box<[AtomicU64]>,
    /// The end of the furthest read (0: none yet).
    read_end: AtomicU64,
    written: AtomicU64,
    holes: AtomicU64,
    late: AtomicU64,
    duplicate: AtomicU64,
    early: AtomicU64,
    /// The largest magnitude stored since the last take.
    peak: AtomicU32,
}

impl Default for Ring {
    fn default() -> Self {
        Self::new()
    }
}

impl Ring {
    /// An empty ring (allocates its [`SLOTS`] slots, 512 KiB).
    pub fn new() -> Self {
        Ring {
            slots: (0..SLOTS).map(|_| AtomicU64::new(EMPTY)).collect(),
            read_end: AtomicU64::new(0),
            written: AtomicU64::new(0),
            holes: AtomicU64::new(0),
            late: AtomicU64::new(0),
            duplicate: AtomicU64::new(0),
            early: AtomicU64::new(0),
            peak: AtomicU32::new(0),
        }
    }

    /// Stores `samples` at positions `pos`, `pos + 1`, …
    pub fn write_at(&self, pos: u64, samples: &[i32]) -> Write {
        let reader = self.read_end.load(Ordering::Relaxed);
        let mut w = Write::default();
        let mut peak = 0;
        for (i, &sample) in samples.iter().enumerate() {
            let p = pos.wrapping_add(i as u64);
            if p < reader {
                w.late += 1;
            } else if reader != 0 && p - reader >= SLOTS as u64 {
                w.early += 1;
            } else {
                let slot = self.slot(p);
                let held = slot.load(Ordering::Relaxed);
                // One writer: the swap fails only when the reader marked
                // the slot read empty since the load.
                if held == read_empty(p)
                    || slot
                        .compare_exchange(
                            held,
                            word(p, sample),
                            Ordering::Relaxed,
                            Ordering::Relaxed,
                        )
                        .is_err()
                {
                    w.late += 1;
                    continue;
                }
                if lap_of(held) == lap(p) {
                    w.duplicate += 1;
                } else {
                    w.written += 1;
                }
                peak = peak.max(sample.unsigned_abs());
            }
        }
        self.peak.fetch_max(peak, Ordering::Relaxed);
        self.written.fetch_add(w.written as u64, Ordering::Relaxed);
        self.late.fetch_add(w.late as u64, Ordering::Relaxed);
        self.duplicate
            .fetch_add(w.duplicate as u64, Ordering::Relaxed);
        self.early.fetch_add(w.early as u64, Ordering::Relaxed);
        w
    }

    /// Stores `samples` at positions `pos`, `pos + 1`, … whatever the
    /// reader did: no late or early check, and the reader's end never
    /// moves; counts them `written` only. The engine's output rings (phase
    /// C, [`crate::stream`]): a block written after the TX thread read its
    /// stamps shows as TX holes, never here.
    pub fn put_at(&self, pos: u64, samples: &[i32]) {
        for (i, &sample) in samples.iter().enumerate() {
            let p = pos.wrapping_add(i as u64);
            self.slot(p).store(word(p, sample), Ordering::Relaxed);
        }
        self.written
            .fetch_add(samples.len() as u64, Ordering::Relaxed);
    }

    /// Reads positions `pos`, `pos + 1`, … into `out`, a hole as 0.
    pub fn read_at(&self, pos: u64, out: &mut [i32]) -> Read {
        let mut holes = 0;
        for (i, o) in out.iter_mut().enumerate() {
            let p = pos.wrapping_add(i as u64);
            let slot = self.slot(p);
            let w = slot.load(Ordering::Relaxed);
            let w = if lap_of(w) == lap(p) || newer(w, p) {
                w
            } else {
                // A hole: mark it read empty, unless the writer stored the
                // sample since the load.
                slot.compare_exchange(w, read_empty(p), Ordering::Relaxed, Ordering::Relaxed)
                    .err()
                    .unwrap_or(w)
            };
            *o = if lap_of(w) == lap(p) {
                w as u32 as i32
            } else {
                holes += 1;
                0
            };
        }
        self.read_end
            .fetch_max(pos.wrapping_add(out.len() as u64), Ordering::Relaxed);
        self.holes.fetch_add(holes as u64, Ordering::Relaxed);
        Read { holes }
    }

    /// Whether the ring holds every position `pos`, `pos + 1`, … of `n`:
    /// a read of them would find no hole. Marks nothing and moves no
    /// read end (TX's look before it sends ahead, dstack #1).
    pub fn holds(&self, pos: u64, n: usize) -> bool {
        (0..n as u64).all(|i| {
            let p = pos.wrapping_add(i);
            lap_of(self.slot(p).load(Ordering::Relaxed)) == lap(p)
        })
    }

    /// The largest magnitude stored since the last call (0 for none).
    pub fn take_peak(&self) -> u32 {
        self.peak.swap(0, Ordering::Relaxed)
    }

    pub fn counters(&self) -> Counters {
        Counters {
            written: self.written.load(Ordering::Relaxed),
            holes: self.holes.load(Ordering::Relaxed),
            late: self.late.load(Ordering::Relaxed),
            duplicate: self.duplicate.load(Ordering::Relaxed),
            early: self.early.load(Ordering::Relaxed),
        }
    }

    fn slot(&self, p: u64) -> &AtomicU64 {
        &self.slots[p as usize % SLOTS]
    }
}

/// The bit above the lap that marks a slot the reader found empty.
pub const READ_EMPTY: u32 = 1 << 31;

/// A slot no write has stored. No write makes this word (its bit 63 is
/// clear), nor a read-empty mark (its low 32 bits are 0).
const EMPTY: u64 = u64::MAX;

/// The lap of position `p`: which time round the ring it is, mod 2³¹.
fn lap(p: u64) -> u32 {
    (p / SLOTS as u64) as u32 & (READ_EMPTY - 1)
}

/// Whether slot word `w` holds a later lap than position `p`'s (up to 2³⁰
/// laps on, mod 2³¹): a read of `p` then leaves it alone.
fn newer(w: u64, p: u64) -> bool {
    if w == EMPTY {
        return false;
    }
    let ahead = (lap_of(w) & (READ_EMPTY - 1)).wrapping_sub(lap(p)) & (READ_EMPTY - 1);
    (1..1 << 30).contains(&ahead)
}

/// Position `p`'s slot after a read found it empty.
fn read_empty(p: u64) -> u64 {
    u64::from(lap(p) + READ_EMPTY) << 32
}

/// The lap a slot's word holds.
fn lap_of(w: u64) -> u32 {
    (w >> 32) as u32
}

/// A slot's word: the lap above, the sample below.
fn word(p: u64, sample: i32) -> u64 {
    (u64::from(lap(p)) << 32) + u64::from(sample as u32)
}

#[cfg(test)]
mod tests;

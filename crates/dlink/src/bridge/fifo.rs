// SPDX-License-Identifier: MIT OR Apache-2.0

//! The FIFO between the dlink pump (the producer, on the network's clock)
//! and the output callback (the consumer, on the output's clock): one
//! writer and one reader, lock-free and allocation-free on both sides, so
//! neither thread ever waits for the other.
//!
//! The samples are `f32` bits in atomics, interleaved by frame: safe code
//! with no `unsafe` cell. The writer publishes a frame count with
//! `Release` after its stores, the reader takes it with `Acquire` before
//! its loads (and the other way round for the frames read), so a reader
//! never sees a frame before its samples.
//!
//! A full FIFO drops the frames that do not fit (the newest) and counts
//! them in [`Fifo::overflows`]: the reader's position is never moved by the
//! writer, which would race it.
//!
//! **Write stamps** (dstack #303): each write records when it came, on the
//! FIFO's [`Clock`] (the app's clock, [`dstack::clock::app_monotonic_ns`];
//! a simulation sets its own with [`Fifo::with_clock`]), with the counts it
//! brought ([`Stamp`]). The player measures its depth against the line
//! through the writes ([`super::player`]), not against a sample of the
//! sawtooth a chunked writer makes. The stamp is a seqlock of four words:
//! no allocation and no lock on either side; a reader that meets a write
//! in progress tries again a few times, then goes without.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering, fence};

/// The clock a [`Fifo`] stamps its writes on and its reader reads, ns:
/// the app's clock ([`dstack::clock::app_monotonic_ns`]) unless set.
pub type Clock = Arc<dyn Fn() -> i64 + Send + Sync>;

/// A FIFO's last write ([`Fifo::last_write`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    /// When it came, ns of the FIFO's [`Clock`].
    pub at_ns: i64,
    /// The frames written (stored) up to and with it.
    pub written: u64,
    /// The frames offered up to and with it, dropped ones too.
    pub arrived: u64,
    /// The writes so far, it included (each of at least one frame).
    pub writes: u64,
}

impl Stamp {
    /// The frames a write offered, on average.
    pub fn mean_write(&self) -> f64 {
        self.arrived as f64 / self.writes.max(1) as f64
    }
}

/// How often a reader tries the stamp while a write is in progress.
const STAMP_TRIES: usize = 4;

/// The clock's name in `Debug`.
struct ClockName(Clock);

impl std::fmt::Debug for ClockName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Clock")
    }
}

/// The FIFO (module doc).
#[derive(Debug)]
pub struct Fifo {
    channels: usize,
    capacity: usize,
    data: Box<[AtomicU32]>,
    /// Frames written so far (the writer's position).
    written: AtomicU64,
    /// Frames read or skipped so far (the reader's position).
    read: AtomicU64,
    /// Frames the producer offered, dropped ones too: the producer's clock.
    arrived: AtomicU64,
    /// Frames dropped on a full FIFO.
    overflows: AtomicU64,
    /// The largest magnitude stored since the last [`Fifo::take_peak`]
    /// (`f32` bits; a non-negative `f32`'s bits order as its value).
    peak: AtomicU32,
    /// The last write's [`Stamp`], behind a sequence word: odd while the
    /// writer changes it.
    stamp_seq: AtomicU64,
    stamp_at: AtomicU64,
    stamp_written: AtomicU64,
    stamp_arrived: AtomicU64,
    stamp_writes: AtomicU64,
    clock: ClockName,
}

impl Fifo {
    /// A FIFO of `capacity` frames of `channels` channels (each at least 1).
    pub fn new(channels: usize, capacity: usize) -> Self {
        let channels = channels.max(1);
        let capacity = capacity.max(1);
        Fifo {
            channels,
            capacity,
            data: (0..channels * capacity)
                .map(|_| AtomicU32::new(0))
                .collect(),
            written: AtomicU64::new(0),
            read: AtomicU64::new(0),
            arrived: AtomicU64::new(0),
            overflows: AtomicU64::new(0),
            peak: AtomicU32::new(0),
            stamp_seq: AtomicU64::new(0),
            stamp_at: AtomicU64::new(0),
            stamp_written: AtomicU64::new(0),
            stamp_arrived: AtomicU64::new(0),
            stamp_writes: AtomicU64::new(0),
            clock: ClockName(Arc::new(dstack::clock::app_monotonic_ns)),
        }
    }

    /// The same FIFO stamping its writes on `clock` (a simulation's time;
    /// module doc). Set it before the first write.
    pub fn with_clock(mut self, clock: Clock) -> Self {
        self.clock = ClockName(clock);
        self
    }

    /// The FIFO's clock now, ns.
    pub fn now_ns(&self) -> i64 {
        (self.clock.0)()
    }

    /// The last write's stamp: `None` before the first write, or when a
    /// write was in progress on every try (module doc).
    pub fn last_write(&self) -> Option<Stamp> {
        for _ in 0..STAMP_TRIES {
            let seq = self.stamp_seq.load(Ordering::Acquire);
            let stamp = Stamp {
                at_ns: self.stamp_at.load(Ordering::Relaxed) as i64,
                written: self.stamp_written.load(Ordering::Relaxed),
                arrived: self.stamp_arrived.load(Ordering::Relaxed),
                writes: self.stamp_writes.load(Ordering::Relaxed),
            };
            fence(Ordering::Acquire);
            if seq.is_multiple_of(2) && self.stamp_seq.load(Ordering::Relaxed) == seq {
                return (stamp.writes > 0).then_some(stamp);
            }
            std::hint::spin_loop();
        }
        None
    }

    /// The frames read or skipped so far (the reader's position).
    pub fn read_frames(&self) -> u64 {
        self.read.load(Ordering::Acquire)
    }

    /// The writer: records a write, now, that brings the stored count to
    /// `written` and the offered one to `arrived` (module doc). Called
    /// before `written` is published, so a reader that took a frame of this
    /// write sees its stamp too.
    fn stamp(&self, written: u64, arrived: u64) {
        let at = self.now_ns();
        self.stamp_seq.fetch_add(1, Ordering::Relaxed);
        fence(Ordering::Release);
        self.stamp_at.store(at as u64, Ordering::Relaxed);
        self.stamp_written.store(written, Ordering::Relaxed);
        self.stamp_arrived.store(arrived, Ordering::Relaxed);
        self.stamp_writes.fetch_add(1, Ordering::Relaxed);
        self.stamp_seq.fetch_add(1, Ordering::Release);
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// The most frames it holds.
    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// The frames it holds now.
    pub fn len(&self) -> usize {
        let read = self.read.load(Ordering::Acquire);
        let written = self.written.load(Ordering::Acquire);
        usize::try_from(written - read).unwrap_or(usize::MAX)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Frames the producer offered so far, the dropped ones too.
    pub fn arrived(&self) -> u64 {
        self.arrived.load(Ordering::Relaxed)
    }

    /// Frames dropped on a full FIFO so far.
    pub fn overflows(&self) -> u64 {
        self.overflows.load(Ordering::Relaxed)
    }

    /// The largest magnitude of the samples stored since the last call
    /// (dstack #184: a log line shows whether its producer hands over
    /// silence); 0 when none or only silence.
    pub fn take_peak(&self) -> f32 {
        f32::from_bits(self.peak.swap(0, Ordering::Relaxed))
    }

    fn slot(&self, frame: u64) -> usize {
        // The remainder is under `capacity`, a usize.
        (frame % self.capacity as u64) as usize * self.channels
    }

    /// The producer: writes `frames` frames, channel `c` of frame `f` being
    /// `sample(f, c)`, as many as fit (the rest is dropped and counted),
    /// stamped on the FIFO's clock (module doc). Returns the frames
    /// written. Only one thread may write.
    pub fn push_with(&self, frames: usize, sample: impl Fn(usize, usize) -> f64) -> usize {
        let arrived = self.arrived.fetch_add(frames as u64, Ordering::Relaxed) + frames as u64;
        let written = self.written.load(Ordering::Relaxed);
        let read = self.read.load(Ordering::Acquire);
        let held = usize::try_from(written - read).unwrap_or(usize::MAX);
        let n = frames.min(self.capacity.saturating_sub(held));
        let mut peak = 0.0f32;
        for f in 0..n {
            let at = self.slot(written + f as u64);
            for c in 0..self.channels {
                let s = sample(f, c) as f32;
                // `max` leaves a NaN out.
                peak = peak.max(s.abs());
                self.data[at + c].store(s.to_bits(), Ordering::Relaxed);
            }
        }
        self.peak.fetch_max(peak.to_bits(), Ordering::Relaxed);
        if frames > 0 {
            self.stamp(written + n as u64, arrived);
        }
        self.written.store(written + n as u64, Ordering::Release);
        self.overflows
            .fetch_add((frames - n) as u64, Ordering::Relaxed);
        n
    }

    /// The consumer: takes `frames` frames into `out`, channel-major
    /// (channel `c` at `c * frames ..`), when it holds that many; else it
    /// takes nothing and returns false. Only one thread may read.
    pub fn pop_planar(&self, out: &mut [f64], frames: usize) -> bool {
        if self.len() < frames || out.len() < frames * self.channels {
            return false;
        }
        let read = self.read.load(Ordering::Relaxed);
        for f in 0..frames {
            let at = self.slot(read + f as u64);
            for c in 0..self.channels {
                out[c * frames + f] =
                    f64::from(f32::from_bits(self.data[at + c].load(Ordering::Relaxed)));
            }
        }
        self.read.store(read + frames as u64, Ordering::Release);
        true
    }

    /// The consumer: drops up to `frames` of the oldest frames; returns how
    /// many it dropped.
    pub fn skip(&self, frames: usize) -> usize {
        let n = frames.min(self.len());
        self.read.fetch_add(n as u64, Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod stamp_tests;

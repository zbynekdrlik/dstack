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

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

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
        }
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
    /// `sample(f, c)`, as many as fit (the rest is dropped and counted).
    /// Returns the frames written. Only one thread may write.
    pub fn push_with(&self, frames: usize, sample: impl Fn(usize, usize) -> f64) -> usize {
        self.arrived.fetch_add(frames as u64, Ordering::Relaxed);
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

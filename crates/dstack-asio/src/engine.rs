// SPDX-License-Identifier: MIT OR Apache-2.0

//! The buffer switch, pure (dstack #243): what one driver callback does
//! with the driver's half-buffers, so the Windows glue only hands it the
//! pointers.
//!
//! Per buffer switch: each used input is decoded into a channel-major `f64`
//! block ([`Engine::decode_input`]), the app's [`Process`] runs on it
//! ([`Engine::run`]) and each driver output is encoded from the app's
//! output channel mapped to it, or zeroed ([`Engine::encode_output`]); the
//! callback is counted and timed in [`Counters`]. Nothing allocates, locks,
//! logs or makes a system call: every buffer is made in [`Engine::new`].
//!
//! A `Process` that panics is caught at this boundary: that block and every
//! later one play silence (the app's state may be broken), the panic counted
//! once in [`Counters::faults`]; the runner closes the driver for it and
//! starts a fresh `Process` after the backoff. Only that fault path leaves
//! the rules above: the default panic hook prints (and allocates) on the
//! driver's thread.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use dstack_audio_io::{Block, Process};

use crate::device::Opened;
use crate::format::Sample;
use crate::state::{GapKind, callback_gap_kind, frames_to_ns};

/// A run's counts, written by its buffer switches and read by the worker.
#[derive(Debug)]
pub struct Counters {
    callbacks: AtomicU64,
    consumed: AtomicU64,
    faults: AtomicU64,
    late: AtomicU64,
    fast: AtomicU64,
    /// The driver's buffer period, ns.
    period_ns: i64,
    /// The last buffer switch, ns on the caller's clock ([`NO_CALLBACK`]
    /// before the first).
    last_ns: AtomicI64,
    /// The longest gap between two buffer switches since the last
    /// [`Counters::take_gap_max_ns`].
    gap_max_ns: AtomicI64,
}

/// `last_ns` before the run's first buffer switch.
const NO_CALLBACK: i64 = i64::MIN;

impl Counters {
    fn new(period_ns: i64) -> Self {
        Counters {
            callbacks: AtomicU64::new(0),
            consumed: AtomicU64::new(0),
            faults: AtomicU64::new(0),
            late: AtomicU64::new(0),
            fast: AtomicU64::new(0),
            period_ns,
            last_ns: AtomicI64::new(NO_CALLBACK),
            gap_max_ns: AtomicI64::new(0),
        }
    }

    /// Buffer switches so far.
    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(Ordering::Relaxed)
    }

    /// Frames the driver took so far (a buffer a switch).
    pub fn consumed(&self) -> u64 {
        self.consumed.load(Ordering::Relaxed)
    }

    /// Panics of the app's `Process` (0 or 1: it is never called again).
    pub fn faults(&self) -> u64 {
        self.faults.load(Ordering::Relaxed)
    }

    /// Buffer switches more than two periods after the one before.
    pub fn late(&self) -> u64 {
        self.late.load(Ordering::Relaxed)
    }

    /// Buffer switches under a quarter period after the one before.
    pub fn fast(&self) -> u64 {
        self.fast.load(Ordering::Relaxed)
    }

    /// The longest gap since the last call, ns (0 when none came after
    /// another); starts the next interval.
    pub fn take_gap_max_ns(&self) -> i64 {
        self.gap_max_ns.swap(0, Ordering::Relaxed)
    }

    /// A buffer switch at `now_ns`: the gap since the one before is kept as
    /// the interval's max and counted by its kind. Atomics only.
    pub fn note_callback(&self, now_ns: i64) {
        let before = self.last_ns.swap(now_ns, Ordering::Relaxed);
        if before == NO_CALLBACK {
            return;
        }
        let gap = now_ns - before;
        self.gap_max_ns.fetch_max(gap, Ordering::Relaxed);
        match callback_gap_kind(gap, self.period_ns) {
            GapKind::Late => {
                self.late.fetch_add(1, Ordering::Relaxed);
            }
            GapKind::Fast => {
                self.fast.fetch_add(1, Ordering::Relaxed);
            }
            GapKind::Normal => {}
        }
    }
}

/// One run's buffer switch (module doc).
pub struct Engine {
    process: Box<dyn Process>,
    frames: usize,
    input_sample: Option<Sample>,
    output_sample: Sample,
    /// The used inputs, channel-major.
    input: Vec<f64>,
    /// The app's outputs, channel-major.
    output: Vec<f64>,
    /// Per driver output channel: the app's output it plays, if any.
    output_of: Vec<Option<usize>>,
    /// The next block is the run's first (the app's `discontinuity`).
    first: bool,
    /// The app panicked: silence from then on.
    faulted: bool,
    counters: Arc<Counters>,
}

impl Engine {
    /// The buffer switch of a driver opened as `opened` (its inputs read
    /// and outputs used), running `process`.
    pub fn new(process: Box<dyn Process>, opened: &Opened) -> Self {
        let frames = opened.buffer_frames as usize;
        let mut output_of = vec![None; opened.driver_outputs as usize];
        for (i, &c) in opened.outputs.iter().enumerate() {
            if let Some(slot) = output_of.get_mut(c as usize) {
                *slot = Some(i);
            }
        }
        Engine {
            process,
            frames,
            input_sample: opened.input_sample,
            output_sample: opened.output_sample,
            input: vec![0.0; opened.inputs.len() * frames],
            output: vec![0.0; opened.outputs.len() * frames],
            output_of,
            first: true,
            faulted: false,
            counters: Arc::new(Counters::new(frames_to_ns(
                i64::from(opened.buffer_frames),
                opened.rate,
            ))),
        }
    }

    /// The run's counts (the worker keeps a clone).
    pub fn counters(&self) -> Arc<Counters> {
        Arc::clone(&self.counters)
    }

    /// This buffer switch came at `now_ns` (a monotonic clock): timed
    /// against the one before ([`Counters::note_callback`]).
    pub fn note_callback(&self, now_ns: i64) {
        self.counters.note_callback(now_ns);
    }

    /// Frames a buffer switch takes.
    pub fn frames(&self) -> usize {
        self.frames
    }

    /// The used inputs.
    pub fn inputs(&self) -> usize {
        self.input.len().checked_div(self.frames).unwrap_or(0)
    }

    /// The driver's outputs.
    pub fn driver_outputs(&self) -> usize {
        self.output_of.len()
    }

    /// The used input `i`'s half-buffer `bytes` (the driver's samples; `&[]`
    /// when the driver gave none: silence).
    pub fn decode_input(&mut self, i: usize, bytes: &[u8]) {
        let (Some(sample), Some(dst)) = (
            self.input_sample,
            self.input.get_mut(i * self.frames..(i + 1) * self.frames),
        ) else {
            return;
        };
        sample.decode(bytes, dst);
    }

    /// The app's `Process` on the decoded inputs: counted as a buffer switch
    /// of `frames()` frames; a panic is caught (module doc).
    pub fn run(&mut self) {
        let Engine {
            process,
            frames,
            input,
            output,
            first,
            faulted,
            counters,
            ..
        } = self;
        if *faulted {
            output.fill(0.0);
        } else {
            let ran = catch_unwind(AssertUnwindSafe(|| {
                if std::mem::take(first) {
                    process.discontinuity();
                }
                process.process(&mut Block::new(*frames, input, output));
            }));
            if ran.is_err() {
                *faulted = true;
                output.fill(0.0);
                counters.faults.fetch_add(1, Ordering::Relaxed);
            }
        }
        counters
            .consumed
            .fetch_add(*frames as u64, Ordering::Relaxed);
        counters.callbacks.fetch_add(1, Ordering::Relaxed);
    }

    /// Driver output `ch`'s half-buffer `bytes`: the app's output mapped to
    /// it, or silence.
    pub fn encode_output(&self, ch: usize, bytes: &mut [u8]) {
        let src = match self.output_of.get(ch).copied().flatten() {
            Some(o) => &self.output[o * self.frames..(o + 1) * self.frames],
            None => &[][..],
        };
        self.output_sample.encode(src, bytes);
    }
}

#[cfg(test)]
mod tests;

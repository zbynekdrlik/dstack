// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The TX pull (dstack #363, part of #346; the design approved 17.11.2026):
//! the TX thread fills the engine rings of an app's pulled TX channels
//! itself, so a late pump no longer leaves sent stamps empty.
//!
//! Up to v0.6.162 the engine rings had one writer, the pump: block k's
//! outputs reach the ring at kB + D (D = B + P + W) only when its
//! processor returns, and the TX thread reads each stamp once, when its
//! packet is due (W + fpp + tx_margin after the block's due time). A block
//! that woke within W and ended past that left its stamps silent
//! (`writes_after_send`, #346); a wake past W re-bootstrapped and skipped
//! blocks (TX holes). For an app whose TX audio runs on its own clock
//! (dsend's capture, OBS's mix) that audio sat in its FIFO all along: only
//! the pump carrying it was late.
//!
//! Now such an app hands the device a [`Process`] for its TX channels
//! (dlink's `bridge::TxPull`: its Player, behind `Rated`'s TX converter
//! when the device converts), claimed once before the TX thread starts
//! ([`TxShared::claim_fill`]; the pump's map then has none of those
//! channels, so each engine ring keeps one writer). Each pass the TX thread:
//!
//! - fills every block that is due, in whole blocks of the device's B:
//!   block k once media now >= kB + B + P (the pump's own due time,
//!   [`Blocks`] on the device's [`Timing`]), its outputs quantized at the
//!   device's bits into the rings at kB + D less the engine shift S (the
//!   pump's own stamp and line, [`super::shift`]);
//! - then sends what is due.
//!
//! So each stamp is written at the moment the pump writes it on time, W +
//! fpp + tx_margin before its send, and no stated latency term moves (D,
//! the app buffer's pop time relative to each stamp, P, tx_margin, the
//! advertised latency). A TX thread that is itself late fills before it
//! sends in the same pass: its own sends never read an unfilled stamp. It
//! catches up blocks up to W + the advertised latency behind (past that
//! the flows have restarted anyway): further behind, the schedule starts
//! again (counted, the blocks between skipped) and the processor hears a
//! `discontinuity` before its next block, as after a pump re-bootstrap;
//! the player in it resyncs once on the start that does not follow on. A
//! start or restart begins as the pump's bootstrap does (the block due
//! now, no catch-up), so a TX flow that starts or starts again with it
//! sends W of holes first, as on the pump's path. A re-anchor moves
//! or keeps the schedule by S's change within W, as the pump's; a new
//! latency P (Dante Controller) starts it again at the new playout point;
//! a new advertised latency only moves the reach. While the clock is not
//! ready nothing is filled; the next ready pass starts afresh.
//!
//! A TX computed from RX in the same block (iemmixer's mix) cannot be
//! pulled (its audio does not exist before the block runs): it keeps the
//! pump path and its `writes_after_send`.
//!
//! The fill is the TX thread's hot path: no allocation (buffers sized at
//! the claim), no lock, no log, no system call; [`TxFill::block`] carries
//! the rtsan attribute. A processor that panics is never called again
//! (its blocks silent, counted), as the pump's.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use dstack_audio_io::{Block, Process, panic_message};

use super::{Ring, Shift, TxShared, ns_of};
use crate::media::rx::RxShared;
use crate::stream::{Blocks, Due, Timing, quantize};

mod watch;
pub use watch::{FillWatch, TX_PULL_WARNING};

/// The TX pull's counts: written by the TX thread, read by the status and
/// the watch ([`FillWatch`]).
#[derive(Debug, Default)]
pub struct TxFillStats {
    blocks: AtomicU64,
    rebootstraps: AtomicU64,
    skipped: AtomicU64,
    writes_after_send: AtomicU64,
    faults: AtomicU64,
    /// The processor's panic message (its one panic: never called again);
    /// written once, on the panic's path only.
    fault: Mutex<Option<String>>,
}

/// [`TxFillStats`] as the status shows them (`tx_pull`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TxFillView {
    /// Blocks filled.
    pub blocks: u64,
    /// Starts again: a TX thread more than W + the latency behind, a clock
    /// that went back, a re-anchor off the block grid, a new latency.
    pub rebootstraps: u64,
    /// Blocks those starts skipped (their stamps sent as holes).
    pub skipped: u64,
    /// Blocks written after the TX thread had sent some of their stamps
    /// (never, by the fill's order; a count to prove it).
    pub writes_after_send: u64,
    /// Blocks the processor panicked in (it is never called again).
    pub faults: u64,
}

impl TxFillStats {
    /// The counts.
    pub fn view(&self) -> TxFillView {
        TxFillView {
            blocks: self.blocks.load(Ordering::Relaxed),
            rebootstraps: self.rebootstraps.load(Ordering::Relaxed),
            skipped: self.skipped.load(Ordering::Relaxed),
            writes_after_send: self.writes_after_send.load(Ordering::Relaxed),
            faults: self.faults.load(Ordering::Relaxed),
        }
    }

    /// The processor's panic message, once it panicked.
    pub fn fault(&self) -> Option<String> {
        self.fault
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn rebootstrapped(&self, skipped: u64) {
        self.rebootstraps.fetch_add(1, Ordering::Relaxed);
        self.skipped.fetch_add(skipped, Ordering::Relaxed);
    }

    /// The processor panicked with `message` (the panic's path, never a
    /// block's: it allocates and locks once).
    fn faulted(&self, message: String) {
        *self.fault.lock().unwrap_or_else(PoisonError::into_inner) = Some(message);
        self.faults.fetch_add(1, Ordering::Relaxed);
    }
}

/// Why [`TxShared::claim_fill`] refused a TX pull, and the pull itself.
#[derive(Debug)]
pub struct Refused {
    /// The reason, for an error line.
    pub reason: &'static str,
    pub fill: Box<TxFill>,
}

/// [`Refused::reason`]: a channel of the pull has no engine ring.
pub const NO_RING: &str = "a TX channel of the pull has no engine ring";
/// [`Refused::reason`]: a pull was claimed already.
pub const CLAIMED: &str = "the device has a TX pull already";
/// [`Refused::reason`]: the TX thread has built its state.
pub const BUILT: &str = "the TX thread was built before the app's TX pull";

impl TxShared {
    /// Hands the TX thread the app's TX pull (dstack #363, [`TxFill`]): it
    /// fills `fill`'s channels' engine rings itself from then on. Once,
    /// before the TX thread is built (the embedded device's start does it
    /// before the host starts); refused ([`Refused`], `fill` back) for a
    /// channel without an engine ring ([`NO_RING`]), a second claim
    /// ([`CLAIMED`]) or after the TX thread was built ([`BUILT`]). It does
    /// not know the pump's map: a caller keeps the pulled channels out of
    /// it ([`engine_channels`], as [`crate::embed::Embedded::start_tx_pulling`]
    /// does), or a ring gets two writers.
    pub fn claim_fill(&self, fill: TxFill) -> Result<(), Refused> {
        // Under the control lock, as the TX thread's state is built
        // ([`TxShared::host_for_tx`]): seen by it, or refused.
        let _built = self.lock();
        let rings = fill
            .channels()
            .iter()
            .all(|&tx| self.engine_ring(tx).is_some());
        let reason = if !rings {
            Some(NO_RING)
        } else if self.fill_stats.get().is_some() {
            Some(CLAIMED)
        } else if self.tx_built.load(Ordering::Relaxed) {
            Some(BUILT)
        } else {
            None
        };
        if let Some(reason) = reason {
            return Err(Refused {
                reason,
                fill: Box::new(fill),
            });
        }
        let _ = self.fill_stats.set(Arc::clone(fill.stats()));
        *self.fill.lock().unwrap_or_else(PoisonError::into_inner) = Some(fill);
        Ok(())
    }

    /// The claimed TX pull for the TX thread's state being built now
    /// (after [`TxShared::host_for_tx`], which closes the claims).
    pub(super) fn fill_for_tx(&self) -> Option<TxFill> {
        self.fill
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// The TX pull's counts (dstack #363); `None` without one.
    pub fn tx_fill(&self) -> Option<TxFillView> {
        self.fill_stats.get().map(|s| s.view())
    }

    /// The TX pull's counts themselves (the watch's), `None` without one.
    pub fn tx_fill_stats(&self) -> Option<&Arc<TxFillStats>> {
        self.fill_stats.get()
    }
}

/// The TX channels with an engine ring: the pump's `pumped` and the TX
/// pull's `pulled` (from 0, of a device of `tx` TX channels); refused
/// (the reason) for a channel past the device's or in both.
pub fn engine_channels(
    pumped: &[usize],
    pulled: &[usize],
    tx: usize,
) -> Result<Vec<usize>, String> {
    if let Some(&c) = pumped.iter().chain(pulled).find(|&&c| c >= tx) {
        return Err(format!(
            "the app's TX channel {} is past the device's {tx} TX channels",
            c + 1
        ));
    }
    let mut all = pumped.to_vec();
    for &c in pulled {
        if all.contains(&c) {
            return Err(format!("the app's TX channel {} is named twice", c + 1));
        }
        all.push(c);
    }
    Ok(all)
}

/// The TX thread's fill of an app's pulled TX channels (module doc).
pub struct TxFill {
    processor: Box<dyn Process>,
    /// Output j fills TX channel `channels[j]`'s engine ring.
    channels: Vec<usize>,
    /// B, P (followed live: [`RxShared::playout`]) and W.
    timing: Timing,
    bits: u8,
    rx: Arc<RxShared>,
    stats: Arc<TxFillStats>,
    frames: usize,
    /// The processor's outputs, channel-major.
    output: Vec<f64>,
    /// One channel's samples of a block.
    samples: Vec<i32>,
    /// `None` until the first ready pass, after a new latency, and while
    /// the clock is not ready.
    blocks: Option<Blocks>,
    /// How far behind a block's due time it is still filled, samples: W +
    /// the advertised latency (the schedule's margin).
    reach: u64,
    /// `Process::discontinuity` is due before the next block.
    discontinuity: bool,
    /// The processor panicked: never called again.
    faulted: bool,
}

impl std::fmt::Debug for TxFill {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxFill")
            .field("channels", &self.channels)
            .field("timing", &self.timing)
            .field("bits", &self.bits)
            .field("blocks", &self.blocks)
            .field("reach", &self.reach)
            .field("faulted", &self.faulted)
            .finish_non_exhaustive()
    }
}

impl TxFill {
    /// `processor` (no inputs; one output a channel of `channels`, TX
    /// channels from 0) run on the TX thread at the device's `timing`
    /// (its block, its latency P as the pump's, followed live through
    /// `rx`'s [`RxShared::playout`], its work margin W), quantized at
    /// `bits`. Every buffer is allocated here; hand it over with
    /// [`super::TxShared::claim_fill`].
    pub fn new(
        processor: Box<dyn Process>,
        channels: Vec<usize>,
        timing: Timing,
        bits: u8,
        rx: Arc<RxShared>,
    ) -> TxFill {
        let frames = usize::try_from(timing.block).unwrap_or(usize::MAX);
        TxFill {
            output: vec![0.0; channels.len() * frames],
            samples: vec![0; frames],
            processor,
            channels,
            timing,
            bits,
            rx,
            stats: Arc::default(),
            frames,
            blocks: None,
            reach: timing.margin,
            discontinuity: false,
            faulted: false,
        }
    }

    /// The TX channels it fills (from 0).
    pub fn channels(&self) -> &[usize] {
        &self.channels
    }

    /// Its counts.
    pub fn stats(&self) -> &Arc<TxFillStats> {
        &self.stats
    }

    /// The schedule's timing: the device's, its margin the reach.
    fn schedule(&self) -> Timing {
        Timing {
            margin: self.reach,
            ..self.timing
        }
    }

    /// When the next block is due at `rate`, PTP ns (`None`: no schedule
    /// yet, the next ready pass starts it).
    pub fn wake_ns(&self, rate: u32) -> Option<u64> {
        self.blocks.map(|b| ns_of(b.due(), rate))
    }

    /// The clock is not ready: nothing is filled, and the next ready pass
    /// starts afresh.
    pub fn idle(&mut self) {
        self.blocks = None;
    }

    /// A re-anchor at media time `now` with the device's `shift` (the
    /// change of S; `None`: the step is unknown), as the pump's
    /// ([`Blocks::re_anchor`]): a restart is counted and the processor
    /// hears a discontinuity.
    pub fn re_anchor(&mut self, shift: Option<i64>, now: u64) {
        let Some(blocks) = self.blocks.as_mut() else {
            return;
        };
        // Moved or kept within W, as the pump's schedule is, so the fill
        // and the pump stay on one line (the reach is only the catch-up's).
        let before = blocks.start();
        let mut within_w = blocks.with_margin(self.timing.margin);
        let (outcome, _) = within_w.re_anchor(shift, now);
        *blocks = within_w.with_margin(self.reach);
        if outcome == Shift::Restarted {
            let fresh = self.fresh(now);
            self.stats.rebootstrapped(self.skipped(before, &fresh));
            self.blocks = Some(fresh);
            self.discontinuity = true;
        }
    }

    /// The schedule started (again) at media time `now`, as the pump's
    /// bootstrap: the first block is the one whose input ends at the
    /// playout point, no catch-up. Starting earlier (the block stamped now,
    /// due W ago) would fill the W of stamps a TX restart sends first, but
    /// pop W of audio in one pass right after the player's resync prime
    /// and leave its FIFO W under the target until the controller brakes
    /// it back (dstack #363's review): the app buffer short, a stated term.
    fn fresh(&self, now: u64) -> Blocks {
        Blocks::bootstrap(now, self.schedule())
    }

    /// The blocks between `before` (the next start) and `fresh`'s start
    /// that never run (0 when it is not later).
    fn skipped(&self, before: u64, fresh: &Blocks) -> u64 {
        fresh.start().saturating_sub(before) / self.timing.block
    }

    /// Every block due at media time `now` into the `engine` rings (each TX
    /// channel's, [`super::TxShared`]'s) on the line of the engine shift
    /// `shift`, with the flows' lag bound `max_lag` (samples: the
    /// longest of the advertised latency and the receivers' stated ones,
    /// dstack #426). A new P starts the schedule again at the new
    /// playout point (counted), as the pump's; a new bound only moves the
    /// reach (the blocks go on).
    pub fn fill_due(&mut self, now: u64, max_lag: u64, engine: &[Option<Ring>], shift: i64) {
        let playout = self.rx.playout().unwrap_or(self.timing.playout);
        if playout != self.timing.playout {
            self.timing.playout = playout;
            if self.blocks.take().is_some() {
                self.stats.rebootstrapped(0);
            }
        }
        let reach = self.timing.margin.saturating_add(max_lag);
        if reach != self.reach {
            self.reach = reach;
            self.blocks = self.blocks.map(|b| b.with_margin(reach));
        }
        let mut blocks = match self.blocks {
            Some(b) => b,
            None => {
                self.discontinuity = true;
                self.fresh(now)
            }
        };
        loop {
            let before = blocks.start();
            match blocks.next(now) {
                Due::Run { start, .. } => self.block(start, engine, shift),
                Due::Wait { .. } => break,
                Due::Rebootstrap { .. } => {
                    blocks = self.fresh(now);
                    self.stats.rebootstrapped(self.skipped(before, &blocks));
                    self.discontinuity = true;
                }
            }
        }
        self.blocks = Some(blocks);
    }

    /// The block whose stamps (on the input side) start at `start`: the
    /// processor's outputs quantized into their channels' `engine` rings
    /// at `start + D`, less `shift` (the real-time part).
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    fn block(&mut self, start: u64, engine: &[Option<Ring>], shift: i64) {
        self.run(start);
        let at = (start + self.timing.offset()).wrapping_add_signed(shift.wrapping_neg());
        let mut after_send = 0;
        for (chan, &tx) in self.output.chunks_exact(self.frames).zip(&self.channels) {
            if let Some(ring) = engine.get(tx).and_then(Option::as_ref) {
                for (q, &y) in self.samples.iter_mut().zip(chan) {
                    *q = quantize(y, self.bits);
                }
                after_send += ring.put_at(at, &self.samples);
            }
        }
        self.stats.blocks.fetch_add(1, Ordering::Relaxed);
        if after_send > 0 {
            self.stats.writes_after_send.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The processor on the block's outputs (zeroed first, a pending
    /// discontinuity before it) inside `catch_unwind`; a panic zeroes them
    /// and the processor is never called again.
    fn run(&mut self, start: u64) {
        self.output.fill(0.0);
        if self.faulted {
            return;
        }
        let discontinuity = std::mem::take(&mut self.discontinuity);
        let TxFill {
            processor,
            output,
            frames,
            ..
        } = self;
        let caught = catch_unwind(AssertUnwindSafe(|| {
            if discontinuity {
                processor.discontinuity();
            }
            processor.process(&mut Block::new(*frames, &[], output).at(start));
        }));
        if let Err(payload) = caught {
            self.output.fill(0.0);
            self.faulted = true;
            self.stats.faulted(panic_message(&*payload));
        }
    }
}

#[cfg(test)]
mod tests;

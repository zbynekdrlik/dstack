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
//! again at now (counted, the blocks between skipped) and the processor
//! hears a `discontinuity` before its next block, as after a pump
//! re-bootstrap; the player in it resyncs once on the start that does not
//! follow on. A re-anchor moves the schedule by S's change, as the pump's;
//! a new latency (Dante Controller) starts it again at the new playout
//! point. While the clock is not ready nothing is filled; the next ready
//! pass starts afresh.
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
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dstack_audio_io::{Block, Process};

use super::{Ring, ns_of};
use crate::media::rx::RxShared;
use crate::stream::{Blocks, Due, Timing, quantize};

/// The TX pull's counts: written by the TX thread, read by the status.
#[derive(Debug, Default)]
pub struct TxFillStats {
    blocks: AtomicU64,
    rebootstraps: AtomicU64,
    skipped: AtomicU64,
    writes_after_send: AtomicU64,
    faults: AtomicU64,
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

    fn rebootstrapped(&self, skipped: u64) {
        self.rebootstraps.fetch_add(1, Ordering::Relaxed);
        self.skipped.fetch_add(skipped, Ordering::Relaxed);
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
        let (outcome, skipped) = blocks.re_anchor(shift, now);
        if outcome == super::Shift::Restarted {
            self.stats.rebootstrapped(skipped);
            self.discontinuity = true;
        }
    }

    /// Every block due at media time `now` into the `engine` rings (each TX
    /// channel's, [`super::TxShared`]'s) on the line of the engine shift
    /// `shift`, with the flows' lag bound `max_lag` (the advertised
    /// latency, samples). A new latency (P or the bound) starts the
    /// schedule again (counted).
    pub fn fill_due(&mut self, now: u64, max_lag: u64, engine: &[Option<Ring>], shift: i64) {
        let playout = self.rx.playout().unwrap_or(self.timing.playout);
        let reach = self.timing.margin.saturating_add(max_lag);
        if playout != self.timing.playout || reach != self.reach {
            self.timing.playout = playout;
            self.reach = reach;
            if self.blocks.take().is_some() {
                self.stats.rebootstrapped(0);
            }
        }
        let mut blocks = match self.blocks {
            Some(b) => b,
            None => {
                self.discontinuity = true;
                Blocks::bootstrap(now, self.schedule())
            }
        };
        loop {
            match blocks.next(now) {
                Due::Run { start, .. } => self.block(start, engine, shift),
                Due::Wait { .. } => break,
                Due::Rebootstrap { skipped, .. } => {
                    self.stats.rebootstrapped(skipped);
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
        if caught.is_err() {
            self.output.fill(0.0);
            self.faulted = true;
            self.stats.faults.fetch_add(1, Ordering::Relaxed);
        }
    }
}

#[cfg(test)]
mod tests;

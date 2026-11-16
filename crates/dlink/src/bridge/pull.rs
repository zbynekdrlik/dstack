// SPDX-License-Identifier: MIT OR Apache-2.0

//! The RX bridge pulled by the thread that has the deadline (dstack #307,
//! the owner's ROZHODNUTÉ of 16.11.2026: a principled fix, no latency
//! term).
//!
//! Up to v0.6.152 the pump wrote dlisten's bridge FIFO block by block and
//! the output callback only read it: a pump that woke late (2 to 8 ms on a
//! loaded PC, a whole-PC stall) let the FIFO run dry before it caught up,
//! though the audio already sat in the RX rings or the flow sockets.
//! Covering that lateness would have cost the bridge's target the pump's
//! lateness (+2 ms; the owner's no).
//!
//! Now the consumer pulls. At each call ([`RxPull::pull`], the top of the
//! output callback) it:
//!
//! - reads the media clock (the device's PTP clock, as the pump reads it)
//!   and drains the flow sockets into the RX rings itself
//!   ([`Intake::drain_into_rings`]: the device's one ring writer, claimed
//!   for the app at its start, dstack #326's single-writer rule;
//!   [`crate::Link::start_pulling`]);
//! - reads the rings from where it stopped up to media now less the
//!   device's latency P, the pump's own playout point (a block that covers
//!   [kB, kB + B) runs once now >= kB + B + P), each channel later by its
//!   flow's delay ([`RxShared::delay`]) and silent while it is not routed,
//!   as the pump reads them. A position no packet filled reads as 0 and is
//!   counted a hole by its ring, as for the pump;
//! - writes those frames into the bridge FIFO, which the [`super::Player`]
//!   then reads through the converter at the same target as before.
//!
//! So the pump is no longer in the bridge's path: its lateness changes
//! nothing there, and the latency stays the same (the same P, the same
//! target). The FIFO's producer is the callback itself, one write a call
//! (the whole read in one push, so the player sees the producer's real
//! cycle): its chunk is the output's period ([`super::AppBuffer::chunk_ns`]).
//! The drain judges a packet late past P plus the time since its last
//! drain, exactly how far this reader may have read (dstack's
//! `media::rx::intake`), not P plus the pump's block.
//!
//! The first call reads nothing (no catch-up, as the pump's bootstrap) and
//! the player primes from there. A gap past [`reach`] (the callback
//! stopped, a clock step) or a clock gone back as far starts again at now
//! (counted, [`PullStats::restarts`]); a smaller step back waits for the
//! clock to come back to where it stopped. While the media clock is not
//! ready the sockets are drained (the packets counted unplaced) and the
//! next ready call starts afresh.
//!
//! Nothing else drains the sockets: while nothing pulls (no output open, a
//! stream that stopped calling back) the packets wait unread, no keepalive
//! goes out, and the device closes the flow as silent and asks for it
//! again ([`crate::Link::start_pulling`]). So pull from the device's start,
//! and when the consumer may stop for a while, call [`RxPull::drain_idle`]
//! from another thread (about once a second) while it does not pull
//! ([`PullStats::pulls`] standing still): it drains and forgets the read
//! position, so the flow stays alive and the next pull starts afresh.
//!
//! [`RxPull::pull`] allocates nothing and takes no lock; its system calls
//! are the clock read and the sockets' non-blocking reads (the drain); the
//! ring read and the FIFO write after them carry the rtsan attribute.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dstack::clock::{PtpClock, to_samples};
use dstack::media::ring::SLOTS;
use dstack::media::rx::{Intake, RxShared};
use dstack::stream::to_f64;

use super::Fifo;

/// The most frames one pull reads (and its scratch holds a channel) for a
/// FIFO of `capacity` frames: the FIFO's capacity, and at most half a
/// ring ([`SLOTS`]), so a read never reaches positions the ring's writer
/// has already lapped (a FIFO of 0.5 s at 192 kHz holds 96 000 frames, a
/// ring 65 536).
pub fn reach(capacity: usize) -> usize {
    capacity.min(SLOTS / 2)
}

/// What a pull does ([`due`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// The first read: nothing yet, the next reads start at `at`.
    Start(u64),
    /// Read positions `from` to `to` (`to` excluded).
    Read { from: u64, to: u64 },
    /// The clock is behind where the last read ended: nothing to read.
    Wait,
    /// Too far from the last read either way: start again at `at`.
    Restart(u64),
}

/// What a pull reads when the last one ended at `next` (`None`: none yet)
/// and the positions due end at `end` (media now less P), reading at most
/// `max` frames at once ([`reach`]): the gap up to `max` is
/// read, a gap past it (or a clock as far back) starts again at `end`.
pub fn due(next: Option<u64>, end: u64, max: u64) -> Due {
    match next {
        None => Due::Start(end),
        Some(n) if end >= n => {
            if end - n > max {
                Due::Restart(end)
            } else {
                Due::Read { from: n, to: end }
            }
        }
        Some(n) => {
            if n - end > max {
                Due::Restart(end)
            } else {
                Due::Wait
            }
        }
    }
}

/// A pull reader's counts, for the app's status thread.
#[derive(Debug, Default)]
pub struct PullStats {
    pulls: AtomicU64,
    restarts: AtomicU64,
    frames: AtomicU64,
    idle_drains: AtomicU64,
}

impl PullStats {
    /// Calls of [`RxPull::pull`]: standing still, nothing pulls.
    pub fn pulls(&self) -> u64 {
        self.pulls.load(Ordering::Relaxed)
    }

    /// Calls of [`RxPull::drain_idle`].
    pub fn idle_drains(&self) -> u64 {
        self.idle_drains.load(Ordering::Relaxed)
    }

    /// Reads started again at now after a gap or a clock step ([`due`]).
    pub fn restarts(&self) -> u64 {
        self.restarts.load(Ordering::Relaxed)
    }

    /// Frames read from the rings into the FIFO.
    pub fn frames(&self) -> u64 {
        self.frames.load(Ordering::Relaxed)
    }
}

/// The RX side of a bridge pulled by its consumer (module doc). Made by
/// [`crate::Link::start_pulling`]; moved to the thread with the deadline,
/// which calls [`RxPull::pull`] before each read of the FIFO.
pub struct RxPull {
    intake: Intake,
    clock: Arc<dyn PtpClock>,
    rx: Arc<RxShared>,
    /// FIFO channel `i` is RX channel `channels[i]`.
    channels: Vec<usize>,
    fifo: Arc<Fifo>,
    /// [`reach`] frames of each channel's samples, channel by channel.
    scratch: Vec<i32>,
    /// [`reach`] of the FIFO: the most frames a pull reads.
    span: usize,
    /// Where the last read ended (`None`: none yet, or the clock was not
    /// ready).
    next: Option<u64>,
    /// P at the start, samples, while the device has no published one.
    playout: u64,
    rate: u32,
    stats: Arc<PullStats>,
}

impl std::fmt::Debug for RxPull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RxPull")
            .field("channels", &self.channels)
            .field("next", &self.next)
            .field("playout", &self.playout)
            .field("rate", &self.rate)
            .finish_non_exhaustive()
    }
}

/// Whether a pull of RX channels `channels` fits a device of `rx` RX
/// channels and a FIFO of `fifo` channels: the reason when not.
pub fn check(channels: &[usize], rx: usize, fifo: usize) -> Result<(), String> {
    if channels.len() != fifo {
        return Err(format!(
            "the bridge FIFO has {fifo} channels for {} RX channels",
            channels.len()
        ));
    }
    match channels.iter().find(|&&c| c >= rx) {
        Some(c) => Err(format!(
            "the app's RX channel {} is past the device's {rx} RX channels",
            c + 1
        )),
        None => Ok(()),
    }
}

impl RxPull {
    /// A reader of `rx`'s rings `channels` into `fifo` on `clock` at
    /// `rate`, writing them through `intake` (the rings' one writer), P
    /// `playout` samples until the device publishes its own. Allocates its
    /// scratch here. An app gets one from [`crate::Link::start_pulling`];
    /// this builds one over an RX side the caller runs itself (the
    /// allocation test, `tests/rx_pull_alloc.rs`).
    pub fn new(
        intake: Intake,
        clock: Arc<dyn PtpClock>,
        rx: Arc<RxShared>,
        channels: Vec<usize>,
        fifo: Arc<Fifo>,
        playout: u64,
        rate: u32,
    ) -> Self {
        let span = reach(fifo.capacity());
        RxPull {
            scratch: vec![0; channels.len() * span],
            span,
            intake,
            clock,
            rx,
            channels,
            fifo,
            next: None,
            playout,
            rate,
            stats: Arc::default(),
        }
    }

    /// The counts, for a status thread.
    pub fn stats(&self) -> Arc<PullStats> {
        Arc::clone(&self.stats)
    }

    /// Drains the flow sockets into the rings and brings the FIFO up to
    /// media now less P (module doc); returns the frames written.
    pub fn pull(&mut self) -> usize {
        self.stats.pulls.fetch_add(1, Ordering::Relaxed);
        let Some(ns) = self.clock.now_ns() else {
            self.intake.drain_into_rings(None);
            self.next = None;
            return 0;
        };
        self.intake.drain_into_rings(Some(ns));
        let playout = self.rx.playout().unwrap_or(self.playout);
        let end = to_samples(ns, self.rate).saturating_sub(playout);
        match due(self.next, end, self.span as u64) {
            Due::Start(at) => {
                self.next = Some(at);
                0
            }
            Due::Restart(at) => {
                self.stats.restarts.fetch_add(1, Ordering::Relaxed);
                self.next = Some(at);
                0
            }
            Due::Wait => 0,
            Due::Read { from, to } => {
                self.next = Some(to);
                self.read(from, to)
            }
        }
    }

    /// For a thread other than the consumer's while the consumer does not
    /// pull (module doc): drains the flow sockets into the rings, so the
    /// flow is heard and kept alive, and forgets the read position, so the
    /// next [`RxPull::pull`] starts afresh at now (no restart counted).
    /// Reads nothing into the FIFO.
    pub fn drain_idle(&mut self) {
        self.stats.idle_drains.fetch_add(1, Ordering::Relaxed);
        self.intake.drain_into_rings(self.clock.now_ns());
        self.next = None;
    }

    /// Reads positions `from` to `to` (at most [`reach`] apart, [`due`]) of
    /// each channel's ring (later by its flow's delay; an unrouted channel
    /// is silence and reads nothing) and writes them into the FIFO in one
    /// push; returns the frames written. No allocation, lock or system
    /// call but the FIFO's stamp (a clock read).
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    fn read(&mut self, from: u64, to: u64) -> usize {
        let rings = self.rx.rings();
        let span = self.span;
        let n = usize::try_from(to - from).map_or(span, |n| n.min(span));
        for (k, &rx) in self.channels.iter().enumerate() {
            let piece = &mut self.scratch[k * span..k * span + n];
            match rings.get(rx).filter(|_| self.rx.routed(rx)) {
                Some(ring) => {
                    ring.read_at(from.saturating_sub(self.rx.delay(rx)), piece);
                }
                None => piece.fill(0),
            }
        }
        let scratch = &self.scratch;
        let written = self.fifo.push_with(n, |f, c| to_f64(scratch[c * span + f]));
        self.stats
            .frames
            .fetch_add(written as u64, Ordering::Relaxed);
        written
    }
}

#[cfg(test)]
mod tests;

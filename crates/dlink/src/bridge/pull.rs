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
//! target). The FIFO's producer is the callback itself, one write a call:
//! its chunk is the output's period ([`super::AppBuffer::chunk_ns`]).
//!
//! The first call reads nothing (no catch-up, as the pump's bootstrap) and
//! the player primes from there. A gap past the FIFO's capacity (the
//! callback stopped, a clock step) or a clock gone back as far starts
//! again at now (counted, [`PullStats::restarts`]); a smaller step back
//! waits for the clock to come back to where it stopped. While the media
//! clock is not ready the sockets are drained (the packets counted
//! unplaced) and the next ready call starts afresh.
//!
//! [`RxPull::pull`] allocates nothing and takes no lock; its system calls
//! are the clock read and the sockets' non-blocking reads (the drain); the
//! ring read and the FIFO write after them carry the rtsan attribute.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use dstack::clock::{PtpClock, to_samples};
use dstack::media::rx::{Intake, RxShared};
use dstack::stream::to_f64;

use super::Fifo;

/// The frames read from the rings in one piece (a stack-free scratch of
/// this many frames a channel, allocated at the start).
pub const PIECE: usize = 256;

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
/// `max` frames at once (the FIFO's capacity): the gap up to `max` is
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
    restarts: AtomicU64,
    frames: AtomicU64,
}

impl PullStats {
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
    /// One piece of each channel's samples, channel by channel.
    scratch: Vec<i32>,
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
    /// scratch here.
    pub(crate) fn new(
        intake: Intake,
        clock: Arc<dyn PtpClock>,
        rx: Arc<RxShared>,
        channels: Vec<usize>,
        fifo: Arc<Fifo>,
        playout: u64,
        rate: u32,
    ) -> Self {
        RxPull {
            scratch: vec![0; channels.len() * PIECE],
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
        let Some(ns) = self.clock.now_ns() else {
            self.intake.drain_into_rings(None);
            self.next = None;
            return 0;
        };
        self.intake.drain_into_rings(Some(ns));
        let playout = self.rx.playout().unwrap_or(self.playout);
        let end = to_samples(ns, self.rate).saturating_sub(playout);
        match due(self.next, end, self.fifo.capacity() as u64) {
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

    /// Reads positions `from` to `to` of each channel's ring (later by its
    /// flow's delay; an unrouted channel is silence and reads nothing) and
    /// writes them into the FIFO, a piece at a time; returns the frames
    /// written. No allocation, lock or system call but the FIFO's stamp
    /// (a clock read).
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    fn read(&mut self, from: u64, to: u64) -> usize {
        let rings = self.rx.rings();
        let mut written = 0;
        let mut pos = from;
        while pos < to {
            let n = usize::try_from(to - pos).map_or(PIECE, |left| left.min(PIECE));
            for (piece, &rx) in self.scratch.chunks_exact_mut(PIECE).zip(&self.channels) {
                let piece = &mut piece[..n];
                match rings.get(rx).filter(|_| self.rx.routed(rx)) {
                    Some(ring) => {
                        ring.read_at(pos.saturating_sub(self.rx.delay(rx)), piece);
                    }
                    None => piece.fill(0),
                }
            }
            let scratch = &self.scratch;
            written += self
                .fifo
                .push_with(n, |f, c| to_f64(scratch[c * PIECE + f]));
            pos += n as u64;
        }
        self.stats
            .frames
            .fetch_add(written as u64, Ordering::Relaxed);
        written
    }
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A flow's packets in time ([`Schedule`], moved out of `tx.rs`, dstack
//! #360) and the time arithmetic around it: a sample count in PTP ns
//! ([`ns_of`]), the media clock a schedule runs on ([`send_clock`]) and a
//! receiver's lateness ([`flow_latency_ns`], [`over_latency`]).

use super::{DISCONTINUITY, Shift, split};
use crate::clock;

/// A flow's packets in time, in media samples ([`send_clock`]): `next` is
/// the stamp of the next packet, due once the clock reaches `next + fpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub next: u64,
    pub fpp: u64,
    /// The lag past which the schedule starts again: the device's latency
    /// in samples (a packet that late is of no use to the receiver).
    pub max_lag: u64,
    /// Starts again since the last [`Schedule::take_rebootstraps`].
    pub(super) rebootstraps: u64,
}

impl Schedule {
    /// A schedule whose first stamp is the first multiple of `fpp` after
    /// `now` (inferno's bootstrap).
    pub fn bootstrap(now: u64, fpp: u16, max_lag: u64) -> Self {
        let fpp = u64::from(fpp.max(1));
        Schedule {
            next: aligned_after(now, fpp),
            fpp,
            max_lag,
            rebootstraps: 0,
        }
    }

    /// The stamp of the packet due at `now`, if one is (the schedule moves
    /// on by fpp). A lag past `max_lag`, or a schedule [`DISCONTINUITY`]
    /// samples ahead of the clock, starts it again (counted) and sends
    /// nothing now.
    pub fn due(&mut self, now: u64) -> Option<u64> {
        self.due_after(now, &split::Before::default())
    }

    /// A re-anchor of the clock at media time `now` (after it) with the
    /// device's `shift` ([`shift`](super::shift), on the grid of the
    /// device's `block`; `None`: the step is unknown): moved by it when it is a whole number
    /// of packets and leaves the next stamp at most the latency (at least
    /// the block) and a packet past now (not counted; a rest of the
    /// step off the grid is a lag or a wait of at most half a block, which
    /// [`Schedule::due`] judges as any other); kept when it would leave it
    /// further (the pass before read the new time with the old
    /// epoch and already started it again there); else started again,
    /// counted.
    pub fn re_anchor(&mut self, shift: Option<i64>, now: u64, block: u64) -> Shift {
        let Some(by) = shift.filter(|by| by.rem_euclid(self.fpp as i64) == 0) else {
            self.realign(now);
            return Shift::Restarted;
        };
        let next = self.next.wrapping_add_signed(by);
        // The room: the latency, or the block when larger (the shift is on
        // the block grid, so its rest can leave the stamp up to half a block
        // ahead; a latency set apart from the block can be smaller), and a
        // packet. A pass that restarted at the new time lands a whole step
        // further.
        let room = self.max_lag.max(block).saturating_add(self.fpp);
        if next > now.saturating_add(room) {
            return Shift::Kept;
        }
        self.next = next;
        Shift::Moved
    }

    /// Starts the schedule again at `now` (counted): a lag, a jump, a
    /// re-anchor of the clock whose step is unknown.
    pub fn realign(&mut self, now: u64) {
        self.next = aligned_after(now, self.fpp);
        self.rebootstraps += 1;
    }

    /// [`Schedule::due`] whose restart line also says what the TX thread
    /// measured of the time `before` the pass (dstack #359): its last
    /// sleep's oversleep and its last pass's wall and CPU time, so a
    /// restart reads scheduled late, pass long or preempted.
    pub fn due_after(&mut self, now: u64, before: &split::Before) -> Option<u64> {
        let send = self.next.saturating_add(self.fpp);
        if now > send.saturating_add(self.max_lag) || send > now.saturating_add(DISCONTINUITY) {
            // Why (dasio, Windows): the lag past the send time, or a
            // jump back. Rare (each one is a restart), so logged.
            tracing::warn!(
                lag = now as i64 - send as i64,
                max_lag = self.max_lag,
                oversleep_us = %split::Us(before.oversleep_ns),
                last_pass_us = %split::Us(before.pass_ns),
                last_pass_cpu_us = %split::Us(before.pass_cpu_ns),
                "the TX schedule starts again"
            );
            self.realign(now);
            return None;
        }
        if now < send {
            return None;
        }
        let stamp = self.next;
        self.next = send;
        Some(stamp)
    }

    /// The starts again since the last call.
    pub fn take_rebootstraps(&mut self) -> u64 {
        std::mem::take(&mut self.rebootstraps)
    }

    /// The PTP ns at which the next packet is due at `rate`, `margin_ns`
    /// after its last sample.
    pub fn wake_ns(&self, rate: u32, margin_ns: u64) -> u64 {
        ns_of(self.next.saturating_add(self.fpp), rate).saturating_add(margin_ns)
    }
}

/// The first multiple of `fpp` after `now`.
fn aligned_after(now: u64, fpp: u64) -> u64 {
    now - now % fpp + fpp
}

/// The first PTP ns by which `samples` samples at `rate` have passed
/// (rounded up, so [`send_clock`] there reads them all). A rate of 0 reads
/// as 1.
pub fn ns_of(samples: u64, rate: u32) -> u64 {
    let rate = u128::from(rate.max(1));
    u64::try_from((u128::from(samples) * 1_000_000_000).div_ceil(rate)).unwrap_or(u64::MAX)
}

/// The latency a receiver of a flow of `fpp`-frame packets needs at least:
/// our advertised `latency_ns`, or, for a packet longer than that (a
/// receiver that asked past the advertised range), the packet's frames +
/// `margin_ns` + the send slack, when it leaves.
pub fn flow_latency_ns(latency_ns: u64, fpp: u16, rate: u32, margin_ns: u64) -> u64 {
    let packet = ns_of(u64::from(fpp), rate) + margin_ns + crate::device::TX_SEND_SLACK_NS;
    latency_ns.max(packet)
}

/// Whether a packet `age_ns` past its stamp is late at a receiver whose
/// latency is `latency_ns`: at the latency itself it still plays.
pub fn over_latency(age_ns: u64, latency_ns: u64) -> bool {
    age_ns > latency_ns
}

/// The media clock a schedule runs on: the samples at `rate` up to
/// `margin_ns` before `now_ns`.
pub fn send_clock(now_ns: u64, rate: u32, margin_ns: u64) -> u64 {
    clock::to_samples(now_ns.saturating_sub(margin_ns), rate)
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A flow's packets in time ([`Schedule`], moved out of `tx.rs`, dstack
//! #360) and the time arithmetic around it: a sample count in PTP ns
//! ([`ns_of`]), the media clock a schedule runs on ([`send_clock`]) and a
//! receiver's lateness ([`flow_latency_ns`], [`over_latency`]).

use super::{Shift, split};
use crate::clock;
use crate::device::MAX_LATENCY_SAMPLES;

/// A flow's packets in time, in media samples ([`send_clock`]): `next` is
/// the stamp of the next packet, due once the clock reaches `next + fpp`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Schedule {
    pub next: u64,
    pub fpp: u64,
    /// The device's latency in samples: the least lag past which the
    /// schedule starts again ([`Schedule::bound`] takes the receiver's when
    /// longer).
    pub max_lag: u64,
    /// The flow's receiver's latency in samples ([`receiver_lag`]; 0:
    /// unknown), set each pass (dstack #426).
    pub receiver_lag: u64,
    /// The flow's slot, which the restart line names (dstack #427: a
    /// reader judges each line by its own receiver's latency); 0 from
    /// [`Schedule::bootstrap`], set by the TX thread for its lane.
    pub slot: usize,
    /// The device's block in samples (0 from [`Schedule::bootstrap`], set
    /// by the TX thread): the least room a re-anchor moves the next stamp
    /// within, so the least of [`Schedule::ahead_limit`] (dstack #440).
    pub block: u64,
    /// Starts again since the last [`Schedule::take_rebootstraps`].
    pub(super) rebootstraps: u64,
    /// Started again as a clock gone back ([`Schedule::ahead_limit`]), and
    /// no pass's re-anchor point since ([`Schedule::re_anchor`],
    /// [`Schedule::unmark`]; dstack #440).
    pub(super) back: bool,
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
            receiver_lag: 0,
            slot: 0,
            block: 0,
            rebootstraps: 0,
            back: false,
        }
    }

    /// The lag past which the schedule starts again (dstack #426): the
    /// latency the receiver plays the flow at, its own when it states a
    /// longer one than ours (Dante's rule, as `PassClock::late_threshold`
    /// counts late packets). A packet later than that can never play; one
    /// within it is sent (the lane's late count judges whether it came in
    /// time). Ours while the receiver's is unknown.
    pub fn bound(&self) -> u64 {
        self.max_lag.max(self.receiver_lag)
    }

    /// How far the next stamp may be ahead of the clock (dstack #440): the
    /// [`Schedule::bound`], at least the [`Schedule::block`], and a packet.
    /// A start leaves it at most a packet ahead and [`Schedule::re_anchor`]
    /// moves it at most our latency (at least the block) and a packet past
    /// now, so a stamp further ahead is the clock gone back (a step back
    /// read under the old epoch), never a wait.
    pub fn ahead_limit(&self) -> u64 {
        self.bound().max(self.block).saturating_add(self.fpp)
    }

    /// The stamp of the packet due at `now`, if one is (the schedule moves
    /// on by fpp). A lag past its [`Schedule::bound`], or a next stamp more
    /// than [`Schedule::ahead_limit`] ahead of the clock, starts it again
    /// (counted) and sends nothing now; the second marks it for the next
    /// pass's re-anchor ([`Schedule::re_anchor`]).
    pub fn due(&mut self, now: u64) -> Option<u64> {
        self.due_after(now, &split::Before::default())
    }

    /// A re-anchor of the clock at media time `now` (after it) with the
    /// device's `shift` ([`shift`](super::shift), on the grid of the
    /// device's block, [`Schedule::block`]; `None`: the step is unknown):
    /// moved by it when it is a whole number
    /// of packets and leaves the next stamp at most the latency (at least
    /// the block) and a packet past now (not counted; a rest of the
    /// step off the grid is a lag or a wait of at most half a block, which
    /// [`Schedule::due`] judges as any other); kept when it would leave it
    /// further (the pass before read the new time with the old
    /// epoch and already started it again there); kept too when it moves
    /// back a schedule the pass before started again as a clock gone back
    /// (that pass read the new time of this step under the old epoch, so a
    /// move would leave it the step behind and start it again, dstack
    /// #440); else started again, counted. It spends the mark either way.
    pub fn re_anchor(&mut self, shift: Option<i64>, now: u64) -> Shift {
        let back = std::mem::take(&mut self.back);
        let Some(by) = shift.filter(|by| by.rem_euclid(self.fpp as i64) == 0) else {
            self.realign(now);
            return Shift::Restarted;
        };
        if back && by.is_negative() {
            return Shift::Kept;
        }
        let next = self.next.wrapping_add_signed(by);
        // The room: the latency, or the block when larger (the shift is on
        // the block grid, so its rest can leave the stamp up to half a block
        // ahead; a latency set apart from the block can be smaller), and a
        // packet. A pass that restarted at the new time lands a whole step
        // further.
        let room = self.max_lag.max(self.block).saturating_add(self.fpp);
        if next > now.saturating_add(room) {
            return Shift::Kept;
        }
        self.next = next;
        Shift::Moved
    }

    /// A pass's re-anchor point with no re-anchor: a clock-back mark from
    /// the pass before is spent, so it keeps a schedule through one pass's
    /// re-anchor only (dstack #440, ROZHODNUTÉ 19.11).
    pub fn unmark(&mut self) {
        self.back = false;
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
        let bound = self.bound();
        let back = self.next > now.saturating_add(self.ahead_limit());
        if now > send.saturating_add(bound) || back {
            // Why (dasio, Windows): the lag past the send time, or a
            // jump back. Rare (each one is a restart), so logged; its
            // `max_lag` is the bound it was judged by, for its `slot`'s
            // receiver.
            tracing::warn!(
                slot = self.slot,
                lag = now as i64 - send as i64,
                max_lag = bound,
                oversleep_us = %split::Us(before.oversleep_ns),
                last_pass_us = %split::Us(before.pass_ns),
                last_pass_cpu_us = %split::Us(before.pass_cpu_ns),
                "the TX schedule starts again"
            );
            self.realign(now);
            self.back = back;
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

/// A receiver's stated latency `receiver_ns` (0: unknown) in samples at
/// `rate`, as a flow's lag bound takes it (dstack #426): at most
/// [`MAX_LATENCY_SAMPLES`], the engine ring's reach behind its writer, so a
/// receiver that states more never makes the TX thread read past it.
pub fn receiver_lag(receiver_ns: u64, rate: u32) -> u64 {
    clock::latency_samples(receiver_ns, rate).min(MAX_LATENCY_SAMPLES)
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

// SPDX-License-Identifier: MIT OR Apache-2.0

//! One packet's send in a TX pass and its counts: the same for a packet
//! sent at once (the UDP socket, past the qdisc) and for a frame of the
//! host sender's batch ([`super::host`]) or its fallback.

use std::io;
use std::sync::atomic::Ordering;
use std::time::Instant;

use super::retry::{count_retried, send_with_retries, spin_for};
use super::{LaneStats, count_send, flow_latency_ns, late_bin, nanos, ns_of, over_latency};

/// A TX pass's clock and the device's timing: what a packet's send and its
/// counts read.
#[derive(Debug, Clone, Copy)]
pub(super) struct PassClock {
    /// The pass's PTP time, read at `pass`.
    pub(super) now_ns: u64,
    pub(super) pass: Instant,
    pub(super) rate: u32,
    /// The device's `tx_margin_ns`.
    pub(super) margin_ns: u64,
    /// The advertised latency, ns.
    pub(super) latency_ns: u64,
}

impl PassClock {
    /// PTP ns now: the pass's time plus the real time since.
    pub(super) fn now(&self) -> u64 {
        self.now_ns.saturating_add(nanos(self.pass.elapsed()))
    }

    /// The latency floor of a flow of `fpp`-frame packets
    /// ([`flow_latency_ns`]).
    pub(super) fn floor(&self, fpp: u16) -> u64 {
        flow_latency_ns(self.latency_ns, fpp, self.rate, self.margin_ns)
    }

    /// The age past which a packet of a flow of `fpp`-frame packets is late
    /// at its receiver, whose own latency is `receiver_ns` (0: unknown): the
    /// latency it plays the flow at (its own, at least our advertised one:
    /// Dante's rule, as our RX plays a flow), or the packet's floor when
    /// that is longer ([`flow_latency_ns`]). `None` while the receiver's
    /// latency is unknown: no packet is called late against a guess (the
    /// owner's ruling, dstack #1).
    pub(super) fn late_threshold(&self, fpp: u16, receiver_ns: u64) -> Option<u64> {
        (receiver_ns != 0).then(|| {
            flow_latency_ns(
                self.latency_ns.max(receiver_ns),
                fpp,
                self.rate,
                self.margin_ns,
            )
        })
    }
}

/// One packet's send: its outcome (`Ok(true)`: it went direct, past the
/// qdisc or through the host sender), the retries made, and the longest
/// single try, ns.
#[derive(Debug)]
pub(super) struct Sent {
    pub(super) result: io::Result<bool>,
    pub(super) retries: u32,
    pub(super) took: u64,
}

/// Sends the packet stamped `stamp` of a flow of `fpp`-frame packets with
/// `send`, again while the NIC refuses it for a moment and it can still
/// arrive in time (dstack #1, [`send_with_retries`]), each try timed.
pub(super) fn send_timed(
    clock: &PassClock,
    stamp: u64,
    fpp: u16,
    mut send: impl FnMut() -> io::Result<bool>,
) -> Sent {
    let stamp_ns = ns_of(stamp, clock.rate);
    let mut took = 0;
    let (result, retries) = send_with_retries(
        crate::clock::Os::current(),
        clock.floor(fpp),
        || clock.now().saturating_sub(stamp_ns),
        || {
            let sent_at = Instant::now();
            let sent = send();
            took = took.max(nanos(sent_at.elapsed()));
            sent
        },
        spin_for,
    );
    Sent {
        result,
        retries,
        took,
    }
}

/// Counts in `stats` the packet stamped `stamp` (`fpp` frames, `n`
/// datagram bytes) whose send returned now: how late it left (the pass's
/// PTP time plus the time since: a thread not scheduled, a send that
/// blocked, or retries, all make it late), its age past its stamp (what a
/// receiver measures), the send's time, retries and outcome.
pub(super) fn account(
    stats: &LaneStats,
    clock: &PassClock,
    stamp: u64,
    fpp: u16,
    sent: &Sent,
    n: usize,
    receiver_ns: u64,
) {
    let left = clock.now();
    let due = ns_of(stamp.saturating_add(fpp.into()), clock.rate).saturating_add(clock.margin_ns);
    let late = left.saturating_sub(due);
    let age = left.saturating_sub(ns_of(stamp, clock.rate));
    stats.age_max_ns.fetch_max(age, Ordering::Relaxed);
    stats.age_peak_ns.fetch_max(age, Ordering::Relaxed);
    if clock
        .late_threshold(fpp, receiver_ns)
        .is_some_and(|t| over_latency(age, t))
    {
        stats.over_latency.fetch_add(1, Ordering::Relaxed);
    }
    stats.late[late_bin(late)].fetch_add(1, Ordering::Relaxed);
    stats.late_max_ns.fetch_max(late, Ordering::Relaxed);
    stats.send_max_ns.fetch_max(sent.took, Ordering::Relaxed);
    count_retried(stats, &sent.result, sent.retries);
    count_send(stats, &sent.result, n);
}

#[cfg(test)]
mod tests;

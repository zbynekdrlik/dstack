//! The late watch: once a second, each TX flow whose packets left later
//! than the advertised latency since the last look gets one warning line,
//! with the count and the oldest packet. A receiver at that latency counts
//! each such packet late (Dante Controller's Latency tab), so the log says
//! when our side was late. A receiver that counts late packets while this
//! stays quiet points past our send: the OS's send path or the network
//! (dstack #1: SongPlayer's flow, 24 late at the FOH card at 12:08:53 on
//! 1.11, and no line on our side to say whose).

use tracing::warn;

use super::rx::RxShared;
use super::tx::{MAX_FLOWS, TxShared};

/// How often the watch looks, ns.
pub const EVERY_NS: u64 = 1_000_000_000;

/// One flow's late packets since the last look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Late {
    /// The TX flow's slot.
    pub slot: usize,
    /// Packets that left later than the advertised latency.
    pub packets: u64,
    /// The oldest packet in that time, ns past its stamp.
    pub age_peak_ns: u64,
}

/// The watch's memory: each slot's late count at the last look.
#[derive(Debug, Default)]
pub struct LateWatch {
    seen: [u64; MAX_FLOWS],
    next_ns: Option<u64>,
}

impl LateWatch {
    pub const fn new() -> Self {
        LateWatch {
            seen: [0; MAX_FLOWS],
            next_ns: None,
        }
    }

    /// At `now_ns`, a look once [`EVERY_NS`] has passed since the last
    /// (the first call only learns the counts): each slot's cumulative late
    /// count from `count`, and its oldest packet from `take_peak` (taken at
    /// every look, so a report's peak covers that second only). The slots
    /// whose count rose; empty between looks.
    pub fn look(
        &mut self,
        now_ns: u64,
        count: impl Fn(usize) -> u64,
        take_peak: impl Fn(usize) -> u64,
    ) -> Vec<Late> {
        if self.next_ns.is_some_and(|next| now_ns < next) {
            return Vec::new();
        }
        let first = self.next_ns.is_none();
        self.next_ns = Some(now_ns.saturating_add(EVERY_NS));
        let mut out = Vec::new();
        for (slot, seen) in self.seen.iter_mut().enumerate() {
            let now = count(slot);
            let peak = take_peak(slot);
            let rose = now.saturating_sub(*seen);
            *seen = now;
            if rose > 0 && !first {
                out.push(Late {
                    slot,
                    packets: rose,
                    age_peak_ns: peak,
                });
            }
        }
        out
    }

    /// [`LateWatch::look`] on `tx`'s flows, each report logged as a warning.
    pub fn watch(&mut self, now_ns: u64, tx: &TxShared) {
        for late in self.look(now_ns, |s| tx.lane(s).over_latency, |s| tx.take_age_peak(s)) {
            warn!(
                slot = late.slot,
                packets = late.packets,
                age_peak_us = late.age_peak_ns / 1_000,
                "TX packets left later than the advertised latency: \
                 receivers count them late"
            );
        }
    }
}

/// One RX flow's losses since the last look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Loss {
    /// The RX flow's slot.
    pub slot: usize,
    /// Gaps: packets that began past the end of the one before.
    pub gaps: u64,
    /// The frames the gaps skipped.
    pub frames: u64,
    /// Datagrams the kernel dropped at the slot's socket (Linux).
    pub drops: u64,
}

/// The RX side's loss watch (iemmixer 1.11: one packet now and then missing
/// from a flow): once a second, each flow whose gaps or socket drops rose
/// gets one warning line. A gap without drops means the packet never
/// reached our socket (the sender, the network, the NIC); drops mean the
/// kernel dropped it at our socket.
#[derive(Debug, Default)]
pub struct RxWatch {
    seen: [(u64, u64, u64); MAX_FLOWS],
    next_ns: Option<u64>,
}

impl RxWatch {
    pub const fn new() -> Self {
        RxWatch {
            seen: [(0, 0, 0); MAX_FLOWS],
            next_ns: None,
        }
    }

    /// At `now_ns`, a look once [`EVERY_NS`] has passed (the first only
    /// learns): each slot's cumulative `(gaps, gap frames, drops)` from
    /// `counts`. The slots where any rose; empty between looks.
    pub fn look(&mut self, now_ns: u64, counts: impl Fn(usize) -> (u64, u64, u64)) -> Vec<Loss> {
        if self.next_ns.is_some_and(|next| now_ns < next) {
            return Vec::new();
        }
        let first = self.next_ns.is_none();
        self.next_ns = Some(now_ns.saturating_add(EVERY_NS));
        let mut out = Vec::new();
        for (slot, seen) in self.seen.iter_mut().enumerate() {
            let now = counts(slot);
            let loss = Loss {
                slot,
                gaps: now.0.saturating_sub(seen.0),
                frames: now.1.saturating_sub(seen.1),
                drops: now.2.saturating_sub(seen.2),
            };
            *seen = now;
            if !first && (loss.gaps > 0 || loss.drops > 0) {
                out.push(loss);
            }
        }
        out
    }

    /// [`RxWatch::look`] on `rx`'s flows, each loss logged as a warning.
    pub fn watch(&mut self, now_ns: u64, rx: &RxShared) {
        if self.next_ns.is_some_and(|next| now_ns < next) {
            return;
        }
        let drops = rx.socket_drops();
        let counts = |s: usize| {
            let lane = rx.lane(s);
            let d = drops.map_or(0, |d| d[s]);
            (lane.gaps, lane.gap_frames, d)
        };
        for loss in self.look(now_ns, counts) {
            warn!(
                slot = loss.slot,
                gaps = loss.gaps,
                frames = loss.frames,
                socket_drops = loss.drops,
                "RX packets missing from a flow: with no socket drops they never reached our socket"
            );
        }
    }
}

#[cfg(test)]
mod tests;

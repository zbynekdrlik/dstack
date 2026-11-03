//! The late watch: once a second, each TX flow whose packets left later
//! than the advertised latency since the last look gets one warning line,
//! with the count and the oldest packet. A receiver at that latency counts
//! each such packet late (Dante Controller's Latency tab), so the log says
//! when our side was late. A receiver that counts late packets while this
//! stays quiet points past our send: the OS's send path or the network
//! (dstack #1: SongPlayer's flow, 24 late at the FOH card at 12:08:53 on
//! 1.11, and no line on our side to say whose).

use tracing::{info, warn};

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

/// One TX flow's failed and retried sends since the last look (dasio
/// 2.11.2026: send errors on both FOH flows, about 100 every ~96 s;
/// WSAENOBUFS bursts, dstack #1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SendErrors {
    /// The TX flow's slot.
    pub slot: usize,
    /// Sends that failed since the last look, after their retries (lost).
    pub errors: u64,
    /// Packets refused for a moment that went out on a retry since the
    /// last look (recovered, `media::tx::send_retry`).
    pub retried: u64,
    /// The last failed send's OS error (raw errno or WSA code; -1 without
    /// one), a retried one's too.
    pub os_error: i32,
}

impl SendErrors {
    /// Whether packets were lost (a warning), not only retried (info).
    pub fn lost(&self) -> bool {
        self.errors > 0
    }
}

/// The TX side's send-error watch: once a second, each flow whose failed
/// or retried sends rose gets one line with the counts and the OS error
/// code: a warning when packets were lost after their retries (the
/// receiver sees a hole), info when every refused packet went out on a
/// retry.
#[derive(Debug, Default)]
pub struct SendWatch {
    seen: [u64; MAX_FLOWS],
    seen_retries: [u64; MAX_FLOWS],
    next_ns: Option<u64>,
}

impl SendWatch {
    pub const fn new() -> Self {
        SendWatch {
            seen: [0; MAX_FLOWS],
            seen_retries: [0; MAX_FLOWS],
            next_ns: None,
        }
    }

    /// At `now_ns`, a look once [`EVERY_NS`] has passed (the first only
    /// learns): each slot's cumulative `(errors, retries, last OS error)`
    /// from `counts`. The slots whose errors or retries rose; empty between
    /// looks.
    pub fn look(
        &mut self,
        now_ns: u64,
        counts: impl Fn(usize) -> (u64, u64, i32),
    ) -> Vec<SendErrors> {
        if self.next_ns.is_some_and(|next| now_ns < next) {
            return Vec::new();
        }
        let first = self.next_ns.is_none();
        self.next_ns = Some(now_ns.saturating_add(EVERY_NS));
        let mut out = Vec::new();
        for (slot, (seen, seen_retries)) in self
            .seen
            .iter_mut()
            .zip(self.seen_retries.iter_mut())
            .enumerate()
        {
            let (now, retries, os_error) = counts(slot);
            let rose = now.saturating_sub(*seen);
            let retried = retries.saturating_sub(*seen_retries);
            *seen = now;
            *seen_retries = retries;
            if (rose > 0 || retried > 0) && !first {
                out.push(SendErrors {
                    slot,
                    errors: rose,
                    retried,
                    os_error,
                });
            }
        }
        out
    }

    /// [`SendWatch::look`] on `tx`'s flows, each report logged: a warning
    /// when packets were lost ([`SendErrors::lost`]), else info.
    pub fn watch(&mut self, now_ns: u64, tx: &TxShared) {
        let counts = |s| {
            let l = tx.lane(s);
            (l.send_errors, l.send_retries, l.last_os_error)
        };
        for e in self.look(now_ns, counts) {
            if e.lost() {
                warn!(
                    slot = e.slot,
                    errors = e.errors,
                    retried = e.retried,
                    os_error = e.os_error,
                    "TX sends failed after their retries (the packets were not sent: receivers see holes)"
                );
            } else {
                info!(
                    slot = e.slot,
                    retried = e.retried,
                    os_error = e.os_error,
                    "TX sends refused for a moment went out on a retry (no packet lost)"
                );
            }
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

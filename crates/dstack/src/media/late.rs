//! The late watch: once a second, each TX flow whose packets were older
//! than the latency their receiver plays at since the last look gets one
//! warning line, with the count, the oldest packet and the receiver's own
//! latency. The receiver counts each such packet late (Dante Controller's
//! Latency tab), so the log says when our side was late. A flow whose
//! receiver's latency is unknown is never called late (the owner's ruling,
//! dstack #1: DVS plays at 10 ms, and packets of 1.15 ms were warned late
//! against our advertised latency). A receiver that counts late packets while this
//! stays quiet points past our send: the OS's send path or the network
//! (dstack #1: SongPlayer's flow, 24 late at the FOH card at 12:08:53 on
//! 1.11, and no line on our side to say whose).

use tracing::{info, warn};

use super::rx::{MAX_FLOWS as RX_FLOWS, RxShared};
use super::tx::{MAX_FLOWS, TxShared};

/// How often the watch looks, ns.
pub const EVERY_NS: u64 = 1_000_000_000;

/// One flow's late packets since the last look.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Late {
    /// The TX flow's slot.
    pub slot: usize,
    /// Packets older than the latency their receiver plays at.
    pub packets: u64,
    /// The oldest packet in that time, ns past its stamp.
    pub age_peak_ns: u64,
    /// Of `packets`, those the relay thread sent while the TX thread blocked
    /// its bounded escape (dstack #177).
    pub relayed: u64,
    /// The shortest time from the last bounded escape's start to one of
    /// them, ns (`None`: no escape before them).
    pub since_escape_ns: Option<u64>,
}

/// What [`LateWatch::look`] reads of one slot at a look.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LateCounts {
    /// Packets older than their receiver's latency since the device
    /// started.
    pub packets: u64,
    /// Of them, the relay thread's.
    pub relayed: u64,
    /// The oldest packet since the last look, ns past its stamp.
    pub age_peak_ns: u64,
    /// The shortest time from an escape's start to a late packet since the
    /// last look, ns.
    pub since_escape_ns: Option<u64>,
}

/// The watch's memory: each slot's late counts at the last look.
#[derive(Debug, Default)]
pub struct LateWatch {
    seen: [u64; MAX_FLOWS],
    seen_relayed: [u64; MAX_FLOWS],
    next_ns: Option<u64>,
}

impl LateWatch {
    pub const fn new() -> Self {
        LateWatch {
            seen: [0; MAX_FLOWS],
            seen_relayed: [0; MAX_FLOWS],
            next_ns: None,
        }
    }

    /// At `now_ns`, a look once [`EVERY_NS`] has passed since the last
    /// (the first call only learns the counts): each slot's counts from
    /// `take` (cumulative ones, and the peak and the time since an escape,
    /// taken at every look so a report covers that second only). The slots
    /// whose count rose; empty between looks.
    pub fn look(&mut self, now_ns: u64, take: impl Fn(usize) -> LateCounts) -> Vec<Late> {
        if self.next_ns.is_some_and(|next| now_ns < next) {
            return Vec::new();
        }
        let first = self.next_ns.is_none();
        self.next_ns = Some(now_ns.saturating_add(EVERY_NS));
        let mut out = Vec::new();
        for (slot, (seen, seen_relayed)) in self
            .seen
            .iter_mut()
            .zip(self.seen_relayed.iter_mut())
            .enumerate()
        {
            let now = take(slot);
            let rose = now.packets.saturating_sub(*seen);
            let relayed = now.relayed.saturating_sub(*seen_relayed);
            *seen = now.packets;
            *seen_relayed = now.relayed;
            if rose > 0 && !first {
                out.push(Late {
                    slot,
                    packets: rose,
                    age_peak_ns: now.age_peak_ns,
                    relayed,
                    since_escape_ns: now.since_escape_ns,
                });
            }
        }
        out
    }

    /// [`LateWatch::look`] on `tx`'s flows, each report logged as a warning:
    /// with how many of the packets the relay thread sent and how long after
    /// the last bounded escape began (`since_escape_us`, absent without
    /// one), so a late pass reads as the relay's or the TX thread's own
    /// (dstack #177).
    pub fn watch(&mut self, now_ns: u64, tx: &TxShared) {
        let take = |s| LateCounts {
            packets: tx.lane(s).over_latency,
            relayed: tx.over_latency_relayed(s),
            age_peak_ns: tx.take_age_peak(s),
            since_escape_ns: tx.take_since_escape(s),
        };
        for late in self.look(now_ns, take) {
            // The flow's other end (dstack #266), read here on the watch's
            // thread, never by the TX thread; a multicast flow has no one
            // receiver (`-`), its `dest` is the group.
            let (receiver, dest) = tx.lock().table.flow(late.slot).map_or_else(
                || (String::from("-"), String::from("-")),
                |f| (named(&f.rx_host), f.dest.to_string()),
            );
            warn!(
                receiver = %receiver,
                dest = %dest,
                slot = late.slot,
                packets = late.packets,
                age_peak_us = late.age_peak_ns / 1_000,
                receiver_latency_us = tx.lane(late.slot).receiver_latency_ns / 1_000,
                relayed = late.relayed,
                since_escape_us = late.since_escape_ns.map(|ns| ns / 1_000),
                "TX packets older than the latency their receiver plays at: \
                 it counts them late"
            );
        }
    }
}

/// A peer's name for a log line (dstack #266): `-` for an empty one (a
/// multicast flow's receiver, a subscription without a device).
pub(crate) fn named(name: &str) -> String {
    if name.is_empty() {
        String::from("-")
    } else {
        name.to_owned()
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

/// The host frame sender's watch (dstack #1, Windows): once a second, when
/// the sender refused whole batches since the last look, one warning with
/// their count and the last OS error. Their packets went out through the
/// UDP sockets (each counted in its lane), so the line says the sender
/// failed, not that packets were lost; never one line per packet.
#[derive(Debug, Default)]
pub struct HostWatch {
    seen: u64,
    next_ns: Option<u64>,
}

impl HostWatch {
    pub const fn new() -> Self {
        HostWatch {
            seen: 0,
            next_ns: None,
        }
    }

    /// At `now_ns`, a look once [`EVERY_NS`] has passed (the first only
    /// learns): the batches refused since the last look, of the cumulative
    /// `refused`; `None` between looks and when none was.
    pub fn look(&mut self, now_ns: u64, refused: u64) -> Option<u64> {
        if self.next_ns.is_some_and(|next| now_ns < next) {
            return None;
        }
        let first = self.next_ns.is_none();
        self.next_ns = Some(now_ns.saturating_add(EVERY_NS));
        let rose = refused.saturating_sub(self.seen);
        self.seen = refused;
        (rose > 0 && !first).then_some(rose)
    }

    /// [`HostWatch::look`] on `tx`'s host sender, a refusal logged as a
    /// warning.
    pub fn watch(&mut self, now_ns: u64, tx: &TxShared) {
        let (refused, os_error) = tx.host_refusals();
        if let Some(batches) = self.look(now_ns, refused) {
            warn!(
                batches,
                os_error,
                "TX host frame sender refused whole batches: their packets went through the UDP socket"
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

    /// [`RxWatch::look`] on `rx`'s flows, each loss logged as a warning
    /// that names the flow's transmitter (dstack #266): its Dante name from
    /// `device` (the slot's flow's, read only for a slot that lost packets)
    /// and the address its audio came from; `-` for either unknown.
    pub fn watch(&mut self, now_ns: u64, rx: &RxShared, device: impl Fn(usize) -> Option<String>) {
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
            let (name, source) = transmitter(rx, loss.slot, &device);
            warn!(
                device = %name,
                source = %source,
                slot = loss.slot,
                gaps = loss.gaps,
                frames = loss.frames,
                socket_drops = loss.drops,
                "RX packets missing from a flow: with no socket drops they never reached our socket"
            );
        }
    }
}

/// How often the RX minute watch looks, ns.
pub const MINUTE_NS: u64 = 60_000_000_000;

/// The RX side's minute watch: once a minute, each flow whose packets came
/// reordered (overtaken on the way, each filling its hole) gets one INFO
/// line, apart from the loss watch's warning: a reordering link shows
/// without a loss alarm (dstack #278). And each flow with stamped, placed packets
/// (Linux) gets one INFO line of its minute's wake delay and network
/// latency, the only way to read them from an app without a status socket
/// (dstack #234: OBS's dante-input).
#[derive(Debug)]
pub struct RxMinute {
    reordered: [u64; RX_FLOWS],
    next_ns: Option<u64>,
}

impl Default for RxMinute {
    fn default() -> Self {
        Self::new()
    }
}

impl RxMinute {
    pub const fn new() -> Self {
        RxMinute {
            reordered: [0; RX_FLOWS],
            next_ns: None,
        }
    }

    /// At `now_ns`, a look once [`MINUTE_NS`] has passed (the first only
    /// learns): each slot's cumulative reordered packets from `reordered`.
    /// `Some` with each slot's rise at a look after the first, `None`
    /// otherwise; `first` tells the first look.
    pub fn look(
        &mut self,
        now_ns: u64,
        reordered: impl Fn(usize) -> u64,
    ) -> Option<(bool, [u64; RX_FLOWS])> {
        if self.next_ns.is_some_and(|next| now_ns < next) {
            return None;
        }
        let first = self.next_ns.is_none();
        self.next_ns = Some(now_ns.saturating_add(MINUTE_NS));
        let rose = std::array::from_fn(|slot| {
            let now = reordered(slot);
            let rose = now.saturating_sub(self.reordered[slot]);
            self.reordered[slot] = now;
            rose
        });
        Some((first, rose))
    }

    /// [`RxMinute::look`] on `rx`'s flows: at each look every slot's
    /// minute is taken ([`RxShared::take_minute`]; the first look's only
    /// starts the next minute clean), and each flow's reorders and timing
    /// of the minute logged at INFO with its transmitter, as the loss
    /// watch names it (`device` is read only for a slot with a line).
    pub fn watch(&mut self, now_ns: u64, rx: &RxShared, device: impl Fn(usize) -> Option<String>) {
        let Some((first, rose)) = self.look(now_ns, |s| rx.lane(s).reordered) else {
            return;
        };
        for (slot, &reordered) in rose.iter().enumerate() {
            let timing = rx.take_minute(slot);
            if first {
                continue;
            }
            if reordered > 0 {
                let (name, source) = transmitter(rx, slot, &device);
                info!(
                    device = %name,
                    source = %source,
                    slot,
                    reordered,
                    "RX packets reordered on the way in the last minute, not lost: each filled its hole"
                );
            }
            if let (Some(wake), Some(net)) = timing {
                let (name, source) = transmitter(rx, slot, &device);
                info!(
                    device = %name,
                    source = %source,
                    slot,
                    packets = wake.packets,
                    wake_delay_p50_us = wake.p50_us,
                    wake_delay_p99_us = wake.p99_us,
                    wake_delay_p999_us = wake.p999_us,
                    wake_delay_max_us = wake.max_us,
                    network_latency_min_us = net.min_us,
                    network_latency_p99_us = net.p99_us,
                    network_latency_max_us = net.max_us,
                    "RX flow timing in the last minute: the RX thread's wake delay and the measured network latency"
                );
            }
        }
    }
}

/// Slot `slot`'s transmitter as the RX lines name it: its Dante name from
/// `device` and the address its audio came from; `-` for either unknown.
fn transmitter(
    rx: &RxShared,
    slot: usize,
    device: &impl Fn(usize) -> Option<String>,
) -> (String, String) {
    let name = device(slot).map_or_else(|| String::from("-"), |d| named(&d));
    let source = rx
        .source(slot)
        .map_or_else(|| String::from("-"), |s| s.to_string());
    (name, source)
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: MIT OR Apache-2.0

//! The bridge's from-ASIO figures for its status (dstack #325, dasio#120).
//!
//! **The receiver figure is measured on the TX side.** A receiver judges
//! each packet by its age past its first sample's media stamp, and the TX
//! side measures exactly that for every packet it sends
//! ([`dstack::media::tx::TxShared::take_app_view`]). The status loop hands
//! that view to [`BlockAge::look`]: the max of the oldest packet over the
//! last minute is the packet age, and that + the network (the device's
//! measured network latency p99, [`dstack::media::rx::RxShared::network_p99_ns`],
//! or [`NETWORK_ALLOWANCE_NS`] while none is measured) is the smallest
//! receiver latency that never sees a late packet from this bridge
//! ([`AgeReport::min_receiver_latency_us`]). A bridge instance's TX is all
//! the bridge's, so every TX lane counts. The once-an-hour warning comes
//! only from a receiver's own judgement: a flow's late count (packets over
//! the latency its receiver states, the TX side's `over_latency`) rising.
//!
//! **The block age is a diagnostic, not a receiver threshold.** It models
//! how old the oldest sample of a driver's block is at the pump's take: a
//! driver hands a whole call's inputs at once, so its first sample is one
//! call period old at the hand-over (128 frames at 96 kHz: 1.333 ms), and
//! the wait for the pump's next block adds to it. dasio's field reading
//! (16.11.2026) showed it above the packets' real ages (1.9 ms modelled,
//! 1.0-1.4 ms measured): a packet's stamp is not the capture time, so the
//! model does not say what a receiver sees. The driver's side records each
//! callback (its period, and the hand-over's time while the pump has not
//! taken an earlier one); the pump's side, when it plays the from-ASIO
//! player, takes the oldest untaken hand-over and records its age. Both on
//! the app's clock, atomics only: no allocation, no lock, no system call
//! beyond the clock's read. It leaves out the from-ASIO FIFO's own depth
//! (the bridge target, [`dlink::bridge::AppBuffer`]).
//!
//! Each max covers the last minute (one to two minutes: two windows of
//! one). The status's loads are not one snapshot: a read in the instant a
//! window rolls can show the new window's max alone once.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use dlink::bridge::Clock;
use dstack::media::tx::{AppView, MAX_FLOWS, ReceiverLateness};
use tracing::warn;

/// A window of the max, ns.
const MINUTE_NS: i64 = 60_000_000_000;
/// The network's share of a receiver's latency while the device measures
/// none (Windows; no RX flow), ns: what [`AgeReport::min_receiver_latency_us`]
/// adds to the packet age.
pub const NETWORK_ALLOWANCE_NS: u64 = 200_000;
/// The least time between two warnings about late receivers, ns.
const HOUR_NS: i64 = 3_600_000_000_000;
/// No time (no hand-over pending, no window, no warning yet).
const NONE: i64 = i64::MIN;

/// The ages' max over windows of a minute: the window running since
/// `start` and the one before it (single writer; module doc). A start of
/// `NONE` (before the first age) is minutes in the past to the saturating
/// arithmetic, so it needs no case of its own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Window {
    /// The current window's start, ns (`NONE` before the first age).
    pub(crate) start: i64,
    /// The current window's max, ns.
    pub(crate) max: u64,
    /// The window before it's max, ns (0 when that window had no ages).
    pub(crate) prev: u64,
}

impl Window {
    /// No age yet.
    pub(crate) const fn new() -> Self {
        Window {
            start: NONE,
            max: 0,
            prev: 0,
        }
    }

    /// An age of `age_ns` taken at `now`: a window a minute old closes (its
    /// max becomes the previous one's, or 0 when it closed over a minute
    /// ago) and the next starts at `now`.
    pub(crate) fn record(&mut self, now: i64, age_ns: u64) {
        if now.saturating_sub(self.start) >= MINUTE_NS {
            self.prev = if now.saturating_sub(self.start) < 2 * MINUTE_NS {
                self.max
            } else {
                0
            };
            self.start = now;
            self.max = age_ns;
        } else {
            self.max = self.max.max(age_ns);
        }
    }

    /// The max over the last minute (one to two) at `now`: `None` once no
    /// age came for one to two minutes (two minutes after the window's
    /// start), or never.
    pub(crate) fn max_at(&self, now: i64) -> Option<u64> {
        let since = now.saturating_sub(self.start);
        if since < MINUTE_NS {
            Some(self.max.max(self.prev))
        } else if since < 2 * MINUTE_NS {
            Some(self.max)
        } else {
            None
        }
    }
}

impl Default for Window {
    fn default() -> Self {
        Self::new()
    }
}

/// What the status shows ([`BlockAge::report`]), µs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgeReport {
    /// The last block's modelled age at the pump's take
    /// (`from_asio_block_age_us`, a diagnostic); `None` while no block was
    /// taken for one to two minutes.
    pub block_age_us: Option<u64>,
    /// Its max over the last minute (`from_asio_block_age_max_us`).
    pub block_age_max_us: Option<u64>,
    /// The oldest TX packet past its stamp over the last minute
    /// (`from_asio_packet_age_us`); `None` while no packet was seen for one
    /// to two minutes.
    pub packet_age_us: Option<u64>,
    /// The packet age + the network: the smallest receiver latency that
    /// never sees a late packet from this bridge
    /// (`from_asio_min_receiver_latency_us`).
    pub min_receiver_latency_us: Option<u64>,
}

/// A [`Window`] in three atomics (one writer: the pump's side for the
/// block age, the status loop for the packet age).
struct Shared {
    start: AtomicI64,
    max: AtomicU64,
    prev: AtomicU64,
}

impl Shared {
    const fn new() -> Self {
        Shared {
            start: AtomicI64::new(NONE),
            max: AtomicU64::new(0),
            prev: AtomicU64::new(0),
        }
    }

    fn load(&self) -> Window {
        Window {
            start: self.start.load(Ordering::Relaxed),
            max: self.max.load(Ordering::Relaxed),
            prev: self.prev.load(Ordering::Relaxed),
        }
    }

    /// Records `age_ns` at `now` (the writer's side).
    fn record(&self, now: i64, age_ns: u64) {
        let mut w = self.load();
        w.record(now, age_ns);
        self.max.store(w.max, Ordering::Relaxed);
        self.prev.store(w.prev, Ordering::Relaxed);
        self.start.store(w.start, Ordering::Relaxed);
    }
}

/// The from-ASIO block age's shared state (module doc).
pub struct BlockAge {
    clock: Clock,
    /// The latest callback's period, ns.
    period_ns: AtomicU64,
    /// The oldest hand-over the pump has not taken, ns (`NONE`: none).
    pending: AtomicI64,
    /// The last block age, ns.
    last_ns: AtomicU64,
    /// The block ages' window (the pump's side writes it).
    blocks: Shared,
    /// The TX packet ages' window (the status loop's [`BlockAge::look`]).
    packets: Shared,
    /// The network's share at the last look, ns.
    network_ns: AtomicU64,
    /// Each slot's late count at the last look (`None`: not seen yet).
    seen: Mutex<[Option<u64>; MAX_FLOWS]>,
    /// The last warning about late receivers, ns (`NONE`: none yet).
    warned: AtomicI64,
}

impl BlockAge {
    /// A bridge's figures on `clock`.
    pub fn new(clock: Clock) -> Self {
        BlockAge {
            clock,
            period_ns: AtomicU64::new(0),
            pending: AtomicI64::new(NONE),
            last_ns: AtomicU64::new(0),
            blocks: Shared::new(),
            packets: Shared::new(),
            network_ns: AtomicU64::new(NETWORK_ALLOWANCE_NS),
            seen: Mutex::new([None; MAX_FLOWS]),
            warned: AtomicI64::new(NONE),
        }
    }

    /// The driver's callback handed `frames` frames at `rate` Hz: its period
    /// and, unless an earlier hand-over is still untaken, its time.
    pub fn handed(&self, frames: usize, rate: u32) {
        let period = frames as u64 * 1_000_000_000 / u64::from(rate.max(1));
        self.period_ns.store(period, Ordering::Relaxed);
        // The oldest untaken hand-over stays: its age is the larger.
        let _ = self.pending.compare_exchange(
            NONE,
            (self.clock)(),
            Ordering::AcqRel,
            Ordering::Relaxed,
        );
    }

    /// The pump took the from-ASIO FIFO: the oldest untaken hand-over's age
    /// (its call period + the wait since) is recorded.
    pub fn taken(&self) {
        let at = self.pending.swap(NONE, Ordering::AcqRel);
        if at == NONE {
            return;
        }
        let now = (self.clock)();
        let wait = u64::try_from(now.saturating_sub(at)).unwrap_or(0);
        let age = self.period_ns.load(Ordering::Relaxed).saturating_add(wait);
        self.blocks.record(now, age);
        self.last_ns.store(age, Ordering::Relaxed);
    }

    /// The status loop's look at the TX side (module doc): `view` from
    /// [`dstack::media::tx::TxShared::take_app_view`], `network_p99_ns`
    /// from [`dstack::media::rx::RxShared::network_p99_ns`]. Records the
    /// view's packet age (none without a packet) and warns, at most once an
    /// hour, when a receiver's late count rose since the last look (a
    /// slot's first look only learns its count). Returns the warning it
    /// logged. Call it from a status loop (about once a second), never the
    /// pump: the view locks the TX table.
    pub fn look(&self, view: &AppView, network_p99_ns: Option<u64>) -> Option<String> {
        let now = (self.clock)();
        self.network_ns.store(
            network_p99_ns.unwrap_or(NETWORK_ALLOWANCE_NS),
            Ordering::Relaxed,
        );
        if view.age_peak_ns != 0 {
            self.packets.record(now, view.age_peak_ns);
        }
        let rose = self.rises(&view.receivers);
        // `NONE` (no warning yet) is hours in the past to the saturation.
        if rose.is_empty() || now.saturating_sub(self.warned.load(Ordering::Relaxed)) < HOUR_NS {
            return None;
        }
        self.warned.store(now, Ordering::Relaxed);
        let text = late_text(&rose, self.report_at(now));
        warn!("{text}");
        Some(text)
    }

    /// Each receiver whose slot's late count rose since the last look, with
    /// the rise; every count seen is kept for the next look.
    fn rises<'a>(&self, receivers: &'a [ReceiverLateness]) -> Vec<(&'a ReceiverLateness, u64)> {
        let mut seen = self.seen.lock().unwrap_or_else(PoisonError::into_inner);
        let mut out = Vec::new();
        for r in receivers {
            let Some(last) = seen.get_mut(r.slot) else {
                continue;
            };
            if let Some(was) = last.replace(r.over_latency) {
                let rose = r.over_latency.saturating_sub(was);
                if rose != 0 {
                    out.push((r, rose));
                }
            }
        }
        out
    }

    /// The status's values (module doc): `None` while neither a block was
    /// taken nor a packet seen for one to two minutes (no driver, or a
    /// closed one, and no TX).
    pub fn report(&self) -> Option<AgeReport> {
        self.report_at((self.clock)())
    }

    fn report_at(&self, now: i64) -> Option<AgeReport> {
        let block = self.blocks.load().max_at(now);
        let packet = self.packets.load().max_at(now);
        if block.is_none() && packet.is_none() {
            return None;
        }
        let network = self.network_ns.load(Ordering::Relaxed);
        Some(AgeReport {
            block_age_us: block.map(|_| self.last_ns.load(Ordering::Relaxed) / 1_000),
            block_age_max_us: block.map(|m| m / 1_000),
            packet_age_us: packet.map(|p| p / 1_000),
            min_receiver_latency_us: packet.map(|p| p.saturating_add(network) / 1_000),
        })
    }
}

/// The warning for the receivers whose late count `rose` (each with its
/// rise), with the packet age and receiver figure of `report` when known.
fn late_text(rose: &[(&ReceiverLateness, u64)], report: Option<AgeReport>) -> String {
    let who: Vec<String> = rose
        .iter()
        .map(|(r, n)| format!("{} at {} us ({n} late)", r.receiver, r.latency_ns / 1_000))
        .collect();
    let mut text = format!(
        "asio bridge: a receiver counted our TX packets late: {}",
        who.join(", ")
    );
    if let Some((Some(age), Some(need))) =
        report.map(|r| (r.packet_age_us, r.min_receiver_latency_us))
    {
        text.push_str(&format!(
            "; our packets were up to {age} us old past their stamps in the last minute, so \
             a receiver needs at least {need} us"
        ));
    }
    text
}

#[cfg(test)]
mod tests;

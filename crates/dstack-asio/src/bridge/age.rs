// SPDX-License-Identifier: MIT OR Apache-2.0

//! The from-ASIO block age (dstack #325, dasio#120): how old the oldest
//! sample of a driver's block is when the pump takes it for TX. The driver
//! hands a whole call's inputs at once, so its first sample is already one
//! call period old (128 frames at 96 kHz: 1.333 ms) at the hand-over; the
//! wait until the pump's next block adds to it. A receiver whose latency is
//! under that age (plus the pump's block) can get late packets from us,
//! whatever the rest of the path does.
//!
//! The driver's side records each callback (its period, and the hand-over's
//! time while the pump has not taken an earlier one); the pump's side,
//! when it plays the from-ASIO player, takes the oldest untaken hand-over
//! and records its age. Both on the app's clock, atomics only: no
//! allocation, no lock, no system call beyond the clock's read. The
//! status reads the current age, the max over the last minute (one to two
//! minutes: two windows of one) and the smallest receiver latency that
//! never sees a late packet from this path ([`BlockAge::report`]); a
//! status loop checks the TX flows' known receiver latencies against it
//! once an hour ([`BlockAge::warn_low_receivers`]).
//!
//! The age counts the driver's call and the wait for the pump's next
//! block (the design of dstack #325). It leaves out the from-ASIO FIFO's
//! own depth (the bridge target, [`dlink::bridge::AppBuffer`]): that is the
//! bridge's delay between the two clocks, set by the target, not a
//! property of the driver's calls.
//!
//! The status's three loads are not one snapshot: a read in the instant
//! the pump rolls the window can show the new window's max alone once.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};

use dlink::bridge::Clock;
use tracing::warn;

/// A window of the max, ns.
const MINUTE_NS: i64 = 60_000_000_000;
/// The least time between two warnings about low receivers, ns.
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
    /// The last block's age at the pump's take (`from_asio_block_age_us`).
    pub block_age_us: u64,
    /// The max over the last minute (`from_asio_block_age_max_us`).
    pub block_age_max_us: u64,
    /// The max + one pump block: the smallest receiver latency that never
    /// sees a late packet from this path
    /// (`from_asio_min_receiver_latency_us`).
    pub min_receiver_latency_us: u64,
}

/// The from-ASIO block age's shared state (module doc).
pub struct BlockAge {
    clock: Clock,
    /// The pump's block, ns at the Dante rate.
    block_ns: u64,
    /// The latest callback's period, ns.
    period_ns: AtomicU64,
    /// The oldest hand-over the pump has not taken, ns (`NONE`: none).
    pending: AtomicI64,
    /// The last age, ns.
    last_ns: AtomicU64,
    /// The [`Window`]'s three fields (the pump's side writes them).
    start: AtomicI64,
    max: AtomicU64,
    prev: AtomicU64,
    /// The last warning about low receivers, ns (`NONE`: none yet).
    warned: AtomicI64,
}

impl BlockAge {
    /// The age of a bridge whose pump block is `block_ns`, on `clock`.
    pub fn new(block_ns: u64, clock: Clock) -> Self {
        BlockAge {
            clock,
            block_ns,
            period_ns: AtomicU64::new(0),
            pending: AtomicI64::new(NONE),
            last_ns: AtomicU64::new(0),
            start: AtomicI64::new(NONE),
            max: AtomicU64::new(0),
            prev: AtomicU64::new(0),
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
        let mut w = self.window();
        w.record(now, age);
        self.last_ns.store(age, Ordering::Relaxed);
        self.max.store(w.max, Ordering::Relaxed);
        self.prev.store(w.prev, Ordering::Relaxed);
        self.start.store(w.start, Ordering::Relaxed);
    }

    fn window(&self) -> Window {
        Window {
            start: self.start.load(Ordering::Relaxed),
            max: self.max.load(Ordering::Relaxed),
            prev: self.prev.load(Ordering::Relaxed),
        }
    }

    /// The status's values (module doc): `None` before the first take, and
    /// once no block was taken for one to two minutes (no driver, or a
    /// closed one).
    pub fn report(&self) -> Option<AgeReport> {
        let max = self.window().max_at((self.clock)())?;
        Some(AgeReport {
            block_age_us: self.last_ns.load(Ordering::Relaxed) / 1_000,
            block_age_max_us: max / 1_000,
            min_receiver_latency_us: max.saturating_add(self.block_ns) / 1_000,
        })
    }

    /// Checks the TX flows' receivers (each one's name and stated latency,
    /// ns; 0: unknown, [`dstack::media::tx::TxShared::receiver_latencies`])
    /// against the smallest receiver latency this path serves, and warns
    /// at most once an hour when a known one is below it. Returns the
    /// warning it logged. Call it from a status loop, not the pump.
    pub fn warn_low_receivers(&self, receivers: &[(String, u64)]) -> Option<String> {
        let report = self.report()?;
        let text = low_receivers_text(report, receivers)?;
        let now = (self.clock)();
        // `NONE` (no warning yet) is hours in the past to the saturation.
        if now.saturating_sub(self.warned.load(Ordering::Relaxed)) < HOUR_NS {
            return None;
        }
        self.warned.store(now, Ordering::Relaxed);
        warn!("{text}");
        Some(text)
    }
}

/// The warning for the `receivers` (name, stated latency ns) whose known
/// latency is below `report`'s smallest serving one; `None` when none is.
pub fn low_receivers_text(report: AgeReport, receivers: &[(String, u64)]) -> Option<String> {
    let need_ns = report.min_receiver_latency_us * 1_000;
    let low: Vec<String> = receivers
        .iter()
        .filter(|(_, ns)| *ns != 0 && *ns < need_ns)
        .map(|(name, ns)| format!("{name} at {} us", ns / 1_000))
        .collect();
    (!low.is_empty()).then(|| {
        format!(
            "asio bridge: a receiver's latency is below the from-ASIO block age: {}; the driver's \
             blocks are up to {} us old at the pump's take, so a receiver needs at least {} us \
             (set it higher, or a smaller ASIO buffer)",
            low.join(", "),
            report.block_age_max_us,
            report.min_receiver_latency_us
        )
    })
}

#[cfg(test)]
mod tests;

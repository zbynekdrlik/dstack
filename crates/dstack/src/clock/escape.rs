// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The bounded escape of the Windows low-latency wait (dstack #1,
//! `DeviceConfig::spin_escape_ms`, a trial, off by default): the spin
//! does not yield (SwitchToThread and Sleep(0) both gave a lower priority
//! the CPU), so none takes a media thread's CPU near a send, and once a period each spinning
//! media thread blocks one timer tick, whose expiry preempts lower
//! priorities at once, so per-CPU system work still runs. The decisions
//! live here, cross-platform and tested on Linux; the Windows wait
//! (`sys_windows`) is the glue. Linux's wait never spins and ignores it.

use super::Os;

/// The shortest bounded-escape period a device may set, ms.
pub const SPIN_ESCAPE_MIN_MS: u64 = 100;
/// The longest bounded-escape period a device may set, ms.
pub const SPIN_ESCAPE_MAX_MS: u64 = 10_000;
/// The bounded escape's block: a relative due time on the high-resolution
/// timer, ns (one tick at the 1 ms resolution).
pub const ESCAPE_BLOCK_NS: u64 = 1_000_000;
/// How late the high-resolution timer wakes at most, ns: one tick + 140 µs
/// at the 1 ms resolution (the FOH PC, 5.11.2026: max 1.11 ms; at 0.5 ms,
/// max 0.64 ms).
pub const TIMER_LATE_NS: u64 = 1_140_000;
/// How far ahead TX sends before a bounded escape, ns: the longest its
/// block lasts, the block and the timer's late wake ([`ESCAPE_BLOCK_NS`] +
/// [`TIMER_LATE_NS`], 2.14 ms). The send ahead repeats from a fresh clock
/// read while it sends anything, so its own send time is covered too
/// (`media::tx::Transmitter::presend_all`).
pub const PRESEND_NS: u64 = ESCAPE_BLOCK_NS + TIMER_LATE_NS;
/// How long past its period TX may put off an escape because a packet in
/// the presend window had no samples yet, ns: a packet held through the
/// block past the latency would restart its flow's schedule. Past this
/// the escape happens anyway, so a thread escapes at least every period +
/// 20 ms.
pub const ESCAPE_DEFER_MAX_NS: u64 = 20_000_000;

/// The bounded-escape period for a device's `spin_escape_ms`, ns: 0
/// (off) when it sets none.
pub fn spin_escape_of(configured_ms: Option<u64>) -> u64 {
    configured_ms.map_or(0, |ms| ms.saturating_mul(1_000_000))
}

/// The start line for a device's `spin_escape_ms` on `os`: `None` when
/// it sets none.
pub fn spin_escape_line(configured_ms: Option<u64>, os: Os) -> Option<String> {
    let ms = configured_ms?;
    Some(match os {
        Os::Windows => format!(
            "bounded escape on (trial, dstack #1): the low-latency spin does not yield, \
             each spinning media thread blocks {} µs on the timer every {ms} ms, TX sends {} µs ahead first",
            ESCAPE_BLOCK_NS / 1_000,
            PRESEND_NS / 1_000
        ),
        Os::Other => {
            format!("spin_escape_ms = {ms} is for Windows only: ignored on this OS")
        }
    })
}

/// A media thread's bounded escape: when its last escape was, and, for a
/// thread that sends ahead first (TX, [`EscapeState::ask`]), whether its
/// next wait escapes. Kept per thread.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct EscapeState {
    /// Monotonic ns of the last escape (or of the first look).
    last: Option<i64>,
    /// The thread asks before its waits ([`EscapeState::ask`]): its waits
    /// escape only when it was told so.
    asks: bool,
    /// The thread was told its next wait escapes.
    armed: bool,
}

impl EscapeState {
    pub const fn new() -> Self {
        EscapeState {
            last: None,
            asks: false,
            armed: false,
        }
    }

    /// Whether an escape is due at monotonic `now` with `period_ns`
    /// between escapes (0: never); the first look only starts the period.
    fn due(&mut self, now: i64, period_ns: u64) -> bool {
        if period_ns == 0 {
            return false;
        }
        match self.last {
            None => {
                self.last = Some(now);
                false
            }
            Some(last) => now.saturating_sub(last) >= i64::try_from(period_ns).unwrap_or(i64::MAX),
        }
    }

    /// A thread that sends ahead before it escapes (TX) asks before its
    /// wait: `true` when the wait will escape. From the first ask, the
    /// thread's waits escape only when an ask said so, so what it sent
    /// ahead and its escape always go together.
    pub fn ask(&mut self, now: i64, period_ns: u64) -> bool {
        self.asks = true;
        self.armed = self.due(now, period_ns);
        self.armed
    }

    /// TX could not send ahead the whole window (a packet's samples were
    /// not there yet): puts off the armed escape at monotonic `now`, unless
    /// it is already [`ESCAPE_DEFER_MAX_NS`] past its period. `true`: put
    /// off (the next ask asks again); `false`: nothing armed, or overdue,
    /// so the wait escapes anyway.
    pub fn defer(&mut self, now: i64, period_ns: u64) -> bool {
        if !self.armed {
            return false;
        }
        let waited = self.last.map_or(0, |last| now.saturating_sub(last));
        let cap = i64::try_from(period_ns.saturating_add(ESCAPE_DEFER_MAX_NS)).unwrap_or(i64::MAX);
        if waited < cap {
            self.armed = false;
            return true;
        }
        false
    }

    /// The wait at monotonic `now`: whether it escapes (an ask said so, or,
    /// for a thread that never asks, the period is over). An escape starts
    /// the next period.
    pub fn take(&mut self, now: i64, period_ns: u64) -> bool {
        let go = if self.asks {
            std::mem::take(&mut self.armed)
        } else {
            self.due(now, period_ns)
        };
        if go {
            self.last = Some(now);
        }
        go
    }
}

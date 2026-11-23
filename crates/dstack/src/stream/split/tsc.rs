// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A thread's CPU cycles as ns (dstack #485): the arithmetic behind the
//! Windows thread CPU read ([`super::thread_cpu_ns`] on Windows x86_64).
//!
//! Windows counts a thread's CPU time per scheduler tick (about 15.6 ms),
//! too coarse for a 1 ms pass, but `QueryThreadCycleTime` counts the
//! thread's cycles in TSC ticks (the invariant TSC) and, since Vista,
//! leaves out the cycles of the interrupts and DPCs that ran meanwhile. So
//! a TX pass held by a DPC, an ISR or another thread reads a wall time far
//! above its CPU time; one that did the work reads them near each other.
//!
//! The cycles become ns at the TSC's rate, measured here without a sleep:
//! the first read keeps a (TSC, wall) point, and the first read at least
//! [`CALIBRATION_NS`] of wall time later sets the rate from the two
//! ([`TscRate::observe`]). Until then a read is unmeasured (`None`), never
//! a guess. Over 100 ms a 100 ns wall clock gives the rate to about 1e-6.
//!
//! Everything here is pure (the TSC and the wall time come in), so it is
//! tested on every OS; only the OS calls stay in the Windows glue.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// The wall time between the two points the TSC's rate is taken from, at
/// least, ns.
pub const CALIBRATION_NS: u64 = 100_000_000;

/// The TSC's rate, ticks a second, from `ticks` counted over `wall_ns` of
/// wall time; `None` under [`CALIBRATION_NS`] of wall time or with no tick
/// (a TSC read that did not move or went back), or a rate past u64.
pub fn rate_hz(ticks: u64, wall_ns: u64) -> Option<u64> {
    if wall_ns < CALIBRATION_NS || ticks == 0 {
        return None;
    }
    u64::try_from(u128::from(ticks) * 1_000_000_000 / u128::from(wall_ns)).ok()
}

/// `cycles` TSC ticks as ns at `hz` ticks a second; `None` at a rate of 0
/// or a time past u64 (never a wrap).
pub fn cycles_ns(cycles: u64, hz: u64) -> Option<u64> {
    if hz == 0 {
        return None;
    }
    u64::try_from(u128::from(cycles) * 1_000_000_000 / u128::from(hz)).ok()
}

/// The TSC's rate, taken once from the first two reads at least
/// [`CALIBRATION_NS`] apart (any thread's), then fixed: one per process
/// (a `static`). Lock-free after the first read; no allocation.
#[derive(Debug, Default)]
pub struct TscRate {
    start: OnceLock<(u64, u64)>,
    hz: AtomicU64,
}

impl TscRate {
    pub const fn new() -> Self {
        TscRate {
            start: OnceLock::new(),
            hz: AtomicU64::new(0),
        }
    }

    /// A read of the TSC (`tsc`) and the wall time (`wall_ns`, ns on one
    /// fixed base): the rate once known, `None` before. The first read is
    /// the start point; a later one at least [`CALIBRATION_NS`] past it
    /// sets the rate for good.
    pub fn observe(&self, tsc: u64, wall_ns: u64) -> Option<u64> {
        let hz = self.hz.load(Ordering::Relaxed);
        if hz != 0 {
            return Some(hz);
        }
        let &(tsc0, wall0) = self.start.get_or_init(|| (tsc, wall_ns));
        let hz = rate_hz(tsc.saturating_sub(tsc0), wall_ns.saturating_sub(wall0))?;
        self.hz.store(hz, Ordering::Relaxed);
        Some(hz)
    }
}

/// A thread's `cycles` as ns, with the TSC read `tsc` and the wall time
/// `wall_ns` taken with them (they feed `rate` until it is known);
/// `None` while the rate is unknown.
pub fn thread_ns(cycles: u64, rate: &TscRate, tsc: u64, wall_ns: u64) -> Option<u64> {
    cycles_ns(cycles, rate.observe(tsc, wall_ns)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_rate_is_the_ticks_over_at_least_100_ms() {
        // 3 GHz: 300 M ticks in 100 ms.
        assert_eq!(rate_hz(300_000_000, 100_000_000), Some(3_000_000_000));
        // 2.5 GHz over 400 ms.
        assert_eq!(rate_hz(1_000_000_000, 400_000_000), Some(2_500_000_000));
        // Under 100 ms: none yet, even with ticks.
        assert_eq!(rate_hz(299_999_999, 99_999_999), None);
        // No tick: none.
        assert_eq!(rate_hz(0, 500_000_000), None);
        // Ticks past u64 a second: none, never a wrap.
        assert_eq!(rate_hz(u64::MAX, 100_000_000), None);
        // The product overflows u64 but not u128: still right.
        assert_eq!(rate_hz(u64::MAX / 10, 1_000_000_000), Some(u64::MAX / 10));
    }

    #[test]
    fn cycles_are_ns_at_the_rate() {
        // 3 GHz: 3_159_000 cycles are 1.053 ms (#485's longest pass).
        assert_eq!(cycles_ns(3_159_000, 3_000_000_000), Some(1_053_000));
        assert_eq!(cycles_ns(1, 1_000_000_000), Some(1));
        assert_eq!(cycles_ns(2_999, 3_000_000_000), Some(999));
        assert_eq!(cycles_ns(0, 3_000_000_000), Some(0));
        assert_eq!(cycles_ns(5, 0), None);
        // A thread's lifetime cycles past u64's product: u128, no wrap.
        assert_eq!(cycles_ns(u64::MAX, 3_000_000_000), Some(u64::MAX / 3));
        // Past u64 ns: none.
        assert_eq!(cycles_ns(u64::MAX, 999_999_999), None);
    }

    #[test]
    fn the_rate_is_known_from_the_first_read_100_ms_past_the_start() {
        let r = TscRate::new();
        assert_eq!(r.observe(1_000, 5_000), None, "the start point");
        assert_eq!(r.observe(150_001_000, 50_005_000), None, "50 ms in");
        // 100 ms after the start, 300 M ticks: 3 GHz.
        assert_eq!(r.observe(300_001_000, 100_005_000), Some(3_000_000_000));
        // Fixed from then on, whatever a later read gives.
        assert_eq!(r.observe(7, 9), Some(3_000_000_000));
        // A TSC read behind the start never sets a rate.
        let back = TscRate::default();
        assert_eq!(back.observe(900, 0), None);
        assert_eq!(back.observe(100, 200_000_000), None);
        assert_eq!(back.observe(600_000_900, 200_000_000), Some(3_000_000_000));
    }

    #[test]
    fn a_thread_s_time_is_unmeasured_until_the_rate_is_known() {
        let r = TscRate::new();
        assert_eq!(thread_ns(3_000, &r, 10, 0), None);
        assert_eq!(thread_ns(3_000, &r, 2_000_000_010, 200_000_000), Some(300));
        assert_eq!(thread_ns(3_159_000, &r, 0, 0), Some(315_900));
    }
}

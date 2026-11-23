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
//! a guess. Each point's wall time is read before and after its TSC read
//! and taken at the middle; a point whose two wall reads are more than
//! [`BRACKET_NS`] apart (the thread was preempted, or a DPC ran, between
//! them) is no calibration point. So the rate is within 2e-4 (20 us over
//! 100 ms), the wall clock's 100 ns quantum adding about 1e-6.
//!
//! Converting cycles by the TSC's rate holds only on a CPU whose TSC runs
//! at a constant rate in every state (the invariant TSC, CPUID
//! 0x8000_0007 EDX bit 8): elsewhere the read stays unmeasured
//! ([`invariant_tsc`]).
//!
//! Everything here is pure (the TSC and the wall time come in), so it is
//! tested on every OS; only the OS calls stay in the Windows glue.

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

/// The wall time between the two points the TSC's rate is taken from, at
/// least, ns.
pub const CALIBRATION_NS: u64 = 100_000_000;

/// The widest wall-time bracket around a calibration point's TSC read,
/// ns ([`TscRate::observe`]).
pub const BRACKET_NS: u64 = 20_000;

/// Whether the TSC is invariant (a constant rate in every power and
/// frequency state): the highest extended CPUID leaf `max_extended_leaf`
/// reaches 0x8000_0007 and that leaf's EDX (`edx_80000007`) has bit 8.
pub fn invariant_tsc(max_extended_leaf: u32, edx_80000007: u32) -> bool {
    max_extended_leaf >= 0x8000_0007 && edx_80000007 & 0x100 != 0
}

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

    /// A read of the TSC (`tsc`) between two wall-time reads
    /// (`wall_before`, `wall_after`: ns on one fixed base): the rate once
    /// known, `None` before. A read bracketed within [`BRACKET_NS`] is a
    /// point at the bracket's middle: the first is the start, a later one
    /// at least [`CALIBRATION_NS`] past it sets the rate for good. A wider
    /// bracket is no point.
    pub fn observe(&self, tsc: u64, wall_before: u64, wall_after: u64) -> Option<u64> {
        let hz = self.hz.load(Ordering::Relaxed);
        if hz != 0 {
            return Some(hz);
        }
        let bracket = wall_after.saturating_sub(wall_before);
        if bracket > BRACKET_NS {
            return None;
        }
        let wall_ns = wall_before + bracket / 2;
        let &(tsc0, wall0) = self.start.get_or_init(|| (tsc, wall_ns));
        let hz = rate_hz(tsc.saturating_sub(tsc0), wall_ns.saturating_sub(wall0))?;
        self.hz.store(hz, Ordering::Relaxed);
        Some(hz)
    }
}

/// A thread's `cycles` as ns, with the TSC read `tsc` and the wall times
/// around it (`wall_before`, `wall_after`) taken with them (they feed
/// `rate` until it is known); `None` while the rate is unknown.
pub fn thread_ns(
    cycles: u64,
    rate: &TscRate,
    tsc: u64,
    wall_before: u64,
    wall_after: u64,
) -> Option<u64> {
    cycles_ns(cycles, rate.observe(tsc, wall_before, wall_after)?)
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
        assert_eq!(r.observe(1_000, 5_000, 5_000), None, "the start point");
        assert_eq!(
            r.observe(150_001_000, 50_005_000, 50_005_000),
            None,
            "50 ms in"
        );
        // 100 ms after the start, 300 M ticks: 3 GHz.
        assert_eq!(
            r.observe(300_001_000, 100_005_000, 100_005_000),
            Some(3_000_000_000)
        );
        // Fixed from then on, whatever a later read gives.
        assert_eq!(r.observe(7, 9, 1_000_000_000), Some(3_000_000_000));
        // A TSC read behind the start never sets a rate.
        let back = TscRate::default();
        assert_eq!(back.observe(900, 0, 0), None);
        assert_eq!(back.observe(100, 200_000_000, 200_000_000), None);
        assert_eq!(
            back.observe(600_000_900, 200_000_000, 200_000_000),
            Some(3_000_000_000)
        );
    }

    #[test]
    fn a_point_is_the_middle_of_a_bracket_of_at_most_20_us() {
        let r = TscRate::new();
        // Too wide a bracket (preempted between the reads): no start.
        assert_eq!(r.observe(5, 0, 20_001), None);
        // The start: TSC 0 at wall 1_000 (the middle of 0..2_000).
        assert_eq!(r.observe(0, 0, 2_000), None);
        // Too wide: no rate, however far past the start.
        assert_eq!(r.observe(600_000_000, 200_000_000, 200_020_001), None);
        // At the bound: TSC 300_003_000 at the middle 100_001_000 of a
        // 20 us bracket, 100_000_000 ns after the start: 3.00003 GHz.
        assert_eq!(
            r.observe(300_003_000, 99_991_000, 100_011_000),
            Some(3_000_030_000)
        );
    }

    #[test]
    fn only_an_invariant_tsc_converts() {
        assert!(invariant_tsc(0x8000_0007, 0x100));
        assert!(invariant_tsc(0x8000_0008, 0xffff_ffff));
        assert!(!invariant_tsc(0x8000_0007, 0xfeff));
        assert!(!invariant_tsc(0x8000_0007, 0));
        // The leaf is not there: its EDX means nothing.
        assert!(!invariant_tsc(0x8000_0006, 0x100));
        assert!(!invariant_tsc(0, 0xffff_ffff));
    }

    #[test]
    fn a_thread_s_time_is_unmeasured_until_the_rate_is_known() {
        let r = TscRate::new();
        assert_eq!(thread_ns(3_000, &r, 10, 0, 0), None);
        assert_eq!(
            thread_ns(3_000, &r, 2_000_000_010, 200_000_000, 200_000_000),
            Some(300)
        );
        assert_eq!(thread_ns(3_159_000, &r, 0, 0, 0), Some(315_900));
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The runner's timer state in a Windows timer test's failure (dstack
//! #339, its fifth case): a VM whose every timer wait oversleeps about
//! 1 ms at the median (v0.6.157 tag run 43961897282, attempt 2: median
//! 1012500 ns) is a VM whose waitable timer fires only on its 1 ms timer
//! resolution, not a regression of ours. `sys_windows`'s timer tests read
//! the state ([`TimerState`]: `NtQueryTimerResolution` and whether the
//! thread got a high-resolution waitable timer) and print
//! [`describe`] in their failure. A run on a coarse timer still fails (a
//! test that cannot run truly fails); only the message names the runner.
//!
//! Cross-platform and pure, so its Linux unit tests run in the mutation
//! gate (the module is compiled in every build for that: a `#[cfg(test)]`
//! module is skipped by cargo-mutants); the Windows reads stay in
//! `sys_windows`'s tests. Nothing outside the tests calls it.

#![cfg_attr(not(test), allow(dead_code))]

/// The timer state of the thread that waited, as the OS reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerState {
    /// The timer resolution now, ns (`NtQueryTimerResolution`'s current);
    /// `None` when it could not be read.
    pub current_ns: Option<u64>,
    /// The finest resolution the OS offers, ns (its "minimum").
    pub finest_ns: Option<u64>,
    /// The coarsest, ns (its "maximum", the default tick: 15.625 ms).
    pub coarsest_ns: Option<u64>,
    /// The thread got a waitable timer made with
    /// `CREATE_WAITABLE_TIMER_HIGH_RESOLUTION`; without one its waits use
    /// `std::thread::sleep` (`sys_windows::block_for`).
    pub high_resolution: bool,
}

/// `NtQueryTimerResolution`'s three values (100 ns units) and its status
/// as the state: `None` for each when the call failed (`status` < 0).
pub fn from_nt(
    status: i32,
    coarsest_100ns: u32,
    finest_100ns: u32,
    current_100ns: u32,
    high_resolution: bool,
) -> TimerState {
    let ns = |v: u32| (status >= 0).then_some(u64::from(v) * 100);
    TimerState {
        current_ns: ns(current_100ns),
        finest_ns: ns(finest_100ns),
        coarsest_ns: ns(coarsest_100ns),
        high_resolution,
    }
}

/// A median oversleep "matches" the resolution when it is within a quarter
/// of it: a wait that fires on the resolution's tick ends about one tick
/// late (the FOH PC, 5.11.2026: at most one tick + 140 us).
fn matches(median_late_ns: i64, resolution_ns: u64) -> bool {
    let Ok(res) = i64::try_from(resolution_ns) else {
        return false;
    };
    res > 0 && (median_late_ns - res).abs() <= res / 4
}

fn ms(ns: Option<u64>) -> String {
    ns.map_or_else(|| "?".to_owned(), |v| format!("{:.3}", v as f64 / 1e6))
}

/// The verdict and the state, for a failure message: the median oversleep
/// of waits on the timer (`on_timer`; a spun wait never waits on it) and
/// the runner's timer state. `RUNNER: coarse timer (res X ms)` when the
/// median matches the timer resolution, `OURS` when the resolution is
/// known and does not explain it, `UNKNOWN` when it could not be read or
/// the thread had no high-resolution timer (its waits were not on ours:
/// the OS refusing the timer is not shown to be the runner's).
pub fn describe(median_late_ns: i64, on_timer: bool, state: &TimerState) -> String {
    let verdict = match state.current_ns {
        _ if !on_timer => "OURS: a spun wait, not on the timer".to_owned(),
        None => "UNKNOWN: the timer resolution could not be read".to_owned(),
        Some(_) if !state.high_resolution => {
            "UNKNOWN: no high-resolution waitable timer for this thread".to_owned()
        }
        Some(res) if matches(median_late_ns, res) => {
            format!("RUNNER: coarse timer (res {} ms)", ms(Some(res)))
        }
        Some(res) => format!(
            "OURS: the median oversleep does not match the timer resolution {} ms",
            ms(Some(res))
        ),
    };
    format!(
        "{verdict}; median oversleep {:.3} ms; timer resolution now {} ms \
         (finest {}, coarsest {}); high-resolution waitable timer {}",
        median_late_ns as f64 / 1e6,
        ms(state.current_ns),
        ms(state.finest_ns),
        ms(state.coarsest_ns),
        if state.high_resolution {
            "granted"
        } else {
            "NOT granted (std::thread::sleep)"
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fifth case's runner as `NtQueryTimerResolution` would read on a
    /// VM at 1 ms: coarsest 15.625 ms, finest 0.5 ms.
    fn at(current_100ns: u32) -> TimerState {
        from_nt(0, 156_250, 5_000, current_100ns, true)
    }

    #[test]
    fn the_fifth_case_reads_a_coarse_timer_at_its_resolution() {
        // v0.6.157 tag run 43961897282 attempt 2: median 1012500 ns.
        let s = describe(1_012_500, true, &at(10_000));
        assert!(s.starts_with("RUNNER: coarse timer (res 1.000 ms);"), "{s}");
        assert!(s.contains("median oversleep 1.012 ms"), "{s}");
        assert!(s.contains("(finest 0.500, coarsest 15.625)"), "{s}");
        assert!(s.contains("high-resolution waitable timer granted"), "{s}");
    }

    #[test]
    fn a_median_off_the_resolution_by_more_than_a_quarter_is_ours() {
        // 1 ms: matches from 0.75 to 1.25 ms.
        assert!(describe(1_250_000, true, &at(10_000)).starts_with("RUNNER"));
        assert!(describe(750_000, true, &at(10_000)).starts_with("RUNNER"));
        let s = describe(1_250_001, true, &at(10_000));
        assert!(
            s.starts_with("OURS: the median oversleep does not match the timer resolution 1.000"),
            "{s}"
        );
        assert!(describe(749_999, true, &at(10_000)).starts_with("OURS"));
        // A fine resolution never explains a 1 ms median.
        assert!(describe(1_012_500, true, &at(5_000)).starts_with("OURS"));
        // The default tick explains a tick-long median.
        assert!(
            describe(15_700_000, true, &at(156_250))
                .starts_with("RUNNER: coarse timer (res 15.625 ms)")
        );
        // A wait that ended early never matches.
        assert!(describe(-1_000_000, true, &at(10_000)).starts_with("OURS"));
        // A resolution of 0 explains nothing, not even a median of 0.
        assert!(describe(0, true, &at(0)).starts_with("OURS"));
    }

    #[test]
    fn a_thread_without_the_high_resolution_timer_is_never_the_runner() {
        let s = describe(1_012_500, true, &from_nt(0, 156_250, 5_000, 10_000, false));
        assert!(
            s.starts_with("UNKNOWN: no high-resolution waitable timer for this thread;"),
            "{s}"
        );
        assert!(s.contains("now 1.000 ms"), "{s}");
    }

    #[test]
    fn a_failed_query_is_unknown_and_a_spun_wait_is_never_the_timer() {
        let failed = from_nt(-1, 156_250, 5_000, 10_000, false);
        assert_eq!(failed.current_ns, None);
        let s = describe(1_012_500, true, &failed);
        assert!(s.starts_with("UNKNOWN"), "{s}");
        assert!(s.contains("now ? ms (finest ?, coarsest ?)"), "{s}");
        assert!(s.contains("NOT granted"), "{s}");
        let spun = describe(1_012_500, false, &at(10_000));
        assert!(spun.starts_with("OURS: a spun wait"), "{spun}");
    }

    #[test]
    fn the_state_is_in_ns() {
        assert_eq!(
            at(10_000),
            TimerState {
                current_ns: Some(1_000_000),
                finest_ns: Some(500_000),
                coarsest_ns: Some(15_625_000),
                high_resolution: true,
            }
        );
    }
}

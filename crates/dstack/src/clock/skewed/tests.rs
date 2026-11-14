// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

const S: i64 = 1_000_000_000;

#[test]
fn a_skew_converts_to_parts_per_ten_to_the_twelve() {
    assert_eq!(TestClockPpm(6.0).ppt(), 6_000_000);
    assert_eq!(TestClockPpm(-6.5).ppt(), -6_500_000);
    assert_eq!(TestClockPpm(0.0000004).ppt(), 0);
    assert_eq!(TestClockPpm(0.0000006).ppt(), 1);
    assert_eq!(TestClockPpm(1000.0).ppt(), 1_000_000_000);
}

#[test]
fn a_skew_out_of_range_or_not_finite_is_a_problem() {
    for ok in [0.0, 6.0, -6.0, 1000.0, -1000.0] {
        assert_eq!(TestClockPpm(ok).problem(), None, "{ok}");
    }
    for bad in [
        1000.001,
        -1000.001,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
    ] {
        let p = TestClockPpm(bad).problem().expect("refused");
        assert!(p.starts_with("test_clock_ppm: "), "{p}");
    }
}

#[test]
fn skews_compare_by_their_bits() {
    assert_eq!(TestClockPpm(6.0), TestClockPpm(6.0));
    assert_ne!(TestClockPpm(6.0), TestClockPpm(-6.0));
    assert_ne!(TestClockPpm(0.0), TestClockPpm(-0.0));
}

/// The design's rate check (dstack #305): over 10 s of the monotonic clock
/// the skewed clock advances 10 s × (1 + ppm), to the ns.
#[test]
fn the_skewed_clock_runs_ppm_off_the_monotonic_clock_over_ten_seconds() {
    let (base, k) = (5 * S, 1_700_000_000 * S);
    for (ppm, extra) in [(6.0, 60_000), (-6.0, -60_000), (100.0, 1_000_000), (0.0, 0)] {
        let ppt = TestClockPpm(ppm).ppt();
        let at_base = skewed_ns(base, k, ppt, base).unwrap();
        assert_eq!(at_base, u64::try_from(base + k).unwrap(), "{ppm}");
        let later = skewed_ns(base, k, ppt, base + 10 * S).unwrap();
        assert_eq!(
            i64::try_from(later - at_base).unwrap(),
            10 * S + extra,
            "{ppm} ppm over 10 s"
        );
    }
}

#[test]
fn the_skew_part_rounds_toward_minus_infinity() {
    // 1 ns elapsed at +0.5 ppm: 0.0000005 ns of skew, floored to 0; at
    // -0.5 ppm, -1.
    assert_eq!(skewed_ns(0, 100, 500_000, 1), Some(101));
    assert_eq!(skewed_ns(0, 100, -500_000, 1), Some(100));
    // Before the base, the skew goes the other way.
    assert_eq!(
        skewed_ns(10 * S, 0, 1_000_000, 9 * S),
        Some(9 * S as u64 - 1_000)
    );
}

#[test]
fn a_time_before_the_epoch_is_none() {
    assert_eq!(skewed_ns(0, -10, 0, 5), None);
    assert_eq!(skewed_ns(0, -10, 0, 10), Some(0));
}

#[test]
fn the_sleep_target_is_the_first_monotonic_time_at_the_ptp_time() {
    let (base, k) = (3 * S, 40 * S);
    for ppm in [6.0, -6.0, 123.456, -1000.0, 1000.0, 0.0] {
        let ppt = TestClockPpm(ppm).ppt();
        for after in [0u64, 1, 999, 1_000_000_007, 7_200 * S as u64] {
            let ptp = u64::try_from(base + k).unwrap() + after;
            let m = unskewed_ns(base, k, ppt, ptp);
            assert!(skewed_ns(base, k, ppt, m).unwrap() >= ptp, "{ppm} {after}");
            assert!(
                skewed_ns(base, k, ppt, m - 1).unwrap() < ptp,
                "{ppm} {after}"
            );
        }
    }
}

#[test]
fn an_exact_sleep_target_is_not_rounded_up() {
    // +1 ppm: 1 000 001 ns of PTP time after the base is exactly 1 ms of
    // monotonic time.
    assert_eq!(unskewed_ns(0, 0, 1_000_000, 1_000_001), 1_000_000);
    assert_eq!(unskewed_ns(0, 0, 1_000_000, 1_000_002), 1_000_001);
}

#[test]
fn a_sleep_target_past_the_monotonic_range_saturates() {
    assert_eq!(unskewed_ns(0, i64::MIN / 2, 0, u64::MAX), i64::MAX);
}

/// The wiring on the real monotonic clock: ready at once, epoch 0, and its
/// time runs `ppm` off the monotonic clock (1000 ppm: 0.1 ms over 0.1 s,
/// far above the reads' own spread).
#[cfg(any(target_os = "linux", windows))]
#[test]
fn the_skewed_clock_reads_the_monotonic_clock_with_its_skew() {
    use crate::clock::{PtpClock, monotonic_ns};
    let base = monotonic_ns();
    let clock = SkewedClock::new(base, 0, TestClockPpm(1000.0));
    assert_eq!(clock.epoch(), 0);
    assert_eq!(clock.last_re_anchor(), None);
    let m0 = monotonic_ns();
    let p0 = clock.now_ns().expect("ready") as i64;
    let m0b = monotonic_ns();
    std::thread::sleep(std::time::Duration::from_millis(100));
    let m1 = monotonic_ns();
    let p1 = clock.now_ns().expect("ready") as i64;
    let m1b = monotonic_ns();
    // p - m = (m - base) / 1000 for each read, within the read's span.
    let skew_lo = (p1 - p0) - (m1b - m0);
    let skew_hi = (p1 - p0) - (m1 - m0b);
    let want = (m1 - m0) / 1000;
    assert!(
        skew_lo - 2_000 <= want && want <= skew_hi + 2_000,
        "skew {skew_lo}..{skew_hi} ns, want {want}"
    );
    // A sleep until 2 ms of PTP time on ends at or after it.
    let target = clock.now_ns().unwrap() + 2_000_000;
    clock.sleep_until(target);
    assert!(clock.now_ns().unwrap() >= target);
}

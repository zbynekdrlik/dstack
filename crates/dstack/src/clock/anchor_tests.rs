// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A re-anchor's step as the clocks report it and the shift the media
//! threads take from it (dstack #1, 6.11.2026).

use super::*;

/// dsync's coordinated date step of 6.11.2026.
const DATE_STEP_NS: i64 = 1_600_000_000;

#[test]
fn a_step_shifts_by_the_nearest_whole_grid() {
    // 1.6 s at 96 kHz: 153 600 samples, whole at fpp 16, 32 and block 256.
    for grid in [16, 32, 256] {
        assert_eq!(grid_shift(DATE_STEP_NS, 96_000, grid), 153_600, "{grid}");
        assert_eq!(grid_shift(-DATE_STEP_NS, 96_000, grid), -153_600, "{grid}");
    }
    // At 48 kHz 76 800, at 44.1 kHz 70 560 (2205 packets of 32).
    assert_eq!(grid_shift(DATE_STEP_NS, 48_000, 32), 76_800);
    assert_eq!(grid_shift(DATE_STEP_NS, 44_100, 32), 70_560);
    // 200 ms at 44.1 kHz: 8820 samples, 275.6 packets: 276.
    assert_eq!(grid_shift(200_000_000, 44_100, 32), 8_832);
    // A local correction of 50 µs (4.8 samples) either way: no shift; with
    // the date step: the date step.
    assert_eq!(grid_shift(50_000, 96_000, 32), 0);
    assert_eq!(grid_shift(-50_000, 96_000, 32), 0);
    assert_eq!(grid_shift(DATE_STEP_NS + 50_000, 96_000, 32), 153_600);
    assert_eq!(grid_shift(DATE_STEP_NS - 50_000, 96_000, 32), 153_600);
    // Half a grid rounds up, either sign (16 samples of 32 at 100 kHz is
    // 160 µs).
    assert_eq!(grid_shift(160_000, 100_000, 32), 32);
    assert_eq!(grid_shift(159_999, 100_000, 32), 0);
    assert_eq!(grid_shift(-160_000, 100_000, 32), 0);
    assert_eq!(grid_shift(-160_001, 100_000, 32), -32);
    assert_eq!(grid_shift(480_000, 100_000, 32), 64);
    assert_eq!(grid_shift(0, 96_000, 32), 0);
}

#[test]
fn nearest_rounds_a_half_up() {
    assert_eq!(nearest(7, 2), 4);
    assert_eq!(nearest(-7, 2), -3);
    assert_eq!(nearest(5, 3), 2);
    assert_eq!(nearest(4, 3), 1);
    assert_eq!(nearest(-4, 3), -1);
    assert_eq!(nearest(-5, 3), -2);
    assert_eq!(nearest(9, 3), 3);
}

#[test]
fn a_step_counts_only_into_the_epoch_after_the_one_it_came_from() {
    let one = |epoch, at| Anchor {
        epoch,
        re_anchor: Some(ReAnchor {
            epoch: at,
            step_ns: -7,
        }),
    };
    assert_eq!(one(8, 8).step_from(7), Some(-7));
    assert_eq!(one(8, 8).step_from(6), None, "two re-anchors since");
    assert_eq!(one(8, 8).step_from(8), None, "no change");
    assert_eq!(one(8, 7).step_from(7), None, "the step of another epoch");
    assert_eq!(one(0, 0).step_from(u32::MAX), Some(-7), "the epoch wraps");
    assert_eq!(Anchor::from(8).step_from(7), None, "no step known");
    assert_eq!(
        Anchor::from(5),
        Anchor {
            epoch: 5,
            re_anchor: None
        }
    );
}

#[test]
fn the_manual_clock_reports_its_re_anchors_steps() {
    let c = ManualClock::new(10_000);
    assert_eq!(PtpClock::last_re_anchor(&c), None);
    assert_eq!(Anchor::of(&c), Anchor::from(0));
    c.re_anchor(-300);
    assert_eq!((c.now_ns(), c.epoch()), (Some(9_700), 1));
    let r = ReAnchor {
        epoch: 1,
        step_ns: -300,
    };
    assert_eq!(PtpClock::last_re_anchor(&c), Some(r));
    c.re_anchor(DATE_STEP_NS);
    let a = Anchor::of(&c as &dyn PtpClock);
    assert_eq!(a.step_from(1), Some(DATE_STEP_NS));
    assert_eq!(a.epoch, 2);
}

/// A clock that keeps the default: no step known.
struct Bare;

impl PtpClock for Bare {
    fn now_ns(&self) -> Option<u64> {
        Some(1)
    }
    fn sleep_until(&self, _: u64) {}
    fn epoch(&self) -> u32 {
        3
    }
}

#[test]
fn a_clock_that_knows_no_step_reports_none() {
    assert_eq!(Anchor::of(&Bare), Anchor::from(3));
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn the_mono_clock_reports_k_s_change_as_its_step() {
    const HOUR: i64 = 3_600_000_000_000;
    let c = MonoClock::new();
    assert_eq!(c.last_re_anchor(), None);
    c.set_k(HOUR);
    assert_eq!(
        c.last_re_anchor(),
        None,
        "the first k makes it ready: no step"
    );
    c.set_k(HOUR);
    assert_eq!(c.last_re_anchor(), None);
    c.set_k(HOUR + DATE_STEP_NS);
    assert_eq!(
        Anchor::of(&c),
        Anchor {
            epoch: 1,
            re_anchor: Some(ReAnchor {
                epoch: 1,
                step_ns: DATE_STEP_NS
            })
        }
    );
    c.set_k(HOUR - 25_000);
    assert_eq!(
        c.last_re_anchor(),
        Some(ReAnchor {
            epoch: 2,
            step_ns: -DATE_STEP_NS - 25_000
        })
    );
    assert_eq!(c.epoch(), 2);
}

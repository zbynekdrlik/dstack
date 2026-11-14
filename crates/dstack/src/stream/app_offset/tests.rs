// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #295 step 2: the play-time offset filter, with numbers.

use super::*;

/// One block at 96 kHz (32 frames), ns, rounded down.
const WAKE: i64 = 333_333;
const OFF: i64 = 5_000_000_000;

#[test]
fn the_slew_bound_is_100_ppm_of_the_elapsed_app_time_rounded_down() {
    assert_eq!(slew_max_ns(1_000_000_000), 100_000);
    assert_eq!(slew_max_ns(WAKE), 33);
    assert_eq!(slew_max_ns(19_999), 1);
    assert_eq!(slew_max_ns(9_999), 0);
    assert_eq!(slew_max_ns(0), 0);
    assert_eq!(slew_max_ns(-1_000_000), 0, "an app time gone back");
}

#[test]
fn a_pair_is_the_media_read_less_the_midpoint_of_the_app_reads() {
    assert_eq!(pair_offset(1_000, 10_000, 1_000), (9_000, 1_000));
    assert_eq!(pair_offset(1_000, 10_000, 3_000), (8_000, 2_000));
    // Rounded toward the first read.
    assert_eq!(pair_offset(1_000, 10_000, 1_003), (8_999, 1_001));
    // A preemption of 2 ms before the media read skews it by 1 ms only.
    assert_eq!(pair_offset(0, 2_000_000, 2_000_000), (1_000_000, 1_000_000));
    assert_eq!(
        pair_offset(i64::MAX - 2, 0, i64::MAX),
        (-(i64::MAX - 1), i64::MAX - 1)
    );
}

#[test]
fn the_first_pair_is_taken() {
    let mut f = OffsetFilter::new();
    assert_eq!(f.offset(), None);
    assert_eq!(f.update(OFF + 2_000_000, 0), Outcome::First);
    assert_eq!(f.offset(), Some(OFF + 2_000_000));
}

#[test]
fn a_pair_within_the_bound_is_followed_by_at_most_the_slew() {
    let mut f = OffsetFilter::new();
    f.update(OFF, 0);
    // 50 us is within; one wake allows 33 ns.
    assert_eq!(f.update(OFF + REJECT_NS as i64, WAKE), Outcome::Followed);
    assert_eq!(f.offset(), Some(OFF + 33));
    assert_eq!(f.update(OFF - 40_000, 2 * WAKE), Outcome::Followed);
    assert_eq!(f.offset(), Some(OFF));
    // Under the slew: taken whole.
    assert_eq!(f.update(OFF + 20, 3 * WAKE), Outcome::Followed);
    assert_eq!(f.offset(), Some(OFF + 20));
    // A second's gap allows 100 us, more than any pair within the bound.
    assert_eq!(
        f.update(OFF + 45_020, 3 * WAKE + 1_000_000_000),
        Outcome::Followed
    );
    assert_eq!(f.offset(), Some(OFF + 45_020));
}

#[test]
fn a_pair_off_by_more_than_the_bound_is_rejected_and_the_offset_kept() {
    let mut f = OffsetFilter::new();
    f.update(OFF, 0);
    assert_eq!(
        f.update(OFF + REJECT_NS as i64 + 1, WAKE),
        Outcome::Rejected
    );
    assert_eq!(f.offset(), Some(OFF));
    assert_eq!(f.update(OFF - 2_000_000, 2 * WAKE), Outcome::Rejected);
    assert_eq!(f.offset(), Some(OFF));
    // The time of rejected pairs counts toward the next followed one's
    // slew: three wakes since the last followed pair, 99 ns.
    assert_eq!(f.update(OFF + 1_000, 3 * WAKE), Outcome::Followed);
    assert_eq!(f.offset(), Some(OFF + 99));
}

#[test]
fn a_step_read_alike_on_three_wakes_in_a_row_is_taken_on_the_third() {
    let mut f = OffsetFilter::new();
    f.update(OFF, 0);
    let step = OFF + 1_000_000;
    assert_eq!(f.update(step, WAKE), Outcome::Rejected);
    // Alike: within the bound of the last rejected pair (it drifts).
    assert_eq!(
        f.update(step + REJECT_NS as i64, 2 * WAKE),
        Outcome::Rejected
    );
    assert_eq!(f.offset(), Some(OFF));
    assert_eq!(
        f.update(step + 30_000, 3 * WAKE),
        Outcome::Stepped { by_ns: 1_030_000 }
    );
    assert_eq!(f.offset(), Some(step + 30_000));
    // Then followed from there, its slew counted from the step.
    assert_eq!(f.update(step + 30_100, 4 * WAKE), Outcome::Followed);
    assert_eq!(f.offset(), Some(step + 30_033));
}

#[test]
fn rejected_pairs_that_differ_or_a_followed_pair_between_them_start_the_run_again() {
    let mut f = OffsetFilter::new();
    f.update(OFF, 0);
    // Two torn reads that differ by more than the bound: no step.
    assert_eq!(f.update(OFF + 1_000_000, WAKE), Outcome::Rejected);
    assert_eq!(
        f.update(OFF + 1_000_000 + REJECT_NS as i64 + 1, 2 * WAKE),
        Outcome::Rejected
    );
    assert_eq!(f.update(OFF + 2_000_000, 3 * WAKE), Outcome::Rejected);
    assert_eq!(f.update(OFF + 2_000_000, 4 * WAKE), Outcome::Rejected);
    assert_eq!(f.offset(), Some(OFF));
    // A clean pair between: the run starts again.
    assert_eq!(f.update(OFF, 5 * WAKE), Outcome::Followed);
    assert_eq!(f.update(OFF + 2_000_000, 6 * WAKE), Outcome::Rejected);
    assert_eq!(f.update(OFF + 2_000_000, 7 * WAKE), Outcome::Rejected);
    assert!(matches!(
        f.update(OFF + 2_000_000, 8 * WAKE),
        Outcome::Stepped { by_ns: 2_000_000 }
    ));
}

#[test]
fn a_known_re_anchor_shifts_the_offset_and_an_unknown_one_starts_afresh() {
    let mut f = OffsetFilter::new();
    f.shift(1_000);
    assert_eq!(f.offset(), None, "nothing to shift before a pair");
    f.update(OFF, 0);
    f.update(OFF + 1_000_000, WAKE);
    f.update(OFF + 1_000_000, 2 * WAKE);
    // The shift also ends the run of rejected pairs.
    f.shift(1_600_000_000);
    assert_eq!(f.offset(), Some(OFF + 1_600_000_000));
    assert_eq!(f.update(OFF + 1_000_000, 3 * WAKE), Outcome::Rejected);
    assert_eq!(f.update(OFF + 1_600_000_100, 4 * WAKE), Outcome::Followed);
    assert_eq!(f.offset(), Some(OFF + 1_600_000_100));
    f.restart();
    assert_eq!(f.offset(), None);
    assert_eq!(f.update(7, 5 * WAKE), Outcome::First);
    assert_eq!(f.offset(), Some(7));
}

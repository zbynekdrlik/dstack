// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

#[test]
fn a_play_time_that_follows_the_media_time_deviates_by_nothing() {
    // 128 frames at 48 kHz: 2 666 667 ns of media time (ns_of rounds up).
    let before = (48_000, 5_000_000_000);
    assert_eq!(deviation_ns(before, (48_128, 5_002_666_667), 48_000), 0);
    // 16 ns short of it (6 ppm of a block, rounded): 16.
    assert_eq!(deviation_ns(before, (48_128, 5_002_666_651), 48_000), 16);
    // Over: the same size.
    assert_eq!(deviation_ns(before, (48_128, 5_002_666_683), 48_000), 16);
}

#[test]
fn a_skipped_block_is_media_time_not_a_deviation() {
    // Three blocks later (a re-bootstrap skipped two): 8 ms of media time.
    let before = (96_000, 0);
    assert_eq!(deviation_ns(before, (96_768, 8_000_000), 96_000), 0);
    // The same play time after one block: a 6.67 ms step.
    assert_eq!(deviation_ns(before, (96_256, 8_000_000), 96_000), 5_333_333);
}

#[test]
fn a_deviation_saturates() {
    assert_eq!(deviation_ns((0, i64::MIN), (0, i64::MAX), 48_000), u64::MAX);
}

#[test]
fn the_track_measures_each_block_from_the_last_one_with_a_play_time() {
    let mut t = Track::default();
    assert_eq!(t.next(0, Some(1_000_000), 48_000), None, "the first");
    assert_eq!(t.next(48, Some(2_000_000), 48_000), Some(0));
    assert_eq!(t.next(96, Some(4_000_000), 48_000), Some(1_000_000));
    // No play time: forgotten, the next one is a first again.
    assert_eq!(t.next(144, None, 48_000), None);
    assert_eq!(t.next(192, Some(9_000_000), 48_000), None);
    assert_eq!(t.next(240, Some(10_000_000), 48_000), Some(0));
}

#[test]
fn the_pump_counts_a_step_over_one_ms_and_keeps_the_largest_deviation() {
    let stats = PumpStats::new(
        super::super::Timing {
            block: 128,
            playout: 432,
            margin: 288,
        },
        48_000,
    );
    stats.play_at_moved(STEP_NS);
    stats.play_at_moved(16);
    let v = stats.view();
    assert_eq!((v.play_at_steps, v.play_at_max_dev_us), (0, 1_000));
    stats.play_at_moved(STEP_NS + 1);
    stats.play_at_moved(2_500_999);
    stats.play_at_moved(5);
    let v = stats.view();
    assert_eq!((v.play_at_steps, v.play_at_max_dev_us), (2, 2_500));
}

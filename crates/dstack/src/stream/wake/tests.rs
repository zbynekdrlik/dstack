// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use super::*;

const MS: u64 = 1_000_000;

#[test]
fn a_wake_falls_in_the_bucket_below_its_first_higher_edge() {
    assert_eq!(bucket(0), 0);
    assert_eq!(bucket(MS / 2 - 1), 0);
    assert_eq!(bucket(MS / 2), 1);
    assert_eq!(bucket(MS - 1), 1);
    assert_eq!(bucket(MS), 2);
    assert_eq!(bucket(2 * MS), 3);
    assert_eq!(bucket(5 * MS - 1), 3);
    assert_eq!(bucket(5 * MS), 4);
    assert_eq!(bucket(10 * MS), 5);
    assert_eq!(bucket(20 * MS), 6);
    assert_eq!(bucket(50 * MS - 1), 6);
    assert_eq!(bucket(50 * MS), 7);
    assert_eq!(bucket(u64::MAX), 7);
}

#[test]
fn every_wake_counts_in_its_bucket_and_the_largest_is_kept() {
    let w = WakeLateness::new();
    assert_eq!(w.buckets(), [0; BUCKETS]);
    assert_eq!((w.max_us(), w.late_wakes()), (0, 0));
    for ns in [10_000, 400_000, 700_000, 3 * MS, 60 * MS, 20 * MS] {
        w.record(ns, false, false, || {
            panic!("no wall time for an early wake")
        });
    }
    assert_eq!(w.buckets(), [2, 1, 0, 1, 0, 0, 1, 1]);
    assert_eq!(w.max_us(), 60_000);
    assert_eq!(w.late_wakes(), 0, "none was late");
    assert!(w.recent().is_empty());
}

#[test]
fn a_second_s_and_a_minute_s_largest_start_afresh_when_taken() {
    let w = WakeLateness::new();
    assert_eq!(w.minute_max_us(), None, "no minute taken yet");
    w.record(3_000_000, false, false, || 0);
    w.record(1_500_000, false, false, || 0);
    assert_eq!(w.take_second_max_us(), 3_000);
    assert_eq!(w.take_second_max_us(), 0, "afresh");
    w.record(700_999, false, false, || 0);
    assert_eq!(w.take_second_max_us(), 700);
    w.take_minute();
    assert_eq!(w.minute_max_us(), Some(3_000));
    w.record(2_000, false, false, || 0);
    w.take_minute();
    assert_eq!(w.minute_max_us(), Some(2), "the last minute's only");
    w.take_minute();
    assert_eq!(w.minute_max_us(), Some(0), "a quiet minute");
    assert_eq!(w.max_us(), 3_000, "the largest since the start stays");
}

#[test]
fn late_wakes_keep_their_time_lateness_and_rebootstrap_oldest_first() {
    let w = WakeLateness::new();
    w.record(400_000, true, false, || 1_700_000_000_001);
    w.record(9_000_999, true, true, || 1_700_000_000_002);
    w.record(1_000, false, false, || panic!("not late: no wall time"));
    assert_eq!(w.late_wakes(), 2);
    assert_eq!(
        w.recent(),
        vec![
            LateWake {
                unix_ms: 1_700_000_000_001,
                behind_us: 400,
                rebootstrap: false
            },
            LateWake {
                unix_ms: 1_700_000_000_002,
                behind_us: 9_000,
                rebootstrap: true
            },
        ]
    );
}

#[test]
fn the_ring_keeps_the_last_32_late_wakes() {
    let w = WakeLateness::new();
    let n = 3 * RECENT as u64 + 5;
    for i in 0..n {
        w.record((i + 1) * 1_000, true, i.is_multiple_of(3), || 1_000 + i);
    }
    assert_eq!(w.late_wakes(), n);
    let recent = w.recent();
    assert_eq!(recent.len(), RECENT);
    for (k, wake) in recent.iter().enumerate() {
        let i = n - RECENT as u64 + k as u64;
        assert_eq!(
            *wake,
            LateWake {
                unix_ms: 1_000 + i,
                behind_us: i + 1,
                rebootstrap: i.is_multiple_of(3)
            },
            "{k}"
        );
    }
}

#[test]
fn a_slot_written_again_or_being_written_is_left_out() {
    let w = WakeLateness::new();
    w.record(1_000, true, false, || 1);
    w.record(2_000, true, false, || 2);
    // The pump starts wake 2's write in slot 2 (odd word): wake 2 is not
    // published yet, the others read as before.
    w.recent[2].seq.fetch_add(1, Ordering::Relaxed);
    assert_eq!(w.recent().len(), 2);
    // Slot 1 already holds a later wake (its word one write further on).
    w.recent[1].seq.fetch_add(2, Ordering::Relaxed);
    assert_eq!(
        w.recent(),
        vec![LateWake {
            unix_ms: 1,
            behind_us: 1,
            rebootstrap: false
        }]
    );
    assert_eq!(w.read(1), None);
    assert_eq!(
        w.read(RECENT as u64 + 1),
        Some(LateWake {
            unix_ms: 2,
            behind_us: 2,
            rebootstrap: false
        }),
        "the slot's word names its second write"
    );
}

#[test]
fn the_wall_time_is_after_this_code_was_written() {
    // 2026-11-01T00:00:00Z.
    assert!(unix_ms_now() > 1_793_491_200_000);
}

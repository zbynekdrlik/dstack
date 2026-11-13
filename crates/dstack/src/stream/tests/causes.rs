// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #281: each re-bootstrap's cause, and how late the pump woke.

use super::*;

/// A pump over RX 1 into TX 1 with its first media step at `T` (one block
/// run, on time); returns the time it asks to wake at.
fn started() -> (Rig, u64) {
    let mut r = rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    );
    fill(&r, 0, T - 1_000, 400_000, counter);
    let wake = r.pump.media(T, 0);
    (r, wake)
}

/// The wakes recorded so far (every bucket's count).
fn wakes(r: &Rig) -> u64 {
    r.stats.wakes().buckets().iter().sum()
}

/// (late, clock_back, reanchor, retime, total) of the pump's view.
fn causes(v: &PumpView) -> (u64, u64, u64, u64, u64) {
    (
        v.rebootstraps_late,
        v.rebootstraps_clock_back,
        v.rebootstraps_reanchor,
        v.rebootstraps_retime,
        v.rebootstraps,
    )
}

#[test]
fn each_rebootstrap_counts_in_its_cause_and_in_the_total() {
    let (mut r, w) = started();
    assert_eq!(causes(&r.stats.view()), (0, 0, 0, 0, 0));
    // More than W behind: late.
    let w = r.pump.media(w + WORK_MARGIN + 1, 0);
    assert_eq!(causes(&r.stats.view()), (1, 0, 0, 0, 1));
    // The clock back more than the discontinuity, on the same epoch.
    let back = w - DISCONTINUITY - 1;
    let w = r.pump.media(back, 0);
    assert!(w >= back && w <= back + B, "{w}");
    assert_eq!(causes(&r.stats.view()), (1, 1, 0, 0, 2));
    // A new epoch whose step is unknown: a re-anchor.
    let w = r.pump.media(w, 1);
    assert_eq!(causes(&r.stats.view()), (1, 1, 1, 0, 3));
    // The device latency moved: a retime.
    r.rx.set_device_latency(2_000_000);
    r.pump.media(w, 1);
    let v = r.stats.view();
    assert_eq!(causes(&v), (1, 1, 1, 1, 4), "{v:?}");
}

#[test]
fn a_wake_s_lateness_is_its_oldest_due_block_s_and_counts_once() {
    let (mut r, w) = started();
    // The start's block was due exactly at T: one wake, 0 late.
    assert_eq!(wakes(&r), 1);
    assert_eq!(r.stats.wakes().buckets()[0], 1);
    // 40 samples (416 667 ns at 96 kHz) behind: late (a block is 32), its
    // catch-up block (8 behind) is no wake of its own.
    let w = r.pump.media(w + 40, 0);
    assert_eq!(wakes(&r), 2, "one record per wake");
    assert_eq!(r.stats.wakes().late_wakes(), 1);
    // 10 behind: on time (under a block), in the first bucket.
    let w = r.pump.media(w + 10, 0);
    assert_eq!(wakes(&r), 3);
    assert_eq!(r.stats.wakes().late_wakes(), 1, "under a block is not late");
    // Woken early (nothing due yet): no wake recorded.
    assert_eq!(r.pump.media(w - 5, 0), w);
    assert_eq!(wakes(&r), 3, "a wait records nothing");
    assert_eq!(r.stats.wakes().buckets(), [3, 0, 0, 0, 0, 0, 0, 0]);
    let v = r.stats.view();
    assert_eq!(v.wake_behind_max_us, 416, "{v:?}");
    assert_eq!(v.wake_behind_minute_max_us, None, "no minute taken");
    let recent = r.stats.wakes().recent();
    assert_eq!(recent.len(), 1, "{recent:?}");
    assert_eq!((recent[0].behind_us, recent[0].rebootstrap), (416, false));
    // 2026-11-01T00:00:00Z: a real wall time.
    assert!(recent[0].unix_ms > 1_793_491_200_000, "{recent:?}");
}

#[test]
fn a_late_rebootstrap_is_a_late_wake_and_a_clock_gone_back_is_none() {
    let (mut r, w) = started();
    // 97 samples behind (1 010 417 ns): past W, re-bootstrapped.
    let w = r.pump.media(w + WORK_MARGIN + 1, 0);
    assert_eq!(wakes(&r), 2);
    assert_eq!(r.stats.wakes().buckets()[2], 1, "1 ms to 2 ms");
    let recent = r.stats.wakes().recent();
    assert_eq!(recent.len(), 1, "{recent:?}");
    assert_eq!((recent[0].behind_us, recent[0].rebootstrap), (1_010, true));
    assert_eq!(r.stats.view().wake_behind_max_us, 1_010);
    // The clock back: a re-bootstrap, no lateness at all.
    r.pump.media(w - DISCONTINUITY - 1, 0);
    assert_eq!(r.stats.view().rebootstraps_clock_back, 1);
    assert_eq!(wakes(&r), 2, "a clock gone back is no wake lateness");
    assert_eq!(r.stats.wakes().late_wakes(), 1);
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #440's residual window: a step back that lands between a pass's
//! epoch read and its time read gives that pass the new time under the old
//! epoch. Its next block then looks due the whole step ahead; under the old
//! rule (a clock gone back only past 2 s at 96 kHz) the pump waited that
//! long, silent, before a counted re-bootstrap or the next epoch's move. A
//! wait further ahead than the margin and a block is a clock gone back: a
//! counted `clock_back` re-bootstrap at once, so the pump runs again within
//! a block.

use std::sync::atomic::{AtomicI64, AtomicU32};

use super::*;
use crate::clock::linked::{Follow, Linked, View};
use crate::clock::{FollowerCounts, MonoClock, monotonic_ns};

/// The steps back: 100 ms and 1 s, both under the old 2 s rule.
const BACK_NS: [u64; 2] = [100_000_000, 1_000_000_000];

/// The pump at epoch 5 reads the time `back_ns` back on that epoch: it
/// re-bootstraps at once, counted as `clock_back`, and asks to wake within
/// a block of the new time instead of the step ahead.
#[test]
fn a_time_read_back_before_its_epoch_rebootstraps_at_once() {
    for back_ns in BACK_NS {
        let (mut r, now) = pump_at_epoch_5();
        let back = now - to_samples(back_ns, RATE);
        let wake = r.pump.media(back, 5);
        assert!(
            wake >= back && wake <= back + B,
            "{back_ns} ns back: wake {wake} for now {back}"
        );
        let v = r.stats.view();
        assert_eq!(
            (v.rebootstraps_clock_back, v.rebootstraps),
            (1, 1),
            "{back_ns} ns back: {v:?}"
        );
        // It runs its blocks again from there, nothing more counted.
        let wake = r.pump.media(wake, 5);
        assert!(wake <= back + 2 * B, "{back_ns} ns back: {wake}");
        let v = r.stats.view();
        assert_eq!(v.rebootstraps, 1, "{back_ns} ns back: {v:?}");
    }
}

/// The epoch follows the time (the next pass reads it, with the step): the
/// pump runs on within a block or two of the new time, the step counted
/// once as `clock_back`. The re-anchor keeps the schedule the clock-back
/// restart started at the new time (ROZHODNUTÉ 19.11: a one-pass mark)
/// instead of moving it the step back once more: no second re-bootstrap,
/// nothing missed, no wake late by the step (a `late` count with the
/// step's blocks missed reads like #440's stall for a pump that never
/// stopped).
#[test]
fn the_epoch_after_a_time_read_back_keeps_the_restarted_schedule() {
    for back_ns in BACK_NS {
        let (mut r, now) = pump_at_epoch_5();
        let behind_before = r.stats.view().wake_behind_max_us;
        let back = now - to_samples(back_ns, RATE);
        let wake = r.pump.media(back, 5);
        let step = -i64::try_from(back_ns).unwrap();
        let wake = r.pump.media(wake, stepped(6, step));
        assert!(wake <= back + 2 * B, "{back_ns} ns back: {wake}");
        let v = r.stats.view();
        assert_eq!(v.rebootstraps_clock_back, 1, "{back_ns} ns back: {v:?}");
        assert_eq!(
            (v.rebootstraps_late, v.rebootstraps, v.missed),
            (0, 1, 0),
            "{back_ns} ns back: {v:?}"
        );
        assert_eq!(v.wake_behind_max_us, behind_before, "{back_ns} ns back");
        // It runs its blocks on from there, nothing more counted.
        let wake = r.pump.media(wake, 6);
        assert!(wake <= back + 3 * B, "{back_ns} ns back: {wake}");
        assert_eq!(r.stats.view().rebootstraps, 1, "{back_ns} ns back");
    }
}

/// A follower whose time is the monotonic clock plus an offset: a step
/// moves the offset only (the time read under the old epoch, the residual
/// window), its epoch stays.
#[derive(Default)]
struct BackFollower {
    offset_ns: AtomicI64,
    epoch: AtomicU32,
}

impl Follow for Arc<BackFollower> {
    fn now_ns(&self) -> Option<i64> {
        Some(monotonic_ns() + self.offset_ns.load(Ordering::SeqCst))
    }

    fn view(&self) -> View {
        View {
            alive: true,
            locked: true,
            settled: true,
            anchored: true,
            epoch: self.epoch.load(Ordering::SeqCst),
        }
    }

    fn counts(&self) -> FollowerCounts {
        FollowerCounts::default()
    }

    fn last_re_anchor(&self) -> Option<(u32, i64)> {
        None
    }
}

/// The block that moves the clock back.
const AT: u64 = 100;
/// Blocks after it that must run.
const AFTER: u64 = 30;

/// A processor that runs `step` inside its `AT`th block.
struct BackInBlock {
    blocks: u64,
    step: Option<Box<dyn FnOnce() + Send>>,
}

impl Process for BackInBlock {
    fn process(&mut self, _block: &mut Block<'_>) {
        self.blocks += 1;
        if self.blocks == AT
            && let Some(step) = self.step.take()
        {
            step();
        }
    }
}

/// On the pump thread (the linked clock on its follower): the follower's
/// time goes back inside a block with no new epoch; the pump runs `AFTER`
/// blocks again within half the step (real time) and counts one
/// `clock_back` re-bootstrap, never waiting the step out.
#[test]
fn a_time_back_inside_a_block_never_silences_the_pump_for_the_step() {
    for back_ns in BACK_NS {
        let follower = Arc::new(BackFollower::default());
        follower
            .offset_ns
            .store(1_700_000_000_000_000_000 - monotonic_ns(), Ordering::SeqCst);
        let linked = Linked::new(
            Some(Box::new(Arc::clone(&follower))),
            Arc::new(MonoClock::new()),
            None,
        );
        assert_eq!(linked.tick(), crate::clock::linked::Source::Internal);
        let cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml");
        let rings: Arc<[Ring]> = (0..cfg.rx.len()).map(|_| Ring::new()).collect();
        let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
        let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
        let stats = Arc::new(PumpStats::new(TIMING, RATE));
        let stepping = Arc::clone(&follower);
        let back = i64::try_from(back_ns).unwrap();
        let pump = Pump::new(
            BackInBlock {
                blocks: 0,
                step: Some(Box::new(move || {
                    stepping.offset_ns.fetch_sub(back, Ordering::SeqCst);
                })),
            },
            Map::default(),
            TIMING,
            24,
            rx,
            tx,
            Arc::clone(&stats),
        );
        let stop = Arc::new(AtomicBool::new(false));
        let thread = spawn(Arc::new(linked), pump, Arc::clone(&stop)).unwrap();
        wait_for(&stats, |v| v.blocks >= AT);
        let stepped = Instant::now();
        let bound = Duration::from_nanos(back_ns / 2);
        loop {
            let v = stats.view();
            if v.blocks >= AT + AFTER {
                break;
            }
            // Asleep for the step: leave its thread (it would hold the
            // test for the whole step) and fail.
            assert!(
                stepped.elapsed() < bound,
                "{back_ns} ns back: the pump ran no block for {bound:?}: {v:?}"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
        stop.store(true, Ordering::SeqCst);
        let deadline = Instant::now() + BOUND;
        while !thread.is_finished() {
            assert!(Instant::now() < deadline, "the pump thread did not end");
            std::thread::sleep(Duration::from_millis(1));
        }
        thread.join().unwrap();
        let v = stats.view();
        assert_eq!(v.rebootstraps_clock_back, 1, "{back_ns} ns back: {v:?}");
    }
}

/// Near media time 0 a bootstrap's first block is due at P + B (no block
/// starts before 0), further ahead than W + B when P > W: that is the
/// start's own wait, never a clock gone back (else the pump re-bootstraps
/// at once, again and again, and never leaves its pass).
#[test]
fn a_schedule_near_time_0_waits_its_first_block() {
    let t = Timing::new(32, 480, 96);
    for now in [0, 1, 100, 480] {
        let mut b = Blocks::bootstrap(now, t);
        assert_eq!(b.due(), 512, "{now}");
        assert_eq!(b.next(now), Due::Wait { until: 512 }, "{now}");
    }
    // Past the floor the limit holds again: due more than W + B ahead of a
    // now past P + B is the clock gone back.
    let fresh = Blocks::bootstrap(10_000, t);
    let mut b = fresh;
    assert!(matches!(
        b.next(fresh.due() - 129),
        Due::Rebootstrap {
            cause: Cause::ClockBack,
            ..
        }
    ));
}

/// A clock-back restart marks the schedule: the next re-anchor's step back
/// (the epoch that carries the step the pass before read under the old
/// one) keeps it where the restart put it, at the new time, instead of
/// moving it the step behind (ROZHODNUTÉ 19.11). The mark is spent by
/// that re-anchor: the one after moves as it always did.
#[test]
fn a_re_anchor_back_after_a_clock_back_restart_keeps_the_schedule() {
    for back in [9_600u64, 96_000] {
        let mut b = Blocks::bootstrap(T, TIMING);
        let new = T - back;
        assert!(
            matches!(
                b.next(new),
                Due::Rebootstrap {
                    cause: Cause::ClockBack,
                    ..
                }
            ),
            "{back} back"
        );
        let restarted = b.start();
        let by = -i64::try_from(back).unwrap();
        assert_eq!(b.re_anchor(Some(by), new), (Shift::Kept, 0), "{back} back");
        assert_eq!(b.start(), restarted, "{back} back");
        assert_eq!(b.re_anchor(Some(by), new), (Shift::Moved, 0), "{back} back");
        assert_eq!(b.start(), restarted - back, "{back} back");
    }
}

/// A re-bootstrap for lateness marks nothing: a re-anchor back after it
/// moves the schedule, as before #440's mark.
#[test]
fn a_re_anchor_back_after_a_late_restart_moves_the_schedule() {
    let mut b = Blocks::bootstrap(T, TIMING);
    let late = b.due() + WORK_MARGIN + 1;
    assert!(matches!(
        b.next(late),
        Due::Rebootstrap {
            cause: Cause::Late,
            ..
        }
    ));
    let restarted = b.start();
    assert_eq!(b.re_anchor(Some(-3_200), late), (Shift::Moved, 0));
    assert_eq!(b.start(), restarted - 3_200);
}

/// The mark keeps a step BACK only: a forward step after a clock-back
/// restart moves the schedule as before (kept, it would be the step behind
/// the clock and re-bootstrap late).
#[test]
fn a_re_anchor_forward_after_a_clock_back_restart_moves_the_schedule() {
    let mut b = Blocks::bootstrap(T, TIMING);
    let new = T - 9_600;
    assert!(matches!(
        b.next(new),
        Due::Rebootstrap {
            cause: Cause::ClockBack,
            ..
        }
    ));
    let restarted = b.start();
    let by = i64::try_from(B).unwrap();
    assert_eq!(b.re_anchor(Some(by), new), (Shift::Moved, 0));
    assert_eq!(b.start(), restarted + B);
}

/// The mark lasts one pass: a pass with no re-anchor after the clock-back
/// restart spends it, and an epoch with the step that comes only after it
/// moves the schedule the step back once more, a second re-bootstrap
/// (`late`), as before the mark.
#[test]
fn a_clock_back_mark_lasts_one_pass() {
    for back_ns in BACK_NS {
        let (mut r, now) = pump_at_epoch_5();
        let back = now - to_samples(back_ns, RATE);
        let wake = r.pump.media(back, 5);
        let wake = r.pump.media(wake, 5);
        let step = -i64::try_from(back_ns).unwrap();
        r.pump.media(wake, stepped(6, step));
        let v = r.stats.view();
        assert_eq!(
            (
                v.rebootstraps_clock_back,
                v.rebootstraps_late,
                v.rebootstraps
            ),
            (1, 1, 2),
            "{back_ns} ns back: {v:?}"
        );
    }
}

/// The mark keeps a step back off the block grid too (the restart already
/// put the schedule at the new time; the pump's own shift is always on the
/// grid, so this is the TX lanes' case kept alike for the one schedule
/// type): kept once, then restarted as before.
#[test]
fn a_marked_schedule_keeps_a_step_back_off_the_block_grid() {
    let mut b = Blocks::bootstrap(T, TIMING);
    let new = T - 9_600;
    assert!(matches!(
        b.next(new),
        Due::Rebootstrap {
            cause: Cause::ClockBack,
            ..
        }
    ));
    let restarted = b.start();
    let by = -9_616;
    assert_eq!(b.re_anchor(Some(by), new), (Shift::Kept, 0));
    assert_eq!(b.start(), restarted);
    assert_eq!(b.re_anchor(Some(by), new).0, Shift::Restarted);
}

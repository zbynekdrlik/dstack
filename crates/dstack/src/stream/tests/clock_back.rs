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
/// once as `clock_back`. (The re-anchor then moves the schedule, already
/// started again at the new time, the step back once more and re-bootstraps
/// it as `late`: a second count, not a silence; left to the main session's
/// call on #440, not pinned here.)
#[test]
fn the_epoch_after_a_time_read_back_runs_the_pump_on() {
    for back_ns in BACK_NS {
        let (mut r, now) = pump_at_epoch_5();
        let back = now - to_samples(back_ns, RATE);
        let wake = r.pump.media(back, 5);
        let step = -i64::try_from(back_ns).unwrap();
        let wake = r.pump.media(wake, stepped(6, step));
        assert!(wake <= back + 2 * B, "{back_ns} ns back: {wake}");
        let v = r.stats.view();
        assert_eq!(v.rebootstraps_clock_back, 1, "{back_ns} ns back: {v:?}");
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

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #440 (iemmixer-snv, 16.11.2026): the service clock re-anchored by
//! -43.9 days between the pump's time read and its sleep, and the sleep,
//! reading k again, slept the whole step: the pump ran no block for three
//! days and nothing counted or logged it. Here a block itself moves the
//! clock (the exact window: after the pass's read, before its sleep), on
//! the service's clock and on the linked clock's follower, back and
//! forward by µs, ms, s and the field's step; the pump must run blocks
//! again within a bound far under the step.

use std::sync::atomic::{AtomicI64, AtomicU32};

use super::*;
use crate::clock::linked::{Follow, Linked, View};
use crate::clock::{FollowerCounts, MonoClock, monotonic_ns};

/// The field's record (`tests/fixtures/pump_stall_440.json`): the service's
/// and the follower's re-anchors of 16.11.2026, and what followed.
const FIELD: &str = include_str!("../../../tests/fixtures/pump_stall_440.json");

/// The field's record, parsed.
fn record() -> serde_json::Value {
    serde_json::from_str(FIELD).unwrap()
}

/// A whole number of the field's record.
fn field(key: &str) -> i64 {
    record()[key]
        .as_i64()
        .unwrap_or_else(|| panic!("{key} in {FIELD}"))
}

/// The steps a clock is moved by inside a block: just past the service
/// tracker's tolerance, a few blocks, seconds, and the field's own step of
/// that clock (`field_key`), each back and forward.
fn steps(field_key: &str) -> Vec<i64> {
    [-40_000, -1_500_000, -3_000_000_000, field(field_key)]
        .into_iter()
        .flat_map(|s| [s, -s])
        .collect()
}
/// The block that moves the clock.
const AT: u64 = 100;
/// Blocks after it that must run.
const AFTER: u64 = 300;
/// How long they may take, real time: 300 blocks are 0.1 s; a sleep that
/// takes the step of seconds or days never makes it.
const RESUME: Duration = Duration::from_secs(1);
/// A PTP time base (2023, ns), far from 0 either way of every step.
const BASE_NS: i64 = 1_700_000_000_000_000_000;

/// A processor that runs `step` inside its `AT`th block.
struct StepsInBlock {
    blocks: u64,
    step: Option<Box<dyn FnOnce() + Send>>,
}

impl Process for StepsInBlock {
    fn process(&mut self, _block: &mut Block<'_>) {
        self.blocks += 1;
        if self.blocks == AT
            && let Some(step) = self.step.take()
        {
            step();
        }
    }
}

/// A pump with no channels whose `AT`th block runs `step`, on its thread on
/// `clock`: waits until `AFTER` more blocks ran (at most [`RESUME`] after
/// the step), stops it.
fn resumes(clock: Arc<dyn PtpClock>, step: Box<dyn FnOnce() + Send>, what: &str) {
    let cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml");
    let rings: Arc<[Ring]> = (0..cfg.rx.len()).map(|_| Ring::new()).collect();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
    let stats = Arc::new(PumpStats::new(TIMING, RATE));
    let pump = Pump::new(
        StepsInBlock {
            blocks: 0,
            step: Some(step),
        },
        Map::default(),
        TIMING,
        24,
        rx,
        tx,
        Arc::clone(&stats),
    );
    let stop = Arc::new(AtomicBool::new(false));
    let thread = spawn(clock, pump, Arc::clone(&stop)).unwrap();
    wait_for(&stats, |v| v.blocks >= AT);
    let stepped = Instant::now();
    loop {
        let v = stats.view();
        if v.blocks >= AT + AFTER {
            break;
        }
        // The pump stays asleep: leave its thread (it would hold the test
        // for the whole step) and fail.
        assert!(
            stepped.elapsed() < RESUME,
            "{what}: the pump ran no block for {RESUME:?} after the clock moved: {v:?}"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    stop.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + BOUND;
    while !thread.is_finished() {
        assert!(
            Instant::now() < deadline,
            "{what}: the pump thread did not end"
        );
        std::thread::sleep(Duration::from_millis(1));
    }
    thread.join().unwrap();
}

/// The 16.11.2026 path: the service's clock (the linked clock's source
/// then) takes a new k between the pump's read and its sleep.
#[test]
fn a_service_clock_re_anchored_inside_a_block_never_stops_the_pump() {
    for step in steps("service_re_anchor_delta_ns") {
        let clock = Arc::new(MonoClock::new());
        let k = BASE_NS - monotonic_ns();
        clock.set_k(k);
        let stepping = Arc::clone(&clock);
        resumes(
            clock,
            Box::new(move || stepping.set_k(k + step)),
            &format!("service step {step} ns"),
        );
    }
}

/// A follower whose time is the monotonic clock plus an offset that its
/// re-anchors move (each a re-align with its step known), locked.
#[derive(Default)]
struct Stepping {
    offset_ns: AtomicI64,
    epoch: AtomicU32,
    step_ns: AtomicI64,
}

impl Stepping {
    /// A re-anchor by `step_ns`: the time first, then the epoch (as dsync
    /// publishes them).
    fn re_anchor(&self, step_ns: i64) {
        self.step_ns.store(step_ns, Ordering::SeqCst);
        self.offset_ns.fetch_add(step_ns, Ordering::SeqCst);
        self.epoch.fetch_add(1, Ordering::SeqCst);
    }
}

impl Follow for Arc<Stepping> {
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
        let epoch = self.epoch.load(Ordering::SeqCst);
        (epoch != 0).then(|| (epoch, self.step_ns.load(Ordering::SeqCst)))
    }
}

/// The linked clock on its follower: the follower's time moves between
/// the pump's read and its sleep.
#[test]
fn a_follower_re_anchored_inside_a_block_never_stops_the_pump() {
    for step in steps("follower_re_anchor_step_ns") {
        let follower = Arc::new(Stepping::default());
        follower
            .offset_ns
            .store(BASE_NS - monotonic_ns(), Ordering::SeqCst);
        let linked = Linked::new(
            Some(Box::new(Arc::clone(&follower))),
            Arc::new(MonoClock::new()),
            None,
        );
        assert_eq!(linked.tick(), crate::clock::linked::Source::Internal);
        resumes(
            Arc::new(linked),
            Box::new(move || follower.re_anchor(step)),
            &format!("follower step {step} ns"),
        );
    }
}

/// The field's record replayed on the pump's schedule (dstack #440): the
/// service's step is the TX shift the record shows on the 16-sample grid,
/// a whole number of the pump's blocks, so once the pump sees its epoch it
/// moves by it and loses nothing; and the blocks counted at the restart
/// are the time from the engine's start to that re-anchor: no block ran
/// after it.
#[test]
fn the_field_s_re_anchor_moves_the_pump_and_its_blocks_stopped_there() {
    let rate = u32::try_from(field("rate")).unwrap();
    let block = u64::try_from(field("block")).unwrap();
    let shift = crate::clock::grid_shift(field("service_re_anchor_delta_ns"), rate, block);
    assert_eq!(shift, field("tx_shift_samples"));
    let timing = Timing {
        block,
        playout: 24,
        margin: 16,
    };
    let mut b = Blocks::bootstrap(T, timing);
    let start = b.start();
    let after = T.wrapping_add_signed(shift);
    assert_eq!(b.re_anchor(Some(shift), after), (Shift::Moved, 0));
    assert_eq!(b.start(), start.wrapping_add_signed(shift));
    assert!(matches!(b.next(after), Due::Wait { .. }));
    // 6000 blocks a second at block 16 and 96 kHz.
    let per_s = f64::from(rate) / block as f64;
    let ran_s = field("engine_blocks_at_restart") as f64 / per_s;
    let to_re_anchor_s = record()["engine_start_to_service_re_anchor_s"]
        .as_f64()
        .unwrap();
    assert!(
        (ran_s - to_re_anchor_s).abs() < 2.0,
        "{ran_s} s against {to_re_anchor_s} s"
    );
}

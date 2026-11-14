// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #295: each block's play time in the app's clock, `start + P +
//! B` as a media time less the media clock's offset from the app's.

use super::*;

use crate::clock::{Anchor, ManualClock};
use crate::media::tx::ns_of;

/// The fake app clock's reading at the first pair.
const APP0: i64 = 7_000_000_000;
/// One ms: three blocks of 32 at 96 kHz.
const MS: i64 = 1_000_000;

#[test]
fn the_play_time_is_start_plus_playout_plus_block_less_the_offset() {
    // 960 + 96 + 32 = 1088 samples at 96 kHz: 11 333 333.3 ns, rounded up
    // as every media time of a sample count (`ns_of`).
    assert_eq!(play_at_ns(960, TIMING, RATE, 1_000), 11_333_334 - 1_000);
    assert_eq!(play_at_ns(960, TIMING, RATE, -1_000), 11_333_334 + 1_000);
    // P and B are the timing's: block 16 is due 16 samples sooner.
    assert_eq!(
        play_at_ns(960, TIMING_16, RATE, 0),
        ns_of(960 + 96 + 16, RATE) as i64
    );
    // A media time past i64 reads as i64::MAX.
    assert_eq!(play_at_ns(u64::MAX / 2, TIMING, RATE, 5), i64::MAX - 5);
}

/// The rig's pump stepped on `clock` with an app clock that runs with it
/// (it moves by each sleep; a re-anchor of the media clock moves it not):
/// `steps` wakes, each reading the pair back to back first.
fn step(r: &mut Rig, clock: &ManualClock, app_ns: &mut i64, steps: usize) {
    for _ in 0..steps {
        let app = *app_ns;
        let now = r.pump.read_clocks(clock, || app).expect("a ready clock");
        let wake = r.pump.media(to_samples(now, RATE), Anchor::of(clock));
        let to = ns_of(wake, RATE);
        assert!(to > now, "a wake ahead of now: {to} at {now}");
        *app_ns += (to - now) as i64;
        clock.sleep_until(to);
    }
}

fn pump() -> Rig {
    rig(
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        &[0],
        &[0],
    )
}

#[test]
fn a_constant_offset_moves_the_play_time_one_block_per_block() {
    let mut r = pump();
    let clock = ManualClock::new(ns_of(T, RATE));
    let mut app = APP0;
    step(&mut r, &clock, &mut app, 30);
    let offset = ns_of(T, RATE) as i64 - APP0;
    let p = &r.pump.processor;
    assert_eq!(p.play_ats.len(), 30, "{:?}", p.play_ats);
    for (start, play_at) in p.starts.iter().zip(&p.play_ats) {
        let start = start.expect("a media block");
        assert_eq!(*play_at, Some(play_at_ns(start, TIMING, RATE, offset)));
    }
    let at: Vec<i64> = p.play_ats.iter().map(|a| a.unwrap()).collect();
    // The first block is due at T (it starts at T - B - P): in the app's
    // clock that is APP0, the app clock's reading there.
    assert_eq!(p.starts[0], Some(T - B - P));
    assert_eq!(at[0], APP0);
    // One block (333 333.3 ns) a block: three blocks are exactly 1 ms.
    for w in at.windows(2) {
        assert!((333_333..=333_334).contains(&(w[1] - w[0])), "{at:?}");
    }
    for (j, a) in at.iter().step_by(3).enumerate() {
        assert_eq!(*a, APP0 + j as i64 * MS, "{at:?}");
    }
}

#[test]
fn a_re_anchor_steps_the_start_and_leaves_the_play_time_on_the_app_s_clock() {
    // dsync's date step (1.6 s, 153 600 samples, whole blocks): the media
    // clock and the senders' stamps step together, the pump moves `start`
    // by the step, and the offset moves by it too, so the play time goes
    // on one block a block in the app's clock, which never stepped.
    const STEP_NS: i64 = 1_600_000_000;
    const STEP: u64 = 153_600;
    let mut r = pump();
    let clock = ManualClock::new(ns_of(T, RATE));
    let mut app = APP0;
    step(&mut r, &clock, &mut app, 9);
    clock.re_anchor(STEP_NS);
    step(&mut r, &clock, &mut app, 9);
    let v = r.stats.view();
    assert_eq!((v.rebootstraps, v.missed), (0, 0), "{v:?}");
    let p = &r.pump.processor;
    assert_eq!(p.starts.len(), 18);
    let starts: Vec<u64> = p.starts.iter().map(|s| s.unwrap()).collect();
    assert_eq!(starts[9] - starts[8], B + STEP, "the start stepped");
    let at: Vec<i64> = p.play_ats.iter().map(|a| a.unwrap()).collect();
    for (j, a) in at.iter().step_by(3).enumerate() {
        assert_eq!(*a, APP0 + j as i64 * MS, "{at:?}");
    }
    // Against the offset before the step, the moved start reads 1.6 s
    // later: the step is in the media time, not in the app's.
    let before = ns_of(T, RATE) as i64 - APP0;
    assert_eq!(play_at_ns(starts[9], TIMING, RATE, before) - at[9], STEP_NS);
}

#[test]
fn no_pair_read_or_free_pacing_gives_no_play_time() {
    let mut r = pump();
    let mut now = T;
    for _ in 0..3 {
        now = r.pump.media(now, 0);
    }
    assert_eq!(r.pump.processor.play_ats, [None, None, None]);
    // After a pair, a free block still has none (no media time).
    let clock = ManualClock::new(ns_of(now, RATE));
    let mut app = APP0;
    step(&mut r, &clock, &mut app, 1);
    assert!(r.pump.processor.play_ats[3].is_some());
    r.pump.free();
    assert_eq!(r.pump.processor.play_ats.last(), Some(&None));
}

#[test]
fn a_clock_not_ready_reads_no_app_time_and_keeps_the_offset() {
    let mut r = pump();
    let clock = ManualClock::new(ns_of(T, RATE));
    clock.set_ready(false);
    let now = r.pump.read_clocks(&clock, || {
        panic!("the app clock is read only with media now")
    });
    assert_eq!(now, None);
    clock.set_ready(true);
    assert_eq!(r.pump.read_clocks(&clock, || APP0), Some(ns_of(T, RATE)));
    clock.set_ready(false);
    assert_eq!(r.pump.read_clocks(&clock, || 0), None);
    // The kept offset (from the ready read) stamps the next block.
    r.pump.media(T, 0);
    assert_eq!(r.pump.processor.play_ats, [Some(APP0)]);
}

/// Each block's play time, shared with the test (the pump thread owns
/// the processor).
struct Stamps(Arc<Mutex<Vec<Option<i64>>>>);

impl Process for Stamps {
    fn process(&mut self, block: &mut Block<'_>) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(block.play_at());
    }
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn the_pump_thread_stamps_each_media_block_in_the_app_s_real_clock() {
    let r = pump();
    let stamps = Arc::new(Mutex::new(Vec::new()));
    let pump = Pump::new(
        Stamps(Arc::clone(&stamps)),
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        TIMING,
        24,
        Arc::clone(&r.rx),
        Arc::clone(&r.tx),
        Arc::new(PumpStats::new(TIMING, RATE)),
    );
    let t0 = ns_of(T, RATE);
    let clock = Arc::new(StepClock::default());
    clock.now.store(t0, Ordering::SeqCst);
    clock.limit.store(t0, Ordering::SeqCst);
    clock.ready.store(true, Ordering::SeqCst);
    let stop = Arc::new(AtomicBool::new(false));
    let before = crate::clock::app_monotonic_ns();
    let thread = spawn(
        Arc::clone(&clock) as Arc<dyn PtpClock>,
        pump,
        Arc::clone(&stop),
    )
    .unwrap();
    clock.run_to(t0 + 10_000_000);
    stop.store(true, Ordering::SeqCst);
    let deadline = Instant::now() + BOUND;
    while !thread.is_finished() {
        assert!(Instant::now() < deadline, "the pump thread did not end");
        std::thread::sleep(Duration::from_millis(1));
    }
    thread.join().unwrap();
    let after = crate::clock::app_monotonic_ns();
    let stamps = stamps.lock().unwrap().clone();
    assert!(stamps.len() >= 31, "{stamps:?}");
    // Each block is stamped from a pair read at its wake: at or before
    // the app's clock there, and within the 10 ms of media time run.
    for s in &stamps {
        let s = s.expect("a media block has a play time");
        assert!(s <= after && s >= before - 10 * MS, "{before} {s} {after}");
    }
}

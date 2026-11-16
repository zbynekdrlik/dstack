// SPDX-License-Identifier: MIT OR Apache-2.0

//! The soak's real-time case (dstack #304): one minute of wall time, the
//! pump on its own thread, the app on the test's thread at real 1024-frame
//! ticks. Real wakes are what the simulation (`tests/soak.rs`) cannot
//! model.
//!
//! Both run on the app's clock ([`dstack::clock::app_monotonic_ns`]: Linux
//! `CLOCK_MONOTONIC`, Windows raw QPC), which the ticks (`Instant`) and the
//! TX FIFO's write stamps read too: the pump's media clock is a
//! [`SkewedClock`] at 0 ppm on it. A bare `MonoClock` that no poller ever
//! fed reads the wall on Windows (`clock::monotonic_ns` before the steer
//! model's first poll is `realtime_ns`), which the runner's time service
//! adjusts under the test (dstack #305 measured the wall-fed model 66 ppm off
//! QPC on windows-2025; a run here read the RX bridge at -38.9 ppm). A
//! forward step of the pump's clock makes the pump run that much audio at
//! once: the TX FIFO loses that depth until the level loop gives it back
//! (about 1.2 ms a minute at its 20 ppm bound), counted only as a late pump
//! wake. Three such
//! steps of 3 ms took the depth before a write from 14.1 to 3.5 ms on dev1,
//! and the main run of v0.6.146 gapped at a tick 2.7 ms late with three
//! late pump wakes (at most 3.7 ms). Clock drift and steps between the
//! pump and the app are the simulated gate's (its drift cases); here they
//! would only hide the player behind the runner's clock.
//!
//! Not in the release gate (no `gate_` name, and its own binary, so the
//! simulated cases never load the box under it): on a shared runner the
//! test thread's wakes are the runner's load. So it judges what the player
//! owns, the way the simulated irregular writer does (dstack #303): a TX
//! gap only right after a tick that ran late enough to empty the FIFO's
//! slack ([`LATE_TICK`]), one at most a late tick, none at a tick on time.
//! An RX gap the same (dstack #304): the app reads RX at its tick, and a
//! held tick reads two ticks' worth back to back and can find the ring dry,
//! which the player does not own; at a tick on time only where the pump
//! itself restarted. The dry read of a hold is at the owed tick the loop runs
//! at once after it, so the tick after a late one counts as held too.

#[path = "soak/harness.rs"]
mod harness;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dstack::clock::{PtpClock, SkewedClock, TestClockPpm, app_monotonic_ns};
use dstack::stream::PumpView;

use harness::*;

/// A tick this late (the app's thread held: the runner's load) may cost
/// one TX gap: at OBS's 22.67 ms target the sawtooth's bottom is about
/// 12 ms; with no floor (dstack #303) only lateness that empties the FIFO
/// before the converter's call (about 10.5 ms: the 12 ms bottom less one
/// call's input) gaps, so 5 ms stays a safe margin under it.
const LATE_TICK: Duration = Duration::from_millis(5);

/// What one tick saw: its lateness past its due time, whether a TX gap
/// (a dropout or a re-centre) came since the last tick, and the RX
/// dropouts since the last tick.
#[derive(Debug, Clone, Copy)]
struct Tick {
    late: Duration,
    gap: bool,
    rx: u64,
}

/// Whether tick `i` is held or catches up after a held one: it or the
/// tick before it ran at least [`LATE_TICK`] late. The app reads RX at its
/// tick; after a hold the loop runs the owed tick at once, reading two
/// ticks' worth back to back, and that catch-up read is the one that can
/// find the ring dry (its own lateness is the hold less a period).
fn held(ticks: &[Tick], i: usize) -> bool {
    ticks[i.saturating_sub(1)..=i]
        .iter()
        .any(|t| t.late >= LATE_TICK)
}

/// The RX dropouts at ticks neither held nor catching up ([`held`]): the
/// run may have only as many as the pump's own restarts (dstack #304: a
/// held app tick may cost one, as for TX).
fn rx_on_time(ticks: &[Tick]) -> u64 {
    (0..ticks.len())
        .filter(|&i| !held(ticks, i))
        .map(|i| ticks[i].rx)
        .sum()
}

/// The first held or catching-up tick ([`held`]) with more than one RX
/// dropout, if any: a hold may cost one a tick.
fn rx_twice_late(ticks: &[Tick]) -> Option<usize> {
    (0..ticks.len()).find(|&i| held(ticks, i) && ticks[i].rx > 1)
}

/// The TX gaps a run of ticks may have: each at a tick at least
/// [`LATE_TICK`] late. Returns the first tick that broke it, if any.
fn gap_on_time(ticks: &[Tick]) -> Option<usize> {
    ticks.iter().position(|t| t.gap && t.late < LATE_TICK)
}

#[test]
fn a_gap_counts_only_at_a_late_tick() {
    let on_time = Duration::from_millis(1);
    let late = LATE_TICK;
    let t = |late, gap| Tick { late, gap, rx: 0 };
    assert_eq!(gap_on_time(&[t(on_time, false), t(late, true)]), None);
    assert_eq!(gap_on_time(&[t(on_time, false), t(on_time, true)]), Some(1));
    let just_under = LATE_TICK - Duration::from_nanos(1);
    assert_eq!(gap_on_time(&[t(just_under, true)]), Some(0));
    assert_eq!(gap_on_time(&[t(late, false), t(on_time, false)]), None);
}

#[test]
fn an_rx_dropout_counts_against_the_restarts_only_at_a_tick_on_time() {
    let on_time = Duration::from_millis(1);
    let just_under = LATE_TICK - Duration::from_nanos(1);
    let t = |late, rx| Tick {
        late,
        gap: false,
        rx,
    };
    assert_eq!(rx_on_time(&[t(LATE_TICK, 1), t(on_time, 0)]), 0);
    assert_eq!(rx_on_time(&[t(on_time, 1), t(just_under, 2)]), 3);
    // The catch-up tick after a held one reads two ticks' worth: allowed.
    assert_eq!(rx_on_time(&[t(LATE_TICK, 0), t(on_time, 1)]), 0);
    // Two ticks after it: on time again.
    assert_eq!(
        rx_on_time(&[t(LATE_TICK, 0), t(on_time, 0), t(on_time, 1)]),
        1
    );
    assert_eq!(rx_on_time(&[t(just_under, 0), t(on_time, 1)]), 1);
    assert_eq!(rx_twice_late(&[t(LATE_TICK, 1), t(on_time, 1)]), None);
    assert_eq!(rx_twice_late(&[t(LATE_TICK, 1), t(on_time, 2)]), Some(1));
    assert_eq!(rx_twice_late(&[t(on_time, 0), t(LATE_TICK, 2)]), Some(1));
    assert_eq!(
        rx_twice_late(&[t(LATE_TICK, 0), t(on_time, 0), t(on_time, 2)]),
        None
    );
    assert_eq!(rx_twice_late(&[t(just_under, 2), t(LATE_TICK, 1)]), None);
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn one_minute_in_real_time() {
    const REAL: Duration = Duration::from_secs(60);
    let Path { pump, ends: mut p } = path();
    // The app's clock, unskewed (module doc).
    let clock = Arc::new(SkewedClock::new(app_monotonic_ns(), 0, TestClockPpm(0.0)));
    let stop = Arc::new(AtomicBool::new(false));
    let thread = dstack::stream::spawn(
        Arc::clone(&clock) as Arc<dyn PtpClock>,
        pump,
        Arc::clone(&stop),
    )
    .unwrap();
    let period = Duration::from_secs_f64(CHUNK as f64 / f64::from(APP_RATE));
    let mut ticker = Ticker::new();
    let start = Instant::now();
    let mut deadline = start;
    let mut primed: Option<(Counts, PumpView)> = None;
    let mut ticks = Vec::with_capacity(3000);
    let mut gaps = 0;
    let mut rx = 0;
    // The TX FIFO's depth before each tick's write, ms: the sawtooth's
    // bottom, shown with a failure.
    let mut depths = Vec::with_capacity(3000);
    while start.elapsed() < REAL {
        let late = Instant::now().saturating_duration_since(deadline);
        let depth_ms = p.tx_fifo.len() as f64 * 1e3 / f64::from(APP_RATE);
        ticker.tick(&mut p);
        let counts = Counts::of(&p, T0);
        if primed.is_none() && !p.tx_stats.priming() && !p.rx_stats.priming() {
            primed = Some((counts, p.pump_stats.view()));
            gaps = counts.tx_sum();
            rx = counts.rx_underruns;
        }
        if primed.is_some() {
            ticks.push(Tick {
                late,
                gap: counts.tx_sum() != gaps,
                rx: counts.rx_underruns - rx,
            });
            depths.push(depth_ms);
            gaps = counts.tx_sum();
            rx = counts.rx_underruns;
        }
        deadline += period;
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }
    stop.store(true, Ordering::SeqCst);
    let ended = Instant::now() + Duration::from_secs(5);
    while !thread.is_finished() {
        assert!(Instant::now() < ended, "the pump thread did not end");
        std::thread::sleep(Duration::from_millis(1));
    }
    thread.join().unwrap();
    let (primed, view0) = primed.expect("both bridges primed");
    let last = Counts::of(&p, T0);
    let view = p.pump_stats.view();
    let restarts = view.rebootstraps - view0.rebootstraps;
    let late_ticks = ticks.iter().filter(|t| t.late >= LATE_TICK).count();
    let tx_gaps = ticks.iter().filter(|t| t.gap).count();
    println!(
        "real time: {} ticks, {late_ticks} late >= {LATE_TICK:?}, {tx_gaps} TX gaps, \
         {restarts} pump restarts, tx_ppm {:.2}, {last:?}",
        ticks.len(),
        last.tx_ppm
    );
    if let Some(i) = gap_on_time(&ticks) {
        let from = i.saturating_sub(5);
        let before: Vec<String> = (from..=i)
            .map(|j| format!("{j}: {:?} late, {:.2} ms", ticks[j].late, depths[j]))
            .collect();
        panic!(
            "a TX gap at tick {i}, {:?} late (under {LATE_TICK:?}); the ticks up to it, \
             late and the TX FIFO's depth before the write: {before:?}: \
             {primed:?} {last:?} {view:?}",
            ticks[i].late
        );
    }
    if let Some(i) = rx_twice_late(&ticks) {
        panic!(
            "{} RX dropouts at tick {i}, {:?} late: {primed:?} {last:?} {view:?}",
            ticks[i].rx, ticks[i].late
        );
    }
    let rx_late = last.rx_underruns - primed.rx_underruns - rx_on_time(&ticks);
    assert!(
        rx_on_time(&ticks) <= restarts,
        "RX dropouts at ticks on time past the pump's own restarts ({restarts}; \
         {rx_late} at late ticks): {primed:?} {last:?} {view:?}"
    );
    assert!(view.blocks > 40_000, "{view:?}");
}

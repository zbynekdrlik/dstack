// SPDX-License-Identifier: MIT OR Apache-2.0

//! The soak's real-time case (dstack #304): one minute of wall time, the
//! pump on its own thread on the box's monotonic clock, the app on the
//! test's thread at real 1024-frame ticks. Real wakes are what the
//! simulation (`tests/soak.rs`) cannot model.
//!
//! Not in the release gate (no `gate_` name, and its own binary, so the
//! simulated cases never load the box under it): on a shared runner the
//! test thread's wakes are the runner's load. So it judges what the player
//! owns, the way the simulated irregular writer does (dstack #303): a TX
//! gap only right after a tick that ran late enough to empty the FIFO's
//! slack ([`LATE_TICK`]), one at most a late tick, none at a tick on time;
//! an RX gap only where the pump itself restarted.

#[path = "soak/harness.rs"]
mod harness;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dstack::clock::{MonoClock, PtpClock};
use dstack::stream::PumpView;

use harness::*;

/// A tick this late (the app's thread held: the runner's load) may cost
/// one TX gap: at OBS's 22.67 ms target the sawtooth's bottom is about
/// 12 ms and the floor 5 ms, so about 7 ms of lateness reaches the floor.
const LATE_TICK: Duration = Duration::from_millis(5);

/// What one tick saw: its lateness past its due time and whether a TX gap
/// (a dropout or a re-centre) came since the last tick.
#[derive(Debug, Clone, Copy)]
struct Tick {
    late: Duration,
    gap: bool,
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
    let t = |late, gap| Tick { late, gap };
    assert_eq!(gap_on_time(&[t(on_time, false), t(late, true)]), None);
    assert_eq!(gap_on_time(&[t(on_time, false), t(on_time, true)]), Some(1));
    let just_under = LATE_TICK - Duration::from_nanos(1);
    assert_eq!(gap_on_time(&[t(just_under, true)]), Some(0));
    assert_eq!(gap_on_time(&[t(late, false), t(on_time, false)]), None);
}

#[cfg(any(target_os = "linux", windows))]
#[test]
fn one_minute_in_real_time() {
    const REAL: Duration = Duration::from_secs(60);
    let Path { pump, ends: mut p } = path();
    let clock = Arc::new(MonoClock::new());
    clock.set_k(0);
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
    while start.elapsed() < REAL {
        let late = Instant::now().saturating_duration_since(deadline);
        ticker.tick(&mut p);
        let counts = Counts::of(&p, T0);
        if primed.is_none() && !p.tx_stats.priming() && !p.rx_stats.priming() {
            primed = Some((counts, p.pump_stats.view()));
            gaps = counts.tx_sum();
        }
        if primed.is_some() {
            ticks.push(Tick {
                late,
                gap: counts.tx_sum() != gaps,
            });
            gaps = counts.tx_sum();
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
        panic!(
            "a TX gap at tick {i}, {:?} late (under {LATE_TICK:?}): {primed:?} {last:?} {view:?}",
            ticks[i].late
        );
    }
    assert!(
        last.rx_underruns - primed.rx_underruns <= restarts,
        "RX dropouts past the pump's own restarts ({restarts}): {primed:?} {last:?} {view:?}"
    );
    assert!(view.blocks > 40_000, "{view:?}");
}

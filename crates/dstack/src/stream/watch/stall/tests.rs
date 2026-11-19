use std::sync::atomic::Ordering;

use super::*;
use crate::stream::Timing;
use crate::test_log::{capture_at, undecided};

/// A block of the test's pump, ns.
const P: u64 = 1_000;
/// [`STALL_BLOCKS`] blocks' time.
const LIMIT: u64 = 3 * P;

#[test]
fn the_first_look_only_learns() {
    let mut w = StallWatch::new();
    assert_eq!(w.look(5 * P, 40, true, P), None);
    // Learnt at 5P: still within the limit at 8P.
    assert_eq!(w.look(5 * P + LIMIT, 40, true, P), None);
}

#[test]
fn blocks_still_over_three_blocks_time_while_live_are_one_stall() {
    let mut w = StallWatch::new();
    assert_eq!(w.look(0, 10, true, P), None);
    assert_eq!(w.look(P, 12, true, P), None, "rising");
    // From P on no rise: at exactly the limit not yet, past it a stall.
    assert_eq!(w.look(P + LIMIT, 12, true, P), None);
    assert_eq!(
        w.look(P + LIMIT + 1, 12, true, P),
        Some(Stall::Stalled {
            for_ns: LIMIT + 1,
            blocks: 12
        })
    );
    // Once: the looks after it say nothing while it lasts.
    for t in [10 * P, 100 * P, 1_000 * P] {
        assert_eq!(w.look(t, 12, true, P), None, "at {t}");
    }
    // Blocks rise: it ended, after the time since their last rise.
    assert_eq!(
        w.look(2_000 * P, 13, true, P),
        Some(Stall::Resumed { for_ns: 1_999 * P })
    );
    assert_eq!(w.look(2_001 * P, 14, true, P), None, "rising again");
    // A second stall is its own.
    assert_eq!(
        w.look(2_001 * P + LIMIT + 1, 14, true, P),
        Some(Stall::Stalled {
            for_ns: LIMIT + 1,
            blocks: 14
        })
    );
}

#[test]
fn a_pump_not_live_never_stalls_and_its_time_counts_from_its_last_look() {
    let mut w = StallWatch::new();
    assert_eq!(w.look(0, 10, true, P), None);
    // Stopped, or the clock not ready: no stall however long.
    assert_eq!(w.look(100 * P, 10, false, P), None);
    assert_eq!(w.look(200 * P, 10, false, P), None);
    // Live again: the time counts from the last look not live.
    assert_eq!(w.look(200 * P + LIMIT, 10, true, P), None);
    assert_eq!(
        w.look(200 * P + LIMIT + 1, 10, true, P),
        Some(Stall::Stalled {
            for_ns: LIMIT + 1,
            blocks: 10
        })
    );
}

#[test]
fn counts_that_went_down_rise() {
    let mut w = StallWatch::new();
    assert_eq!(w.look(0, 500, true, P), None);
    // A new engine attached: its blocks start at 0.
    assert_eq!(w.look(P, 0, true, P), None);
    assert_eq!(w.look(P + LIMIT, 0, true, P), None);
}

#[test]
fn the_block_s_time_sets_the_limit() {
    for period in [333_333, 5_804_988] {
        let mut w = StallWatch::new();
        assert_eq!(w.look(0, 1, true, period), None);
        assert_eq!(w.look(3 * period, 1, true, period), None, "{period}");
        assert!(
            matches!(
                w.look(3 * period + 1, 1, true, period),
                Some(Stall::Stalled { .. })
            ),
            "{period}"
        );
    }
}

/// The test's pump: B 32 at 96 kHz (a block 333 333 ns), running.
fn running() -> PumpStats {
    let s = PumpStats::new(
        Timing {
            block: 32,
            playout: 96,
            margin: 96,
        },
        96_000,
    );
    s.running.store(true, Ordering::Release);
    s
}

const S: u64 = 250_000_000;

#[test]
fn the_watch_warns_once_per_stall_and_tells_when_blocks_run_again() {
    undecided();
    let s = running();
    let mut w = StallWatch::new();
    let look = |w: &mut StallWatch, t: u64, ready: bool| {
        capture_at(tracing::Level::TRACE, || w.watch(t, "d1", Some(&s), ready))
    };
    s.record(1);
    assert_eq!(look(&mut w, 0, true), "");
    s.record(1);
    assert_eq!(look(&mut w, S, true), "", "a block ran");
    // A look without a block, 250 ms past the last rise: one warning.
    let log = look(&mut w, 2 * S, true);
    assert_eq!(log.lines().count(), 1, "{log}");
    for part in [
        STALL_WARNING,
        "WARN",
        "device=\"d1\"",
        "blocks=2",
        "stalled_ms=250",
        "block_us=333",
        "media=false",
        "late=0",
        "missed=0",
        "rebootstraps=0",
    ] {
        assert!(log.contains(part), "{part} in {log}");
    }
    assert_eq!(look(&mut w, 3 * S, true), "");
    assert_eq!(look(&mut w, 4 * S, true), "");
    s.record(1);
    let log = look(&mut w, 5 * S, true);
    assert_eq!(log.lines().count(), 1, "{log}");
    for part in [RESUMED_LINE, "INFO", "stalled_ms=1000", "blocks=3"] {
        assert!(log.contains(part), "{part} in {log}");
    }
}

#[test]
fn the_watch_says_nothing_with_the_clock_not_ready_a_stopped_pump_or_no_engine() {
    undecided();
    let s = running();
    let mut w = StallWatch::new();
    let quiet = |w: &mut StallWatch, t: u64, stats: Option<&PumpStats>, ready: bool| {
        let log = capture_at(tracing::Level::TRACE, || w.watch(t, "d1", stats, ready));
        assert_eq!(log, "", "at {t}");
    };
    quiet(&mut w, 0, Some(&s), true);
    for t in 1..4 {
        quiet(&mut w, t * S, Some(&s), false);
    }
    s.running.store(false, Ordering::Release);
    for t in 4..8 {
        quiet(&mut w, t * S, Some(&s), true);
    }
    // No engine: the next look learns.
    s.running.store(true, Ordering::Release);
    quiet(&mut w, 8 * S, None, true);
    quiet(&mut w, 9 * S, Some(&s), true);
    let log = capture_at(tracing::Level::TRACE, || {
        w.watch(10 * S, "d1", Some(&s), true)
    });
    assert!(log.contains(STALL_WARNING), "{log}");
}

#[test]
fn another_engine_is_learnt_afresh() {
    undecided();
    let a = running();
    let b = running();
    let mut w = StallWatch::new();
    w.watch(0, "d1", Some(&a), true);
    assert!(
        capture_at(tracing::Level::TRACE, || w.watch(S, "d1", Some(&a), true))
            .contains(STALL_WARNING)
    );
    // A later attach: learnt, then judged on its own.
    let log = capture_at(tracing::Level::TRACE, || {
        w.watch(2 * S, "d1", Some(&b), true)
    });
    assert_eq!(log, "");
    b.record(1);
    let log = capture_at(tracing::Level::TRACE, || {
        w.watch(3 * S, "d1", Some(&b), true)
    });
    assert_eq!(
        log, "",
        "its blocks rose: no resume line for the other's stall"
    );
}

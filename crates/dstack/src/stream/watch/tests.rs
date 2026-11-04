use super::*;
use crate::stream::Timing;
use crate::test_log::{capture_at, capture_warnings, undecided};

const S: u64 = EVERY_NS;

/// A 48 kHz app on a 96 kHz device (intercom's CI case): B 64, P 385, W 303.
const TIMING: Timing = Timing {
    block: 64,
    playout: 385,
    margin: 303,
};

fn stats() -> PumpStats {
    PumpStats::new(TIMING, 96_000)
}

fn view(rebootstraps: u64, missed: u64, overruns: u64) -> PumpView {
    PumpView {
        rebootstraps,
        missed,
        overruns,
        ..PumpView::default()
    }
}

#[test]
fn the_first_look_only_learns_the_counts() {
    let mut w = PumpWatch::new();
    // Restarts from before the watch started (an engine already running).
    assert_eq!(w.look(7 * S, &view(3, 40, 2)), None);
    assert_eq!(w.look(8 * S, &view(3, 40, 2)), None, "nothing new");
}

#[test]
fn a_growth_is_reported_once_with_its_deltas() {
    let mut w = PumpWatch::new();
    assert_eq!(w.look(0, &view(1, 5, 0)), None);
    assert_eq!(w.look(S - 1, &view(2, 9, 1)), None, "between looks");
    assert_eq!(
        w.look(S, &view(2, 9, 1)),
        Some(Grew {
            rebootstraps: 1,
            missed: 4,
            overruns: 1
        })
    );
    assert_eq!(w.look(2 * S, &view(2, 9, 1)), None, "a quiet second");
    // Each count alone is a growth.
    assert_eq!(
        w.look(3 * S, &view(3, 9, 1)).map(|g| g.rebootstraps),
        Some(1)
    );
    assert_eq!(w.look(4 * S, &view(3, 11, 1)).map(|g| g.missed), Some(2));
    assert_eq!(w.look(5 * S, &view(3, 11, 4)).map(|g| g.overruns), Some(3));
}

#[test]
fn counts_that_went_down_are_learnt_not_reported() {
    let mut w = PumpWatch::new();
    w.look(0, &view(5, 50, 5));
    // A new engine was attached: its counts start at 0.
    assert_eq!(w.look(S, &view(0, 0, 0)), None);
    assert_eq!(
        w.look(2 * S, &view(1, 2, 0)),
        Some(Grew {
            rebootstraps: 1,
            missed: 2,
            overruns: 0
        })
    );
}

#[test]
fn the_timing_reads_back_b_p_and_w() {
    let s = stats();
    assert_eq!(s.timing(), TIMING);
    // Dante Controller moved the latency: P and D move, W stays.
    let at = Timing {
        playout: 960,
        ..TIMING
    };
    s.retimed(at);
    assert_eq!(s.timing(), at);
}

#[test]
fn the_watch_warns_once_for_a_second_whose_counts_grew() {
    undecided();
    let s = stats();
    let mut w = PumpWatch::new();
    assert_eq!(capture_warnings(|| w.watch(0, "d1", Some(&s))), "");
    s.rebootstrapped(3);
    s.record(u64::MAX);
    assert_eq!(
        capture_warnings(|| w.watch(S - 1, "d1", Some(&s))),
        "",
        "between looks"
    );
    let log = capture_at(tracing::Level::TRACE, || w.watch(S, "d1", Some(&s)));
    assert_eq!(log.lines().count(), 1, "{log}");
    for part in [
        PUMP_WARNING,
        "WARN",
        "device=\"d1\"",
        "rebootstraps=1",
        "missed=3",
        "overruns=1",
        "max_process_us=18446744073709551",
        "media=false",
        "block=64",
        "margin=303",
        "playout=385",
        "offset=752",
    ] {
        assert!(log.contains(part), "{part} in {log}");
    }
    // A quiet second: nothing, at any level.
    assert_eq!(
        capture_at(tracing::Level::TRACE, || w.watch(2 * S, "d1", Some(&s))),
        ""
    );
}

#[test]
fn no_engine_logs_nothing_and_its_first_look_only_learns() {
    undecided();
    let s = stats();
    s.rebootstrapped(7);
    let mut w = PumpWatch::new();
    assert_eq!(capture_warnings(|| w.watch(0, "d1", None)), "");
    // The engine attaches later: its first look learns its counts.
    assert_eq!(capture_warnings(|| w.watch(S, "d1", Some(&s))), "");
    s.rebootstrapped(1);
    assert!(capture_warnings(|| w.watch(2 * S, "d1", Some(&s))).contains("missed=1"));
}

#[test]
fn another_engine_s_counts_are_learnt_not_reported() {
    undecided();
    let (a, b) = (stats(), stats());
    let mut w = PumpWatch::new();
    w.watch(0, "d1", Some(&a));
    // A later attach: the new engine already restarted more than the old
    // one had counted, still nothing (its counts are learnt).
    b.rebootstrapped(9);
    assert_eq!(capture_warnings(|| w.watch(S, "d1", Some(&b))), "");
    assert_eq!(capture_warnings(|| w.watch(2 * S, "d1", Some(&b))), "");
    b.rebootstrapped(2);
    let log = capture_warnings(|| w.watch(3 * S, "d1", Some(&b)));
    assert!(log.contains("missed=2"), "{log}");
}

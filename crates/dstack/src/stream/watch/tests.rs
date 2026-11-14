use super::*;
use crate::stream::{Cause, Timing};
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
            overruns: 1,
            ..Grew::default()
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
            overruns: 0,
            ..Grew::default()
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
    s.rebootstrapped(3, Cause::Late);
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
    s.rebootstrapped(7, Cause::Late);
    let mut w = PumpWatch::new();
    assert_eq!(capture_warnings(|| w.watch(0, "d1", None)), "");
    // The engine attaches later: its first look learns its counts.
    assert_eq!(capture_warnings(|| w.watch(S, "d1", Some(&s))), "");
    s.rebootstrapped(1, Cause::Late);
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
    b.rebootstrapped(9, Cause::Late);
    assert_eq!(capture_warnings(|| w.watch(S, "d1", Some(&b))), "");
    assert_eq!(capture_warnings(|| w.watch(2 * S, "d1", Some(&b))), "");
    b.rebootstrapped(2, Cause::Late);
    let log = capture_warnings(|| w.watch(3 * S, "d1", Some(&b)));
    assert!(log.contains("missed=2"), "{log}");
}

/// dstack #1 (camera-box, 4.11.2026): the warning's `max_process_us` is
/// that second's longest block, not the longest since the start (which
/// repeated one old long block in every later line). The since-start max
/// stays in the pump's view (the status).
#[test]
fn the_warning_s_max_process_us_is_that_second_s() {
    undecided();
    let s = stats();
    let mut w = PumpWatch::new();
    // A long block before the first look is only learnt with the counts.
    s.record(40_000_000);
    w.watch(0, "d1", Some(&s));
    let line = |w: &mut PumpWatch, at: u64| capture_warnings(|| w.watch(at, "d1", Some(&s)));
    s.record(5_000_000);
    s.rebootstrapped(1, Cause::Late);
    let log = line(&mut w, S);
    assert!(log.contains("max_process_us=5000 "), "{log}");
    s.record(2_000_000);
    s.rebootstrapped(1, Cause::Late);
    let log = line(&mut w, 2 * S);
    assert!(log.contains("max_process_us=2000 "), "{log}");
    // A quiet second's longest block (under the 0.67 ms period: no
    // overrun) is that second's, not the next one's.
    s.record(600_000);
    assert_eq!(line(&mut w, 3 * S), "");
    s.record(100_000);
    s.rebootstrapped(1, Cause::Late);
    let log = line(&mut w, 4 * S);
    assert!(log.contains("max_process_us=100 "), "{log}");
    // Between looks nothing is taken: the block counts in the next look.
    s.record(7_000_000);
    assert_eq!(line(&mut w, 5 * S - 1), "");
    s.rebootstrapped(1, Cause::Late);
    let log = line(&mut w, 5 * S);
    assert!(log.contains("max_process_us=7000 "), "{log}");
    // The status keeps the longest since the start.
    assert_eq!(s.view().max_process_us, 40_000);
}

/// dstack #281: the warning names the causes of the re-bootstraps that
/// grew in its second.
#[test]
fn the_cause_word_names_each_grown_cause_in_order() {
    let g = |late, clock_back, reanchor, retime| Grew {
        late,
        clock_back,
        reanchor,
        retime,
        ..Grew::default()
    };
    assert_eq!(cause_word(&g(0, 0, 0, 0)), "none");
    assert_eq!(cause_word(&g(2, 0, 0, 0)), "late");
    assert_eq!(cause_word(&g(0, 1, 0, 0)), "clock_back");
    assert_eq!(cause_word(&g(0, 0, 1, 0)), "reanchor");
    assert_eq!(cause_word(&g(0, 0, 0, 1)), "retime");
    assert_eq!(cause_word(&g(1, 0, 3, 1)), "late+reanchor+retime");
    assert_eq!(
        cause_word(&g(1, 1, 1, 1)),
        "late+clock_back+reanchor+retime"
    );
}

#[test]
fn each_cause_s_growth_is_its_own() {
    let mut w = PumpWatch::new();
    let at = |late, clock_back, reanchor, retime| PumpView {
        rebootstraps: late + clock_back + reanchor + retime,
        rebootstraps_late: late,
        rebootstraps_clock_back: clock_back,
        rebootstraps_reanchor: reanchor,
        rebootstraps_retime: retime,
        ..PumpView::default()
    };
    assert_eq!(w.look(0, &at(1, 2, 3, 4)), None);
    assert_eq!(
        w.look(S, &at(2, 4, 6, 8)),
        Some(Grew {
            rebootstraps: 10,
            late: 1,
            clock_back: 2,
            reanchor: 3,
            retime: 4,
            ..Grew::default()
        })
    );
}

#[test]
fn the_warning_names_the_causes_and_that_second_s_latest_wake() {
    undecided();
    let s = stats();
    let mut w = PumpWatch::new();
    w.watch(0, "d1", Some(&s));
    // A wake 3.2 ms late before the second: not the line's (taken at the
    // quiet look).
    s.wakes().record(3_200_000, true, false, || 1);
    assert_eq!(capture_warnings(|| w.watch(S, "d1", Some(&s))), "");
    s.wakes().record(2_500_000, true, true, || 2);
    s.rebootstrapped(2, Cause::Late);
    s.rebootstrapped(0, Cause::Retime);
    let log = capture_warnings(|| w.watch(2 * S, "d1", Some(&s)));
    for part in ["cause=late+retime", "behind_max_us=2500", "rebootstraps=2"] {
        assert!(log.contains(part), "{part} in {log}");
    }
    // Only overruns: no cause.
    s.record(u64::MAX);
    let log = capture_warnings(|| w.watch(3 * S, "d1", Some(&s)));
    assert!(log.contains("cause=none"), "{log}");
    assert!(log.contains("behind_max_us=0"), "{log}");
    assert_eq!(s.view().wake_behind_max_us, 3_200, "the status keeps it");
}

/// dstack #281: the watch ends the pump's wake minute once a minute (the
/// status's `wake_behind_minute_max_us`), at quiet looks too.
#[test]
fn the_watch_ends_the_wake_minute_every_minute() {
    const M: u64 = crate::media::late::MINUTE_NS;
    let s = stats();
    let mut w = PumpWatch::new();
    w.watch(0, "d1", Some(&s));
    s.wakes().record(4_000_000, false, false, || 0);
    w.watch(M - S, "d1", Some(&s));
    assert_eq!(s.view().wake_behind_minute_max_us, None, "not a minute yet");
    w.watch(M, "d1", Some(&s));
    assert_eq!(s.view().wake_behind_minute_max_us, Some(4_000));
    s.wakes().record(1_000_000, false, false, || 0);
    w.watch(M + S, "d1", Some(&s));
    assert_eq!(
        s.view().wake_behind_minute_max_us,
        Some(4_000),
        "mid-minute"
    );
    w.watch(2 * M, "d1", Some(&s));
    assert_eq!(s.view().wake_behind_minute_max_us, Some(1_000));
    w.watch(3 * M, "d1", Some(&s));
    assert_eq!(
        s.view().wake_behind_minute_max_us,
        Some(0),
        "a quiet minute"
    );
    // Another engine: its minute starts at its first look.
    let b = stats();
    w.watch(3 * M + S, "d1", Some(&b));
    b.wakes().record(9_000_000, false, false, || 0);
    w.watch(4 * M, "d1", Some(&b));
    assert_eq!(b.view().wake_behind_minute_max_us, None, "under its minute");
    w.watch(4 * M + S, "d1", Some(&b));
    assert_eq!(b.view().wake_behind_minute_max_us, Some(9_000));
}

/// dstack #295 step 2: one info line a minute in which the play-time
/// filter rejected pairs, with that minute's count; none in a quiet one.
#[test]
fn the_watch_logs_the_rejected_play_time_pairs_once_a_minute() {
    use std::sync::atomic::Ordering;
    const M: u64 = crate::media::late::MINUTE_NS;
    undecided();
    let s = stats();
    let mut w = PumpWatch::new();
    // Rejects from before the watch started are learnt.
    s.play_at_rejected.store(4, Ordering::Relaxed);
    let info = |w: &mut PumpWatch, at: u64, s: &PumpStats| {
        capture_at(tracing::Level::INFO, || w.watch(at, "d1", Some(s)))
    };
    assert!(!info(&mut w, 0, &s).contains(REJECTED_LINE));
    s.play_at_rejected.store(7, Ordering::Relaxed);
    assert!(
        !info(&mut w, M - S, &s).contains(REJECTED_LINE),
        "mid-minute"
    );
    let log = info(&mut w, M, &s);
    assert_eq!(log.matches(REJECTED_LINE).count(), 1, "{log}");
    assert!(
        log.contains("rejected=3") && log.contains("total=7"),
        "{log}"
    );
    assert!(log.contains("d1"), "{log}");
    let log = info(&mut w, 2 * M, &s);
    assert!(!log.contains(REJECTED_LINE), "a quiet minute: {log}");
    s.play_at_rejected.store(8, Ordering::Relaxed);
    let log = info(&mut w, 3 * M, &s);
    assert!(
        log.contains("rejected=1") && log.contains("total=8"),
        "{log}"
    );
    // Another engine: its count is learnt at its first look.
    let b = stats();
    b.play_at_rejected.store(50, Ordering::Relaxed);
    assert!(!info(&mut w, 3 * M + S, &b).contains(REJECTED_LINE));
    b.play_at_rejected.store(51, Ordering::Relaxed);
    let log = info(&mut w, 4 * M + S, &b);
    assert!(
        log.contains("rejected=1") && log.contains("total=51"),
        "{log}"
    );
}

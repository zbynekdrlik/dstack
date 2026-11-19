// SPDX-License-Identifier: MIT OR Apache-2.0

use super::*;

/// Counts with `bins` (index, count) risen, nothing else.
fn bins(set: &[(usize, u64)]) -> UnderrunCounts {
    let mut c = UnderrunCounts::default();
    for &(bin, n) in set {
        c.late_bins[bin] = n;
    }
    c
}

/// Counts with `bins` risen at dsg's driver shape (128 @ 96 kHz, a
/// 1.333 ms period: the side's floor is half of it, 666.5 us).
fn dsg(set: &[(usize, u64)]) -> UnderrunCounts {
    UnderrunCounts {
        period_us: 1333,
        ..bins(set)
    }
}

/// At dsg's 1.333 ms from-ASIO target (a 1.333 ms period) a callback
/// that alone reaches the target is the driver's; one the side's floor
/// (half the period) plus it reaches the target is the target's: from
/// 666.5 us late, so 0.75-1.0 and 1.0-1.25 ms; under that the bridge's.
/// The highest risen bin decides, and any gap over two periods is the
/// driver's whatever the bins.
#[test]
fn a_from_asio_underrun_is_judged_against_the_target_less_the_floor() {
    assert_eq!(from_asio_cause(&dsg(&[]), 1333), Cause::Bridge);
    assert_eq!(from_asio_cause(&dsg(&[(1, 5)]), 1333), Cause::Bridge);
    assert_eq!(from_asio_cause(&dsg(&[(2, 5)]), 1333), Cause::Target);
    assert_eq!(from_asio_cause(&dsg(&[(3, 1)]), 1333), Cause::Target);
    assert_eq!(
        from_asio_cause(&dsg(&[(0, 9), (3, 1)]), 1333),
        Cause::Target
    );
    assert_eq!(from_asio_cause(&dsg(&[(7, 1)]), 1333), Cause::Driver);
    // The floor's edge: 500 us late + 666.5 us reaches 1166 us, not 1167.
    assert_eq!(from_asio_cause(&dsg(&[(1, 1)]), 1166), Cause::Target);
    assert_eq!(from_asio_cause(&dsg(&[(1, 1)]), 1167), Cause::Bridge);
    // The driver's edge: 1000 us late alone reaches 1000 us, not 1001.
    assert_eq!(from_asio_cause(&dsg(&[(3, 1)]), 1000), Cause::Driver);
    assert_eq!(from_asio_cause(&dsg(&[(3, 1)]), 1001), Cause::Target);
    // No driver shape known (no open): no floor, the lateness alone.
    assert_eq!(from_asio_cause(&bins(&[(3, 1)]), 1000), Cause::Driver);
    assert_eq!(from_asio_cause(&bins(&[(3, 1)]), 1001), Cause::Bridge);
    // A 4 ms target (the default 256 at 96 kHz, a period `of` reads as
    // 2666 us): the top bin by the period, 2666 us + 1333 us of floor
    // reaches 3999 us, not 4000 (an even period: the edge is equality).
    assert_eq!(from_asio_cause(&bins(&[(7, 3)]), 4000), Cause::Bridge);
    let top = UnderrunCounts {
        period_us: 2666,
        ..bins(&[(7, 3)])
    };
    assert_eq!(from_asio_cause(&top, 3999), Cause::Target);
    assert_eq!(from_asio_cause(&top, 4000), Cause::Bridge);
    let late = UnderrunCounts {
        late_callbacks: 1,
        ..UnderrunCounts::default()
    };
    assert_eq!(from_asio_cause(&late, 4000), Cause::Driver);
}

/// The open top bin reaches up to one period late, so at a long period it
/// is judged by the period: 256 at 48 kHz (5.333 ms) is the driver's up to
/// a 5.333 ms target and the target's up to 8 ms (half the period of
/// floor, 2666.5 us); a closed bin's lower edge never is.
#[test]
fn the_top_bin_is_judged_by_the_period_it_reaches() {
    let top = UnderrunCounts {
        period_us: 5333,
        ..bins(&[(7, 1)])
    };
    assert_eq!(from_asio_cause(&top, 5333), Cause::Driver);
    assert_eq!(from_asio_cause(&top, 5334), Cause::Target);
    assert_eq!(from_asio_cause(&top, 6000), Cause::Target);
    assert_eq!(from_asio_cause(&top, 7999), Cause::Target, "7999.5");
    assert_eq!(from_asio_cause(&top, 8000), Cause::Bridge, "7999.5");
    let below = UnderrunCounts {
        period_us: 5333,
        ..bins(&[(6, 1)])
    };
    assert_eq!(from_asio_cause(&below, 6000), Cause::Bridge, "1.75-2.0 ms");
    // No period known (no open): the top bin's lower edge, 2.0 ms, alone.
    assert_eq!(from_asio_cause(&bins(&[(7, 1)]), 2000), Cause::Driver);
    assert_eq!(from_asio_cause(&bins(&[(7, 1)]), 2001), Cause::Bridge);
}

/// With no from-ASIO target (0: the player not started) nothing is judged.
#[test]
fn a_from_asio_underrun_with_no_target_has_no_known_cause() {
    let late = UnderrunCounts {
        late_callbacks: 1,
        ..bins(&[(7, 1)])
    };
    assert_eq!(from_asio_cause(&late, 0), Cause::Unknown);
    assert_eq!(from_asio_cause(&late, 1), Cause::Driver);
    assert_eq!(us_of_ms(1.3339), 1334, "rounded, not truncated");
    assert_eq!(us_of_ms(f64::NAN), 0);
}

/// To-ASIO pulls and plays in one switch: only a gap over two periods is
/// the driver's there, never a lateness under it.
#[test]
fn a_to_asio_underrun_is_the_drivers_only_past_two_periods() {
    let top = UnderrunCounts {
        period_us: 5333,
        ..bins(&[(7, 4)])
    };
    assert_eq!(to_asio_cause(&top), Cause::Bridge);
    let late = UnderrunCounts {
        late_callbacks: 1,
        ..bins(&[])
    };
    assert_eq!(to_asio_cause(&late), Cause::Driver);
}

#[test]
fn the_lateness_names_each_risen_bin_by_its_edges_and_the_count_past_two_periods() {
    assert_eq!(lateness_text(&bins(&[])), "none 250 us late or more");
    assert_eq!(
        lateness_text(&bins(&[(0, 2), (3, 1), (6, 4)])),
        "250-500 us: 2, 1000-1250 us: 1, 1750-2000 us: 4"
    );
    assert_eq!(lateness_text(&bins(&[(7, 1)])), "2000 us or more: 1");
    let late = UnderrunCounts {
        late_callbacks: 3,
        ..bins(&[(4, 1)])
    };
    assert_eq!(lateness_text(&late), "1250-1500 us: 1, over two periods: 3");
    let only_late = UnderrunCounts {
        late_callbacks: 1,
        ..bins(&[])
    };
    assert_eq!(lateness_text(&only_late), "over two periods: 1");
}

/// The watch warns only on an underrun rise, with each side's new count,
/// whose it reads as and the lateness risen over the same checks; the
/// lateness alone never warns, and a fallen count counts from 0.
#[test]
fn the_watch_names_the_callback_lateness_around_each_underrun_rise() {
    let mut w = UnderrunWatch::new();
    let mut now = UnderrunCounts {
        period_us: 1333,
        ..UnderrunCounts::default()
    };
    assert_eq!(w.look(now, 1.333_333), None, "nothing yet");
    // Lateness without an underrun: no line, but its rise is spent.
    now.late_bins[2] = 4;
    assert_eq!(w.look(now, 1.333_333), None, "no underrun");
    // A from-ASIO underrun with a callback 1.0-1.25 ms late meanwhile:
    // under the target alone, over it with the side's floor (666.5 us).
    now.from_asio_underruns = 2;
    now.late_bins[3] = 1;
    let text = w.look(now, 1.333_333).expect("a from-ASIO rise");
    assert_eq!(
        text,
        "asio bridge underruns since the last check: from-ASIO 2 (target 1333 us), \
         cause: the target under the side's floor plus the callbacks' lateness; \
         driver callbacks late meanwhile: 1000-1250 us: 1"
    );
    // The same counts again: nothing rose.
    assert_eq!(w.look(now, 1.333_333), None, "no new underrun");
    // A to-ASIO underrun and a from-ASIO one with only 0.5 ms late.
    now.to_asio_underruns = 1;
    now.from_asio_underruns = 3;
    now.late_bins[1] = 5;
    let text = w.look(now, 1.333_333).expect("both rose");
    assert_eq!(
        text,
        "asio bridge underruns since the last check: from-ASIO 1 (target 1333 us), \
         cause: the bridge (no callback late enough); to-ASIO 1, cause: the bridge \
         (no callback late enough); driver callbacks late meanwhile: 500-750 us: 5"
    );
    // A new run's counters (fallen) count from 0: its one late gap is the
    // driver's to-ASIO.
    now = UnderrunCounts {
        to_asio_underruns: 2,
        from_asio_underruns: 3,
        late_bins: [0; LATE_BINS],
        late_callbacks: 1,
        period_us: 1333,
        late_max_us: 0,
    };
    let text = w.look(now, 4.0).expect("a to-ASIO rise");
    assert_eq!(
        text,
        "asio bridge underruns since the last check: to-ASIO 1, cause: driver callback \
         lateness; driver callbacks late meanwhile: over two periods: 1"
    );
    // Both players' counts fell (new players): each counts from 0.
    now.to_asio_underruns = 1;
    now.from_asio_underruns = 1;
    let text = w.look(now, 4.0).expect("fallen counts");
    assert!(
        text.starts_with(
            "asio bridge underruns since the last check: from-ASIO 1 (target 4000 us), \
             cause: the bridge (no callback late enough); to-ASIO 1, cause: the bridge"
        ),
        "{text}"
    );
    assert!(text.ends_with("none 250 us late or more"), "{text}");
    // Before the from-ASIO player starts its target reads 0.
    now.from_asio_underruns = 2;
    let text = w.look(now, 0.0).expect("a from-ASIO rise");
    assert!(
        text.contains("from-ASIO 1 (target 0 us), cause: unknown (no target yet);"),
        "{text}"
    );
}

/// The counts come from the runner's status, with the driver's period from
/// its last open (0 before any: no rate).
#[test]
fn the_counts_come_from_the_runners_status_and_the_players() {
    let status = Status {
        late_bins: [1, 2, 3, 4, 5, 6, 7, 8],
        late_callbacks: 9,
        late_max_us: 2345,
        rate: 48_000,
        buffer_frames: 256,
        ..Status::default()
    };
    assert_eq!(
        UnderrunCounts::of(&status, 10, 11),
        UnderrunCounts {
            to_asio_underruns: 10,
            from_asio_underruns: 11,
            late_bins: [1, 2, 3, 4, 5, 6, 7, 8],
            late_callbacks: 9,
            period_us: 5333,
            late_max_us: 2345,
        }
    );
    let closed = Status {
        rate: 0,
        buffer_frames: 256,
        ..Status::default()
    };
    assert_eq!(UnderrunCounts::of(&closed, 0, 0).period_us, 0);
}

/// dstack #391: after each look the watch hands the device's figures the
/// last counts and each side's latest underrun's cause; a side with no
/// underrun yet says none, and a cause changes only on its own side's
/// rise.
#[test]
fn the_watch_publishes_the_counts_and_each_side_s_latest_cause() {
    use dstack::info::{FigureSlot, OwnFigures, UnderrunCause};
    let slot = FigureSlot::started_at(0);
    slot.set_source(OwnFigures::default);
    let read = |slot: &FigureSlot| {
        let f = slot.figures().unwrap();
        (
            f.to_asio_underruns,
            f.from_asio_underruns,
            f.to_asio_cause,
            f.from_asio_cause,
        )
    };
    let mut w = UnderrunWatch::new();
    let mut now = UnderrunCounts {
        period_us: 1333,
        ..UnderrunCounts::default()
    };
    w.look(now, 1.333_333);
    w.publish(&slot);
    assert_eq!(
        read(&slot),
        (0, 0, UnderrunCause::None, UnderrunCause::None)
    );
    // From-ASIO with a callback 1.0 ms late: the target's, sent as the
    // driver's (the wire has no code of its own for it).
    now.from_asio_underruns = 2;
    now.late_bins[3] = 1;
    w.look(now, 1.333_333);
    w.publish(&slot);
    assert_eq!(
        read(&slot),
        (0, 2, UnderrunCause::None, UnderrunCause::Driver)
    );
    // To-ASIO with no gap over two periods: the bridge's; from-ASIO did
    // not rise, so its cause stays.
    now.to_asio_underruns = 1;
    w.look(now, 1.333_333);
    w.publish(&slot);
    assert_eq!(
        read(&slot),
        (1, 2, UnderrunCause::Bridge, UnderrunCause::Driver)
    );
    // To-ASIO after a gap over two periods: the driver's; from-ASIO again
    // with nothing late in its bins: still the driver's (the gap).
    now.to_asio_underruns = 4;
    now.late_callbacks = 1;
    w.look(now, 1.333_333);
    w.publish(&slot);
    assert_eq!(
        read(&slot),
        (4, 2, UnderrunCause::Driver, UnderrunCause::Driver)
    );
    // From-ASIO with nothing late: now the bridge's; to-ASIO's stays.
    now.from_asio_underruns = 5;
    w.look(now, 1.333_333);
    w.publish(&slot);
    assert_eq!(
        read(&slot),
        (4, 5, UnderrunCause::Driver, UnderrunCause::Bridge)
    );
    // Before any target: unknown.
    now.from_asio_underruns = 6;
    w.look(now, 0.0);
    w.publish(&slot);
    assert_eq!(read(&slot).3, UnderrunCause::Unknown);
    assert_eq!(UnderrunCause::from(Cause::Driver), UnderrunCause::Driver);
    assert_eq!(UnderrunCause::from(Cause::Bridge), UnderrunCause::Bridge);
    assert_eq!(UnderrunCause::from(Cause::Unknown), UnderrunCause::Unknown);
    assert_eq!(UnderrunCause::from(Cause::Target), UnderrunCause::Driver);
}

/// A lateness summed over several switches can pass the period the top
/// bin is otherwise judged by (dstack #393): when the run's peak lateness
/// rose over the check, the top bin reaches that peak. 4.6 ms behind at a
/// 4.5 ms target is the driver's, though the period (2666 us, 256 at
/// 96 kHz as `UnderrunCounts::of` reads it, with its floor of 1333 us
/// under 4.5 ms) is not; a check whose peak did not rise falls back to
/// the period. A peak from 3167 us reaches the target with the floor.
#[test]
fn the_top_bin_reaches_the_peak_lateness_that_rose_over_the_check() {
    let mut w = UnderrunWatch::new();
    let at = |from_asio_underruns, top, late_max_us| UnderrunCounts {
        from_asio_underruns,
        period_us: 2666,
        late_max_us,
        ..bins(&[(7, top)])
    };
    let text = w.look(at(1, 1, 4600), 4.5).expect("an underrun rose");
    assert!(text.contains("cause: driver callback lateness"), "{text}");
    let text = w.look(at(2, 2, 4600), 4.5).expect("an underrun rose");
    assert!(text.contains("cause: the bridge"), "{text}");
    // A new run's counters (the peak fell): its own peak counts.
    let text = w.look(at(1, 1, 4500), 4.5).expect("an underrun rose");
    assert!(text.contains("cause: driver callback lateness"), "{text}");
    let low = |late_max_us| UnderrunCounts {
        late_max_us,
        ..at(1, 1, 0)
    };
    assert_eq!(from_asio_cause(&low(2900), 4500), Cause::Bridge);
    assert_eq!(from_asio_cause(&low(3166), 4500), Cause::Bridge);
    assert_eq!(from_asio_cause(&low(3167), 4500), Cause::Target);
}

/// dsg's case (dstack #393): a from-ASIO target of 1 ms with a 128-frame
/// driver at 96 kHz, two callbacks 0.6 ms late, then an underrun. The side
/// needs about half a period (0.667 ms, #422's measured floor) plus the
/// callbacks' lateness: the bin's lower edge (500 us) + 666.5 us is over
/// the 1 ms target, so the cause is the target, not the bridge.
#[test]
fn a_target_under_the_floor_plus_the_lateness_is_named_so() {
    let status = Status {
        rate: 96_000,
        buffer_frames: 128,
        ..Status::default()
    };
    let mut w = UnderrunWatch::new();
    let mut now = UnderrunCounts::of(&status, 0, 0);
    assert_eq!(now.period_us, 1333);
    assert_eq!(w.look(now, 1.0), None, "nothing yet");
    let bin = crate::state::lateness_bin(600_000).expect("600 us is binned");
    now.late_bins[bin] = 2;
    now.from_asio_underruns = 1;
    let text = w.look(now, 1.0).expect("a from-ASIO rise");
    assert_eq!(
        text,
        "asio bridge underruns since the last check: from-ASIO 1 (target 1000 us), \
         cause: the target under the side's floor plus the callbacks' lateness; \
         driver callbacks late meanwhile: 500-750 us: 2"
    );
}

/// A new run (its counts fell) whose peak lateness equals the old run's
/// is that run's own peak (dstack #393 review): its top bin is judged by
/// it, not by the period.
#[test]
fn a_new_runs_equal_peak_is_its_own() {
    let mut w = UnderrunWatch::new();
    let at = |from_asio_underruns, top, late_max_us| UnderrunCounts {
        from_asio_underruns,
        period_us: 2666,
        late_max_us,
        ..bins(&[(7, top)])
    };
    w.look(at(1, 3, 4600), 4.5).expect("an underrun rose");
    let text = w.look(at(2, 1, 4600), 4.5).expect("an underrun rose");
    assert!(text.contains("cause: driver callback lateness"), "{text}");
}

/// A new run is also told by its late count falling alone (dstack #393
/// review): its top bin is judged by its own equal peak.
#[test]
fn a_fallen_late_count_alone_marks_a_new_run() {
    let mut w = UnderrunWatch::new();
    let before = UnderrunCounts {
        from_asio_underruns: 1,
        late_callbacks: 2,
        period_us: 2666,
        late_max_us: 4600,
        ..bins(&[(7, 1)])
    };
    w.look(before, 4.5).expect("an underrun rose");
    let now = UnderrunCounts {
        from_asio_underruns: 2,
        late_callbacks: 0,
        late_bins: bins(&[(7, 2)]).late_bins,
        ..before
    };
    let text = w.look(now, 4.5).expect("an underrun rose");
    assert!(text.contains("cause: driver callback lateness"), "{text}");
}

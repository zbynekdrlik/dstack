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

/// At dsg's 1.333 ms from-ASIO target a callback 1.0 ms late or more is
/// the driver's (3/4 of the target: 999.75 us), one 0.75-1.0 ms late the
/// bridge's; the highest risen bin decides, and any gap over two periods is
/// the driver's whatever the bins.
#[test]
fn a_from_asio_underrun_is_the_drivers_from_three_quarters_of_its_target() {
    assert_eq!(from_asio_cause(&bins(&[]), 1333), Cause::Bridge);
    assert_eq!(from_asio_cause(&bins(&[(2, 5)]), 1333), Cause::Bridge);
    assert_eq!(from_asio_cause(&bins(&[(3, 1)]), 1333), Cause::Driver);
    assert_eq!(
        from_asio_cause(&bins(&[(0, 9), (3, 1)]), 1333),
        Cause::Driver
    );
    assert_eq!(from_asio_cause(&bins(&[(7, 1)]), 1333), Cause::Driver);
    // The edge itself: 3/4 of 1000 us is the 750 us bin's lower edge.
    assert_eq!(from_asio_cause(&bins(&[(2, 1)]), 1000), Cause::Driver);
    assert_eq!(from_asio_cause(&bins(&[(1, 1)]), 1000), Cause::Bridge);
    assert_eq!(from_asio_cause(&bins(&[(2, 1)]), 1001), Cause::Bridge);
    // A 4 ms target (the default 256 at 96 kHz): even 2.0 ms late is not.
    assert_eq!(from_asio_cause(&bins(&[(7, 3)]), 4000), Cause::Bridge);
    let late = UnderrunCounts {
        late_callbacks: 1,
        ..UnderrunCounts::default()
    };
    assert_eq!(from_asio_cause(&late, 4000), Cause::Driver);
}

/// To-ASIO pulls and plays in one switch: only a gap over two periods is
/// the driver's there, never a lateness under it.
#[test]
fn a_to_asio_underrun_is_the_drivers_only_past_two_periods() {
    assert_eq!(to_asio_cause(&bins(&[(7, 4)])), Cause::Bridge);
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
/// whose it reads as and the lateness risen over the same looks; the
/// lateness alone never warns, and a fallen count counts from 0.
#[test]
fn the_watch_names_the_callback_lateness_around_each_underrun_rise() {
    let mut w = UnderrunWatch::new();
    let mut now = UnderrunCounts::default();
    assert_eq!(w.look(now, 1.333_333), None, "nothing yet");
    // Lateness without an underrun: no line, but its rise is spent.
    now.late_bins[2] = 4;
    assert_eq!(w.look(now, 1.333_333), None, "no underrun");
    // A from-ASIO underrun with a callback 1.0-1.25 ms late meanwhile.
    now.from_asio_underruns = 2;
    now.late_bins[3] = 1;
    let text = w.look(now, 1.333_333).expect("a from-ASIO rise");
    assert_eq!(
        text,
        "asio bridge underruns since the last look: from-ASIO 2 (target 1333 us): \
         the driver's lateness; driver callbacks late meanwhile: 1000-1250 us: 1"
    );
    // The same counts again: nothing rose.
    assert_eq!(w.look(now, 1.333_333), None, "no new underrun");
    // A to-ASIO underrun and a from-ASIO one with only 0.75 ms late.
    now.to_asio_underruns = 1;
    now.from_asio_underruns = 3;
    now.late_bins[2] = 5;
    let text = w.look(now, 1.333_333).expect("both rose");
    assert_eq!(
        text,
        "asio bridge underruns since the last look: from-ASIO 1 (target 1333 us): \
         the bridge's, to-ASIO 1: the bridge's; driver callbacks late meanwhile: \
         750-1000 us: 1"
    );
    // A new run's counters (fallen) count from 0: its one late gap is the
    // driver's on both sides.
    now = UnderrunCounts {
        to_asio_underruns: 2,
        from_asio_underruns: 3,
        late_bins: [0; LATE_BINS],
        late_callbacks: 1,
    };
    let text = w.look(now, 4.0).expect("a to-ASIO rise");
    assert_eq!(
        text,
        "asio bridge underruns since the last look: to-ASIO 1: the driver's lateness; \
         driver callbacks late meanwhile: over two periods: 1"
    );
    // Both players' counts fell (new players): each counts from 0.
    now.to_asio_underruns = 1;
    now.from_asio_underruns = 1;
    let text = w.look(now, 4.0).expect("fallen counts");
    assert!(
        text.starts_with(
            "asio bridge underruns since the last look: from-ASIO 1 (target 4000 us): \
             the bridge's, to-ASIO 1: the bridge's;"
        ),
        "{text}"
    );
    assert!(text.ends_with("none 250 us late or more"), "{text}");
}

#[test]
fn the_counts_come_from_the_runners_status_and_the_players() {
    let status = Status {
        late_bins: [1, 2, 3, 4, 5, 6, 7, 8],
        late_callbacks: 9,
        ..Status::default()
    };
    assert_eq!(
        UnderrunCounts::of(&status, 10, 11),
        UnderrunCounts {
            to_asio_underruns: 10,
            from_asio_underruns: 11,
            late_bins: [1, 2, 3, 4, 5, 6, 7, 8],
            late_callbacks: 9,
        }
    );
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use super::*;

use crate::test_log::{capture_warnings, undecided};

const S: u64 = 1_000_000_000;

fn view(blocks: u64, rebootstraps: u64, skipped: u64, after: u64, faults: u64) -> TxFillView {
    TxFillView {
        blocks,
        rebootstraps,
        skipped,
        writes_after_send: after,
        faults,
    }
}

#[test]
fn the_first_look_learns_and_a_quiet_second_says_nothing() {
    let mut w = FillWatch::new();
    assert_eq!(w.look(S, view(5, 1, 2, 3, 0)), None, "the first learns");
    assert_eq!(w.look(2 * S, view(400, 1, 2, 3, 0)), None, "blocks alone");
}

#[test]
fn each_cause_alone_is_reported_with_what_grew() {
    for (k, grown) in [
        view(10, 1, 0, 0, 0),
        view(10, 0, 3, 0, 0),
        view(10, 0, 0, 1, 0),
        view(10, 0, 0, 0, 1),
    ]
    .into_iter()
    .enumerate()
    {
        let mut w = FillWatch::new();
        let base = view(100, 5, 6, 7, 0);
        assert_eq!(w.look(S, base), None);
        let now = view(
            base.blocks + grown.blocks,
            base.rebootstraps + grown.rebootstraps,
            base.skipped + grown.skipped,
            base.writes_after_send + grown.writes_after_send,
            base.faults + grown.faults,
        );
        assert_eq!(w.look(2 * S, now), Some(grown), "case {k}");
    }
}

#[test]
fn a_look_comes_once_a_second_and_the_counts_carry_over() {
    let mut w = FillWatch::new();
    assert_eq!(w.look(S, view(0, 0, 0, 0, 0)), None);
    // Within the second: not looked at, nothing learnt.
    assert_eq!(w.look(2 * S - 1, view(1, 1, 0, 0, 0)), None);
    assert_eq!(
        w.look(2 * S, view(2, 1, 0, 0, 0)),
        Some(view(2, 1, 0, 0, 0))
    );
    assert_eq!(w.look(3 * S, view(3, 1, 0, 0, 0)), None);
}

#[test]
fn the_warning_names_the_counts_and_the_panic() {
    undecided();
    let stats = TxFillStats::default();
    let mut w = FillWatch::new();
    w.watch_stats(S, "d1", &stats);
    stats.rebootstrapped(4);
    stats.faulted("the app's TX processor panics".to_string());
    let log = capture_warnings(|| w.watch_stats(2 * S, "d1", &stats));
    let lines: Vec<&str> = log
        .lines()
        .filter(|l| l.contains(TX_PULL_WARNING))
        .collect();
    assert_eq!(lines.len(), 1, "{log}");
    for field in [
        "device=\"d1\"",
        "rebootstraps=1",
        "skipped=4",
        "faults=1",
        "writes_after_send=0",
        "the app's TX processor panics",
    ] {
        assert!(lines[0].contains(field), "{field}: {log}");
    }
    // A second with only a restart: no panic named again.
    stats.rebootstrapped(0);
    let log = capture_warnings(|| w.watch_stats(3 * S, "d1", &stats));
    assert!(
        log.contains("fault=\"-\"") || log.contains("fault=-"),
        "{log}"
    );
    assert_eq!(
        stats.fault().as_deref(),
        Some("the app's TX processor panics")
    );
}

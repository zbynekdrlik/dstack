// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #459: one stall that re-bootstraps both peers' pumps at once
//! counts as one restart in the peer test's window, and how the window
//! counts its restarts.

use std::collections::BTreeSet;

use super::Counts;

/// A pump late wake that re-bootstrapped, as the witness merged it
/// (dstack #459): its wall time (ms since the Unix epoch), how far behind
/// it woke and how late its own sleep before it returned (µs; `None`
/// unmeasured).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Rebootstrap {
    pub(super) unix_ms: u64,
    pub(super) behind_us: u64,
    pub(super) oversleep_us: Option<u64>,
}

/// How far apart two peers' re-bootstrapping wakes may be and still be one
/// stall, ms (dstack #459): the runner did not run either pump past its
/// timer (the whole process, or the CPU holding both), and each wakes at
/// its own next timer once run again, a block (0.67 ms) or so apart. Every
/// incident's two wakes were in the same ms; once the witness thread ran
/// on time beside them (v0.6.199 main, ubuntu-24.04: largest gap 1992 µs).
const TOGETHER_MS: u64 = 2;

/// How far a re-bootstrap's own sleep may have returned short of or past
/// its lateness and still be the scheduler's miss, µs: the sleep alone put
/// the pump that late. The same 500 µs slack as `ci_peer_gate.py`'s rule
/// for a lone re-bootstrap (dstack #339), applied both ways. The
/// incidents: 1 to 10 µs.
const OVERSLEEP_SLACK_US: u64 = 500;

/// Whether a re-bootstrap's wake is in the counted window: after `from_ms`
/// and by `to_ms` (dstack #459).
pub(super) fn in_window(r: &Rebootstrap, from_ms: u64, to_ms: u64) -> bool {
    r.unix_ms > from_ms && r.unix_ms <= to_ms
}

/// The pairs of both peers' pump re-bootstraps that one stall caused
/// (dstack #459): a re-bootstrap of peer 0 and one of peer 1
/// within [`TOGETHER_MS`] of each other, each woken late by its own sleep
/// ([`OVERSLEEP_SLACK_US`]), each after `from_ms` and by `to_ms` (the
/// counted window), and each in one pair at most. A re-bootstrap with no
/// measured oversleep, or one pump's alone, is in no pair.
pub(super) fn shared_stalls(
    rebootstraps: &[BTreeSet<Rebootstrap>; 2],
    from_ms: u64,
    to_ms: u64,
) -> u64 {
    let judged = |peer: usize| {
        rebootstraps[peer].iter().filter(move |r| {
            in_window(r, from_ms, to_ms)
                && r.oversleep_us
                    .is_some_and(|o| o.abs_diff(r.behind_us) <= OVERSLEEP_SLACK_US)
        })
    };
    let mut theirs: Vec<&Rebootstrap> = judged(1).collect();
    let mut pairs = 0;
    for mine in judged(0) {
        if let Some(i) = theirs
            .iter()
            .position(|t| t.unix_ms.abs_diff(mine.unix_ms) <= TOGETHER_MS)
        {
            theirs.remove(i);
            pairs += 1;
        }
    }
    pairs
}

/// The restarts the window counts (dstack #459): the sender's TX restarts
/// and both pumps' re-bootstraps, less the `pairs` one stall caused each
/// ([`shared_stalls`]). The pairs are credited only when neither peer has
/// more re-bootstrap wakes in the window (`wakes`, mine and theirs) than
/// counted pump restarts: the pump records a wake before it counts its
/// re-bootstrap, so a wake in the window whose count came after the last
/// read credits nothing; and never more pairs than either pump's restarts.
pub(super) fn counted_restarts(
    mine: &Counts,
    theirs: &Counts,
    wakes: (u64, u64),
    pairs: u64,
) -> u64 {
    let pumps = theirs.pump_restarts + mine.pump_restarts;
    let credited = if wakes.0 <= mine.pump_restarts && wakes.1 <= theirs.pump_restarts {
        pairs.min(mine.pump_restarts).min(theirs.pump_restarts)
    } else {
        0
    };
    theirs.tx_restarts + pumps - credited
}

/// One stall that re-bootstraps both peers' pumps is one pair (dstack
/// #459, the CI failures of 19-20.11.2026: both wakes in the same ms, each
/// one's own sleep that late); one pump's re-bootstrap alone,
/// two apart, a wake not woken late by its sleep and a wake outside the
/// counted window are in none.
#[test]
fn both_pumps_restarted_by_one_stall_are_one_pair() {
    let r = |unix_ms, behind_us, oversleep_us| Rebootstrap {
        unix_ms,
        behind_us,
        oversleep_us,
    };
    let pairs = |a: &[Rebootstrap], b: &[Rebootstrap]| {
        let set = |rs: &[Rebootstrap]| rs.iter().copied().collect::<BTreeSet<_>>();
        shared_stalls(&[set(a), set(b)], 1_000, 9_000)
    };
    // v0.6.200 main, windows-2025 (run 44684350870) and v0.6.199 main,
    // ubuntu-24.04 (run 44676550352), behind and oversleep in µs.
    let a = r(5_000, 7_250, Some(7_251));
    assert_eq!(pairs(&[a], &[r(5_000, 6_604, Some(6_607))]), 1);
    assert_eq!(
        pairs(
            &[r(5_000, 6_770, Some(6_780))],
            &[r(5_000, 6_083, Some(6_085))]
        ),
        1
    );
    // One pump alone, either peer: today's rule.
    assert_eq!(pairs(&[a], &[]), 0);
    assert_eq!(pairs(&[], &[a]), 0);
    // Apart: within TOGETHER_MS either way is one stall, past it two.
    assert_eq!(pairs(&[a], &[r(5_002, 6_604, Some(6_607))]), 1);
    assert_eq!(pairs(&[r(5_002, 6_604, Some(6_607))], &[a]), 1);
    assert_eq!(pairs(&[a], &[r(5_003, 6_604, Some(6_607))]), 0);
    assert_eq!(pairs(&[a], &[r(4_997, 6_604, Some(6_607))]), 0);
    // Each woken late by its own sleep, within 0.5 ms either way; a block
    // that ran long (the sleep on time) or an unmeasured sleep is no stall.
    let b = |oversleep| r(5_000, 6_600, oversleep);
    assert_eq!(pairs(&[a], &[b(Some(6_100))]), 1);
    assert_eq!(pairs(&[a], &[b(Some(7_100))]), 1);
    assert_eq!(pairs(&[a], &[b(Some(6_099))]), 0);
    assert_eq!(pairs(&[a], &[b(Some(7_101))]), 0);
    assert_eq!(pairs(&[a], &[b(None)]), 0);
    assert_eq!(pairs(&[b(None)], &[a]), 0);
    assert_eq!(pairs(&[r(5_000, 7_250, Some(10))], &[a]), 0);
    // Only inside the counted window: after its start, by its end.
    let at = |ms| r(ms, 6_600, Some(6_600));
    assert_eq!(pairs(&[at(1_000)], &[at(1_000)]), 0);
    assert_eq!(pairs(&[at(1_001)], &[at(1_001)]), 1);
    assert_eq!(pairs(&[at(9_000)], &[at(9_000)]), 1);
    assert_eq!(pairs(&[at(9_001)], &[at(9_001)]), 0);
    assert_eq!(pairs(&[at(1_001)], &[at(1_000)]), 0);
    assert_eq!(pairs(&[at(9_000)], &[at(9_001)]), 0);
    // Each wake in one pair at most; two stalls are two pairs.
    assert_eq!(pairs(&[at(5_000), at(5_001)], &[at(5_000)]), 1);
    assert_eq!(pairs(&[at(5_000)], &[at(5_000), at(5_001)]), 1);
    assert_eq!(pairs(&[at(5_000), at(7_000)], &[at(5_001), at(7_000)]), 2);
}

/// The counted restarts (dstack #459): a stall's pair counts once, credited
/// only within both pumps' counted restarts and only when every
/// re-bootstrap wake in the window was counted; a TX restart and a lone
/// pump restart count as they did.
#[test]
fn a_pair_counts_once_within_the_counted_restarts() {
    let c = |tx_restarts, pump_restarts| Counts {
        tx_restarts,
        pump_restarts,
        ..Counts::default()
    };
    // The incident: both pumps once, one pair.
    assert_eq!(counted_restarts(&c(0, 1), &c(0, 1), (1, 1), 1), 1);
    assert_eq!(counted_restarts(&c(0, 1), &c(0, 1), (1, 1), 0), 2);
    // A pair, yet one pump's restart not counted: no credit.
    assert_eq!(counted_restarts(&c(0, 0), &c(0, 1), (1, 1), 1), 1);
    assert_eq!(counted_restarts(&c(0, 1), &c(0, 0), (1, 1), 1), 1);
    // More re-bootstrap wakes in the window than counted restarts, either
    // peer (a count after the last read): no credit.
    assert_eq!(counted_restarts(&c(0, 1), &c(0, 1), (2, 1), 1), 2);
    assert_eq!(counted_restarts(&c(0, 1), &c(0, 1), (1, 2), 1), 2);
    // Fewer wakes than restarts (a wake lost from the recent list): credit.
    assert_eq!(counted_restarts(&c(0, 2), &c(0, 2), (1, 1), 1), 3);
    // The sender's TX restart is never a pair's: it still counts.
    assert_eq!(counted_restarts(&c(5, 1), &c(1, 1), (1, 1), 1), 2);
    // A lone pump restart, either peer: as before.
    assert_eq!(counted_restarts(&c(0, 1), &c(0, 0), (1, 0), 0), 1);
    assert_eq!(counted_restarts(&c(0, 0), &c(0, 1), (0, 1), 0), 1);
    // Two stalls, two pairs: two restarts; never more credit than pumps.
    assert_eq!(counted_restarts(&c(0, 2), &c(0, 2), (2, 2), 2), 2);
    assert_eq!(counted_restarts(&c(0, 2), &c(0, 1), (2, 1), 2), 2);
}

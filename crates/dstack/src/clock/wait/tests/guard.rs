// SPDX-License-Identifier: MIT OR Apache-2.0

//! The no-yield tail and the give cap (dstack #351): no SwitchToThread in
//! the last [`NO_YIELD_NS`] before a due time, and no spin longer than
//! [`GIVE_CAP_NS`] without a give.

use super::*;

/// The shipped plan: a 50 ms spin tail (`clock::SPIN_TAIL_NS`), no escape.
fn shipped() -> Plan {
    Plan {
        spin_tail_ns: 50_000_000,
        escape: false,
        escape_block_ns: 500_000,
        no_yield_ns: NO_YIELD_NS,
        give_cap_ns: GIVE_CAP_NS,
    }
}

/// dasio's trace on dsg (#351): the pump yielded right up to its due time
/// and Defender kept its CPU 1.4 ms. Now each turn with 250 µs or less left
/// pauses; the turns before still yield. Each read moves the clock 50 µs.
#[test]
fn no_turn_yields_in_the_last_250_us() {
    let mut os = Fake::at(0, 50_000);
    let mut gives = Gives::new();
    until(&mut os, 600_000, shipped(), &mut gives);
    // The spin's reads at 50..=300 µs leave 550..=300 µs: yields; at
    // 350 µs exactly 250 µs left: a pause, and every turn after.
    let mut want = vec![Ev::Yield; 6];
    want.extend([Ev::Pause; 5]);
    assert_eq!(os.log, want);
    assert_eq!(gives.last, Some(350_000), "the last yield (read at 300 µs)");
}

/// A wait longer than the spin tail still blocks on the timer to its tail,
/// then yields until the no-yield tail and pauses only there.
#[test]
fn a_long_wait_blocks_to_its_tail_and_yields_before_the_no_yield_tail() {
    let mut os = Fake::at(0, 100_000);
    let plan = Plan {
        spin_tail_ns: 1_000_000,
        ..shipped()
    };
    let mut gives = Gives::new();
    until(&mut os, 2_000_000, plan, &mut gives);
    // The read at 0 leaves 2 ms: a block of 1 ms to 1.1 ms; the reads at
    // 1.1..=1.7 ms leave 900..=300 µs (yields), at 1.8 and 1.9 ms 200 and
    // 100 µs (pauses).
    let mut want = vec![Ev::Block(1_000_000)];
    want.extend([Ev::Yield; 7]);
    want.extend([Ev::Pause; 2]);
    assert_eq!(os.log, want);
}

/// A wait that starts inside the no-yield tail would give nothing: when it
/// ends a cap or more after the thread's last give it goes on the timer for
/// its whole length (the timer's expiry preempts a lower priority at once,
/// a bounded late wake); a nanosecond short of the cap it spins, pausing.
#[test]
fn a_wait_that_would_end_a_cap_past_the_last_give_goes_on_the_timer() {
    // Each read moves the clock 10 µs: the spin's first read at 10 µs
    // leaves 90 µs, and the app's clock then reads 20 µs, so the wait ends
    // (at 110 µs on the app's clock) exactly a cap after this give.
    let mut os = Fake::at(0, 10_000);
    let mut gives = Gives::new();
    gives.gave(110_000 - GIVE_CAP_NS);
    until(&mut os, 100_000, shipped(), &mut gives);
    assert_eq!(os.log, [Ev::Block(90_000)]);
    assert_eq!(gives.last, Some(110_000), "the block is a give");

    // One nanosecond short of the cap: the spin, all pauses.
    let mut os = Fake::at(0, 10_000);
    let mut gives = Gives::new();
    gives.gave(110_000 - GIVE_CAP_NS + 1);
    until(&mut os, 100_000, shipped(), &mut gives);
    assert_eq!(os.log, [Ev::Pause; 9]);
    assert_eq!(
        gives.last,
        Some(110_000 - GIVE_CAP_NS + 1),
        "a pause gives nothing"
    );
}

/// A wait that yields before the tail needs no capped block, however long
/// ago the last give was: its yields give.
#[test]
fn a_wait_with_turns_before_the_tail_yields_instead_of_blocking() {
    let mut os = Fake::at(0, 100_000);
    let mut gives = Gives::new();
    gives.gave(-50_000_000);
    until(&mut os, 400_000, shipped(), &mut gives);
    assert_eq!(os.log, [Ev::Yield, Ev::Pause, Ev::Pause]);
    assert_eq!(gives.last, Some(200_000));
}

/// The gives carry across a thread's waits on the app's clock, whatever the
/// deadline's clock reads (dstack #328: a thread's ask and its wait never mix
/// clocks): a yield, the timer to the tail and the escape's block each
/// restart the count, and the first look starts it.
#[test]
fn every_give_restarts_the_count_on_the_apps_clock() {
    let mut os = Fake::at(0, 100_000);
    os.app_offset = 5_000_000_000_000;
    let mut gives = Gives::new();
    gives.gave(os.app_offset - 50_000_000);
    // A yield at 200 µs (the app's clock less its offset): the last give.
    until(&mut os, 400_000, shipped(), &mut gives);
    assert_eq!(os.log, [Ev::Yield, Ev::Pause, Ev::Pause]);
    assert_eq!(gives.last, Some(os.app_offset + 200_000));
    // A wait inside the tail ending 9.6 ms after that yield: spun.
    os.now = 9_500_000;
    os.log.clear();
    until(&mut os, 9_700_000, shipped(), &mut gives);
    assert_eq!(os.log, [Ev::Pause]);
    // One ending 10 ms after it (its first look at 10.1 ms, 100 µs
    // left): on the timer.
    os.now = 9_900_000;
    os.log.clear();
    until(&mut os, 10_100_000, shipped(), &mut gives);
    assert_eq!(os.log, [Ev::Block(100_000)]);

    // The timer to the tail gives: a spin right after it is not capped.
    let mut os = Fake::at(0, 100_000);
    let mut gives = Gives::new();
    gives.gave(-50_000_000);
    let plan = Plan {
        spin_tail_ns: 200_000,
        ..shipped()
    };
    until(&mut os, 1_000_000, plan, &mut gives);
    assert_eq!(os.log, [Ev::Block(800_000), Ev::Pause]);
    assert_eq!(gives.last, Some(900_000));

    // The escape's block gives (and its spin pauses, uncapped).
    let mut os = Fake::at(0, 100_000);
    os.escape = true;
    let mut gives = Gives::new();
    gives.gave(-50_000_000);
    let plan = Plan {
        escape: true,
        ..shipped()
    };
    until(&mut os, 700_000, plan, &mut gives);
    assert_eq!(os.log, [Ev::Escape, Ev::Block(500_000), Ev::Pause]);
    assert_eq!(gives.last, Some(500_000));

    // A fresh thread's first look starts the count: no block.
    let mut os = Fake::at(0, 100_000);
    let mut gives = Gives::new();
    until(&mut os, 200_000, shipped(), &mut gives);
    assert_eq!(os.log, [Ev::Pause]);
    assert_eq!(gives.last, Some(200_000));
}

/// Under the bounded escape the spin pauses and the cap does not apply (its
/// once-a-period block is its give): a wait inside the tail long after the
/// last give still spins.
#[test]
fn the_escape_spin_is_not_capped() {
    let mut os = Fake::at(0, 100_000);
    let mut gives = Gives::new();
    gives.gave(-50_000_000);
    let plan = Plan {
        escape: true,
        ..shipped()
    };
    until(&mut os, 400_000, plan, &mut gives);
    assert_eq!(os.log, [Ev::Escape, Ev::Pause, Ev::Pause, Ev::Pause]);
}

/// The shipped numbers (#351): the no-yield tail leaves a yield in every
/// period of the tightest pump (block 32 at 96 kHz, 333 µs) and keeps the
/// trace's longest Defender slice (1416 µs) under that pump's budget (W 96
/// and block 32 at 96 kHz, 1.333 ms); the capped block's late wake fits it
/// too; the cap is under one Windows clock interval (15.625 ms).
#[test]
fn the_shipped_numbers_fit_the_tightest_pump() {
    const BUDGET_NS: i64 = 128 * 1_000_000_000 / 96_000;
    const PERIOD_NS: i64 = 32 * 1_000_000_000 / 96_000;
    const _: () = assert!(NO_YIELD_NS < PERIOD_NS);
    const _: () = assert!(1_416_000 - NO_YIELD_NS < BUDGET_NS);
    const _: () = assert!((crate::clock::TIMER_LATE_NS as i64) < BUDGET_NS);
    const _: () = assert!(GIVE_CAP_NS > 30 * PERIOD_NS && GIVE_CAP_NS < 15_625_000);
    assert_eq!((NO_YIELD_NS, GIVE_CAP_NS), (250_000, 10_000_000));
}

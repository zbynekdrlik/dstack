// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use super::*;

#[test]
fn the_mean_between_two_looks_is_the_sum_s_rise_over_the_count_s() {
    // 3 packets of 100, 200 and 600 µs since the first look: 300 µs.
    assert_eq!(
        mean_between((5_000_000, 10), (5_900_000, 13)),
        Some(300_000)
    );
    // Rounded down: 1000 ns over 3 packets.
    assert_eq!(mean_between((0, 0), (1_000, 3)), Some(333));
    // One packet: its own age; a packet of age 0: 0, a measured 0.
    assert_eq!(mean_between((7, 1), (7 + 250_000, 2)), Some(250_000));
    assert_eq!(mean_between((7, 1), (7, 2)), Some(0));
    // No packet between the looks: none, whatever the sum.
    assert_eq!(mean_between((900, 4), (900, 4)), None);
    assert_eq!(mean_between((0, 0), (0, 0)), None);
}

#[test]
fn the_mean_between_two_looks_holds_across_a_wrap_of_the_totals() {
    // The totals wrap (only in theory: AGE_CAP_NS keeps the sum far
    // below); the rises still hold.
    assert_eq!(mean_between((u64::MAX - 99, u64::MAX), (100, 1)), Some(100));
}

#[test]
fn a_packet_adds_its_age_up_to_the_cap_and_one_to_the_count() {
    assert_eq!(AGE_CAP_NS, 4_294_967_296);
    let a = Ages::default();
    assert_eq!(a.totals(), (0, 0));
    a.record(120_000);
    a.record(0);
    assert_eq!(a.totals(), (120_000, 2));
    // An age past the cap adds the cap; at the cap, the cap.
    a.record(u64::MAX);
    assert_eq!(a.totals(), (120_000 + AGE_CAP_NS, 3));
    a.record(AGE_CAP_NS);
    assert_eq!(a.totals(), (120_000 + 2 * AGE_CAP_NS, 4));
    a.record(AGE_CAP_NS - 1);
    assert_eq!(a.totals(), (120_000 + 3 * AGE_CAP_NS - 1, 5));
}

#[test]
fn each_take_keeps_the_mean_of_its_own_second() {
    let a = Ages::default();
    // Before any take, and a take of a second without a packet: none.
    assert_eq!(a.last_mean_ns(), None);
    a.take();
    assert_eq!(a.last_mean_ns(), None);
    // 100, 200 and 600 µs: 300 µs.
    for ns in [100_000, 200_000, 600_000] {
        a.record(ns);
    }
    a.take();
    assert_eq!(a.last_mean_ns(), Some(300_000));
    // Reading takes nothing.
    assert_eq!(a.last_mean_ns(), Some(300_000));
    // The next second's own packets only: 50 and 70 µs, 60 µs.
    a.record(50_000);
    a.record(70_000);
    a.take();
    assert_eq!(a.last_mean_ns(), Some(60_000));
    // A second without a packet: none again, the totals kept.
    a.take();
    assert_eq!(a.last_mean_ns(), None);
    assert_eq!(a.totals(), (1_020_000, 5));
    // A second of one packet of age 0: a measured 0.
    a.record(0);
    a.take();
    assert_eq!(a.last_mean_ns(), Some(0));
}

const MS: u64 = 1_000_000;

#[test]
fn a_kernel_stamp_gives_the_arrival_age_exactly() {
    // dstack #350: 10 ms at the read, 3 ms of it waited after the kernel
    // had it: 7 ms, whatever the bracket says.
    assert_eq!(
        arrival_age_ns(10 * MS, Some(3 * MS), Some(5 * MS), MS),
        7 * MS
    );
    assert_eq!(arrival_age_ns(10 * MS, Some(3 * MS), None, MS), 7 * MS);
    // A wake of the whole age: arrived at its stamp, 0.
    assert_eq!(arrival_age_ns(MS, Some(MS), None, MS / 2), 0);
    // A wake past the age (the wall clock stepped forward) is no arrival:
    // the bracket decides, else the age at the read.
    assert_eq!(arrival_age_ns(MS, Some(MS + 1), None, MS / 2), MS);
    assert_eq!(
        arrival_age_ns(10 * MS, Some(20 * MS), Some(2 * MS), 4 * MS),
        8 * MS
    );
}

#[test]
fn without_a_stamp_two_reads_bracket_the_arrival() {
    // The newest packet's age at the read bounds the read's shared delay.
    assert_eq!(arrival_age_ns(10 * MS, None, Some(10 * MS), MS / 2), MS / 2);
    // A packet already 8 ms old at the read before (10 at this read, the
    // reads 2 ms apart): at least 8 ms, more than the newest's 4.
    assert_eq!(arrival_age_ns(10 * MS, None, Some(2 * MS), 4 * MS), 8 * MS);
    // The bracket's floor under 0 (reads further apart than the packet
    // is old): the newest's.
    assert_eq!(arrival_age_ns(MS, None, Some(20 * MS), MS / 4), MS / 4);
    // No bracket and no stamp: the age at the read.
    assert_eq!(arrival_age_ns(10 * MS, None, None, MS), 10 * MS);
}

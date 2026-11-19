// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #447: packets read while no time is known (an app's idle drain
//! while its consumer does not pull, a clock not ready yet) are counted
//! `unplaced`, and they came: the next placed packet must not open a hole
//! across them. On dsg (19.11.2026) a driver waiting 74.5 s for its clock
//! read as six "never reached our socket" gaps, 7154848 frames in all, exactly
//! the 223589 unplaced packets of 32 frames, while songplayer-pp sent every
//! packet and dsg's keepalives kept its flow.

use super::*;

/// Ten years of a grandmaster's uptime, as a position.
const P0: u64 = 10 * 365 * 86_400 * RATE as u64;

#[test]
fn packets_read_unplaced_open_no_gap_and_a_real_hole_after_them_still_counts() {
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, false, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    intake.drain_into_rings(None);
    let t = transmitter(LOOPBACK);
    let send = |k: u64| {
        t.send_to(&packet_at(P0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    };
    send(0);
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 32, RATE)));
    // The consumer stops pulling: the idle drain reads with no time.
    for k in 1..100 {
        send(k);
    }
    settle();
    intake.drain_into_rings(None);
    // It pulls again: the next packet is placed.
    send(100);
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 32 * 101, RATE)));
    // Well past the playout: a hole would be lost by now.
    intake.drain_into_rings(Some(ns_of(P0 + 32 * 101 + RATE as u64, RATE)));
    let c = s.lane(0);
    assert_eq!(
        (c.packets, c.unplaced, c.gaps, c.gap_frames),
        (101, 99, 0, 0),
        "{c:?}"
    );
    // A packet that never came after them is still a gap: 102 is missing.
    send(101);
    send(103);
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 32 * 104, RATE)));
    intake.drain_into_rings(Some(ns_of(P0 + 32 * 104 + RATE as u64, RATE)));
    let c = s.lane(0);
    assert_eq!((c.packets, c.gaps, c.gap_frames), (103, 1, 32), "{c:?}");
}

/// dstack #447 (review): a time-base step (#379) while a flow reads
/// unplaced still starts the flow over at the next placed packet: the old
/// base's open hole is counted lost, never left open above the clock.
#[test]
fn a_clock_gone_back_across_an_unplaced_run_still_loses_the_old_base_s_holes() {
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, false, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    intake.drain_into_rings(None);
    let t = transmitter(LOOPBACK);
    let at = |pos: u64| {
        t.send_to(&packet_at(pos, 1, 32), slot_addr(&s, 0)).unwrap();
    };
    let step = crate::clock::to_samples(3_792_840_060_134_767, RATE);
    let p0 = step + 1_000_000_000;
    // The old base: 0 and 2, a hole at 1 still open.
    at(p0);
    at(p0 + 64);
    settle();
    intake.drain_into_rings(Some(ns_of(p0 + 100, RATE)));
    assert_eq!(s.lane(0).gaps, 0, "still open");
    // Read unplaced, then the clock and the stamps go back together.
    at(p0 + 96);
    settle();
    intake.drain_into_rings(None);
    let q0 = p0 - step;
    at(q0);
    settle();
    intake.drain_into_rings(Some(ns_of(q0 + 100, RATE)));
    intake.drain_into_rings(Some(ns_of(q0 + 100 + RATE as u64, RATE)));
    let c = s.lane(0);
    assert_eq!((c.packets, c.gaps, c.gap_frames), (4, 1, 32), "{c:?}");
}

/// dstack #447: a hole open before an unplaced run stays open and still
/// expires as a gap; only the run itself opens none.
#[test]
fn a_hole_open_before_an_unplaced_run_still_counts() {
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, false, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    intake.drain_into_rings(None);
    let t = transmitter(LOOPBACK);
    let send = |k: u64| {
        t.send_to(&packet_at(P0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    };
    send(0);
    send(2);
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 96, RATE)));
    for k in 3..10 {
        send(k);
    }
    settle();
    intake.drain_into_rings(None);
    send(10);
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 32 * 11, RATE)));
    intake.drain_into_rings(Some(ns_of(P0 + 32 * 11 + RATE as u64, RATE)));
    let c = s.lane(0);
    assert_eq!(
        (c.packets, c.unplaced, c.gaps, c.gap_frames),
        (10, 7, 1, 32),
        "{c:?}"
    );
}

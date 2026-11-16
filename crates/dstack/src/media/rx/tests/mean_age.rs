// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The placed packets' mean age (dstack #342) through each ring writer:
//! the RX thread's pass, the pump's drain (#326) and an app's drain
//! (#307) record it the same way.

use super::*;

/// Ten years of a grandmaster's uptime, as a position: a whole number of
/// ms at 96 kHz, so every age below is exact in ns.
const P0: u64 = 10 * 365 * 86_400 * RATE as u64;
/// Our now: P0 plus 10 ms.
const NOW: u64 = P0 + 960;

/// Slot 0 with a flow from loopback (its playout 192 samples) feeding RX
/// channel 0.
fn flow_in_slot_0(s: &RxShared) {
    publish(
        s,
        &[(0, plan(LOOPBACK, 1, true, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
}

/// Sends one one-frame packet to slot 0 for each age in `ms`, ms before
/// [`NOW`].
fn send_aged(s: &RxShared, t: &UdpSocket, ms: &[u64]) {
    for &m in ms {
        t.send_to(&packet_at(NOW - 96 * m, 1, 1), slot_addr(s, 0))
            .unwrap();
    }
}

/// The ages of 1, 2 and 6 ms (the last one late, past 192 samples, and
/// still placed): 9 ms summed over 3 packets, a mean of 3 ms.
const AGES: [u64; 3] = [1, 2, 6];

/// What the first placing writer of `AGES` leaves: the totals, the second
/// the heartbeat takes, and a later look's mean against an earlier one.
/// `place(now, n)` places what waits at PTP time `now` (`None`: the clock
/// not ready), `n` the datagrams slot 0 has read once it is done.
fn check_aged(s: &RxShared, place: &mut dyn FnMut(Option<u64>, u64)) {
    // Synthetic times against the kernel's real receive stamps: the
    // arrival is the age at the read only without a stamp (dstack #350;
    // the stamped arrival is `arrival.rs`'s).
    s.set_stamps(false);
    let t = transmitter(LOOPBACK);
    // A packet read before our clock is ready is not placed: no age.
    send_aged(s, &t, &[4]);
    settle();
    place(None, 1);
    assert_eq!(s.lane(0).unplaced, 1);
    assert_eq!(
        (s.lane(0).lateness_sum_ns, s.lane(0).lateness_count),
        (0, 0)
    );
    let before = s.lane(0);
    send_aged(s, &t, &AGES);
    settle();
    place(Some(ns_of(NOW, RATE)), 4);
    let c = s.lane(0);
    assert_eq!((c.packets, c.late), (4, 1), "{c:?}");
    assert_eq!((c.lateness_sum_ns, c.lateness_count), (9 * MS, 3), "{c:?}");
    assert_eq!(c.lateness_mean_ns_since(&before), Some(3 * MS));
    assert_eq!(c.lateness_mean_ns_since(&c), None);
    // The heartbeat's second: the mean of its own packets, read without a
    // take of its own.
    assert_eq!(s.last_lateness_mean_ns(0), None);
    s.take_max_lateness(0);
    assert_eq!(s.last_lateness_mean_ns(0), Some(3 * MS));
    assert_eq!(s.last_lateness_mean_ns(0), Some(3 * MS));
    // Its least is the judge's: the same figure.
    assert_eq!(s.last_min_lateness_ns(0), Some(MS));
    // The next second: 5 ms alone.
    send_aged(s, &t, &[5]);
    settle();
    place(Some(ns_of(NOW, RATE)), 5);
    s.take_max_lateness(0);
    assert_eq!(s.last_lateness_mean_ns(0), Some(5 * MS));
    assert_eq!(s.lane(0).lateness_mean_ns_since(&c), Some(5 * MS));
    assert_eq!(s.lane(0).lateness_mean_ns_since(&before), Some(14 * MS / 4));
    // A second without a packet: none.
    s.take_max_lateness(0);
    assert_eq!(s.last_lateness_mean_ns(0), None);
}

#[test]
fn the_rx_thread_s_pass_sums_and_counts_each_placed_packet_s_age() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    flow_in_slot_0(&s);
    check_aged(&s, &mut |now, n| pump(&mut rx, 0, n, now, 0));
}

#[test]
fn the_pump_s_drain_sums_and_counts_each_placed_packet_s_age() {
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    flow_in_slot_0(&s);
    intake.drain_into_rings(None);
    check_aged(&s, &mut |now, _| {
        intake.drain_into_rings(now);
    });
}

#[test]
fn an_app_s_drain_sums_and_counts_each_placed_packet_s_age() {
    let s = shared(1);
    let mut intake = s.app_intake().unwrap();
    flow_in_slot_0(&s);
    intake.drain_into_rings(None);
    check_aged(&s, &mut |now, _| {
        intake.drain_into_rings(now);
    });
}

#[test]
fn a_slot_s_playout_is_its_published_plan_s() {
    let s = shared(1);
    // No flow yet, and past the slots: none.
    assert_eq!(s.flow_playout(0), None);
    assert_eq!(s.flow_playout(MAX_FLOWS), None);
    flow_in_slot_0(&s);
    assert_eq!(s.flow_playout(0), Some(192));
    assert_eq!(s.flow_playout(1), None);
    // A new latency, then the flow gone.
    let mut p = plan(LOOPBACK, 1, true, 1);
    p.latency = 480;
    publish(&s, &[(0, p)], &[]);
    assert_eq!(s.flow_playout(0), Some(480));
    publish(&s, &[], &[]);
    assert_eq!(s.flow_playout(0), None);
    // A slot past the slots has no mean either.
    assert_eq!(s.last_lateness_mean_ns(MAX_FLOWS), None);
}

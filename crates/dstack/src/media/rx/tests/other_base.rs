// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #489 (strih-snv, 25.11.2026): a flow re-granted after its
//! transmitter's silence brought its first 764 packets stamped about 8.6
//! days behind our clock (all late, every write refused by the ring), then
//! packets on our base. The first gap of the new flow read 71074092504
//! frames lost (`rx_loss`, `gaps=1`), the distance between the two bases.
//! A packet a whole ring or more behind now is another time base's, as one
//! a ring ahead is (#379): no measure of holes, and a flow whose furthest
//! end is a ring or more behind the clock (our clock stepped forward to a
//! new base) starts over.

use super::*;

/// Ten years of a grandmaster's uptime, as a position.
const P0: u64 = 10 * 365 * 86_400 * RATE as u64;
/// The field's distance between the two bases, frames (`frames=` of the
/// `RX packets missing from a flow` line).
const FIELD_JUMP: u64 = 71_074_092_504;

#[test]
fn a_new_flow_stamped_days_behind_our_clock_then_on_it_loses_nothing() {
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
    // Our clock at P0; the new flow's first packets 8.6 days behind it.
    let old = P0 - FIELD_JUMP;
    for k in 0..4 {
        at(old + 32 * k);
    }
    settle();
    intake.drain_into_rings(Some(ns_of(P0, RATE)));
    // Then on our base, in order, one missing (packet 2).
    for k in [0, 1, 3] {
        at(P0 + 32 * k);
    }
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 128, RATE)));
    // Well past the playout: every open hole is judged by now.
    intake.drain_into_rings(Some(ns_of(P0 + 128 + RATE as u64, RATE)));
    let c = s.lane(0);
    assert_eq!(
        (c.packets, c.late, c.gaps, c.gap_frames),
        (7, 4, 1, 32),
        "{c:?}"
    );
    // The other base's packets measure no age or network latency: the
    // peaks and the ages are our base's packets' alone (read within 128
    // samples plus the 30 ms the test leaves them in the socket).
    let bound = 128 + 3 * 960;
    assert!(
        c.lateness_peak <= bound && c.lateness_at_read_peak <= 128,
        "{c:?}"
    );
    assert_eq!((c.wire_late, c.lateness_count), (0, 3), "{c:?}");
}

/// dstack #489 (review): a flow silent for more than a ring on our base,
/// with the reader's clock running through the silence, still loses the
/// packets the silence took: its next packet's hole is counted.
#[test]
fn a_silence_past_a_ring_lived_through_on_our_base_still_counts_its_hole() {
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
    let ring = crate::media::ring::SLOTS as u64;
    at(P0);
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 32, RATE)));
    // 4464 samples (46.5 ms) more than a ring: the reader's clock runs
    // 100 ms meanwhile, so the clock did not step.
    let silence = ring + 4464;
    std::thread::sleep(Duration::from_millis(100));
    at(P0 + 32 + silence);
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 64 + silence, RATE)));
    intake.drain_into_rings(Some(ns_of(P0 + 64 + silence + RATE as u64, RATE)));
    let c = s.lane(0);
    assert_eq!((c.packets, c.gaps, c.gap_frames), (2, 1, silence), "{c:?}");
}

#[test]
fn our_clock_stepping_forward_to_a_new_base_starts_the_flow_over() {
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
    // The old base: 0 and 2, a hole at 1 still open.
    at(P0);
    at(P0 + 64);
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    assert_eq!(s.lane(0).gaps, 0, "still open");
    // The clock and the stamps step forward together by the field's
    // distance: the old base's hole is lost, the distance is not.
    let q0 = P0 + FIELD_JUMP;
    for k in [0, 1, 3] {
        at(q0 + 32 * k);
    }
    settle();
    intake.drain_into_rings(Some(ns_of(q0 + 128, RATE)));
    intake.drain_into_rings(Some(ns_of(q0 + 128 + RATE as u64, RATE)));
    let c = s.lane(0);
    assert_eq!(
        (c.packets, c.gaps, c.gap_frames),
        (5, 2, 64),
        "the old hole and the new base's missing packet: {c:?}"
    );
}

/// Within a ring behind now nothing changes: a late packet still fills
/// nothing and a jump still opens a hole that is lost.
#[test]
fn a_step_back_within_a_ring_still_counts_its_hole() {
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
    let ring = crate::media::ring::SLOTS as u64;
    // A packet one ring less 64 behind now: late, still the flow's end.
    let first = P0 - (ring - 64);
    at(first);
    settle();
    intake.drain_into_rings(Some(ns_of(P0, RATE)));
    // The next packet on time: the hole between them is lost.
    at(P0);
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 32, RATE)));
    intake.drain_into_rings(Some(ns_of(P0 + 32 + RATE as u64, RATE)));
    let c = s.lane(0);
    assert_eq!(
        (c.packets, c.late, c.gaps, c.gap_frames),
        (2, 1, 1, ring - 64 - 32),
        "{c:?}"
    );
}

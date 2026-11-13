// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #278: a packet overtaken on the way is reordered, not lost. Two
//! wire captures on stream-snv (13.11.2026, fpp 32 at 96 kHz, about 1.8 M
//! packets each) held no missing position and about 3000 reorders, the
//! late packet overtaken by at most 6 others.

use super::*;

/// A lane on slot 0 feeding RX channel 1 at a playout of `latency`
/// samples, its receiver and a transmitter.
fn rig(latency: u32) -> (Arc<RxShared>, Receiver, UdpSocket) {
    let s = shared(1);
    let rx = Receiver::new(Arc::clone(&s)).unwrap();
    let mut p = plan(LOOPBACK, 1, true, 1);
    p.latency = latency;
    publish(&s, &[(0, p)], &[(0, RxRoute { flow: 0, slot: 0 })]);
    (s, rx, transmitter(LOOPBACK))
}

/// The captured shape: `n` packets in runs of `depth + 1`, each run's
/// first packet overtaken by the `depth` after it (it comes last).
fn overtaken(n: u64, depth: u64) -> Vec<u64> {
    let mut order = Vec::new();
    let mut k = 0;
    while k < n {
        let run = (depth + 1).min(n - k);
        order.extend(k + 1..k + run);
        order.push(k);
        k += run;
    }
    order
}

#[test]
fn the_captured_order_is_each_run_s_first_packet_last() {
    assert_eq!(overtaken(6, 1), [1, 0, 3, 2, 5, 4]);
    assert_eq!(overtaken(7, 2), [1, 2, 0, 4, 5, 3, 6]);
}

/// Reorders of depth 1 to 6, as captured: no gap, every frame in the ring.
#[test]
fn a_reordered_packet_is_no_gap_and_every_frame_is_placed() {
    for depth in 1..=6u64 {
        let (s, mut rx, t) = rig(96_000);
        let p0 = 1_000_000_000u64;
        let n = 4 * (depth + 1);
        for k in overtaken(n, depth) {
            t.send_to(&packet_at(p0 + 32 * k, 1, 32), slot_addr(&s, 0))
                .unwrap();
        }
        let now = ns_of(p0 + 32 * n + 10, RATE);
        pump(&mut rx, 0, n, Some(now), 0);
        let c = s.lane(0);
        assert_eq!(
            (c.packets, c.gaps, c.gap_frames, c.out_of_order),
            (n, 0, 0, 4),
            "depth {depth}: {c:?}"
        );
        let mut got = vec![0i32; (32 * n) as usize];
        assert_eq!(s.rings()[0].read_at(p0, &mut got).holes, 0, "depth {depth}");
        let want: Vec<i32> = (0..32 * n).map(|f| value(p0 + f, 0)).collect();
        assert_eq!(got, want, "depth {depth}");
    }
}

/// A hole no packet fills is a gap once it is behind the playout point
/// (the flow's latency plus the block), not before.
#[test]
fn a_real_hole_still_counts_once_it_is_behind_the_playout() {
    let (s, mut rx, t) = rig(192);
    let p0 = 1_000_000_000u64;
    // Packets 0, 1, 3, 4: 2 (frames 64..96) never comes.
    for k in [0, 1, 3, 4] {
        t.send_to(&packet_at(p0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    }
    pump(&mut rx, 0, 4, Some(ns_of(p0 + 160, RATE)), 0);
    assert_eq!((s.lane(0).gaps, s.lane(0).gap_frames), (0, 0), "still open");
    // The hole ends at 96; the pump reads it at 96 + 192.
    rx.pass(Some(ns_of(p0 + 96 + 192 - 1, RATE)), 0);
    assert_eq!(s.lane(0).gaps, 0, "a sample before the playout");
    rx.pass(Some(ns_of(p0 + 96 + 192, RATE)), 0);
    let c = s.lane(0);
    assert_eq!((c.gaps, c.gap_frames, c.out_of_order), (1, 32, 0), "{c:?}");
    // Counted once.
    rx.pass(Some(ns_of(p0 + 1_000, RATE)), 0);
    assert_eq!((s.lane(0).gaps, s.lane(0).gap_frames), (1, 32));
}

/// A packet that fills a hole is reordered; a repeat of a packet already
/// placed is out of order but no reorder.
#[test]
fn a_filled_hole_counts_reordered_and_a_repeat_does_not() {
    let (s, mut rx, t) = rig(96_000);
    let p0 = 1_000_000_000u64;
    for k in [0, 2, 1, 1, 0] {
        t.send_to(&packet_at(p0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    }
    pump(&mut rx, 0, 5, Some(ns_of(p0 + 200, RATE)), 0);
    let c = s.lane(0);
    assert_eq!(
        (c.out_of_order, c.reordered, c.gaps, c.gap_frames),
        (3, 1, 0, 0),
        "{c:?}"
    );
}

/// A part of a hole a reordered packet left open still counts once behind
/// the playout point, with only its own frames.
#[test]
fn what_a_reordered_packet_left_of_a_hole_is_lost_alone() {
    let (s, mut rx, t) = rig(192);
    let p0 = 1_000_000_000u64;
    // 0, then 3 (64 frames missing), then 1: frames 64..96 stay open.
    for k in [0, 3, 1] {
        t.send_to(&packet_at(p0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    }
    pump(&mut rx, 0, 3, Some(ns_of(p0 + 130, RATE)), 0);
    assert_eq!((s.lane(0).reordered, s.lane(0).gaps), (1, 0));
    rx.pass(Some(ns_of(p0 + 96 + 192, RATE)), 0);
    let c = s.lane(0);
    assert_eq!((c.gaps, c.gap_frames), (1, 32), "{c:?}");
}

/// A new flow in the slot: the old flow's open holes never fill, so they
/// are lost.
#[test]
fn a_new_flow_loses_the_old_flow_s_open_holes() {
    let (s, mut rx, t) = rig(96_000);
    let p0 = 1_000_000_000u64;
    for k in [0, 2] {
        t.send_to(&packet_at(p0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    }
    pump(&mut rx, 0, 2, Some(ns_of(p0 + 100, RATE)), 0);
    assert_eq!(s.lane(0).gaps, 0);
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, true, 2))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    rx.pass(Some(ns_of(p0 + 100, RATE)), 0);
    let c = s.lane(0);
    assert_eq!((c.gaps, c.gap_frames), (1, 32), "{c:?}");
}

/// The minute watch names a reordering flow at INFO, "not lost", and the
/// loss watch stays quiet (dstack #278).
#[test]
fn the_minute_watch_tells_reorders_and_the_loss_watch_stays_quiet() {
    use crate::media::late::{EVERY_NS, MINUTE_NS, RxMinute, RxWatch};
    let (s, mut rx, t) = rig(96_000);
    let p0 = 1_000_000_000u64;
    let named = |slot: usize| (slot == 0).then(|| "FOH-Card".to_string());
    let mut minute = RxMinute::new();
    let mut loss = RxWatch::new();
    let info = |f: &mut dyn FnMut()| crate::test_log::capture_at(tracing::Level::INFO, f);
    assert_eq!(info(&mut || minute.watch(0, &s, None, named)), "");
    loss.watch(0, &s, named);
    for k in overtaken(8, 3) {
        t.send_to(&packet_at(p0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    }
    pump(&mut rx, 0, 8, Some(ns_of(p0 + 300, RATE)), 0);
    // The flow's first packet (1) opens no hole before it, so 0 is out of
    // order but no reorder; 4 filled its hole.
    assert_eq!((s.lane(0).out_of_order, s.lane(0).reordered), (2, 1));
    assert_eq!(info(&mut || loss.watch(EVERY_NS, &s, named)), "");
    assert_eq!(
        info(&mut || minute.watch(MINUTE_NS - 1, &s, None, named)),
        ""
    );
    let all = info(&mut || minute.watch(MINUTE_NS, &s, None, named));
    // The minute's timing line (dstack #234, Linux) comes beside it.
    let lines: Vec<&str> = all.lines().filter(|l| l.contains("reordered")).collect();
    assert_eq!(lines.len(), 1, "{all}");
    let log = lines[0];
    assert!(log.contains(" INFO "), "{log}");
    assert!(
        log.contains(&format!(
            "device=FOH-Card source={} slot=0 reordered=1",
            v4(&t)
        )),
        "{log}"
    );
    assert!(
        log.contains("reordered on the way in the last minute, not lost"),
        "{log}"
    );
    assert_eq!(
        info(&mut || minute.watch(2 * MINUTE_NS, &s, None, named)),
        ""
    );
}

/// The playout point counts the flow's block too: a hole is lost at its
/// end plus latency plus block, not at latency alone.
#[test]
fn a_hole_is_lost_at_the_latency_plus_the_block() {
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    let mut p = plan(LOOPBACK, 1, true, 1);
    p.latency = 192;
    p.block = 32;
    publish(&s, &[(0, p)], &[(0, RxRoute { flow: 0, slot: 0 })]);
    let t = transmitter(LOOPBACK);
    let p0 = 1_000_000_000u64;
    for k in [0, 1, 3] {
        t.send_to(&packet_at(p0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    }
    pump(&mut rx, 0, 3, Some(ns_of(p0 + 130, RATE)), 0);
    rx.pass(Some(ns_of(p0 + 96 + 192 + 32 - 1, RATE)), 0);
    assert_eq!(s.lane(0).gaps, 0, "a sample before latency + block");
    rx.pass(Some(ns_of(p0 + 96 + 192 + 32, RATE)), 0);
    assert_eq!((s.lane(0).gaps, s.lane(0).gap_frames), (1, 32));
}

/// A filler that comes after its hole fell behind the playout is late:
/// the hole is a gap, the filler no reorder (dstack #278 review).
#[test]
fn a_filler_after_the_playout_is_late_and_its_hole_a_gap() {
    let (s, mut rx, t) = rig(192);
    let p0 = 1_000_000_000u64;
    for k in [0, 1, 3] {
        t.send_to(&packet_at(p0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    }
    pump(&mut rx, 0, 3, Some(ns_of(p0 + 130, RATE)), 0);
    assert_eq!(s.lane(0).gaps, 0);
    // Packet 2 comes once the pump read past its frames, in the same pass
    // that first sees that time.
    t.send_to(&packet_at(p0 + 64, 1, 32), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 4, Some(ns_of(p0 + 96 + 192 + 10, RATE)), 0);
    let c = s.lane(0);
    assert_eq!(
        (c.gaps, c.gap_frames, c.reordered, c.late, c.out_of_order),
        (1, 32, 0, 1, 1),
        "{c:?}"
    );
}

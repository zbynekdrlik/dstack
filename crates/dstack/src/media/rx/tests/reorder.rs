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

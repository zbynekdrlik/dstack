// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #234: the holes a flow's channels read before its first packet
//! are start holes, apart from the holes of a running flow (the editing
//! PC's OBS device, 13.11.2026: 3324 holes in its first 26 s, then a few
//! stalls an hour); and the minute's holes line gives each stall its time.

use super::*;
use crate::media::ring::{Counters, Read};

const T: u64 = 1_000_000_000;
const N: usize = 16;

fn read(s: &RxShared, rx: usize, pos: u64) -> Read {
    s.rings()[rx].read_at(pos, &mut [0i32; N])
}

fn holes(n: usize) -> Read {
    Read {
        holes: n,
        start_holes: 0,
    }
}

fn start_holes(n: usize) -> Read {
    Read {
        holes: 0,
        start_holes: n,
    }
}

#[test]
fn a_new_flow_is_another_transmitter_or_generation_only() {
    let a = plan(Ipv4Addr::new(10, 0, 0, 1), 2, true, 1);
    let b = plan(Ipv4Addr::new(10, 0, 0, 2), 2, true, 1);
    assert!(new_flow(RxPlan::default(), a), "the first flow");
    assert!(new_flow(a, b), "another transmitter");
    assert!(new_flow(a, plan(Ipv4Addr::new(10, 0, 0, 1), 2, true, 2)));
    // The same flow granted, or at another latency: not new.
    assert!(!new_flow(a, plan(Ipv4Addr::new(10, 0, 0, 1), 2, false, 1)));
    assert!(!new_flow(a, RxPlan { latency: 500, ..a }));
    assert!(!new_flow(a, a));
    // No flow is no new flow.
    assert!(!new_flow(a, RxPlan::default()));
}

/// A publish restarts the rings of a slot whose flow is new, and of a
/// channel routed anew; a plan change of the same flow, or another slot's
/// new flow, leaves them running.
#[test]
fn a_publish_restarts_the_rings_a_new_flow_feeds() {
    let s = shared(3);
    let src = Ipv4Addr::new(10, 0, 0, 1);
    let routes = [
        (0, RxRoute { flow: 0, slot: 0 }),
        (1, RxRoute { flow: 1, slot: 0 }),
    ];
    publish(&s, &[(0, plan(src, 1, true, 1))], &routes);
    // Before the flow's first sample: start holes.
    assert_eq!(read(&s, 0, T), start_holes(N));
    s.rings()[0].write_at(T + 32, &[1 << 8; N]);
    s.rings()[1].write_at(T + 32, &[1 << 8; N]);
    assert_eq!(read(&s, 0, T + 16), start_holes(N), "before its first");
    assert_eq!(read(&s, 0, T + 48), holes(N), "a running flow's hole");
    // The same flow granted no longer, another slot's new flow: no
    // restart.
    publish(
        &s,
        &[(0, plan(src, 1, false, 1)), (1, plan(src, 1, true, 7))],
        &routes,
    );
    assert_eq!(read(&s, 0, T + 64), holes(N));
    // Slot 1's flow is new: its channel restarts.
    assert_eq!(read(&s, 1, T + 64), start_holes(N));
    // Another flow in slot 0: its channel restarts, with no route change.
    publish(
        &s,
        &[(0, plan(src, 1, true, 2)), (1, plan(src, 1, true, 7))],
        &routes,
    );
    assert_eq!(read(&s, 0, T + 80), start_holes(N));
    s.rings()[0].write_at(T + 112, &[1 << 8; N]);
    assert_eq!(read(&s, 0, T + 96), start_holes(N));
    assert_eq!(read(&s, 0, T + 128), holes(N));
    // RX 3 was written while unrouted; routed to slot 0 now, it restarts.
    s.rings()[2].write_at(T, &[1 << 8; N]);
    let joined = [
        (0, RxRoute { flow: 0, slot: 0 }),
        (1, RxRoute { flow: 1, slot: 0 }),
        (2, RxRoute { flow: 0, slot: 0 }),
    ];
    publish(
        &s,
        &[(0, plan(src, 1, true, 2)), (1, plan(src, 1, true, 7))],
        &joined,
    );
    assert_eq!(read(&s, 2, T + 32), start_holes(N));
    assert_eq!(read(&s, 0, T + 144), holes(N), "its route did not change");
}

/// A flow's ring counts are its routed channels' rings, summed.
#[test]
fn a_flow_s_ring_counts_are_its_channels_summed() {
    let s = shared(3);
    publish(
        &s,
        &[(0, plan(LOOPBACK, 2, true, 1))],
        &[
            (0, RxRoute { flow: 0, slot: 0 }),
            (2, RxRoute { flow: 0, slot: 1 }),
            (1, RxRoute { flow: 1, slot: 0 }),
        ],
    );
    let mut out = [0i32; N];
    for (rx, extra) in [(0, 0), (1, 3), (2, 5)] {
        let ring = &s.rings()[rx];
        ring.read_at(T, &mut out[..1 + extra]);
        ring.write_at(T + 64, &[1 << 8; 4]);
        ring.write_at(T + 64, &[1 << 8; 2]);
        ring.write_at(T + 200, &[1 << 8; 1]);
        ring.read_at(T + 70, &mut out[..3 + extra]);
        ring.write_at(T, &[1 << 8; 7]);
        ring.write_at(T + 70 + 3 * SLOTS_AHEAD, &[1 << 8; 9]);
    }
    let sum = |a: Counters, b: Counters| {
        let mut c = a;
        c += b;
        c
    };
    let c0 = s.rings()[0].counters();
    let c2 = s.rings()[2].counters();
    assert_ne!(c0, c2);
    assert_eq!(s.flow_rings(0), sum(c0, c2));
    assert_eq!(s.flow_rings(1), s.rings()[1].counters());
    assert_eq!(s.flow_rings(2), Counters::default());
}

/// Far enough ahead to be early: a whole ring.
const SLOTS_AHEAD: u64 = crate::media::ring::SLOTS as u64;

/// Once a minute, each flow whose channels read holes gets one INFO line of
/// the minute's holes, written-late samples, late packets and the engine's
/// re-bootstraps; start holes and a quiet minute get none.
#[test]
fn the_minute_watch_logs_each_flow_s_holes_with_their_minute() {
    use crate::media::late::{MINUTE_NS, RxMinute};
    let s = shared(2);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 2, true, 1))],
        &[
            (0, RxRoute { flow: 0, slot: 0 }),
            (1, RxRoute { flow: 0, slot: 1 }),
        ],
    );
    let named = |slot: usize| (slot == 0).then(|| "Stage-Box".to_string());
    let info = |f: &mut dyn FnMut()| crate::test_log::capture_at(tracing::Level::INFO, f);
    let mut minute = RxMinute::new();
    // Before the flow's first packet: start holes only, never on a line.
    for rx in 0..2 {
        read(&s, rx, T);
    }
    // The first look learns (and an earlier hole or re-bootstrap is not
    // this minute's).
    for rx in 0..2 {
        s.rings()[rx].write_at(T + 16, &[1 << 8; N]);
    }
    read(&s, 0, T + 32);
    assert_eq!(info(&mut || minute.watch(0, &s, Some(4), named)), "");
    let t = transmitter(LOOPBACK);
    // A packet 400 samples late: past the playout (192), the pump read it.
    s.rings()[0].read_at(T + 48, &mut [0i32; 400]);
    s.rings()[1].read_at(T + 48, &mut [0i32; 400]);
    t.send_to(&packet_at(T + 100, 2, 4), slot_addr(&s, 0))
        .unwrap();
    pump(&mut rx, 0, 1, Some(ns_of(T + 500, RATE)), 0);
    let lane = s.lane(0);
    assert_eq!((lane.late, lane.late_at_read), (1, 1));
    let log = info(&mut || minute.watch(MINUTE_NS, &s, Some(6), named));
    let lines: Vec<&str> = log.lines().filter(|l| l.contains("RX holes")).collect();
    assert_eq!(lines.len(), 1, "{log}");
    assert!(lines[0].contains(" INFO "), "{log}");
    assert!(lines[0].contains("dstack::media::late"), "{log}");
    assert!(
        lines[0].contains(&format!(
            "RX holes in the last minute device=Stage-Box source={} slot=0 holes=800 \
             written_late=8 rx_late=1 rebootstraps=2",
            SocketAddrV4::new(LOOPBACK, t.local_addr().unwrap().port())
        )),
        "{log}"
    );
    // A minute with no hole: no line, whatever else rose.
    s.rings()[0].write_at(T + 1_000, &[1 << 8; N]);
    let log = info(&mut || minute.watch(2 * MINUTE_NS, &s, Some(9), named));
    assert!(!log.contains("RX holes"), "{log}");
    // Another flow in the slot (its source unknown until a packet comes):
    // its start holes are on no line, its holes are. Without an engine the
    // line says 0 re-bootstraps, and an unnamed device reads `-`.
    publish(
        &s,
        &[(0, plan(LOOPBACK, 2, true, 2))],
        &[
            (0, RxRoute { flow: 0, slot: 0 }),
            (1, RxRoute { flow: 0, slot: 1 }),
        ],
    );
    rx.pass(None, 0);
    assert_eq!(s.source(0), None);
    assert_eq!(read(&s, 1, T + 2_000), start_holes(N));
    s.rings()[1].write_at(T + 3_000, &[1 << 8; N]);
    s.rings()[1].read_at(T + 3_016, &mut [0i32; 5]);
    let log = info(&mut || minute.watch(3 * MINUTE_NS, &s, None, |_| None));
    assert!(
        log.contains(
            "RX holes in the last minute device=- source=- slot=0 holes=5 written_late=0 \
             rx_late=0 rebootstraps=0"
        ),
        "{log}"
    );
}

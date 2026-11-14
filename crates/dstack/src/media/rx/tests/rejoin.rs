// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #282: the RX multicast flows' groups joined again after an
//! interface change.

use super::*;

/// dstack #282: an interface change asks every slot with a group to join
/// it again (the membership went with the address); a slot without one
/// joins nothing.
#[test]
fn a_rejoin_asked_joins_every_slot_s_group_again_once() {
    let group = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 3), free_port());
    let s = shared(2);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    let multicast = RxPlan {
        group: Some(group),
        ..plan(LOOPBACK, 2, false, 1)
    };
    publish(
        &s,
        &[(3, multicast), (1, plan(LOOPBACK, 2, false, 1))],
        &[
            (0, RxRoute { flow: 3, slot: 0 }),
            (1, RxRoute { flow: 3, slot: 1 }),
        ],
    );
    rx.pass(None, 0);
    assert_eq!((s.lane(3).joins, s.lane(3).join_errors), (1, 0));
    // Nothing asked: no join.
    rx.pass(None, 0);
    assert_eq!(s.lane(3).joins, 1);
    s.rejoin_groups();
    rx.pass(None, 0);
    assert_eq!((s.lane(3).joins, s.lane(3).join_errors), (2, 0));
    assert_eq!(s.lane(1).joins, 0, "a slot with no group joins none");
    // Asked once, done once.
    rx.pass(None, 0);
    assert_eq!(s.lane(3).joins, 2);
    // Twice more: joined twice more.
    s.rejoin_groups();
    rx.pass(None, 0);
    s.rejoin_groups();
    rx.pass(None, 0);
    assert_eq!(s.lane(3).joins, 4);
    // The rejoined socket receives the flow.
    let t = multicast_transmitter();
    let p0 = 10 * 365 * 86_400 * u64::from(RATE);
    for k in 0..2 {
        t.send_to(&packet_at(p0 + 32 * k, 2, 32), group).unwrap();
    }
    pump(&mut rx, 3, 2, Some(ns_of(p0 + 100, RATE)), 0);
    let c = s.lane(3);
    assert_eq!((c.packets, c.stray), (2, 0), "{c:?}");
}

/// A rejoin asked in the pass where a slot's plan names a new group: the
/// new group is joined once (the two asks are one join).
#[test]
fn a_rejoin_asked_with_a_new_group_joins_the_new_group_once() {
    let first = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 4), free_port());
    let second = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 5), free_port());
    let s = shared(2);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    let at = |group, generation| RxPlan {
        group: Some(group),
        ..plan(LOOPBACK, 2, false, generation)
    };
    let routes = [
        (0, RxRoute { flow: 3, slot: 0 }),
        (1, RxRoute { flow: 3, slot: 1 }),
    ];
    publish(&s, &[(3, at(first, 1))], &routes);
    rx.pass(None, 0);
    assert_eq!(s.lane(3).joins, 1);
    publish(&s, &[(3, at(second, 2))], &routes);
    s.rejoin_groups();
    rx.pass(None, 0);
    assert_eq!((s.lane(3).joins, s.lane(3).join_errors), (2, 0));
    let t = multicast_transmitter();
    let p0 = 10 * 365 * 86_400 * u64::from(RATE);
    for k in 0..2 {
        t.send_to(&packet_at(p0 + 32 * k, 2, 32), second).unwrap();
    }
    pump(&mut rx, 3, 2, Some(ns_of(p0 + 100, RATE)), 0);
    assert_eq!(s.lane(3).packets, 2, "{:?}", s.lane(3));
}

/// A rejoin whose join fails is reported once, so the daemon asks again;
/// one that joins reports nothing.
#[test]
fn a_rejoin_that_cannot_join_a_slot_s_group_is_reported_once() {
    let s = shared(2);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    // Nothing asked yet: nothing to report.
    assert!(!s.take_groups_missing());
    let not = SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 1), 4321);
    let bad = RxPlan {
        group: Some(not),
        ..plan(LOOPBACK, 2, false, 1)
    };
    publish(&s, &[(3, bad)], &[]);
    rx.pass(None, 0);
    // A plan change's failed join alone is no rejoin to report.
    assert!(!s.take_groups_missing());
    s.rejoin_groups();
    rx.pass(None, 0);
    assert_eq!(s.lane(3).join_errors, 2);
    assert!(s.take_groups_missing());
    assert!(!s.take_groups_missing(), "reported once");
    // A good group asked again: joined, nothing to report.
    let group = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 6), free_port());
    let good = RxPlan {
        group: Some(group),
        ..plan(LOOPBACK, 2, false, 2)
    };
    publish(&s, &[(3, good)], &[]);
    rx.pass(None, 0);
    s.rejoin_groups();
    rx.pass(None, 0);
    assert_eq!(s.lane(3).joins, 2);
    assert!(!s.take_groups_missing());
}

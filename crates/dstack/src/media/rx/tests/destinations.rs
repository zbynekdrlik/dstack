// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #464: each publish leaves the (address, port) pairs its plans
//! receive on, with a generation that moves when the set changes.

use super::*;

fn group(last: u8) -> SocketAddrV4 {
    SocketAddrV4::new(Ipv4Addr::new(239, 255, 1, last), 4321)
}

fn port(s: &RxShared, slot: usize) -> u16 {
    s.local_addr(slot).unwrap().port()
}

#[test]
fn a_new_rx_side_has_no_destinations_and_generation_0() {
    let s = shared(1);
    assert_eq!(s.rx_destinations(), RxDestinations::default());
    publish(&s, &[], &[]);
    assert_eq!(s.rx_destinations().generation, 0, "no flow is no change");
}

#[test]
fn a_unicast_flow_is_our_address_and_its_slot_socket_s_port() {
    let s = shared(2);
    publish(&s, &[(3, plan(LOOPBACK, 2, false, 1))], &[]);
    let d = s.rx_destinations();
    assert_eq!(d.generation, 1);
    assert_eq!(d.pairs, [SocketAddrV4::new(LOOPBACK, port(&s, 3))]);
    assert_ne!(port(&s, 3), 0);
}

#[test]
fn a_multicast_flow_is_its_group_and_mixed_flows_give_both() {
    let s = shared(2);
    let multicast = RxPlan {
        group: Some(group(9)),
        ..plan(LOOPBACK, 2, true, 1)
    };
    publish(&s, &[(0, multicast)], &[]);
    assert_eq!(s.rx_destinations().pairs, [group(9)]);
    publish(&s, &[(0, multicast), (5, plan(LOOPBACK, 1, true, 2))], &[]);
    let d = s.rx_destinations();
    assert_eq!(d.generation, 2);
    assert_eq!(
        d.pairs,
        [SocketAddrV4::new(LOOPBACK, port(&s, 5)), group(9)]
    );
}

#[test]
fn a_plan_change_moves_the_generation_and_a_same_set_publish_does_not() {
    let s = shared(1);
    let first = plan(LOOPBACK, 2, false, 1);
    publish(&s, &[(0, first)], &[]);
    assert_eq!(s.rx_destinations().generation, 1);
    // The same flow granted, a new latency: the same pairs.
    let granted = RxPlan {
        live: true,
        latency: 480,
        ..first
    };
    publish(&s, &[(0, granted)], &[]);
    publish(&s, &[(0, granted)], &[]);
    assert_eq!(s.rx_destinations().generation, 1);
    // The flow moves to another slot: another port.
    publish(&s, &[(1, granted)], &[]);
    let d = s.rx_destinations();
    assert_eq!(d.generation, 2);
    assert_eq!(d.pairs, [SocketAddrV4::new(LOOPBACK, port(&s, 1))]);
    // Now multicast: its group replaces the slot's port.
    let moved = RxPlan {
        group: Some(group(4)),
        ..granted
    };
    publish(&s, &[(1, moved)], &[]);
    assert_eq!(
        (s.rx_destinations().generation, s.rx_destinations().pairs),
        (3, vec![group(4)])
    );
    // Every flow gone.
    publish(&s, &[], &[]);
    let d = s.rx_destinations();
    assert_eq!((d.generation, d.pairs.len()), (4, 0));
}

#[test]
fn a_slot_with_no_socket_gives_no_unicast_pair() {
    let rings: Arc<[Ring]> = vec![Ring::new()].into();
    let s = RxShared::with_slots(RATE, LOOPBACK, rings, None, 2).unwrap();
    publish(
        &s,
        &[
            (1, plan(LOOPBACK, 1, true, 1)),
            (2, plan(LOOPBACK, 1, true, 2)),
        ],
        &[],
    );
    assert_eq!(
        s.rx_destinations().pairs,
        [SocketAddrV4::new(LOOPBACK, port(&s, 1))]
    );
}

#[test]
fn a_changed_set_logs_one_line_with_its_generation() {
    let s = shared(1);
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        publish(&s, &[(0, plan(LOOPBACK, 1, true, 1))], &[]);
        publish(&s, &[(0, plan(LOOPBACK, 1, true, 1))], &[]);
    });
    assert_eq!(
        log.matches("RX flow destinations changed").count(),
        1,
        "{log}"
    );
    assert!(log.contains("generation=1 count=1"), "{log}");
}

//! The engine's pump as the rings' writer (dstack #326): the claim, the
//! drain, and the RX thread's part beside it (groups, keepalives, the
//! slots without a flow).

use super::*;
use crate::media::rx::intake::{PUMP_DRAIN, discard, pick};

/// Ten years of a grandmaster's uptime, as a position.
const P0: u64 = 10 * 365 * 86_400 * RATE as u64;

/// Waits until a datagram sent is in its socket (or group's) for a read.
fn settle() {
    std::thread::sleep(Duration::from_millis(30));
}

#[test]
fn the_first_claim_of_the_rings_holds_for_good() {
    // The pump first: the RX thread then never writes, and a second pump
    // gets nothing.
    let s = shared(1);
    assert!(!s.pump_writes());
    let intake = s.pump_intake();
    assert!(intake.is_some());
    assert!(s.pump_writes());
    assert!(s.pump_intake().is_none(), "one writer");
    assert!(!s.thread_writes());
    let rx = Receiver::new(Arc::clone(&s)).unwrap();
    assert!(rx.intake.is_none());
    assert!(rx.lanes().is_empty());
    // The RX thread first: it writes; neither a second receiver nor the
    // pump does.
    let s = shared(1);
    let rx = Receiver::new(Arc::clone(&s)).unwrap();
    assert!(rx.intake.is_some());
    assert!(!s.pump_writes());
    let second = Receiver::new(Arc::clone(&s)).unwrap();
    assert!(second.intake.is_none(), "one writer");
    assert!(s.pump_intake().is_none());
    assert!(!s.pump_writes());
}

#[test]
fn the_pump_reads_a_slot_s_own_socket_or_its_group_s_when_it_is_the_plan_s() {
    let a = SocketAddrV4::new(Ipv4Addr::new(239, 1, 2, 3), 4321);
    let b = SocketAddrV4::new(Ipv4Addr::new(239, 1, 2, 4), 4321);
    let word = |g| counts::source_word(Some(g));
    // No group: the own socket, whatever the RX thread still holds.
    assert_eq!(pick(None, 0, 0, Some(5)), Some(5));
    assert_eq!(pick(None, 9, word(a), Some(5)), Some(5));
    // The plan's group, open: its socket.
    assert_eq!(pick(Some(a), 9, word(a), Some(5)), Some(9));
    // No group socket (the join failed or is still to come): the own one.
    assert_eq!(pick(Some(a), 0, 0, Some(5)), Some(5));
    // Another group's socket (not regrouped yet): nothing.
    assert_eq!(pick(Some(b), 9, word(a), Some(5)), None);
    assert_eq!(pick(Some(a), 9, 0, Some(5)), None);
}

#[test]
fn a_discard_drops_and_counts_until_the_socket_is_empty_or_the_cap() {
    let s = shared(1);
    let stats = &s.lanes[0];
    let mut buf = [0u8; 64];
    let mut left = 3;
    let read = |_: &mut [u8]| {
        if left == 0 {
            return Err(io::Error::from(ErrorKind::WouldBlock));
        }
        left -= 1;
        Ok(socket::Received {
            len: 1,
            from: None,
            at_ns: None,
        })
    };
    assert_eq!(discard(stats, &mut buf, 10, read), 3);
    assert_eq!((s.lane(0).stray, s.lane(0).recv_errors), (3, 0));
    let any = || {
        Ok(socket::Received {
            len: 1,
            from: None,
            at_ns: None,
        })
    };
    assert_eq!(discard(stats, &mut buf, 2, |_| any()), 2, "the cap");
    assert_eq!(s.lane(0).stray, 5);
    let broken = |_: &mut [u8]| Err(io::Error::from(ErrorKind::ConnectionRefused));
    assert_eq!(discard(stats, &mut buf, 2, broken), 0);
    assert_eq!((s.lane(0).stray, s.lane(0).recv_errors), (5, 1));
}

#[test]
fn under_the_pump_the_rx_thread_reads_no_flow_socket_and_the_drain_does() {
    let s = shared(2);
    let mut intake = s.pump_intake().unwrap();
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 2, false, 1))],
        &[(1, RxRoute { flow: 0, slot: 1 })],
    );
    intake.drain_into_rings(None);
    let t = transmitter(LOOPBACK);
    for k in 0..3 {
        t.send_to(&packet_at(P0 + 32 * k, 2, 32), slot_addr(&s, 0))
            .unwrap();
    }
    // And two to a slot without a flow.
    for _ in 0..2 {
        t.send_to(&packet_at(P0, 2, 32), slot_addr(&s, 5)).unwrap();
    }
    settle();
    rx.wait(Duration::from_millis(10)).unwrap();
    assert_eq!(rx.ready, 0, "no flow socket polled");
    rx.pass(Some(ns_of(P0 + 100, RATE)), 0);
    assert_eq!(read_of(s.lane(0)), 0, "the RX thread read no flow");
    // It empties the slot without a flow: strays.
    let c = s.lane(5);
    assert_eq!((c.stray, c.packets), (2, 0), "{c:?}");
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    let c = s.lane(0);
    assert_eq!((c.packets, c.stray, c.late), (3, 0, 0), "{c:?}");
    let mut got = [0i32; 96];
    assert_eq!(s.rings()[1].read_at(P0, &mut got).holes, 0);
    assert_eq!(got[40], value(P0 + 40, 1));
    // An empty socket: the drain returns at once, nothing counted.
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    assert_eq!(s.lane(0).packets, 3);
    assert_eq!(s.lane(0).recv_errors, 0);
}

#[test]
fn a_new_flow_under_the_pump_starts_past_what_waited_before_it() {
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    let t = transmitter(LOOPBACK);
    // An old flow's datagrams, still in the slot's socket.
    for k in 0..4 {
        t.send_to(&packet_at(P0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    }
    settle();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, false, 7))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    intake.drain_into_rings(Some(ns_of(P0 + 10_000, RATE)));
    let c = s.lane(0);
    assert_eq!((c.packets, c.stray, c.late), (0, 4, 0), "{c:?}");
    // The flow's own packets after it: read.
    t.send_to(&packet_at(P0 + 9_900, 1, 32), slot_addr(&s, 0))
        .unwrap();
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 10_000, RATE)));
    let c = s.lane(0);
    assert_eq!((c.packets, c.stray, c.late), (1, 4, 0), "{c:?}");
    // Another plan of the same flow (granted now): nothing dropped.
    t.send_to(&packet_at(P0 + 9_932, 1, 32), slot_addr(&s, 0))
        .unwrap();
    settle();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, true, 7))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    intake.drain_into_rings(Some(ns_of(P0 + 10_000, RATE)));
    assert_eq!((s.lane(0).packets, s.lane(0).stray), (2, 4));
}

#[test]
fn one_drain_reads_more_than_a_pass_of_the_rx_thread() {
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, false, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    intake.drain_into_rings(None);
    let t = transmitter(LOOPBACK);
    let n = DRAIN as u64 + 26;
    assert!(n < PUMP_DRAIN as u64);
    for k in 0..n {
        t.send_to(&packet_at(P0 + 32 * k, 1, 32), slot_addr(&s, 0))
            .unwrap();
    }
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 32 * n, RATE)));
    assert_eq!(s.lane(0).packets, n);
}

#[test]
fn a_drain_before_our_clock_is_ready_counts_its_packets_unplaced() {
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    publish(
        &s,
        &[(0, plan(LOOPBACK, 1, false, 1))],
        &[(0, RxRoute { flow: 0, slot: 0 })],
    );
    intake.drain_into_rings(None);
    let t = transmitter(LOOPBACK);
    t.send_to(&packet_at(P0, 1, 32), slot_addr(&s, 0)).unwrap();
    settle();
    intake.drain_into_rings(None);
    let c = s.lane(0);
    assert_eq!((c.packets, c.unplaced), (1, 1), "{c:?}");
}

#[test]
fn the_keepalives_follow_the_writer_s_packets_and_the_plan_s_transmitter() {
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    let live = plan(LOOPBACK, 1, true, 1);
    publish(&s, &[(0, live)], &[]);
    intake.drain_into_rings(None);
    let t = transmitter(LOOPBACK);
    // No packet yet: no keepalive, however long.
    rx.pass(None, 0);
    rx.pass(None, KEEPALIVE_NS * 2);
    assert_eq!(s.lane(0).keepalives, 0);
    t.send_to(&packet_at(P0, 1, 32), slot_addr(&s, 0)).unwrap();
    settle();
    let heard = s.now_ns();
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    // The schedule starts at the RX thread's pass after the packet.
    rx.pass(None, heard);
    assert_eq!(s.lane(0).keepalives, 0);
    rx.pass(None, heard + KEEPALIVE_NS);
    assert_eq!(s.lane(0).keepalives, 1);
    let mut buf = [0u8; 8];
    let (n, _) = t.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], &KEEPALIVE);
    // The plan moves to another transmitter: the RX thread sees it before
    // the writer does, and sends nothing to the old one meanwhile.
    publish(
        &s,
        &[(0, plan(Ipv4Addr::new(127, 0, 0, 2), 1, true, 2))],
        &[],
    );
    rx.pass(None, heard + 2 * KEEPALIVE_NS);
    rx.pass(None, heard + 3 * KEEPALIVE_NS);
    assert_eq!(s.lane(0).keepalives, 1);
    // A packet heard longer ago than LIVE_NS keeps nothing alive.
    publish(&s, &[(0, live)], &[]);
    intake.drain_into_rings(None);
    rx.pass(None, heard + LIVE_NS);
    rx.pass(None, heard + LIVE_NS + KEEPALIVE_NS);
    assert_eq!(s.lane(0).keepalives, 1);
}

#[test]
fn under_the_pump_a_group_is_read_by_its_handle_and_a_left_one_closes_after_a_drain() {
    let group = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 5), free_port());
    let other = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 6), free_port());
    let s = shared(1);
    let mut intake = s.pump_intake().unwrap();
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    let multicast = |group, generation| RxPlan {
        group: Some(group),
        ..plan(LOOPBACK, 1, false, generation)
    };
    publish(
        &s,
        &[(2, multicast(group, 1))],
        &[(0, RxRoute { flow: 2, slot: 0 })],
    );
    rx.pass(None, 0);
    intake.drain_into_rings(None);
    assert_eq!((s.lane(2).joins, s.lane(2).join_errors), (1, 0));
    assert_ne!(s.handles.get(2).0, 0);
    let t = multicast_transmitter();
    t.send_to(&packet_at(P0, 1, 32), group).unwrap();
    settle();
    rx.wait(Duration::from_millis(10)).unwrap();
    assert_eq!(rx.ready, 0, "the group socket is not polled");
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    assert_eq!(s.lane(2).packets, 1);
    // Another group: until the RX thread joined it, the writer reads
    // neither group (the old one's datagrams are not the new flow's).
    publish(
        &s,
        &[(2, multicast(other, 2))],
        &[(0, RxRoute { flow: 2, slot: 0 })],
    );
    t.send_to(&packet_at(P0 + 32, 1, 32), group).unwrap();
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    assert_eq!(read_of(s.lane(2)), 1, "{:?}", s.lane(2));
    // The old socket waits for the writer's next drain.
    rx.pass(None, 0);
    assert_eq!(rx.retired.len(), 1, "kept while the writer may read it");
    rx.pass(None, 0);
    assert_eq!(rx.retired.len(), 1);
    intake.drain_into_rings(None);
    rx.pass(None, 0);
    assert!(rx.retired.is_empty(), "closed after the drain");
    t.send_to(&packet_at(P0 + 64, 1, 32), other).unwrap();
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    assert_eq!(s.lane(2).packets, 2, "the new group read");
    // The RX thread ends first: its group socket stays open with the
    // shared state, for the pump's reads.
    drop(rx);
    assert_eq!(s.parked.lock().unwrap().len(), 1);
    t.send_to(&packet_at(P0 + 96, 1, 32), other).unwrap();
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 200, RATE)));
    assert_eq!(s.lane(2).packets, 3);
}

#[test]
fn while_the_rx_thread_writes_its_dropped_sockets_close_with_it() {
    let group = SocketAddrV4::new(Ipv4Addr::new(239, 255, 77, 7), free_port());
    let s = shared(1);
    let mut rx = Receiver::new(Arc::clone(&s)).unwrap();
    publish(
        &s,
        &[(
            0,
            RxPlan {
                group: Some(group),
                ..plan(LOOPBACK, 1, false, 1)
            },
        )],
        &[],
    );
    rx.pass(None, 0);
    assert_eq!(s.lane(0).joins, 1);
    drop(rx);
    assert!(s.parked.lock().unwrap().is_empty());
}

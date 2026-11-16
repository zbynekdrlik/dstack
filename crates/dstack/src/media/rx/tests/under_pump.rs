//! The engine's pump as the rings' writer (dstack #326): the claim, the
//! drain, and the RX thread's part beside it (groups, keepalives).

use super::*;

/// Ten years of a grandmaster's uptime, as a position.
const P0: u64 = 10 * 365 * 86_400 * RATE as u64;

/// Waits until `slot`'s socket (or group) has a datagram for the drain.
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
    // The RX thread first: it writes, and again for a second receiver;
    // the pump gets nothing.
    let s = shared(1);
    let rx = Receiver::new(Arc::clone(&s)).unwrap();
    assert!(rx.intake.is_some());
    assert!(s.thread_writes());
    assert!(s.pump_intake().is_none());
    assert!(!s.pump_writes());
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
    let t = transmitter(LOOPBACK);
    for k in 0..3 {
        t.send_to(&packet_at(P0 + 32 * k, 2, 32), slot_addr(&s, 0))
            .unwrap();
    }
    // And one to a slot without a flow.
    t.send_to(&packet_at(P0, 2, 32), slot_addr(&s, 5)).unwrap();
    settle();
    rx.wait(Duration::from_millis(10)).unwrap();
    assert_eq!(rx.ready, 0, "no flow socket polled");
    rx.pass(Some(ns_of(P0 + 100, RATE)), 0);
    assert_eq!(read_of(s.lane(0)), 0, "the RX thread read nothing");
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    let c = s.lane(0);
    assert_eq!((c.packets, c.stray, c.late), (3, 0, 0), "{c:?}");
    let mut got = [0i32; 96];
    assert_eq!(s.rings()[1].read_at(P0, &mut got).holes, 0);
    assert_eq!(got[40], value(P0 + 40, 1));
    // A slot without a flow is not read under the pump.
    assert_eq!(read_of(s.lane(5)), 0);
    // An empty socket: the drain returns at once, nothing counted.
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    assert_eq!(s.lane(0).packets, 3);
    assert_eq!(s.lane(0).recv_errors, 0);
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
    assert_eq!((s.lane(2).joins, s.lane(2).join_errors), (1, 0));
    assert_ne!(s.handles.get(2), 0);
    let t = multicast_transmitter();
    t.send_to(&packet_at(P0, 1, 32), group).unwrap();
    settle();
    rx.wait(Duration::from_millis(10)).unwrap();
    assert_eq!(rx.ready, 0, "the group socket is not polled");
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    assert_eq!(s.lane(2).packets, 1);
    // Another group: the old socket waits for the writer's next drain.
    publish(
        &s,
        &[(2, multicast(other, 2))],
        &[(0, RxRoute { flow: 2, slot: 0 })],
    );
    rx.pass(None, 0);
    assert_eq!(rx.retired.len(), 1, "kept while the writer may read it");
    rx.pass(None, 0);
    assert_eq!(rx.retired.len(), 1);
    intake.drain_into_rings(None);
    rx.pass(None, 0);
    assert!(rx.retired.is_empty(), "closed after the drain");
    t.send_to(&packet_at(P0 + 32, 1, 32), other).unwrap();
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
    assert_eq!(s.lane(2).packets, 2, "the new group read");
    // The RX thread ends first: its group socket stays open with the
    // shared state, for the pump's reads.
    drop(rx);
    assert_eq!(s.parked.lock().unwrap().len(), 1);
    t.send_to(&packet_at(P0 + 64, 1, 32), other).unwrap();
    settle();
    intake.drain_into_rings(Some(ns_of(P0 + 100, RATE)));
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

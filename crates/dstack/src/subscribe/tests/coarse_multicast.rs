// SPDX-License-Identifier: MIT OR Apache-2.0
//! dstack #297: a receiver whose own unicast flow would carry fewer frames
//! a packet than the transmitter's multicast flow takes its own unicast
//! flow, make-before-break, on a free transmitter slot only, and stays on
//! the multicast flow when that cannot be had. We never create, change or
//! end the transmitter's multicast flow: the manager's only actions are
//! our own unicast flow's request and stop.

use super::*;

/// A device that opts in: its config sets `arc_rx_fpp` (the main's
/// decision on #297). Shadows the default-config [`super::manager`].
fn manager() -> Manager {
    let cfg = DeviceConfig {
        arc_rx_fpp: Some(4),
        ..cfg()
    };
    Manager::new(&cfg, "test-device", OUR_IP, ports())
}

/// A low-latency receiver's view of the peer: its records ask 4 frames a
/// packet (an ARC-read transmitter at `arc_rx_fpp = 4`), its multicast
/// flow of TX 1 and 2 sends 32 ([`mics`]).
fn fine_network() -> Shared {
    let n = Shared::default();
    for id in 1..=8 {
        let a = Advertised {
            fpp_min: 4,
            fpp_max: 4,
            ..adv(PEER_IP, id)
        };
        n.records.add("peer-device", &format!("TX {id}"), a);
    }
    n.set("peer-device", mics());
    n
}

/// The flow slot that joins the multicast group, if any.
fn group_slot(m: &Manager) -> Option<usize> {
    (0..MAX_FLOWS).find(|&s| m.rx_plan(s).group.is_some())
}

/// Every action so far: a request to the peer for our own unicast flow, or
/// a stop of one we were granted; never anything to the group.
fn only_our_unicast(out: &[Action], granted: &[Handle]) {
    for a in out {
        match a {
            Action::Request { to, req, .. } => {
                assert_eq!(*to, SocketAddrV4::new(PEER_IP, 4455), "{a:?}");
                assert_eq!(*req.rx.ip(), OUR_IP, "{a:?}");
            }
            Action::Stop { handle, .. } => assert!(granted.contains(handle), "{a:?}"),
        }
    }
}

#[test]
fn a_channel_on_a_coarser_multicast_flow_asks_its_own_unicast_flow_and_moves_once_it_delivers() {
    let net = fine_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    let mut all = Vec::new();
    // It joins the multicast flow at once: audio from the first tick.
    let out = m.tick(0, &NONE, &net);
    assert_eq!(out, []);
    assert_eq!(group_slot(&m), Some(0));
    assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
    // Its packets come: a unicast flow at our 4 frames is asked for, the
    // multicast flow kept.
    let mut heard = NONE;
    heard[0] = 1;
    let out = m.tick(S / 4, &heard, &net);
    let r = requests(&out);
    assert_eq!(r.len(), 1, "{out:?}");
    let (flow, _, req) = r[0].clone();
    assert_eq!(req.fpp, 4);
    assert_eq!(req.channels, vec![Some(1), None, None, None]);
    assert_ne!(flow.slot, 0);
    assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
    assert_eq!(m.status(0), Some(SubStatus::ReceivingMulticast));
    all.extend(out);
    // Granted: still on the multicast flow until the unicast flow delivers.
    let h = handle(0, 7);
    assert_eq!(m.answered(flow, Answer::Granted(h), S / 2), []);
    heard[0] = 2;
    let out = m.tick(S, &heard, &net);
    assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
    assert_eq!(group_slot(&m), Some(0));
    assert!(m.rx_plan(flow.slot).live);
    all.extend(out);
    // Its packets come: the channel moves, then the group is left.
    heard[0] = 3;
    heard[flow.slot] = 1;
    let out = m.tick(S + S / 4, &heard, &net);
    assert_eq!(
        m.rx_route(0),
        Some(RxRoute {
            flow: flow.slot,
            slot: 0
        })
    );
    assert_eq!(group_slot(&m), None);
    assert_eq!(m.status(0), Some(SubStatus::Receiving));
    all.extend(out);
    // Later ticks keep it there and ask nothing more.
    for t in 2..40 {
        heard[flow.slot] = t;
        all.extend(m.tick(t * S, &heard, &net));
    }
    assert_eq!(requests(&all).len(), 1);
    assert_eq!(stops(&all), []);
    only_our_unicast(&all, &[h]);
}

#[test]
fn a_unicast_flow_stays_when_a_coarser_multicast_flow_appears() {
    let net = fine_network();
    net.set("peer-device", MulticastLookup::None);
    let mut m = manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    let r = requests(&m.tick(0, &NONE, &net));
    let (flow, _, _) = r[0].clone();
    m.answered(flow, Answer::Granted(handle(0, 9)), MS);
    let mut heard = NONE;
    heard[flow.slot] = 1;
    m.tick(S / 4, &heard, &net);
    net.set("peer-device", mics());
    heard[flow.slot] = 2;
    let out = m.tick(S / 2, &heard, &net);
    assert_eq!(out, []);
    assert_eq!(group_slot(&m), None);
    assert_eq!(
        m.rx_route(0),
        Some(RxRoute {
            flow: flow.slot,
            slot: 0
        })
    );
}

#[test]
fn a_refused_unicast_flow_leaves_the_channel_on_the_multicast_flow_and_is_not_asked_again() {
    for refusal in [
        Answer::Refused(status::TOO_MANY_TX_FLOWS),
        Answer::Refused(status::FPP_REFUSED),
        Answer::Refused(status::SAMPLE_RATE_MISMATCH),
        Answer::Failed,
    ] {
        let net = fine_network();
        let mut m = manager();
        m.set(0, Some(sub("TX 1", "peer-device")));
        m.tick(0, &NONE, &net);
        let mut heard = NONE;
        heard[0] = 1;
        let out = m.tick(S / 4, &heard, &net);
        let (flow, _, _) = requests(&out)[0].clone();
        let mut all = out;
        all.extend(m.answered(flow, refusal, S / 2));
        // The channel never left the multicast flow, nor its status.
        assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
        assert_eq!(
            m.status(0),
            Some(SubStatus::ReceivingMulticast),
            "{refusal:?}"
        );
        // No other request, ever after: no loop.
        for t in 1..200 {
            heard[0] = 1 + t;
            all.extend(m.tick(S / 2 + t * S, &heard, &net));
            assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
        }
        assert_eq!(requests(&all).len(), 1, "{refusal:?}");
        assert_eq!(stops(&all), [], "{refusal:?}");
        assert_eq!(m.status(0), Some(SubStatus::ReceivingMulticast));
    }
}

/// [`mics`] as the transmitter's flow list shows it with `free` TX flow
/// slots free.
fn mics_free(free: Option<u16>) -> MulticastLookup {
    let MulticastLookup::Found(m) = mics() else {
        unreachable!()
    };
    MulticastLookup::Found(Multicast {
        free_slots: free,
        ..m
    })
}

/// A channel joined to the coarser multicast flow, its packets heard: the
/// manager, the tick's actions and the packet counts.
fn joined(net: &Shared) -> (Manager, Vec<Action>, [u64; MAX_FLOWS]) {
    let mut m = manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    let mut out = m.tick(0, &NONE, net);
    let mut heard = NONE;
    heard[0] = 1;
    out.extend(m.tick(S / 4, &heard, net));
    (m, out, heard)
}

#[test]
fn no_free_or_no_known_transmitter_slot_asks_nothing_and_says_so_once() {
    for (free, why) in [
        (Some(0), "the transmitter has no free TX flow slot"),
        (None, "the transmitter's free TX flow slots are not known"),
    ] {
        let net = fine_network();
        net.set("peer-device", mics_free(free));
        let text = crate::test_log::capture_at(tracing::Level::INFO, || {
            let (mut m, mut all, mut heard) = joined(&net);
            for t in 1..100 {
                heard[0] = 1 + t;
                all.extend(m.tick(S / 4 + t * S, &heard, &net));
            }
            assert_eq!(all, [], "{free:?}");
            assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
            assert_eq!(m.status(0), Some(SubStatus::ReceivingMulticast));
        });
        assert_eq!(text.matches(why).count(), 1, "{text}");
        let line = text.lines().find(|l| l.contains(why)).unwrap();
        assert!(line.contains("multicast_fpp=32"), "{line}");
        assert!(line.contains("our_fpp=4"), "{line}");
    }
    // A free slot asks.
    let net = fine_network();
    net.set("peer-device", mics_free(Some(1)));
    assert_eq!(requests(&joined(&net).1).len(), 1);
}

#[test]
fn a_multicast_flow_with_at_most_our_frames_is_kept_and_asks_nothing() {
    for ours in [32, 64] {
        let net = fine_network();
        for id in 1..=8 {
            let a = Advertised {
                fpp_min: 4,
                fpp_max: ours,
                ..adv(PEER_IP, id)
            };
            net.records.add("peer-device", &format!("TX {id}"), a);
        }
        let (m, out, _) = joined(&net);
        assert_eq!(out, [], "ours {ours}");
        assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
    }
}

#[test]
fn the_request_and_its_choice_are_logged_with_both_fpps() {
    let net = fine_network();
    let text = crate::test_log::capture_at(tracing::Level::INFO, || {
        let mut m = manager();
        m.set(0, Some(sub("TX 1", "peer-device")));
        m.tick(0, &NONE, &net);
        let mut heard = NONE;
        heard[0] = 1;
        let (flow, _, _) = requests(&m.tick(S / 4, &heard, &net))[0].clone();
        m.answered(flow, Answer::Granted(handle(0, 7)), S / 2);
        heard[flow.slot] = 1;
        m.tick(S, &heard, &net);
    });
    let asked = text
        .lines()
        .find(|l| l.contains("our own unicast flow is asked for"))
        .expect(&text);
    for field in [
        "multicast_fpp=32",
        "our_fpp=4",
        "free_slots=1",
        "tx_device=peer-device",
        "rx=[1]",
    ] {
        assert!(asked.contains(field), "{field}: {asked}");
    }
    let joined = text
        .lines()
        .find(|l| l.contains("a transmitter's multicast flow is received"))
        .expect(&text);
    assert!(joined.contains("our_fpp=4"), "{joined}");
    let moved = text
        .lines()
        .find(|l| l.contains("its channels leave the multicast flow"))
        .expect(&text);
    assert!(
        moved.contains("fpp=4") && moved.contains("channels=1"),
        "{moved}"
    );
}

#[test]
fn a_granted_unicast_flow_that_never_delivers_is_stopped_and_the_multicast_kept() {
    let net = fine_network();
    let (mut m, out, mut heard) = joined(&net);
    let (flow, to, _) = requests(&out)[0].clone();
    let h = handle(0, 5);
    m.answered(flow, Answer::Granted(h), S / 2);
    let mut all = Vec::new();
    for t in 1..60 {
        heard[0] = 1 + t;
        all.extend(m.tick(S / 2 + t * S / 4, &heard, &net));
        assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
    }
    // Stopped once it is old and silent, never asked again.
    assert_eq!(stops(&all), [(to, h)]);
    assert_eq!(requests(&all), []);
    assert_eq!(m.rx_plan(flow.slot), RxPlan::default());
    assert_eq!(m.status(0), Some(SubStatus::ReceivingMulticast));
}

#[test]
fn an_unanswered_unicast_request_is_given_up_and_the_multicast_kept() {
    let net = fine_network();
    let (mut m, out, mut heard) = joined(&net);
    let (flow, _, _) = requests(&out)[0].clone();
    let mut all = Vec::new();
    for t in 1..100 {
        heard[0] = 1 + t;
        all.extend(m.tick(S / 4 + t * S, &heard, &net));
    }
    assert_eq!(requests(&all), []);
    assert_eq!(m.rx_plan(flow.slot), RxPlan::default());
    assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
    // A grant that still comes is stopped.
    let h = handle(0, 3);
    assert_eq!(
        m.answered(flow, Answer::Granted(h), 200 * S),
        [Action::Stop {
            to: SocketAddrV4::new(PEER_IP, 4455),
            handle: h
        }]
    );
}

#[test]
fn every_channel_of_the_transmitter_on_the_multicast_flow_moves_together() {
    let net = fine_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 2", "peer-device")));
    m.set(1, Some(sub("TX 1", "peer-device")));
    m.set(2, Some(sub("TX 2", "peer-device")));
    m.tick(0, &NONE, &net);
    let mut heard = NONE;
    heard[0] = 1;
    let r = requests(&m.tick(S / 4, &heard, &net));
    assert_eq!(r.len(), 1);
    let (flow, _, req) = r[0].clone();
    assert_eq!(req.channels, vec![Some(2), Some(1), None, None]);
    m.answered(flow, Answer::Granted(handle(0, 1)), S / 2);
    heard[flow.slot] = 1;
    m.tick(S, &heard, &net);
    let route = |slot| {
        Some(RxRoute {
            flow: flow.slot,
            slot,
        })
    };
    assert_eq!(
        [m.rx_route(0), m.rx_route(1), m.rx_route(2)],
        [route(0), route(1), route(0)]
    );
    assert_eq!(group_slot(&m), None);
}

#[test]
fn a_full_list_of_kept_transmitters_asks_none_and_grows_no_further() {
    // Full: nothing is asked.
    let net = fine_network();
    let mut m = manager();
    for i in 0..crate::subscribe::coarse::MAX_KEPT {
        m.multicast_kept
            .insert(SocketAddrV4::new(Ipv4Addr::new(10, 1, 0, i as u8), 4455));
    }
    m.set(0, Some(sub("TX 1", "peer-device")));
    m.tick(0, &NONE, &net);
    let mut heard = NONE;
    heard[0] = 1;
    assert_eq!(m.tick(S / 4, &heard, &net), []);
    // One short: asked; the list fills meanwhile, and the refusal adds
    // nothing past it.
    let (mut m, out, _) = joined(&net);
    let (flow, _, _) = requests(&out)[0].clone();
    for i in 0..crate::subscribe::coarse::MAX_KEPT {
        m.multicast_kept
            .insert(SocketAddrV4::new(Ipv4Addr::new(10, 1, 0, i as u8), 4455));
    }
    m.answered(flow, Answer::Refused(status::TOO_MANY_TX_FLOWS), S / 2);
    assert_eq!(m.multicast_kept.len(), crate::subscribe::coarse::MAX_KEPT);
    assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
}

#[test]
fn another_transmitter_s_multicast_flow_is_left_alone() {
    // The other device's multicast flow is fine for us: its channel stays
    // there when the peer's channel moves to its own unicast flow.
    let net = fine_network();
    for id in 1..=2 {
        let a = Advertised {
            fpp_max: 32,
            ..adv(OTHER_IP, id)
        };
        net.records.add("other-device", &format!("TX {id}"), a);
    }
    let other = SocketAddrV4::new(Ipv4Addr::new(239, 255, 0, 2), 4321);
    let MulticastLookup::Found(m0) = mics() else {
        unreachable!()
    };
    net.set(
        "other-device",
        MulticastLookup::Found(Multicast { group: other, ..m0 }),
    );
    let mut m = manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    m.set(1, Some(sub("TX 1", "other-device")));
    m.tick(0, &NONE, &net);
    let mut heard = NONE;
    heard[0] = 1;
    heard[1] = 1;
    let r = requests(&m.tick(S / 4, &heard, &net));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].1, SocketAddrV4::new(PEER_IP, 4455));
    let (flow, _, _) = r[0].clone();
    m.answered(flow, Answer::Granted(handle(0, 2)), S / 2);
    heard[flow.slot] = 1;
    heard[0] = 2;
    heard[1] = 2;
    m.tick(S, &heard, &net);
    assert_eq!(m.rx_route(0).unwrap().flow, flow.slot);
    let groups: Vec<_> = (0..MAX_FLOWS).filter_map(|s| m.rx_plan(s).group).collect();
    assert_eq!(groups, [other]);
}

/// The main's decision on #297: a device whose config leaves `arc_rx_fpp`
/// out (the band's FOH/PA feed among them) keeps today's behaviour on the
/// same network: it stays on the coarser multicast flow and asks nothing,
/// and its unicast flow moves to a multicast flow that appears.
#[test]
fn a_device_on_the_default_config_stays_on_the_coarser_multicast_flow() {
    let net = fine_network();
    let mut m = super::manager();
    assert_eq!(cfg().arc_rx_fpp, None, "the test device leaves it out");
    m.set(0, Some(sub("TX 1", "peer-device")));
    let mut all = m.tick(0, &NONE, &net);
    let mut heard = NONE;
    for t in 1..100 {
        heard[0] = t;
        all.extend(m.tick(t * S / 4, &heard, &net));
    }
    assert_eq!(all, []);
    assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
    assert_eq!(m.status(0), Some(SubStatus::ReceivingMulticast));
    // On a unicast flow when the multicast flow appears: it moves there.
    let net = fine_network();
    net.set("peer-device", MulticastLookup::None);
    let mut m = super::manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    let (flow, to, _) = requests(&m.tick(0, &NONE, &net))[0].clone();
    let h = handle(0, 9);
    m.answered(flow, Answer::Granted(h), MS);
    heard = NONE;
    heard[flow.slot] = 1;
    m.tick(S / 4, &heard, &net);
    net.set("peer-device", mics());
    let out = m.tick(S / 2, &heard, &net);
    assert_eq!(stops(&out), [(to, h)]);
    assert!(group_slot(&m).is_some());
}

/// A second channel subscribed while the unicast flow is granted but has
/// delivered nothing joins the multicast flow, never the silent unicast
/// flow; both move once it delivers.
#[test]
fn a_channel_subscribed_before_the_unicast_flow_delivers_joins_the_multicast_flow() {
    let net = fine_network();
    let (mut m, out, mut heard) = joined(&net);
    let (flow, _, _) = requests(&out)[0].clone();
    m.answered(flow, Answer::Granted(handle(0, 4)), S / 2);
    // Asked, then granted: a new channel of TX 1 joins the multicast flow.
    m.set(1, Some(sub("TX 1", "peer-device")));
    heard[0] = 2;
    m.tick(S, &heard, &net);
    assert_eq!(m.rx_route(1), Some(RxRoute { flow: 0, slot: 0 }));
    heard[flow.slot] = 1;
    heard[0] = 3;
    m.tick(S + S / 4, &heard, &net);
    let on = |slot| {
        Some(RxRoute {
            flow: flow.slot,
            slot,
        })
    };
    assert_eq!((m.rx_route(0), m.rx_route(1)), (on(0), on(0)));
}

/// Packets in the unicast flow's slot before its grant (a stray sender)
/// move nothing: only a granted flow that delivers takes the channels.
#[test]
fn packets_before_the_grant_move_nothing() {
    let net = fine_network();
    let (mut m, out, mut heard) = joined(&net);
    let (flow, _, _) = requests(&out)[0].clone();
    heard[flow.slot] = 5;
    heard[0] = 2;
    m.tick(S / 2, &heard, &net);
    assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
    m.answered(flow, Answer::Granted(handle(0, 4)), S / 2);
    heard[flow.slot] = 6;
    m.tick(S, &heard, &net);
    assert_eq!(m.rx_route(0).unwrap().flow, flow.slot);
}

/// Rule 3, no other side effect: the transmitter's counts and request wait
/// are left as they were, so another channel of it that needs its own
/// unicast flow is asked at once, and an unanswered request counts nothing.
#[test]
fn the_request_and_its_failure_leave_the_transmitter_s_wait_and_counts_alone() {
    let net = fine_network();
    let (mut m, out, mut heard) = joined(&net);
    let to = SocketAddrV4::new(PEER_IP, 4455);
    let (flow, _, _) = requests(&out)[0].clone();
    assert_eq!(m.peers[&to].last_request, None);
    m.answered(flow, Answer::Failed, S / 2);
    let p = &m.peers[&to];
    assert_eq!((p.unanswered, p.failures, p.in_flight.len()), (0, 0, 0));
    // TX 5 is in no multicast flow: its own flow is asked at the next tick.
    m.set(1, Some(sub("TX 5", "peer-device")));
    heard[0] = 2;
    let r = requests(&m.tick(S, &heard, &net));
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].2.channels[0], Some(5));
    // A late answer for a given-up request counts nothing either.
    let (mut m, out, mut heard) = joined(&net);
    let (flow, _, _) = requests(&out)[0].clone();
    heard[0] = 2;
    m.tick(S / 4 + GIVE_UP_NS, &heard, &net);
    assert_eq!(m.answered(flow, Answer::Failed, 10 * S), []);
    assert_eq!(m.peers.get(&to).map_or(0, |p| p.unanswered), 0);
}

/// A transmitter in its request wait (a failed request of another channel)
/// is asked only after the wait.
#[test]
fn the_request_waits_for_the_transmitter_s_retry_wait() {
    let net = fine_network();
    net.set("peer-device", mics_free(Some(1)));
    let mut m = manager();
    // TX 5 (no multicast) asked and refused at 0: the next request waits.
    m.set(1, Some(sub("TX 5", "peer-device")));
    let (flow, _, _) = requests(&m.tick(0, &NONE, &net))[0].clone();
    m.answered(flow, Answer::Refused(status::SAMPLE_RATE_MISMATCH), MS);
    m.set(1, None);
    m.set(0, Some(sub("TX 1", "peer-device")));
    m.tick(S / 4, &NONE, &net);
    let g = group_slot(&m).expect("joined");
    let mut heard = NONE;
    let mut asked = Vec::new();
    for t in 2..60 {
        heard[g] = t;
        for r in requests(&m.tick(t * S / 4, &heard, &net)) {
            asked.push((t * S / 4, r));
        }
    }
    assert_eq!(asked.len(), 1, "{asked:?}");
    assert!(asked[0].0 >= retry_after(1), "{}", asked[0].0);
    assert!(asked[0].0 < retry_after(1) + S / 2, "{}", asked[0].0);
}

/// A record that changed to another rate after the join (the multicast
/// flow keeps it routed) is never asked for its own unicast flow.
#[test]
fn a_record_at_another_rate_asks_nothing() {
    let net = fine_network();
    let mut m = manager();
    m.set(0, Some(sub("TX 1", "peer-device")));
    m.tick(0, &NONE, &net);
    let a = net.records.lookup("peer-device", "TX 1").unwrap();
    net.records
        .add("peer-device", "TX 1", Advertised { rate: 48_000, ..a });
    let mut heard = NONE;
    heard[0] = 1;
    assert_eq!(requests(&m.tick(S / 4, &heard, &net)), []);
    assert_eq!(m.rx_route(0), Some(RxRoute { flow: 0, slot: 0 }));
}

/// The move is said once, and a refusal's line names both fpps.
#[test]
fn the_move_is_said_once_and_a_refusal_names_both_fpps() {
    let net = fine_network();
    let text = crate::test_log::capture_at(tracing::Level::INFO, || {
        let (mut m, out, mut heard) = joined(&net);
        let (flow, _, _) = requests(&out)[0].clone();
        m.answered(flow, Answer::Granted(handle(0, 7)), S / 2);
        for t in 1..10 {
            heard[flow.slot] = t;
            heard[0] = 1 + t;
            m.tick(S / 2 + t * S / 4, &heard, &net);
        }
    });
    assert_eq!(
        text.matches("its channels leave the multicast flow")
            .count(),
        1,
        "{text}"
    );
    let text = crate::test_log::capture_at(tracing::Level::INFO, || {
        let (mut m, out, _) = joined(&net);
        let (flow, _, _) = requests(&out)[0].clone();
        m.answered(flow, Answer::Refused(status::TOO_MANY_TX_FLOWS), S / 2);
    });
    let line = text
        .lines()
        .find(|l| l.contains("our own unicast flow was refused"))
        .expect(&text);
    assert!(line.contains("multicast_fpp=32"), "{line}");
    assert!(line.contains("our_fpp=4"), "{line}");
}

/// Its channels unsubscribed while the unicast flow is asked: the flow
/// ends with nothing refused, so the transmitter is not kept on its
/// multicast flow.
#[test]
fn an_unsubscribed_channel_s_unicast_flow_ends_without_keeping_the_multicast() {
    let net = fine_network();
    let (mut m, out, _) = joined(&net);
    let (flow, _, _) = requests(&out)[0].clone();
    m.set(0, None);
    m.tick(S / 2, &NONE, &net);
    m.answered(flow, Answer::Refused(status::TOO_MANY_TX_FLOWS), S);
    assert!(m.multicast_kept.is_empty());
    assert_eq!(m.rx_plan(flow.slot), RxPlan::default());
}

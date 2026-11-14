// SPDX-License-Identifier: MIT OR Apache-2.0
//! dstack #297: a receiver whose own unicast flow would carry fewer frames
//! a packet than the transmitter's multicast flow takes its own unicast
//! flow, make-before-break, on a free transmitter slot only, and stays on
//! the multicast flow when that cannot be had. We never create, change or
//! end the transmitter's multicast flow: the manager's only actions are
//! our own unicast flow's request and stop.

use super::*;

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
        all.extend(m.answered(flow, refusal.clone(), S / 2));
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

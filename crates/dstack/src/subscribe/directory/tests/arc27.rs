//! A transmitter that speaks ARC 2.7 only (dstack #271): it refuses the
//! 2.8.9 TX flow list (0x2600) with status 0x0030 and lists its flows with
//! 0x2200. The directory reads that list, finds the multicast flow in it,
//! and asks 0x2200 first from then on.

use super::*;

use std::sync::mpsc;

use crate::arc::flows::tests::{CARD_27_PAGE, CARD_2600_REFUSED, hex};
use crate::wire::HEADER_LEN;

/// The card's multicast flow (flow 2 of the page: TX 1 and 2).
const CARD_GROUP: SocketAddrV4 = SocketAddrV4::new(Ipv4Addr::new(239, 69, 0, 2), 4321);

/// A card on loopback that refuses 0x2600 (0x0030) and answers 0x2200 with
/// its page; each request's (start code, op) goes to the returned channel.
/// It ends after `BOUND` without a request.
fn card_27() -> (SocketAddrV4, mpsc::Receiver<(u16, u16)>) {
    let sock = UdpSocket::bind((LOOPBACK, 0)).unwrap();
    sock.set_read_timeout(Some(BOUND)).unwrap();
    let std::net::SocketAddr::V4(at) = sock.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let (tell, asked) = mpsc::channel();
    std::thread::spawn(move || {
        let mut buf = [0u8; 2048];
        while let Ok((n, from)) = sock.recv_from(&mut buf) {
            if n < HEADER_LEN {
                continue;
            }
            let start = u16::from_be_bytes([buf[0], buf[1]]);
            let op = u16::from_be_bytes([buf[6], buf[7]]);
            let mut reply = match op {
                0x2600 => hex(CARD_2600_REFUSED),
                0x2200 => hex(CARD_27_PAGE),
                _ => continue,
            };
            reply[4..6].copy_from_slice(&buf[4..6]);
            sock.send_to(&reply, from).unwrap();
            if tell.send((start, op)).is_err() {
                return;
            }
        }
    });
    (at, asked)
}

#[test]
fn a_2_7_card_s_flows_are_read_with_0x2200_after_its_0x2600_refusal() {
    let (at, asked) = card_27();
    let (mdns, _) = responder(vec![vec![rec(
        ARC_SERVICE,
        "card",
        "card",
        LOOPBACK,
        at.port(),
    )]]);
    let mut arc = ArcDirectory::new(Timing {
        settle_ns: 0,
        max_age_ns: MAX_AGE_NS,
        reply: BOUND,
        idle: Duration::from_millis(10),
        fpp: FPP,
    });
    let requests = Arc::new(Requests::default());
    assert!(arc.start(LOOPBACK, mdns, Arc::clone(&requests)).unwrap());
    let records = arc.records();
    eventually("the card's record", || {
        records.arc_at("card", LOOPBACK).is_ok()
    });
    let a = peer_channel(2);
    eventually("the card's multicast flow", || {
        matches!(arc.multicast("card", "02", &a), MulticastLookup::Found(_))
    });
    let MulticastLookup::Found(m) = arc.multicast("card", "02", &a) else {
        panic!("found once, kept")
    };
    assert_eq!(m.group, CARD_GROUP);
    assert_eq!(m.channels, vec![1, 2]);
    assert_eq!(m.fpp, 16);
    let first: Vec<(u16, u16)> = (0..2)
        .map(|_| {
            asked
                .recv_timeout(BOUND)
                .expect("the first read's requests")
        })
        .collect();
    assert_eq!(first, vec![(0x2809, 0x2600), (0x27ff, 0x2200)]);
    arc.join_within(BOUND);
}

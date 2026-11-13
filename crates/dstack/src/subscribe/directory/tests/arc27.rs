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

/// The card's flows as `arc::flows` reads them.
fn card_flows() -> Flows {
    let mut c = crate::arc::flows::tests::Scripted::new(&[CARD_27_PAGE]);
    crate::arc::flows::v27::list_tx_flows_27(&mut c).unwrap()
}

#[test]
fn the_form_that_answered_is_asked_first_next_time_and_a_failure_keeps_it() {
    let mut book = peer_book();
    let a = peer_channel(2);
    book.multicast("peer-device", &a, MS);
    let job = book.next(MS).unwrap();
    assert_eq!(job.form, ListForm::V289);
    assert!(!book.finished_flows(&job, Ok((card_flows(), ListForm::V27)), 2, MS));
    assert_eq!(book.counts().requests, 2);
    assert_eq!(book.counts().refused, 0);
    let MulticastLookup::Found(m) = book.multicast("peer-device", &a, MS) else {
        panic!("the 2.7 list's multicast flow")
    };
    assert_eq!(m.group, CARD_GROUP);
    // Aged: the next read asks 0x2200 first.
    let old = MS + MAX_AGE_NS;
    book.multicast("peer-device", &a, old);
    let job = book.next(old).unwrap();
    assert_eq!(job.form, ListForm::V27);
    // A read that failed leaves the form as it was.
    book.finished_flows(&job, Err(ReadError::Timeout { op: 0x2200 }), 1, old);
    let later = old + MAX_AGE_NS;
    book.multicast("peer-device", &a, later);
    assert_eq!(book.next(later).unwrap().form, ListForm::V27);
}

#[test]
fn a_device_s_refusal_is_told_once_and_other_failures_never() {
    let mut book = peer_book();
    let a = peer_channel(1);
    let refused = || ReadError::Refused {
        op: 0x2200,
        status: 0x0022,
    };
    let mut told = Vec::new();
    let mut t = MS;
    for result in [
        Err(ReadError::Timeout { op: 0x2600 }),
        Err(refused()),
        Err(refused()),
        Err(refused()),
    ] {
        book.multicast("peer-device", &a, t);
        let job = book.next(t).unwrap();
        told.push(book.finished_flows(&job, result, 1, t));
        t += MAX_AGE_NS + crate::subscribe::RETRY_MAX_NS;
    }
    assert_eq!(told, vec![false, true, false, false]);
    assert_eq!(book.counts().refused, 3);
    // A success tells nothing.
    book.multicast("peer-device", &a, t);
    let job = book.next(t).unwrap();
    assert!(!book.finished_flows(&job, Ok((card_flows(), ListForm::V27)), 1, t));
    // Another device's first refusal is told too.
    let mut other = peer_book();
    other.multicast("peer-device", &a, MS);
    let job = other.next(MS).unwrap();
    assert!(other.finished_flows(&job, Err(refused()), 1, MS));
}

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #442: the taps on the daemon's socket, and the process's daemon
//! on an address.

use super::*;

use std::net::{SocketAddrV6, UdpSocket};
use std::time::Instant;

/// The daemons of these tests serve loopback, one test at a time: the
/// registry is the process's, and a test that drops its daemon expects the
/// next to be new.
static ONE_DAEMON: Mutex<()> = Mutex::new(());

fn from(last: u8, port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, last), port))
}

/// An mDNS query header (no questions) and a response header.
const QUERY: [u8; 12] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
const RESPONSE: [u8; 12] = [0, 0, 0x84, 0, 0, 0, 0, 0, 0, 0, 0, 0];

#[test]
fn a_query_is_a_header_whose_qr_bit_is_clear() {
    assert!(is_query(&[0, 0, 0x7f]));
    assert!(!is_query(&[0, 0, 0x80]));
    assert!(is_query(&QUERY));
    assert!(!is_query(&RESPONSE));
    assert!(!is_query(&[0, 0]), "too short for the flags");
    assert!(Heard::Queries.takes(&QUERY));
    assert!(!Heard::Queries.takes(&RESPONSE));
    assert!(Heard::All.takes(&RESPONSE));
    assert!(Heard::All.takes(&[]));
}

#[test]
fn a_dropped_datagram_is_warned_about_at_each_power_of_two() {
    let at: Vec<u64> = (1..=9).filter(|n| warn_at(*n)).collect();
    assert_eq!(at, [1, 2, 4, 8]);
    assert!(!warn_at(0));
}

#[test]
fn every_listener_takes_what_it_hears_in_order_with_its_source() {
    let taps = Arc::new(Taps::default());
    let all = taps.listen(Heard::All);
    let queries = taps.listen(Heard::Queries);
    assert_eq!(taps.listeners(), 2);
    taps.feed(&RESPONSE, from(2, 5353));
    taps.feed(&QUERY, from(3, 40000));
    // An IPv6 source is none of ours.
    taps.feed(
        &QUERY,
        SocketAddr::V6(SocketAddrV6::new(std::net::Ipv6Addr::LOCALHOST, 5353, 0, 0)),
    );
    let wait = Duration::from_millis(10);
    let got = |l: &Listener| {
        let mut out = Vec::new();
        while let Ok(d) = l.recv_timeout(wait) {
            out.push(d);
        }
        out
    };
    let v4 = |a: SocketAddr| match a {
        SocketAddr::V4(a) => a,
        SocketAddr::V6(_) => unreachable!(),
    };
    assert_eq!(
        got(&all),
        [
            Datagram {
                bytes: RESPONSE.to_vec(),
                from: v4(from(2, 5353)),
            },
            Datagram {
                bytes: QUERY.to_vec(),
                from: v4(from(3, 40000)),
            },
        ]
    );
    assert_eq!(
        got(&queries),
        [Datagram {
            bytes: QUERY.to_vec(),
            from: v4(from(3, 40000)),
        }]
    );
    drop(all);
    assert_eq!(taps.listeners(), 1, "a dropped listener leaves");
    drop(queries);
    assert_eq!(taps.listeners(), 0);
    taps.feed(&QUERY, from(3, 40000));
    assert_eq!(taps.dropped(), 0, "no listener, nothing dropped");
}

#[test]
fn a_full_listener_drops_and_counts_never_waits() {
    crate::test_log::undecided();
    let taps = Arc::new(Taps::default());
    let stalled = taps.listen(Heard::All);
    let warns = crate::test_log::capture_warnings(|| {
        for _ in 0..QUEUE + 4 {
            taps.feed(&QUERY, from(2, 5353));
        }
    });
    assert_eq!(taps.dropped(), 4);
    assert_eq!(
        warns.matches("its reader's queue is full").count(),
        3,
        "the 1st, 2nd and 4th drop: {warns}"
    );
    assert!(warns.contains("dropped=4"), "{warns}");
    let mut kept = 0;
    while stalled.recv_timeout(Duration::from_millis(1)).is_ok() {
        kept += 1;
    }
    assert_eq!(kept, QUEUE);
}

/// The datagrams the daemon's socket hands `l` until one carries `tag`
/// from `sender`, sending `bytes` to the group from it each 20 ms (at most
/// 10 s): the daemon opens its socket on its own thread.
fn heard_from(l: &Listener, sender: &UdpSocket, bytes: &[u8]) -> Datagram {
    let group = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
    let SocketAddr::V4(me) = sender.local_addr().unwrap() else {
        panic!("IPv4");
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        sender.send_to(bytes, group).unwrap();
        while let Ok(d) = l.recv_timeout(Duration::from_millis(20)) {
            if d.from == me && d.bytes == bytes {
                return d;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the daemon's socket never heard us"
        );
    }
}

/// The real daemon on loopback (dstack #366: nothing leaves the box): a
/// datagram to the group is handed to the meter and the listeners, a
/// query also to a responder's; the same daemon for every holder on the
/// address, shut down with the last.
#[test]
fn the_daemon_s_socket_feeds_the_meter_and_the_listeners_and_ends_with_its_last_holder() {
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let ip = Ipv4Addr::LOCALHOST;
    let mdns = Mdns::shared(ip).unwrap();
    let again = Mdns::shared(ip).unwrap();
    assert!(Arc::ptr_eq(&mdns, &again), "one per process and address");
    let meter = mdns.meter().expect("the address's meter");
    let all = mdns.listen(Heard::All);
    let queries = mdns.listen(Heard::Queries);
    // The meter's listener and ours.
    assert_eq!(mdns.listeners(), 3);
    let sender = UdpSocket::bind((ip, 0)).unwrap();
    socket2::SockRef::from(&sender)
        .set_multicast_if_v4(&ip)
        .unwrap();
    // A response with a tag in its id, never a query: the responder's
    // listener must not take it.
    let mut response = RESPONSE;
    response[..2].copy_from_slice(&0x4420u16.to_be_bytes());
    heard_from(&all, &sender, &response);
    let mut query = QUERY;
    query[..2].copy_from_slice(&0x4421u16.to_be_bytes());
    heard_from(&queries, &sender, &query);
    while let Ok(d) = queries.recv_timeout(Duration::from_millis(50)) {
        assert!(is_query(&d.bytes), "a response handed on: {d:?}");
    }
    assert!(meter.heard() >= 2, "the meter counts the same socket's");
    mdns.settle(GOODBYE_WAIT)
        .expect("the daemon runs its queue");
    drop((all, queries, again));
    // The last holder's drop shuts the daemon down: a clone of its handle
    // finds it gone, and the next holder gets a new one.
    let handle = mdns.daemon().clone();
    drop(mdns);
    assert!(handle.get_metrics().is_err(), "the daemon still runs");
    let next = Mdns::shared(ip).unwrap();
    next.settle(GOODBYE_WAIT).expect("a new daemon runs");
}

/// The daemon's count of withdrawn records so far.
fn unregistered(m: &Metrics) -> i64 {
    m.get("unregister").copied().unwrap_or(0)
}

/// A device's goodbye withdraws its own records only: another device's on
/// the same daemon stay registered until its own goodbye.
#[test]
fn a_device_s_withdrawal_takes_its_own_records_only() {
    use crate::announce::{CHAN_SERVICE, ServiceRecord};
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let mdns = Mdns::shared(Ipv4Addr::LOCALHOST).unwrap();
    let record = |device: &str| ServiceRecord {
        service: CHAN_SERVICE,
        instance: format!("TX 1@{device}"),
        host: format!("{device}.local."),
        ip: Ipv4Addr::LOCALHOST,
        port: 4455,
        txt: vec!["id=1".to_string()],
    };
    let mut a = Announcer::new(mdns.daemon().clone());
    let mut b = Announcer::new(mdns.daemon().clone());
    a.announce(&[record("mdns-test-a")]).unwrap();
    b.announce(&[record("mdns-test-b"), record("mdns-test-c")])
        .unwrap();
    let before = unregistered(&mdns.settle(GOODBYE_WAIT).unwrap());
    mdns.withdraw(&mut a, GOODBYE_WAIT).unwrap();
    let after_a = unregistered(&mdns.settle(GOODBYE_WAIT).unwrap());
    assert_eq!(after_a - before, 1, "A's one record");
    mdns.withdraw(&mut a, GOODBYE_WAIT).unwrap();
    assert_eq!(
        unregistered(&mdns.settle(GOODBYE_WAIT).unwrap()),
        after_a,
        "nothing of A's is left, and none of B's goes"
    );
    mdns.withdraw(&mut b, GOODBYE_WAIT).unwrap();
    assert_eq!(
        unregistered(&mdns.settle(GOODBYE_WAIT).unwrap()) - after_a,
        2,
        "B's two at its own goodbye"
    );
}

/// iemmixer's device has about 200 records, mdns-sd's queue holds 100
/// commands: a withdrawal fills it, and its round trip and the shutdown
/// after it wait the queue out instead of failing (`Error::Again`).
#[test]
fn a_withdrawal_of_more_records_than_the_queue_holds_is_confirmed() {
    use crate::announce::{CHAN_SERVICE, ServiceRecord};
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let mdns = Mdns::shared(Ipv4Addr::LOCALHOST).unwrap();
    let recs: Vec<ServiceRecord> = (0..250)
        .map(|i| ServiceRecord {
            service: CHAN_SERVICE,
            instance: format!("{i}@mdns-test-many"),
            host: "mdns-test-many.local.".to_string(),
            ip: Ipv4Addr::LOCALHOST,
            port: 4455,
            txt: vec![format!("id={i}")],
        })
        .collect();
    let mut a = Announcer::new(mdns.daemon().clone());
    a.announce(&recs).unwrap();
    let before = unregistered(&mdns.settle(GOODBYE_WAIT).unwrap());
    mdns.withdraw(&mut a, GOODBYE_WAIT)
        .expect("the goodbyes confirmed");
    let after = unregistered(&mdns.settle(GOODBYE_WAIT).unwrap());
    assert_eq!(after - before, 250);
    // A round trip and the last holder's shutdown, each right after a
    // burst that keeps the queue full, still go through.
    let handle = mdns.daemon().clone();
    let burst = || {
        for _ in 0..3 {
            for rec in &recs {
                let _queued = handle.unregister(&format!("{}.{}", rec.instance, rec.service));
            }
        }
    };
    burst();
    mdns.settle(GOODBYE_WAIT)
        .expect("a round trip behind a full queue");
    burst();
    drop(mdns);
    assert!(handle.get_metrics().is_err(), "the daemon still runs");
}

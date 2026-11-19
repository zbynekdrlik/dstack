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
    // The ended one's entry is gone: the new one is found, not a third.
    let found = Mdns::shared(ip).unwrap();
    assert!(Arc::ptr_eq(&next, &found), "the running daemon is shared");
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
/// commands: a withdrawal of more than that is confirmed whole.
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
    // registration burst, still go through. On a loaded box the queue can
    // be full at the burst's end (`Error::Again`, waited out); on an idle
    // dev1 the daemon keeps up, so this does not prove that wait (19.11.2026:
    // it passed with the wait removed, 3 of 3).
    a.announce(&recs).unwrap();
    mdns.settle(GOODBYE_WAIT)
        .expect("a round trip behind a full queue");
    a.announce(&[]).unwrap();
    a.announce(&recs).unwrap();
    let handle = mdns.daemon().clone();
    drop(mdns);
    assert!(handle.get_metrics().is_err(), "the daemon still runs");
}

/// A listener on the running daemon (`traffic::neighbours`' discovery,
/// dstack #442): found on its address, never opened, and holding no
/// daemon: the last holder's drop still shuts it down.
#[test]
fn a_listener_on_the_running_daemon_opens_none_and_keeps_none() {
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let ip = Ipv4Addr::LOCALHOST;
    assert!(Mdns::listen_on(ip, Heard::All).is_none(), "none runs yet");
    assert!(
        running(&mut lock(&SHARED), ip).is_none(),
        "the look opened none"
    );
    let mdns = Mdns::shared(ip).unwrap();
    let before = mdns.listeners();
    let heard = Mdns::listen_on(ip, Heard::All).expect("the running daemon");
    assert_eq!(mdns.listeners(), before + 1);
    assert!(
        Mdns::listen_on(Ipv4Addr::new(127, 0, 0, 2), Heard::All).is_none(),
        "another address's"
    );
    let sender = UdpSocket::bind((ip, 0)).unwrap();
    socket2::SockRef::from(&sender)
        .set_multicast_if_v4(&ip)
        .unwrap();
    let mut response = RESPONSE;
    response[..2].copy_from_slice(&0x4422u16.to_be_bytes());
    heard_from(&heard, &sender, &response);
    let handle = mdns.daemon().clone();
    drop(mdns);
    assert!(
        handle.get_metrics().is_err(),
        "the listener kept the daemon"
    );
    assert!(Mdns::listen_on(ip, Heard::All).is_none(), "it ended");
    drop(heard);
}

/// The #444 trial's warning: only where the OS hands an address-bound
/// socket no group datagram (Linux).
#[test]
fn a_bound_daemon_is_warned_about_only_where_it_hears_no_group() {
    let note = bound_note(false).expect("a warning where the group is not delivered");
    assert!(note.contains("hears no group query"), "{note}");
    assert!(note.contains("#444"), "{note}");
    assert_eq!(bound_note(true), None);
}

/// The datagrams `l` hands on within `wait`, from `me` only.
fn heard_from_within(l: &Listener, me: SocketAddrV4, wait: Duration) -> Vec<Vec<u8>> {
    let deadline = Instant::now() + wait;
    let mut out = Vec::new();
    while !deadline.saturating_duration_since(Instant::now()).is_zero() {
        if let Ok(d) = l.recv_timeout(Duration::from_millis(10))
            && d.from == me
        {
            out.push(d.bytes);
        }
    }
    out
}

/// The #444 trial on loopback: the daemon's socket bound to the address
/// (`mdns_bind_address`) hears a unicast query to `<address>:5353`,
/// answers it from that address and port, and hears the group only where
/// the OS delivers it to an address-bound socket (Windows; Linux never).
/// The device then needs no responder of its own. A later device on the
/// address shares it whatever it asks.
#[test]
fn a_daemon_bound_to_its_address_hears_unicast_and_answers_from_it() {
    use crate::announce::{CHAN_SERVICE, ServiceRecord};
    use crate::subscribe::instance;
    crate::test_log::undecided();
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let ip = Ipv4Addr::LOCALHOST;
    let mut opened = None;
    let warns = crate::test_log::capture_warnings(|| {
        opened = Some(Mdns::shared_bound(ip, true).unwrap());
    });
    let mdns = opened.unwrap();
    assert!(mdns.bound());
    assert!(
        !mdns.own_responder_needed(),
        "the bound socket answers itself"
    );
    assert_eq!(
        warns.contains("hears no group query"),
        !GROUP_TO_BOUND_SOCKET,
        "{warns}"
    );
    let heard = mdns.listen(Heard::Queries);
    let record = ServiceRecord {
        service: CHAN_SERVICE,
        instance: "TX 1@mdns-test-bound".to_string(),
        host: "mdns-test-bound.local.".to_string(),
        ip,
        port: 4455,
        txt: vec!["id=1".to_string()],
    };
    let mut a = Announcer::new(mdns.daemon().clone());
    a.announce(std::slice::from_ref(&record)).unwrap();
    let full = format!("{}.{}", record.instance, CHAN_SERVICE);
    let client = UdpSocket::bind((ip, 0)).unwrap();
    socket2::SockRef::from(&client)
        .set_multicast_if_v4(&ip)
        .unwrap();
    client
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();
    let SocketAddr::V4(me) = client.local_addr().unwrap() else {
        panic!("IPv4");
    };
    // To the group, then to the address, each round: the address's query
    // is heard on both OSes, the group's only where it is delivered.
    let group = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
    let to_group = instance::query(0x4441, &full).unwrap();
    let to_address = instance::query(0x4442, &full).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut answer, mut group_heard, mut address_heard) = (None, false, false);
    while answer.is_none() || !address_heard {
        assert!(
            Instant::now() < deadline,
            "the bound daemon never answered (heard the address's query: {address_heard})"
        );
        client.send_to(&to_group, group).unwrap();
        client
            .send_to(&to_address, SocketAddrV4::new(ip, 5353))
            .unwrap();
        for bytes in heard_from_within(&heard, me, Duration::from_millis(50)) {
            group_heard |= bytes == to_group;
            address_heard |= bytes == to_address;
        }
        let mut buf = [0u8; 9000];
        while let Ok((n, from)) = client.recv_from(&mut buf) {
            if instance::read(&buf[..n], &full).is_some_and(|f| f.srv.is_some()) {
                answer = Some(from);
            }
        }
    }
    // A group query sent in the last round is heard within this wait
    // where the OS delivers it.
    for bytes in heard_from_within(&heard, me, Duration::from_millis(200)) {
        group_heard |= bytes == to_group;
    }
    assert_eq!(
        answer,
        Some(SocketAddr::V4(SocketAddrV4::new(ip, 5353))),
        "answered from the bound address and port"
    );
    assert_eq!(group_heard, GROUP_TO_BOUND_SOCKET, "the group's query");
    mdns.withdraw(&mut a, GOODBYE_WAIT).unwrap();
    drop(heard);
    let mut again = None;
    let warns = crate::test_log::capture_warnings(|| {
        again = Some(Mdns::shared_bound(ip, false).unwrap());
    });
    let again = again.unwrap();
    assert!(Arc::ptr_eq(&mdns, &again), "one socket an address");
    assert!(again.bound(), "the running daemon kept as it is");
    assert!(
        warns.contains("differs from the running mDNS daemon"),
        "{warns}"
    );
    let warns = crate::test_log::capture_warnings(|| {
        drop(Mdns::shared_bound(ip, true).unwrap());
    });
    assert!(!warns.contains("differs"), "the same form: {warns}");
    let handle = mdns.daemon().clone();
    drop((again, mdns));
    assert!(handle.get_metrics().is_err(), "the daemon still runs");
}

/// Off (the default), the daemon's socket is the wildcard's and a Windows
/// device keeps its own responder, as before #444.
#[test]
fn the_default_daemon_is_unbound_and_needs_the_responder() {
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let mdns = Mdns::shared(Ipv4Addr::LOCALHOST).unwrap();
    assert!(!mdns.bound());
    assert!(mdns.own_responder_needed());
}

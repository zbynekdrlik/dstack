// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #442: the taps on the daemon's socket, and the process's daemon
//! on an address.

use super::*;

#[cfg(target_os = "linux")]
use std::collections::BTreeSet;
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

/// `/proc/net/udp`'s hex of 127.0.0.1 (a row's local address).
#[cfg(target_os = "linux")]
const LOOPBACK_HEX: &str = "0100007F";

/// `/proc/net/udp`'s hex of 0.0.0.0, the wildcard daemon's address.
#[cfg(target_os = "linux")]
const WILDCARD_HEX: &str = "00000000";

/// How long [`rows_5353_until`] reads the table for a row it waits on.
#[cfg(target_os = "linux")]
const TABLE_BOUND: Duration = Duration::from_secs(5);

/// The inodes of this process's open sockets, exact: `/proc/self/fd` is
/// listed by fd number, so a socket closed or opened meanwhile never hides
/// another. A socket opened again is a new inode; a socket closed is gone.
#[cfg(target_os = "linux")]
fn socket_inodes() -> BTreeSet<String> {
    let mut ours = BTreeSet::new();
    for fd in std::fs::read_dir("/proc/self/fd").unwrap().flatten() {
        if let Ok(link) = std::fs::read_link(fd.path())
            && let Some(inode) = link
                .to_string_lossy()
                .strip_prefix("socket:[")
                .and_then(|s| s.strip_suffix(']'))
        {
            ours.insert(inode.to_string());
        }
    }
    ours
}

/// One read of `/proc/net/udp`'s rows on port 5353 (any process's): the
/// local address in hex with the socket's inode. The read is NOT exact
/// (#455): the kernel hands the table out a page (31 rows) a `read`, and
/// starts each `read` by walking the table again to the count of rows it
/// gave, so every row listed earlier that closes between two `read`s
/// skips a later one (a row inserted earlier repeats one, which the set
/// folds). A full lib run's other tests open and close sockets all the
/// time, and the reopen test once read no row at all for 127.0.0.1:5353.
/// So never judge a socket gone by its row's absence alone: wait for a
/// row that is there ([`rows_5353_until`]) and judge a close by
/// [`socket_inodes`].
#[cfg(target_os = "linux")]
fn rows_5353() -> BTreeSet<(String, String)> {
    std::fs::read_to_string("/proc/net/udp")
        .unwrap()
        .lines()
        .skip(1)
        .filter_map(|line| {
            let cols: Vec<&str> = line.split_whitespace().collect();
            let (addr, port) = cols.get(1)?.split_once(':')?;
            let inode = *cols.get(9)?;
            (port == "14E9").then(|| (addr.to_string(), inode.to_string()))
        })
        .collect()
}

/// The inodes among `ours` ([`socket_inodes`]) that `rows` lists on `addr`.
#[cfg(target_os = "linux")]
fn ours_on(
    rows: &BTreeSet<(String, String)>,
    ours: &BTreeSet<String>,
    addr: &str,
) -> BTreeSet<String> {
    rows.iter()
        .filter(|(a, inode)| a == addr && ours.contains(inode))
        .map(|(_, inode)| inode.clone())
        .collect()
}

/// Reads [`rows_5353`] every 5 ms until `holds` is true of a read, at most
/// [`TABLE_BOUND`], and returns that read; fails with the last read. Only
/// a row that is there can be waited for: a skipped row comes back in a
/// later read, a gone one never.
#[cfg(target_os = "linux")]
fn rows_5353_until(
    what: &str,
    holds: impl Fn(&BTreeSet<(String, String)>) -> bool,
) -> BTreeSet<(String, String)> {
    let deadline = Instant::now() + TABLE_BOUND;
    loop {
        let rows = rows_5353();
        if holds(&rows) {
            return rows;
        }
        assert!(
            !deadline.saturating_duration_since(Instant::now()).is_zero(),
            "{what}: never in /proc/net/udp within {TABLE_BOUND:?}; the last read's 5353 rows: {rows:?}"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// The #444 trial on loopback: the daemon's socket bound to the address
/// (`mdns_bind_address`; on Linux read in `/proc/net/udp`) hears a unicast
/// query to `<address>:5353`, answers it (from `<address>:5353`, which on
/// loopback a wildcard socket's reply also comes from), and hears the
/// group only where the OS delivers it to an address-bound socket
/// (Windows; Linux never). The device then needs no responder of its own.
/// A later device on the address shares it whatever it asks.
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
    // A bound socket is opened before `shared_bound` returns: one of the
    // sockets open then is on 127.0.0.1:5353 (its row waited for, #455).
    #[cfg(target_os = "linux")]
    {
        let ours = socket_inodes();
        rows_5353_until("bound to 127.0.0.1:5353", |rows| {
            !ours_on(rows, &ours, LOOPBACK_HEX).is_empty()
        });
    }
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
    // Its thread opened its socket before it ran a command: none of this
    // process's 5353 sockets is bound to 127.0.0.1 (no other unit test
    // binds 127.0.0.1:5353).
    mdns.settle(GOODBYE_WAIT).expect("the daemon runs");
    // Judged on a read that lists the wildcard's own row (#455: a read
    // can skip rows).
    #[cfg(target_os = "linux")]
    {
        let ours = socket_inodes();
        let rows = rows_5353_until("the wildcard daemon's 0.0.0.0:5353", |rows| {
            !ours_on(rows, &ours, WILDCARD_HEX).is_empty()
        });
        assert!(
            ours_on(&rows, &ours, LOOPBACK_HEX).is_empty(),
            "the wildcard's: {rows:?}"
        );
    }
}

/// A bound socket whose bind fails (an address not on this host) is the
/// call's error: no daemon runs without a socket (#444's review).
#[test]
fn a_bound_daemon_whose_bind_fails_is_an_error() {
    // TEST-NET-1: never on a host; the bind fails before anything is sent.
    let nowhere = Ipv4Addr::new(192, 0, 2, 1);
    let e = Mdns::shared_bound(nowhere, true)
        .err()
        .expect("no daemon without its socket");
    assert!(e.contains("the mDNS daemon"), "{e}");
    assert!(e.contains("192.0.2.1:5353"), "{e}");
    assert!(Mdns::listen_on(nowhere, Heard::All).is_none(), "none kept");
}

/// A PTR query for `service` (QU bit clear).
fn ptr_query(id: u16, service: &str) -> Vec<u8> {
    use simple_dns::{CLASS, Name, Packet, QCLASS, QTYPE, Question, TYPE};
    let mut p = Packet::new_query(id);
    p.questions.push(Question::new(
        Name::new_unchecked(service.trim_end_matches('.')),
        QTYPE::TYPE(TYPE::PTR),
        QCLASS::CLASS(CLASS::IN),
        false,
    ));
    p.build_bytes_vec().unwrap()
}

/// `bytes` is a response with id `id` holding a PTR answer naming `instance`.
fn answers_ptr(bytes: &[u8], id: u16, instance: &str) -> bool {
    use simple_dns::Packet;
    use simple_dns::rdata::RData;
    Packet::parse(bytes).is_ok_and(|p| {
        p.id() == id
            && p.answers.iter().any(|r| {
                matches!(&r.rdata, RData::PTR(ptr)
                    if ptr.0.to_string().eq_ignore_ascii_case(instance.trim_end_matches('.')))
            })
    })
}

/// `bytes` is an answer to a direct query from 5353 with id `id`: no
/// question echoed and its SRV's TTL the full one (over the 10 s a legacy
/// answer is capped to), so not reshaped as a legacy one.
fn direct_shaped(bytes: &[u8], id: u16) -> bool {
    use simple_dns::Packet;
    use simple_dns::rdata::RData;
    Packet::parse(bytes).is_ok_and(|p| {
        p.id() == id
            && p.questions.is_empty()
            && p.answers
                .iter()
                .any(|r| matches!(r.rdata, RData::SRV(_)) && r.ttl > 10)
    })
}

/// Whether a datagram is the answer a test waits for.
type IsAnswer<'a> = &'a dyn Fn(&[u8]) -> bool;

/// The sources of the datagrams `client` reads within `wait` that
/// `is_answer` takes.
fn answers_within(client: &UdpSocket, wait: Duration, is_answer: IsAnswer) -> Vec<SocketAddr> {
    let deadline = Instant::now() + wait;
    let mut out = Vec::new();
    let mut buf = [0u8; 9000];
    while !deadline.saturating_duration_since(Instant::now()).is_zero() {
        if let Ok((n, from)) = client.recv_from(&mut buf)
            && is_answer(&buf[..n])
        {
            out.push(from);
        }
    }
    out
}

/// dstack #444: a direct query from port 5353 to the bound daemon's
/// address (RFC 6762 §5.5: sent to the address, not to the group) is
/// answered once, unicast to its source address and port, from
/// `<address>:5353`; a PTR one is not deferred to the group. Up to
/// v0.6.196 the daemon decided by the source port alone and answered it to
/// the group, which this querier (bound to 127.0.0.2:5353, joined to
/// nothing) never hears. A legacy query (another source port) is answered
/// to its source already (`a_daemon_bound_to_its_address_hears_unicast_and_answers_from_it`).
#[test]
fn a_bound_daemon_answers_a_direct_query_from_5353_to_its_source() {
    use crate::announce::{CHAN_SERVICE, ServiceRecord};
    use crate::subscribe::instance;
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let ip = Ipv4Addr::LOCALHOST;
    let mdns = Mdns::shared_bound(ip, true).unwrap();
    let heard = mdns.listen(Heard::Queries);
    let record = ServiceRecord {
        service: CHAN_SERVICE,
        instance: "TX 1@mdns-test-direct".to_string(),
        host: "mdns-test-direct.local.".to_string(),
        ip,
        port: 4455,
        txt: vec!["id=1".to_string()],
    };
    let mut a = Announcer::new(mdns.daemon().clone());
    a.announce(std::slice::from_ref(&record)).unwrap();
    let full = format!("{}.{}", record.instance, CHAN_SERVICE);
    let querier = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 2), 5353);
    let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).unwrap();
    sock.set_reuse_address(true).unwrap();
    #[cfg(unix)]
    sock.set_reuse_port(true).unwrap();
    sock.bind(&querier.into()).unwrap();
    let client: UdpSocket = sock.into();
    client
        .set_read_timeout(Some(Duration::from_millis(10)))
        .unwrap();
    let to = SocketAddrV4::new(ip, 5353);
    let srv_query = instance::query(0x4443, &full).unwrap();
    let ptr = ptr_query(0x4444, CHAN_SERVICE);
    let srv_answer = |b: &[u8]| {
        instance::read(b, &full).is_some_and(|f| f.srv.is_some()) && direct_shaped(b, 0x4443)
    };
    let ptr_answer = |b: &[u8]| answers_ptr(b, 0x4444, &full);
    let cases: [(&str, &[u8], IsAnswer); 2] = [
        ("SRV/TXT", &srv_query, &srv_answer),
        ("PTR", &ptr, &ptr_answer),
    ];
    for (what, query, is_answer) in cases {
        // Until the record is announced; then one query counted alone.
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut tapped = false;
        loop {
            client.send_to(query, to).unwrap();
            tapped |= heard_from_within(&heard, querier, Duration::from_millis(20))
                .iter()
                .any(|b| b == query);
            if !answers_within(&client, Duration::from_millis(100), is_answer).is_empty() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the {what} query from {querier} to {to} was never answered to its source \
                 (the daemon heard it: {tapped})"
            );
        }
        // A PTR answer to the group would come 20-120 ms late: wait it out.
        answers_within(&client, Duration::from_millis(300), is_answer);
        client.send_to(query, to).unwrap();
        assert_eq!(
            answers_within(&client, Duration::from_millis(300), is_answer),
            [SocketAddr::V4(to)],
            "the {what} query: one answer, from the bound address and port"
        );
    }
    mdns.withdraw(&mut a, GOODBYE_WAIT).unwrap();
    drop(heard);
    let handle = mdns.daemon().clone();
    drop(mdns);
    assert!(handle.get_metrics().is_err(), "the daemon still runs");
}

/// dstack #444's reopen line: a bound daemon's watch (the #282 watch, on
/// the device's address) opens its socket again once the address is listed
/// again after a look found it gone, or on another interface index, with
/// one INFO line; never at the first look, while the address is gone or
/// while it stays. A failed reopen is warned about and asked again at the
/// next look.
#[test]
fn a_bound_daemon_s_watch_opens_its_socket_again_once_its_address_came_back() {
    use std::cell::Cell;
    crate::test_log::undecided();
    let ip = Ipv4Addr::LOCALHOST;
    let mut m = Membership::every(ip, Duration::from_secs(2));
    let t0 = Instant::now();
    let at = |s: u64| t0 + Duration::from_secs(s);
    let on = |index: u32| -> std::io::Result<Vec<Link>> {
        Ok(vec![Link {
            name: "lo".to_string(),
            ip,
            index: Some(index),
        }])
    };
    let gone = || -> std::io::Result<Vec<Link>> { Ok(Vec::new()) };
    let (opened, refused) = (Cell::new(0), Cell::new(0));
    let ok = || -> Result<Reopened, String> {
        opened.set(opened.get() + 1);
        Ok(Reopened {
            interfaces: 1,
            services: 4,
        })
    };
    let fails = || -> Result<Reopened, String> {
        refused.set(refused.get() + 1);
        Err("bind refused".to_string())
    };
    let mut looks = Vec::new();
    let mut warns = String::new();
    let lines = crate::test_log::capture_at(tracing::Level::INFO, || {
        looks.push(keep_bound(&mut m, at(0), || on(3), ip, ok));
        looks.push(keep_bound(&mut m, at(2), gone, ip, ok));
        looks.push(keep_bound(&mut m, at(4), || on(3), ip, ok));
        looks.push(keep_bound(&mut m, at(6), || on(3), ip, ok));
        looks.push(keep_bound(&mut m, at(8), || on(5), ip, ok));
        looks.push(keep_bound(&mut m, at(10), gone, ip, ok));
        warns = crate::test_log::capture_warnings(|| {
            looks.push(keep_bound(&mut m, at(12), || on(5), ip, fails));
        });
        looks.push(keep_bound(&mut m, at(14), || on(5), ip, ok));
        looks.push(keep_bound(&mut m, at(16), || on(5), ip, ok));
    });
    assert_eq!(
        looks,
        [false, false, true, false, true, false, false, true, false],
        "learnt, gone, back, stays, moved, gone, refused, asked again, stays"
    );
    assert_eq!((opened.get(), refused.get()), (3, 1));
    assert_eq!(lines.matches(BOUND_REOPENED).count(), 3, "{lines}");
    assert!(lines.contains("interface=lo"), "{lines}");
    assert!(lines.contains("new_index=5"), "{lines}");
    assert!(lines.contains("joined_interfaces=1"), "{lines}");
    assert!(lines.contains("announced_records=4"), "{lines}");
    assert_eq!(
        warns
            .matches("the bound daemon's socket not opened again")
            .count(),
        1,
        "{warns}"
    );
    assert!(warns.contains("bind refused"), "{warns}");
}

/// The daemon's count of announcements sent again so far (a registration's
/// second one, and since #444 a reopen's).
fn resends(m: &Mdns) -> i64 {
    m.settle(GOODBYE_WAIT)
        .unwrap()
        .get("register-resend")
        .copied()
        .unwrap_or(0)
}

/// Asks `full`'s SRV/TXT from `client` to the group and to `to` (the bound
/// daemon's address) every 50 ms until it is answered and the address's
/// query was handed on by `heard` (at most 10 s); `ids` tag the two
/// queries. The answer's source, and whether the group's query was handed
/// on too (another 200 ms waited for it).
fn asked(
    client: &UdpSocket,
    heard: &Listener,
    full: &str,
    to: SocketAddrV4,
    ids: (u16, u16),
) -> (SocketAddr, bool) {
    use crate::subscribe::instance;
    let SocketAddr::V4(me) = client.local_addr().unwrap() else {
        panic!("IPv4");
    };
    let group = SocketAddrV4::new(Ipv4Addr::new(224, 0, 0, 251), 5353);
    let to_group = instance::query(ids.0, full).unwrap();
    let to_address = instance::query(ids.1, full).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let (mut answer, mut group_heard, mut address_heard) = (None, false, false);
    while answer.is_none() || !address_heard {
        assert!(
            Instant::now() < deadline,
            "never answered (the address's query heard: {address_heard})"
        );
        client.send_to(&to_group, group).unwrap();
        client.send_to(&to_address, to).unwrap();
        for bytes in heard_from_within(heard, me, Duration::from_millis(50)) {
            group_heard |= bytes == to_group;
            address_heard |= bytes == to_address;
        }
        let mut buf = [0u8; 9000];
        while let Ok((n, from)) = client.recv_from(&mut buf) {
            if instance::read(&buf[..n], full).is_some_and(|f| f.srv.is_some()) {
                answer = Some(from);
            }
        }
    }
    for bytes in heard_from_within(heard, me, Duration::from_millis(200)) {
        group_heard |= bytes == to_group;
    }
    (answer.unwrap(), group_heard)
}

/// The test's own socket on `0.0.0.0:5353` joined to the group on `ip`
/// (beside the daemon's bound socket, `SO_REUSEADDR`/`SO_REUSEPORT`): it
/// reads what the daemon multicasts. A unicast to `<ip>:5353` still goes
/// to the daemon's socket, the more specific bind.
///
/// Non-blocking, never a read timeout (dstack #453): on Windows a blocking
/// `recvfrom` that ends on `SO_RCVTIMEO` can lose a datagram arriving as it
/// times out. On windows-2025 under load, two such listeners (10 ms) each
/// missed announcements the daemon had sent (`send_to` Ok every time) and
/// the other one read, 28 runs of 40, while a non-blocking one beside them
/// read every announcement in all 40.
fn group_listener(ip: Ipv4Addr) -> UdpSocket {
    let sock = socket2::Socket::new(socket2::Domain::IPV4, socket2::Type::DGRAM, None).unwrap();
    sock.set_reuse_address(true).unwrap();
    #[cfg(unix)]
    sock.set_reuse_port(true).unwrap();
    sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5353).into())
        .unwrap();
    sock.join_multicast_v4(&Ipv4Addr::new(224, 0, 0, 251), &ip)
        .unwrap();
    let sock: UdpSocket = sock.into();
    sock.set_nonblocking(true).unwrap();
    sock
}

/// How many announcements of `full` (a response from `from` with `full`'s
/// SRV) `l` reads within `wait`.
fn announcements(l: &UdpSocket, full: &str, from: SocketAddr, wait: Duration) -> usize {
    announcements_each(l, &[full], from, wait)[0]
}

/// Waits (at most 8 s) for an announcement of `full` on `l`, then until
/// none has come for 1.2 s, past the record's multicast rate limit of a
/// second (`MULTICAST_RATE_LIMIT_MILLIS`): every announcement of the
/// registration is then out, and the next is not held back.
fn quiet_after_announcements(l: &UdpSocket, full: &str, from: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while announcements(l, full, from, Duration::from_millis(100)) == 0 {
        assert!(
            Instant::now() < deadline,
            "the registration never announced"
        );
    }
    while announcements(l, full, from, Duration::from_millis(1200)) > 0 {
        assert!(Instant::now() < deadline, "the announcements never ended");
    }
}

/// dstack #444's reopen line through the real daemon on loopback: the
/// bound daemon's address removed and added again (a fake list, read every
/// 5 ms) opens its socket again once: a new socket on `127.0.0.1:5353`
/// (Linux: a new inode, the old one closed), the record still answered to
/// a query to the address, heard by a listener taken before the reopen,
/// announced once more, and the group heard where the OS hands it to a
/// bound socket (Windows: the new socket joined it). mbc's step 4a
/// (19.11.2026): the socket of v0.6.195 kept its address and heard nothing
/// for 2.5 min after the address came back.
#[test]
fn a_bound_daemon_opens_its_socket_again_after_its_address_came_back_with_its_records_and_taps() {
    use crate::announce::{CHAN_SERVICE, ServiceRecord};
    use crate::membership::fake::{Fake, until};
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let ip = Ipv4Addr::LOCALHOST;
    let fake = Fake::at(1);
    let mdns = Mdns::shared_listed(ip, true, fake.lister(ip, Duration::from_millis(5))).unwrap();
    let heard = mdns.listen(Heard::Queries);
    let record = ServiceRecord {
        service: CHAN_SERVICE,
        instance: "TX 1@mdns-test-reopen".to_string(),
        host: "mdns-test-reopen.local.".to_string(),
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
    let to = SocketAddrV4::new(ip, 5353);
    let wire = group_listener(ip);
    let (from, _) = asked(&client, &heard, &full, to, (0x4451, 0x4452));
    assert_eq!(from, SocketAddr::V4(to), "answered before the change");
    // The registration's announcements are on the wire and past the
    // record's rate limit (a record is multicast at most once a second,
    // RFC 6762 §6) before the address goes: the reopen's is then sent,
    // and the counts below are the reopen's alone.
    quiet_after_announcements(&wire, &full, SocketAddr::V4(to));
    let resent = resends(&mdns);
    // The bound socket's inode, from a read that lists it (#455: a read of
    // `/proc/net/udp` can skip rows).
    #[cfg(target_os = "linux")]
    let before = {
        let ours = socket_inodes();
        let rows = rows_5353_until("the bound socket", |rows| {
            !ours_on(rows, &ours, LOOPBACK_HEX).is_empty()
        });
        let on = ours_on(&rows, &ours, LOOPBACK_HEX);
        assert_eq!(on.len(), 1, "one socket on 127.0.0.1:5353: {rows:?}");
        on.into_iter().next().unwrap()
    };
    assert_eq!(mdns.reopens(), 0, "nothing changed yet");
    fake.remove();
    fake.read_more(2);
    assert_eq!(mdns.reopens(), 0, "never while the address is gone");
    fake.recreate(1);
    until("the bound socket opened again", || mdns.reopens() == 1);
    fake.read_more(2);
    assert_eq!(mdns.reopens(), 1, "once a return");
    // The reopen announced before it answered: its datagram is queued.
    assert_eq!(
        announcements(&wire, &full, SocketAddr::V4(to), Duration::from_millis(300)),
        1,
        "the record announced once on the wire after the reopen"
    );
    // The old socket closed: its inode is no open fd of the process (an
    // exact list). The new one is on the address: a read that lists a row
    // of the sockets open now on 127.0.0.1:5353 with another inode, and
    // only that one there (#455).
    #[cfg(target_os = "linux")]
    {
        let ours = socket_inodes();
        assert!(
            !ours.contains(&before),
            "the old socket closed: {before} still open"
        );
        let rows = rows_5353_until("a new socket on 127.0.0.1:5353", |rows| {
            ours_on(rows, &ours, LOOPBACK_HEX)
                .iter()
                .any(|inode| *inode != before)
        });
        let after = ours_on(&rows, &ours, LOOPBACK_HEX);
        assert_eq!(after.len(), 1, "one socket on 127.0.0.1:5353: {rows:?}");
    }
    let (from, group_heard) = asked(&client, &heard, &full, to, (0x4453, 0x4454));
    assert_eq!(
        from,
        SocketAddr::V4(to),
        "the record answered again from the bound address"
    );
    assert_eq!(
        group_heard, GROUP_TO_BOUND_SOCKET,
        "the new socket's group (joined again)"
    );
    assert_eq!(
        resends(&mdns) - resent,
        1,
        "its one record asked to announce once by the reopen"
    );
    mdns.withdraw(&mut a, GOODBYE_WAIT).unwrap();
    drop(heard);
    let handle = mdns.daemon().clone();
    drop(mdns);
    assert!(handle.get_metrics().is_err(), "the daemon still runs");
    let reads = fake.reads();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(fake.reads(), reads, "the watch ended with the daemon");
}

/// Off (the default), the wildcard daemon keeps no watch of its own: the
/// list is never read for it (the path of v0.6.197).
#[test]
fn the_wildcard_daemon_keeps_no_watch_of_its_address() {
    use crate::membership::fake::Fake;
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let ip = Ipv4Addr::LOCALHOST;
    let fake = Fake::at(1);
    let mdns = Mdns::shared_listed(ip, false, fake.lister(ip, Duration::from_millis(1))).unwrap();
    mdns.settle(GOODBYE_WAIT).expect("the daemon runs");
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(fake.reads(), 0, "no watch on the wildcard's daemon");
    assert_eq!(mdns.reopens(), 0);
}

/// How many announcements of each of `fulls` (a response from `from` with
/// that name's SRV) `l` (a [`group_listener`]) reads within `wait`; an
/// empty read waits 1 ms, and any other read error fails the test.
fn announcements_each(
    l: &UdpSocket,
    fulls: &[&str],
    from: SocketAddr,
    wait: Duration,
) -> Vec<usize> {
    use crate::subscribe::instance;
    let deadline = Instant::now() + wait;
    let mut n = vec![0; fulls.len()];
    let mut buf = [0u8; 9000];
    while !deadline.saturating_duration_since(Instant::now()).is_zero() {
        match l.recv_from(&mut buf) {
            Ok((len, src)) if src == from && !is_query(&buf[..len]) => {
                for (i, full) in fulls.iter().enumerate() {
                    if instance::read(&buf[..len], full).is_some_and(|f| f.srv.is_some()) {
                        n[i] += 1;
                    }
                }
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(e) => panic!("the group listener's read failed: {e}"),
        }
    }
    n
}

/// dstack #450: every record is announced twice at its registration, one
/// second apart (RFC 6762 §8.3), also one whose name has upper case, as
/// every Dante `TX 1@...` record. Up to v0.6.197 the second announcement
/// looked its record up by the original-case name, while the daemon keys
/// its records in lower case: such a record was announced once, and one
/// datagram lost on a busy leg left the device unseen until a query. An
/// all-lower-case record beside it is the control (two before and after).
#[test]
fn every_record_is_announced_twice_also_one_whose_name_has_upper_case() {
    use crate::announce::ServiceRecord;
    let _one = ONE_DAEMON.lock().unwrap_or_else(PoisonError::into_inner);
    let ip = Ipv4Addr::LOCALHOST;
    let mdns = Mdns::shared(ip).unwrap();
    // A token of the process in every name and a service type of its own:
    // parallel mutant jobs run this test at once on one box, each beside
    // the others' daemons on 5353, and a browse of a real type by any
    // process would add answers to the count (`tests/mdns_answer_only.rs`).
    let pid = std::process::id();
    let ty: &'static str = Box::leak(format!("_d450{pid:08x}._udp.local.").into_boxed_str());
    let record = |instance: &str, host: &str| ServiceRecord {
        service: ty,
        instance: instance.to_string(),
        host: format!("{host}.local."),
        ip,
        port: 4455,
        txt: vec!["id=1".to_string()],
    };
    let upper = record(
        &format!("TX 1@Mdns-Test-Case-{pid}"),
        &format!("mdns-test-case-upper-{pid}"),
    );
    let lower = record(
        &format!("tx 1@mdns-test-case-lower-{pid}"),
        &format!("mdns-test-case-lower-{pid}"),
    );
    let full_upper = format!("{}.{ty}", upper.instance);
    let full_lower = format!("{}.{ty}", lower.instance);
    let wire = group_listener(ip);
    let mut a = Announcer::new(mdns.daemon().clone());
    a.announce(&[upper, lower]).unwrap();
    // The probes (3 x 250 ms), the first announcement, the second one
    // 1.1-1.15 s after it, and time to spare for a third that must not come.
    let counts = announcements_each(
        &wire,
        &[&full_upper, &full_lower],
        SocketAddr::V4(SocketAddrV4::new(ip, 5353)),
        Duration::from_secs(5),
    );
    assert_eq!(
        counts,
        [2, 2],
        "announcements of the upper-case record and of the lower-case one"
    );
    mdns.withdraw(&mut a, GOODBYE_WAIT).unwrap();
}

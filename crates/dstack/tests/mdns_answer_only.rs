// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #319: our mDNS daemon only answers and announces. An old dtray
//! with standing mdns-sd browses refreshed every learned `_netaudio-arc`
//! record at 80 % of its 120 s TTL, so every 96 s it sent multicast
//! queries, every Dante host answered in the same second, and a receiver
//! on the network counted late packets. The vendored daemon
//! (`vendor/mdns-sd/DSTACK.md`) has no browse, cache browse, hostname
//! resolve or verify outside its own unit tests, so no build can ask it to
//! query or refresh anything.
//!
//! The test runs the daemon as a device runs it (`host.rs`: only the
//! routed address's interface, our records through `announce::Announcer`),
//! with the cache taking every response it hears (`accept_unsolicited`, a
//! superset of the device's setting). For 2.5 TTLs of a 4 s TTL, the
//! scaled 5 minutes of a Dante record's 120 s TTL, it hears other devices'
//! records (each due for refresh at 3.2 s, re-heard once a TTL), a
//! controller's multicast question for our type, and a subscription's
//! legacy-unicast lookup of our instance each second (`subscribe::instance`
//! asks from its own port). Every message the daemon sends about the test's
//! names must be an answer or announcement, or a probe for our own names
//! (RFC 6762 §8.1, a query with an authority section): never a query of its
//! own. This replaces `mdns_refresh.rs` (dstack #1), which drove the
//! refresh pass through `browse`, now gone.
//!
//! The test's names carry a per-process token and a type of its own, so a
//! process elsewhere on the box (another account's device) never mixes in.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::net::{IpAddr, Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use dstack::announce::{Announcer, ServiceRecord};
use mdns_sd::{IfKind, ServiceDaemon};
use socket2::{Domain, Protocol, Socket, Type};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
/// The TTL of the other devices' records: 1/30 of Dante's 120 s.
const TTL_S: u32 = 4;
/// How long the test runs: 2.5 TTLs, the scaled 5 minutes.
const RUN: Duration = Duration::from_millis(2500 * TTL_S as u64);
/// Other devices on the network.
const OTHERS: usize = 4;

/// The box's routed IPv4 (sends nothing).
fn routed_ip() -> Ipv4Addr {
    let s = UdpSocket::bind("0.0.0.0:0").unwrap();
    s.connect("192.0.2.1:9").unwrap();
    match s.local_addr().unwrap().ip() {
        IpAddr::V4(ip) => ip,
        other => panic!("no routed IPv4: {other}"),
    }
}

/// A socket on 5353 in the mDNS group on `ip`'s interface, beside the
/// daemon's: it hears what the daemon sends and speaks for the network.
fn network(ip: Ipv4Addr) -> UdpSocket {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
    s.set_reuse_address(true).unwrap();
    s.set_reuse_port(true).unwrap();
    s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5353).into())
        .unwrap();
    s.join_multicast_v4(&GROUP, &ip).unwrap();
    s.set_multicast_if_v4(&ip).unwrap();
    s.set_multicast_loop_v4(true).unwrap();
    let s: UdpSocket = s.into();
    s.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    s
}

fn name(out: &mut Vec<u8>, dotted: &str) {
    for label in dotted.trim_end_matches('.').split('.') {
        out.push(label.len() as u8);
        out.extend_from_slice(label.as_bytes());
    }
    out.push(0);
}

fn record(out: &mut Vec<u8>, owner: &str, ty: u16, class: u16, rdata: &[u8]) {
    name(out, owner);
    out.extend_from_slice(&ty.to_be_bytes());
    out.extend_from_slice(&class.to_be_bytes());
    out.extend_from_slice(&TTL_S.to_be_bytes());
    out.extend_from_slice(&(rdata.len() as u16).to_be_bytes());
    out.extend_from_slice(rdata);
}

/// Another device's announcement of one instance: PTR, SRV, TXT and A.
fn response(ty: &str, instance: &str, host: &str, ip: Ipv4Addr) -> Vec<u8> {
    let mut out = vec![0, 0, 0x84, 0, 0, 0, 0, 4, 0, 0, 0, 0];
    let mut ptr = Vec::new();
    name(&mut ptr, instance);
    record(&mut out, ty, 12, 1, &ptr);
    let mut srv = vec![0, 0, 0, 0, 0x12, 0x34];
    name(&mut srv, host);
    record(&mut out, instance, 33, 0x8001, &srv);
    record(&mut out, instance, 16, 0x8001, &[0]);
    record(&mut out, host, 1, 0x8001, &ip.octets());
    out
}

/// A query of `questions` (name, type), with query id `id`.
fn query(id: u16, questions: &[(&str, u16)]) -> Vec<u8> {
    let mut out = id.to_be_bytes().to_vec();
    out.extend_from_slice(&[0, 0]);
    out.extend_from_slice(&(questions.len() as u16).to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]);
    for (q, ty) in questions {
        name(&mut out, q);
        out.extend_from_slice(&ty.to_be_bytes());
        out.extend_from_slice(&1u16.to_be_bytes());
    }
    out
}

/// The name at `at` in `msg`, compression pointers followed (RFC 1035
/// 4.1.4), and the offset after it in place.
fn read_name(msg: &[u8], mut at: usize) -> (String, usize) {
    let (mut labels, mut end, mut hops) = (Vec::new(), None, 0);
    while let Some(&len) = msg.get(at) {
        if len & 0xc0 == 0xc0 {
            end.get_or_insert(at + 2);
            at = usize::from(u16::from_be_bytes([len & 0x3f, msg[at + 1]]));
            hops += 1;
            assert!(hops < 32, "a pointer loop");
            continue;
        }
        if len == 0 {
            end.get_or_insert(at + 1);
            break;
        }
        let label = &msg[at + 1..at + 1 + usize::from(len)];
        labels.push(String::from_utf8_lossy(label).into_owned());
        at += 1 + usize::from(len);
    }
    (labels.join("."), end.unwrap_or(at))
}

/// The question names of a message.
fn questions(msg: &[u8]) -> Vec<String> {
    let count = u16::from_be_bytes([msg[4], msg[5]]);
    let mut at = 12;
    let mut out = Vec::new();
    for _ in 0..count {
        let (qname, end) = read_name(msg, at);
        out.push(qname.to_ascii_lowercase());
        at = end + 4;
    }
    out
}

/// What a message the daemon sent is, by RFC 6762: an answer or
/// announcement, a probe for `own` names (a query with an authority
/// section), or a query of its own.
#[derive(Debug, PartialEq)]
enum Sent {
    Answer,
    Probe,
    Query(Vec<String>),
}

fn classify(msg: &[u8], own: &[String]) -> Sent {
    if msg[2] & 0x80 != 0 {
        return Sent::Answer;
    }
    let qs = questions(msg);
    let authority = u16::from_be_bytes([msg[8], msg[9]]);
    if authority > 0 && qs.iter().all(|q| own.contains(q)) {
        Sent::Probe
    } else {
        Sent::Query(qs)
    }
}

#[test]
fn the_classifier_tells_answers_probes_and_queries() {
    let own = vec!["dev.t._x._udp.local".to_string()];
    let mut answer = query(0, &[]);
    answer[2] = 0x84;
    assert_eq!(classify(&answer, &own), Sent::Answer);
    let mut probe = query(0, &[("dev.t._x._udp.local.", 255)]);
    probe[9] = 1;
    assert_eq!(classify(&probe, &own), Sent::Probe);
    // A query without an authority section is a query, own name or not.
    let plain = query(0, &[("dev.t._x._udp.local.", 33)]);
    assert_eq!(
        classify(&plain, &own),
        Sent::Query(vec!["dev.t._x._udp.local".into()])
    );
    // A "probe" for a name not ours is a query too.
    let mut foreign = query(0, &[("other.t._x._udp.local.", 255)]);
    foreign[9] = 1;
    assert!(matches!(classify(&foreign, &own), Sent::Query(_)));
}

#[test]
fn the_daemon_sends_no_query_of_its_own_in_five_scaled_minutes() {
    let ip = routed_ip();
    let token = format!("dstk{:08x}", std::process::id() ^ 0x0319_a0a0);
    let ty: &'static str = Box::leak(format!("_{token}._udp.local.").into_boxed_str());
    let net = network(ip);
    let lookup = UdpSocket::bind(SocketAddrV4::new(ip, 0)).unwrap();
    lookup
        .set_read_timeout(Some(Duration::from_millis(20)))
        .unwrap();

    // The device's daemon, as host.rs sets it up.
    let daemon = ServiceDaemon::new().expect("the mDNS daemon");
    daemon.disable_interface(IfKind::All).unwrap();
    daemon
        .enable_interface(IfKind::Addr(IpAddr::V4(ip)))
        .unwrap();
    daemon.accept_unsolicited(true).unwrap();
    let ours = ServiceRecord {
        service: ty,
        instance: format!("dev{token}"),
        host: format!("dev{token}.local."),
        ip,
        port: 4440,
        txt: vec!["arcp_vers=2.7.41".into()],
    };
    let own_instance = format!("dev{token}.{ty}");
    let own = vec![
        own_instance.trim_end_matches('.').to_ascii_lowercase(),
        ours.host.trim_end_matches('.').to_ascii_lowercase(),
    ];
    let mut announcer = Announcer::new(daemon.clone());
    announcer
        .announce(std::slice::from_ref(&ours))
        .expect("our records announced");

    let group = SocketAddrV4::new(GROUP, 5353);
    let ours_at = SocketAddrV4::new(ip, 5353);
    // What the test itself sends to the group (looped back to `net`).
    let mut mine: HashSet<Vec<u8>> = HashSet::new();
    let send = |mine: &mut HashSet<Vec<u8>>, pkt: Vec<u8>| {
        net.send_to(&pkt, group).unwrap();
        mine.insert(pkt);
    };

    let (mut answers, mut probes, mut queries) = (0, 0, Vec::new());
    let mut unicast_answers = 0;
    let mut buf = [0u8; 9000];
    let t0 = Instant::now();
    let (mut heard_at, mut asked_at) = (None::<Instant>, None::<Instant>);
    let mut id = 1u16;
    while t0.elapsed() < RUN {
        // Other devices re-announce once a TTL, a little apart.
        if heard_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(u64::from(TTL_S))) {
            for i in 0..OTHERS {
                let instance = format!("o{i}{token}.{ty}");
                let host = format!("o{i}{token}.local.");
                send(&mut mine, response(ty, &instance, &host, ip));
            }
            heard_at = Some(Instant::now());
        }
        // A controller's question for our type and a subscription's
        // lookup of our instance (legacy unicast), each second.
        if asked_at.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
            send(&mut mine, query(0, &[(ty, 12)]));
            let lookup_q = query(id, &[(&own_instance, 33), (&own_instance, 16)]);
            lookup.send_to(&lookup_q, group).unwrap();
            id = id.wrapping_add(1);
            asked_at = Some(Instant::now());
        }
        if let Ok((n, from)) = lookup.recv_from(&mut buf)
            && from == std::net::SocketAddr::V4(ours_at)
            && n >= 12
            && buf[2] & 0x80 != 0
        {
            unicast_answers += 1;
        }
        let Ok((n, from)) = net.recv_from(&mut buf) else {
            continue;
        };
        let msg = &buf[..n];
        let about_us = msg.windows(token.len()).any(|w| w == token.as_bytes());
        if n < 12 || from != std::net::SocketAddr::V4(ours_at) || !about_us || mine.contains(msg) {
            continue;
        }
        match classify(msg, &own) {
            Sent::Answer => answers += 1,
            Sent::Probe => probes += 1,
            Sent::Query(qs) => queries.push(qs),
        }
    }
    let _ = daemon.shutdown();
    assert!(
        queries.is_empty(),
        "the daemon sent queries of its own: {queries:?} ({answers} answers, {probes} probes)"
    );
    // The daemon was live: it announced and answered the controller each
    // second, and answered the subscription's lookups.
    assert!(
        answers >= 5,
        "answers and announcements on the wire: {answers}"
    );
    assert!(
        unicast_answers >= 5,
        "legacy-unicast answers to the lookups: {unicast_answers}"
    );
}

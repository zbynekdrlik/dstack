// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #1 (dasio, FOH 5.11.2026): mdns-sd queried each cached instance
//! in its own packet at exactly 80 % of the record's TTL. Records learned
//! together re-queried together: every 96 s each of FOH's two device
//! processes sent about 60 queries in one second, and the answers made a
//! burst of over 500 packets. The vendored copy (`vendor/mdns-sd/DSTACK.md`)
//! asks every question due in one refresh pass in one message, and takes
//! along records whose refresh is due within 2 % of their TTL.
//!
//! The test feeds a browsing daemon a few instances, each in its own
//! response a little apart (as devices answer), with a 10 s TTL, nobody
//! answering the refreshes. It counts the queries for them on the wire
//! until the records expire: one message per refresh stage (80, 85, 90 and
//! 95 %), never one per instance. The daemon sends on every interface; the
//! test counts the copies from the routed address only, and only messages
//! with an instance's question (the browse sends PTR queries of its own).

#![cfg(target_os = "linux")]

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::time::{Duration, Instant};

use socket2::{Domain, Protocol, Socket, Type};

const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 251);
const TTL_S: u32 = 10;
const INSTANCES: usize = 4;

/// The box's routed IPv4 (sends nothing).
fn routed_ip() -> Ipv4Addr {
    let s = UdpSocket::bind("0.0.0.0:0").unwrap();
    s.connect("192.0.2.1:9").unwrap();
    match s.local_addr().unwrap().ip() {
        std::net::IpAddr::V4(ip) => ip,
        other => panic!("no routed IPv4: {other}"),
    }
}

/// A socket on 5353 in the mDNS group on `ip`'s interface, as a second
/// listener beside the daemon's.
fn listener(ip: Ipv4Addr) -> UdpSocket {
    let s = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP)).unwrap();
    s.set_reuse_address(true).unwrap();
    s.set_reuse_port(true).unwrap();
    s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 5353).into())
        .unwrap();
    s.join_multicast_v4(&GROUP, &ip).unwrap();
    s.set_multicast_if_v4(&ip).unwrap();
    s.set_multicast_loop_v4(true).unwrap();
    let s: UdpSocket = s.into();
    s.set_read_timeout(Some(Duration::from_millis(50))).unwrap();
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

/// A device's answer for one instance: PTR, SRV, TXT and its host's A.
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

/// How many of a query's questions name one of the test's instances
/// (`i<N>.` and the token).
fn instance_questions(msg: &[u8], token: &str) -> usize {
    let count = u16::from_be_bytes([msg[4], msg[5]]);
    let mut at = 12;
    let mut n = 0;
    for _ in 0..count {
        let (qname, end) = read_name(msg, at);
        at = end + 4;
        let instance =
            qname.starts_with('i') && qname[1..].starts_with(|c: char| c.is_ascii_digit());
        if instance && qname.contains(token) {
            n += 1;
        }
    }
    n
}

#[test]
fn a_refresh_pass_asks_every_due_question_in_one_message() {
    let ip = routed_ip();
    let token = format!("dstk{:08x}", std::process::id() ^ 0x5eed_1e55);
    let ty = format!("_{token}._udp.local.");
    let host = format!("h{token}.local.");
    let rx = listener(ip);
    let daemon = mdns_sd::ServiceDaemon::new().expect("the mDNS daemon");
    let _events = daemon.browse(&ty).expect("browse");
    std::thread::sleep(Duration::from_millis(300));
    // Drain the browse's own queries.
    let mut buf = [0u8; 9000];
    while rx.recv_from(&mut buf).is_ok() {}

    for i in 0..INSTANCES {
        let instance = format!("i{i}.{ty}");
        let pkt = response(&ty, &instance, &host, ip);
        rx.send_to(&pkt, SocketAddrV4::new(GROUP, 5353)).unwrap();
        std::thread::sleep(Duration::from_millis(50));
    }

    let (mut messages, mut questions) = (0, 0);
    let t0 = Instant::now();
    while t0.elapsed() < Duration::from_secs(u64::from(TTL_S) + 1) {
        let Ok((n, from)) = rx.recv_from(&mut buf) else {
            continue;
        };
        // The daemon sends on every interface; count the routed one's copy.
        let mine = from.ip() == std::net::IpAddr::V4(ip);
        let is_query = n >= 12 && buf[2] & 0x80 == 0;
        // Instances asked about (the browse's own PTR queries ask none).
        let asked = if mine && is_query {
            instance_questions(&buf[..n], &token)
        } else {
            0
        };
        if asked > 0 {
            messages += 1;
            questions += asked;
        }
    }
    let _ = daemon.shutdown();
    // A stage asks every instance's SRV and TXT.
    assert!(
        questions >= 2 * INSTANCES,
        "a refresh stage on the wire: {messages} messages, {questions} questions"
    );
    assert!(
        messages <= 4,
        "one message per refresh stage: {messages} messages, {questions} questions"
    );
}

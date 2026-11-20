// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #454: a UDP read that waits at most a while
//! (`dstack::recv::recv_from_within`) loses no datagram under load, on
//! every OS. On Windows a blocking read that ends on `SO_RCVTIMEO` can lose
//! a datagram arriving as it times out (Microsoft: "If a blocking receive
//! call times out, the connection is in an indeterminate state"); #453's
//! windows-2025 runs and #454's probe (run 44661855528, 27 rounds of 300
//! datagrams, 8 busy threads) lost a datagram at 0.5-6 % of the timeouts
//! that fired at 10 and 50 ms waits, multicast and unicast alike, and a
//! non-blocking read waited on with `WSAPoll` none. Against the timeout
//! read this test failed 15 of 15 there (run 44663445297). On Linux, where
//! the helper keeps the read timeout, it is the measurement that the
//! timeout read loses nothing.
//!
//! The load is the test's own: busy threads at normal priority for the
//! run. Its own binary, so cargo runs no other test beside it. Readers at
//! the stack's waits (10 ms as #453's listener, 50 ms as discovery's
//! `READ_WAIT`), on a multicast group (as discovery and the heartbeat
//! listener) and on unicast sockets (as the query and request loops),
//! each read every datagram of a sender spaced 0-20 ms apart. Each datagram
//! carries the process id: another process's run on the same group and
//! port is not counted.

use std::net::{Ipv4Addr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Datagrams a run (about 6 s at 10 ms apart on average).
const DATAGRAMS: u32 = 600;
/// The busy threads beside the readers (8, as #453's PowerShell jobs).
const BUSY: usize = 8;
/// After the last send, a reader ends after this long with nothing.
const QUIET: Duration = Duration::from_millis(500);
/// A group of the test's own (administratively scoped), on loopback.
const GROUP: Ipv4Addr = Ipv4Addr::new(239, 255, 45, 4);

/// What one reader heard: each sequence number's count, its waits that
/// ended with nothing, and the shortest of those (never under its wait).
struct Heard {
    name: String,
    wait: Duration,
    counts: Vec<u32>,
    empty_waits: u64,
    shortest_empty: Duration,
}

/// A socket on `GROUP`:`port`, joined on 127.0.0.1.
fn group_member(port: u16) -> UdpSocket {
    let s = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    s.set_reuse_address(true).unwrap();
    s.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, port).into())
        .unwrap();
    s.join_multicast_v4(&GROUP, &Ipv4Addr::LOCALHOST).unwrap();
    s.into()
}

/// Reads `sock` with waits of `wait` until `sent` and [`QUIET`] with
/// nothing.
fn read(name: String, sock: UdpSocket, wait: Duration, sent: Arc<AtomicBool>) -> Heard {
    let mut counts = vec![0u32; DATAGRAMS as usize];
    let mut empty_waits = 0;
    let mut shortest_empty = Duration::MAX;
    let mut buf = [0u8; 64];
    let mut quiet_since: Option<Instant> = None;
    loop {
        let start = Instant::now();
        match dstack::recv::recv_from_within(&sock, &mut buf, wait) {
            Ok(Some((n, _))) => {
                let datagram: [u8; 8] = buf
                    .get(..n)
                    .and_then(|b| b.try_into().ok())
                    .expect("a process id and a sequence number");
                let [p0, p1, p2, p3, s0, s1, s2, s3] = datagram;
                if u32::from_be_bytes([p0, p1, p2, p3]) == std::process::id() {
                    counts[u32::from_be_bytes([s0, s1, s2, s3]) as usize] += 1;
                    quiet_since = None;
                }
            }
            Ok(None) => {
                empty_waits += 1;
                shortest_empty = shortest_empty.min(start.elapsed());
                if sent.load(Ordering::Acquire)
                    && quiet_since.get_or_insert_with(Instant::now).elapsed() >= QUIET
                {
                    return Heard {
                        name,
                        wait,
                        counts,
                        empty_waits,
                        shortest_empty,
                    };
                }
            }
            Err(e) => panic!("{name}: the read failed: {e}"),
        }
    }
}

#[test]
fn a_read_with_a_wait_loses_no_datagram_under_load() {
    let port = 45400 + u16::try_from(std::process::id() % 500).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let busy: Vec<_> = (0..BUSY)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut turns = 0u64;
                while !stop.load(Ordering::Relaxed) {
                    turns = std::hint::black_box(turns.wrapping_add(1));
                }
            })
        })
        .collect();
    let sent = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::new();
    let mut unicast = Vec::new();
    for ms in [10, 50] {
        let wait = Duration::from_millis(ms);
        let member = group_member(port);
        let to = Arc::clone(&sent);
        let name = format!("multicast, {ms} ms");
        readers.push(std::thread::spawn(move || read(name, member, wait, to)));
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        unicast.push(sock.local_addr().unwrap());
        let to = Arc::clone(&sent);
        let name = format!("unicast, {ms} ms");
        readers.push(std::thread::spawn(move || read(name, sock, wait, to)));
    }
    let sender = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    sender.set_multicast_if_v4(&Ipv4Addr::LOCALHOST).unwrap();
    sender
        .bind(&SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0).into())
        .unwrap();
    let sender: UdpSocket = sender.into();
    // The readers are in their reads before the first datagram.
    std::thread::sleep(Duration::from_millis(100));
    let mut lcg: u64 = 0x9e37_79b9;
    for seq in 0..DATAGRAMS {
        let mut bytes = [0u8; 8];
        bytes[..4].copy_from_slice(&std::process::id().to_be_bytes());
        bytes[4..].copy_from_slice(&seq.to_be_bytes());
        sender
            .send_to(&bytes, SocketAddrV4::new(GROUP, port))
            .unwrap();
        for to in &unicast {
            sender.send_to(&bytes, to).unwrap();
        }
        lcg = lcg
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        std::thread::sleep(Duration::from_micros((lcg >> 33) % 20_000));
    }
    sent.store(true, Ordering::Release);
    let heard: Vec<Heard> = readers.into_iter().map(|r| r.join().unwrap()).collect();
    stop.store(true, Ordering::Relaxed);
    for b in busy {
        b.join().unwrap();
    }
    let report: Vec<String> = heard
        .iter()
        .map(|h| {
            let lost: Vec<usize> = (0..h.counts.len()).filter(|&i| h.counts[i] == 0).collect();
            let twice = h.counts.iter().filter(|&&c| c > 1).count();
            format!(
                "{}: lost {} {:?}, twice {}, empty waits {} (shortest {:?})",
                h.name,
                lost.len(),
                lost,
                twice,
                h.empty_waits,
                h.shortest_empty
            )
        })
        .collect();
    println!("{}", report.join("\n"));
    assert!(
        heard.iter().all(|h| h.counts.iter().all(|&c| c == 1)),
        "every reader hears every datagram once:\n{}",
        report.join("\n")
    );
    // A read with nothing returns only once its wait is over: an early
    // return would end a request's wait or a heartbeat window early.
    assert!(
        heard.iter().all(|h| h.shortest_empty >= h.wait),
        "no read with nothing before its wait:\n{}",
        report.join("\n")
    );
}

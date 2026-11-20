// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use std::io;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use super::*;

fn loopback() -> UdpSocket {
    UdpSocket::bind("127.0.0.1:0").unwrap()
}

#[test]
fn a_read_is_a_datagram_nothing_yet_or_a_failure() {
    assert!(matches!(read_of(Ok(3)), Read::Got(3)));
    for kind in [io::ErrorKind::WouldBlock, io::ErrorKind::TimedOut] {
        assert!(
            matches!(read_of::<()>(Err(kind.into())), Read::Nothing),
            "{kind:?}"
        );
    }
    let Read::Failed(e) = read_of::<()>(Err(io::ErrorKind::ConnectionReset.into())) else {
        panic!("a reset is a failure");
    };
    assert_eq!(e.kind(), io::ErrorKind::ConnectionReset);
}

#[test]
fn a_judged_read_gives_its_datagram_none_or_its_error() {
    assert_eq!(result_of(Read::Got(7)).unwrap(), Some(7));
    assert_eq!(result_of::<u8>(Read::Nothing).unwrap(), None);
    let e = result_of::<u8>(Read::Failed(io::ErrorKind::PermissionDenied.into())).unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn a_zero_wait_reads_nothing() {
    let sock = loopback();
    let got = within(&sock, Duration::ZERO, |_| -> io::Result<()> {
        panic!("no read for a zero wait")
    });
    assert_eq!(got.unwrap(), None);
}

#[test]
fn a_datagram_waiting_is_read_with_its_sender() {
    let (sock, peer) = (loopback(), loopback());
    peer.send_to(b"hello", sock.local_addr().unwrap()).unwrap();
    let mut buf = [0u8; 16];
    let (n, from) = recv_from_within(&sock, &mut buf, Duration::from_secs(2))
        .unwrap()
        .expect("the datagram");
    assert_eq!(
        (&buf[..n], from),
        (&b"hello"[..], peer.local_addr().unwrap())
    );
}

#[test]
fn a_datagram_sent_during_the_wait_is_read() {
    let (sock, peer) = (loopback(), loopback());
    let to = sock.local_addr().unwrap();
    let sender = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        peer.send_to(b"late", to).unwrap();
    });
    let mut buf = [0u8; 16];
    let got = recv_from_within(&sock, &mut buf, Duration::from_secs(5)).unwrap();
    sender.join().unwrap();
    assert_eq!(got.map(|(n, _)| n), Some(4));
}

#[test]
fn nothing_within_the_wait_is_none_after_the_wait() {
    let sock = loopback();
    let mut buf = [0u8; 16];
    let wait = Duration::from_millis(60);
    let start = Instant::now();
    assert_eq!(recv_from_within(&sock, &mut buf, wait).unwrap(), None);
    assert!(
        start.elapsed() >= wait - Duration::from_millis(5),
        "{:?}",
        start.elapsed()
    );
}

#[test]
fn a_connected_socket_reads_its_peer_s_datagram() {
    let (sock, peer) = (loopback(), loopback());
    sock.connect(peer.local_addr().unwrap()).unwrap();
    peer.send_to(b"reply", sock.local_addr().unwrap()).unwrap();
    let mut buf = [0u8; 16];
    let n = recv_within(&sock, &mut buf, Duration::from_secs(2))
        .unwrap()
        .expect("the reply");
    assert_eq!(&buf[..n], b"reply");
    assert_eq!(
        recv_within(&sock, &mut buf, Duration::from_millis(20)).unwrap(),
        None
    );
}

#[test]
fn a_failed_read_is_an_error() {
    let sock = loopback();
    let e = within(&sock, Duration::from_millis(20), |_| -> io::Result<()> {
        Err(io::ErrorKind::ConnectionRefused.into())
    })
    .unwrap_err();
    assert_eq!(e.kind(), io::ErrorKind::ConnectionRefused);
}

#[test]
fn a_poll_waits_whole_milliseconds_rounded_up_and_capped() {
    let us = Duration::from_micros;
    assert_eq!(poll_ms(Duration::ZERO), 0);
    assert_eq!(poll_ms(us(1)), 1);
    assert_eq!(poll_ms(us(999)), 1);
    assert_eq!(poll_ms(us(1000)), 1);
    assert_eq!(poll_ms(us(1001)), 2);
    assert_eq!(poll_ms(Duration::from_millis(50)), 50);
    let max = Duration::from_millis(u64::try_from(i32::MAX).unwrap());
    assert_eq!(poll_ms(max), i32::MAX);
    assert_eq!(poll_ms(max + Duration::from_millis(1)), i32::MAX);
    assert_eq!(poll_ms(Duration::MAX), i32::MAX);
}

/// A non-blocking read's script: each call takes the next result.
fn script(results: Vec<io::Result<u8>>) -> impl FnMut() -> io::Result<u8> {
    let mut results = results.into_iter();
    move || results.next().expect("no read past the script")
}

fn nothing() -> io::Result<u8> {
    Err(io::ErrorKind::WouldBlock.into())
}

#[test]
fn a_datagram_already_there_is_read_without_a_wait() {
    let until = Instant::now() + Duration::from_secs(1);
    let got = read_when_ready(until, script(vec![Ok(5)]), |_| -> io::Result<bool> {
        panic!("no wait")
    });
    assert_eq!(got.unwrap(), Some(5));
}

#[test]
fn a_failed_read_ends_without_a_wait() {
    let until = Instant::now() + Duration::from_secs(1);
    let got = read_when_ready(
        until,
        script(vec![Err(io::ErrorKind::ConnectionReset.into())]),
        |_| -> io::Result<bool> { panic!("no wait") },
    );
    assert_eq!(got.unwrap_err().kind(), io::ErrorKind::ConnectionReset);
}

#[test]
fn nothing_yet_waits_for_the_time_left_then_reads_again() {
    let wait = Duration::from_secs(1);
    let until = Instant::now() + wait;
    let mut waits = Vec::new();
    let got = read_when_ready(until, script(vec![nothing(), Ok(9)]), |left| {
        waits.push(left);
        Ok(true)
    });
    assert_eq!(got.unwrap(), Some(9));
    assert_eq!(waits.len(), 1);
    assert!(waits[0] > Duration::ZERO && waits[0] <= wait, "{waits:?}");
}

#[test]
fn a_wait_that_ends_with_nothing_reads_no_more() {
    let until = Instant::now() + Duration::from_secs(1);
    let mut waits = 0;
    let got = read_when_ready(until, script(vec![nothing()]), |_| {
        waits += 1;
        Ok(false)
    });
    assert_eq!((got.unwrap(), waits), (None, 1));
}

#[test]
fn a_read_after_the_wait_can_fail() {
    let until = Instant::now() + Duration::from_secs(1);
    let reads = vec![nothing(), Err(io::ErrorKind::PermissionDenied.into())];
    let got = read_when_ready(until, script(reads), |_| Ok(true));
    assert_eq!(got.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
}

#[test]
fn a_failed_wait_is_an_error() {
    let until = Instant::now() + Duration::from_secs(1);
    let got = read_when_ready(until, script(vec![nothing()]), |_| {
        Err(io::ErrorKind::InvalidInput.into())
    });
    assert_eq!(got.unwrap_err().kind(), io::ErrorKind::InvalidInput);
}

#[test]
fn past_its_end_it_reads_once_and_never_waits() {
    let until = Instant::now();
    std::thread::sleep(Duration::from_millis(2));
    let got = read_when_ready(until, script(vec![nothing()]), |_| -> io::Result<bool> {
        panic!("no wait past the end")
    });
    assert_eq!(got.unwrap(), None);
}

#[test]
fn a_readiness_with_nothing_to_read_waits_again_until_the_end() {
    // A readiness the read then finds empty (another reader took the
    // datagram): wait again for what is left, never past the end.
    let wait = Duration::from_millis(40);
    let start = Instant::now();
    let mut waits = Vec::new();
    let got = read_when_ready(start + wait, nothing, |left| {
        waits.push(left);
        std::thread::sleep(left.min(Duration::from_millis(5)));
        Ok(true)
    });
    assert_eq!(got.unwrap(), None);
    assert!(start.elapsed() >= wait, "{:?}", start.elapsed());
    assert!(waits.len() >= 2, "{waits:?}");
    assert!(waits.windows(2).all(|w| w[1] < w[0]), "{waits:?}");
}

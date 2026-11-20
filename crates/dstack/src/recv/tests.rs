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

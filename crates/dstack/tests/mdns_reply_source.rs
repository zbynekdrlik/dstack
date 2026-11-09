// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! dstack #155: our mDNS daemon's legacy unicast replies leave from the
//! interface's address. Upstream mdns-sd sent them with `send_to` on its
//! socket bound to 0.0.0.0:5353, so the kernel chose the source from the
//! main routing table. strih-obs, whose Dante leg is routed only for its
//! own address (a policy table), answered dev1's direct queries from its
//! LAN address, through the router (9.11.2026). The vendored copy sends
//! them with IP_PKTINFO's source (`vendor/mdns-sd/DSTACK.md`).

#![cfg(target_os = "linux")]

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::os::unix::io::AsRawFd;
use std::time::Duration;

#[test]
fn a_reply_leaves_from_the_address_asked_not_the_one_the_routes_pick() {
    // Linux's loopback holds all of 127.0.0.0/8: a reply to 127.0.0.1
    // from a socket bound to every address would leave from 127.0.0.1.
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).unwrap();
    let asker = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    asker
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let SocketAddr::V4(to) = asker.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let ours = Ipv4Addr::new(127, 0, 0, 2);
    let sent = mdns_sd::send_from_v4(sock.as_raw_fd(), b"reply", to, ours).unwrap();
    assert_eq!(sent, 5);
    let mut buf = [0u8; 16];
    let (n, from) = asker.recv_from(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"reply");
    let SocketAddr::V4(from) = from else {
        panic!("IPv4")
    };
    assert_eq!(*from.ip(), ours);
    assert_eq!(from.port(), sock.local_addr().unwrap().port());
    // The destination is the one asked.
    assert_eq!(to, SocketAddrV4::new(Ipv4Addr::LOCALHOST, to.port()));
}

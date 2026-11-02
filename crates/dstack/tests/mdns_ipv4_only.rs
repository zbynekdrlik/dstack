// SPDX-License-Identifier: GPL-3.0-or-later

//! dstack #1 (dasio#1): our mDNS daemon opens no IPv6 socket. mdns-sd
//! always bound `[::]:5353`, which received every interface's mDNS; on a
//! Windows 10 PC it was suspected of lengthening a WFP receive DPC. The
//! vendored copy (`vendor/mdns-sd/DSTACK.md`) skips it.

#![cfg(target_os = "linux")]

use std::collections::HashSet;
use std::time::{Duration, Instant};

/// The socket inodes this process holds.
fn own_socket_inodes() -> HashSet<String> {
    std::fs::read_dir("/proc/self/fd")
        .unwrap()
        .filter_map(|e| std::fs::read_link(e.ok()?.path()).ok())
        .filter_map(|l| {
            let l = l.to_string_lossy().into_owned();
            Some(l.strip_prefix("socket:[")?.strip_suffix(']')?.to_string())
        })
        .collect()
}

/// How many of our sockets in `table` (`/proc/net/udp` or `udp6`) are on
/// local port 5353.
fn ours_on_5353(table: &str, ours: &HashSet<String>) -> usize {
    std::fs::read_to_string(table)
        .unwrap()
        .lines()
        .skip(1)
        .filter(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            f.len() > 9 && f[1].ends_with(":14E9") && ours.contains(f[9])
        })
        .count()
}

#[test]
fn our_mdns_daemon_binds_ipv4_and_no_ipv6_socket() {
    let daemon = mdns_sd::ServiceDaemon::new().expect("the mDNS daemon");
    let t0 = Instant::now();
    let mut v4 = 0;
    while v4 == 0 && t0.elapsed() < Duration::from_secs(5) {
        std::thread::sleep(Duration::from_millis(50));
        v4 = ours_on_5353("/proc/net/udp", &own_socket_inodes());
    }
    let v6 = ours_on_5353("/proc/net/udp6", &own_socket_inodes());
    let _ = daemon.shutdown();
    assert_eq!(v4, 1, "the IPv4 mDNS socket");
    assert_eq!(v6, 0, "an IPv6 mDNS socket");
}

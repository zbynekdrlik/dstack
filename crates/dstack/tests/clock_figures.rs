// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A running device asked for its clock figures over the info port, as
//! dcontroller asks every dstack device (dstack #429): the reply comes
//! back to the asking socket and reads with the strict walker
//! (`conformance/clock_figures.rs`) and with the client. The device runs
//! on the test system clock: its figures name it, ready, with no follower
//! and no service.
//!
//! The device runs on loopback (dstack #366).

#![cfg(any(target_os = "linux", windows))]

#[path = "conformance/clock_figures.rs"]
mod walk;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use dstack::daemon::{Options, Place};
use dstack::host::{self, ClockSource, Signals};
use dstack::info::client;
use dstack::info::clock_figures::{ClockFigures, TimeSource};

const TEST_DEVICE: &str = include_str!("../../../config/test-device.toml");
/// How long the device may take to answer once started.
const BOUND: Duration = Duration::from_secs(10);
/// The info port (Dante's; written here, not taken from the crate).
const PORT: u16 = 8700;

/// The test device's config with its state file in `dir`.
fn config_in(dir: &Path) -> PathBuf {
    let text: Vec<String> = TEST_DEVICE
        .lines()
        .map(|l| {
            if l.starts_with("state_file") {
                format!(
                    "state_file = {:?}",
                    dir.join("state.toml").display().to_string()
                )
            } else {
                l.to_string()
            }
        })
        .collect();
    let path = dir.join("device.toml");
    std::fs::write(&path, text.join("\n")).unwrap();
    path
}

#[test]
fn a_running_device_answers_its_clock_figures_to_the_asker() {
    let dir = tempfile::tempdir().unwrap();
    let ip = Ipv4Addr::LOCALHOST;
    let opts = Options {
        config: config_in(dir.path()),
        place: Place::Ip(ip),
        control: dir.path().join("control.sock"),
    };
    let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("the device set up");
    let ours = dstack::cmc::device_id(ip, prepared.config().process_id);
    let stop = Arc::new(AtomicBool::new(false));
    let host = prepared
        .start(Signals::Report(Arc::clone(&stop)))
        .expect("the device started");
    let sock = UdpSocket::bind(SocketAddrV4::new(ip, 0)).unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let mut request = vec![0xff, 0xff, 0x00, 0x20, 0x12, 0x34, 0x00, 0x00];
    request.extend_from_slice(&[0; 8]);
    request.extend_from_slice(b"Audinate");
    request.extend_from_slice(&[0x07, 0x31, 0xd5, 0xb3, 0, 0, 0, 0]);
    let to = SocketAddrV4::new(ip, PORT);
    let mut replies = Vec::new();
    let mut buf = [0u8; 2048];
    let start = Instant::now();
    let mut next = start;
    while replies.len() < 2 && start.elapsed() < BOUND {
        if Instant::now() >= next {
            sock.send_to(&request, to).unwrap();
            next = Instant::now() + Duration::from_millis(250);
        }
        let Ok((n, SocketAddr::V4(from))) = sock.recv_from(&mut buf) else {
            continue;
        };
        if *from.ip() != ip || buf.get(8..16) != Some(&ours[..]) {
            continue;
        }
        replies.push(walk::reply(&buf[..n]).unwrap_or_else(|e| panic!("our reply: {e}")));
    }
    let client_sock = client::unicast_socket(ip).unwrap();
    let read = client::clock_figures(&client_sock, ip, Some(ours), BOUND);
    let stopped = host.stop(Duration::from_secs(5));
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");

    assert_eq!(replies.len(), 2, "replies within {BOUND:?}");
    for (k, r) in replies.iter().enumerate() {
        assert_eq!(r.get("sequence"), k as u64 + 1);
        // The test system clock (3), ready, no follower, no service.
        for (key, want) in [
            ("source", 3),
            ("ready", 1),
            ("follower", 0),
            ("service_poll", 0),
        ] {
            assert_eq!(r.get(key), want, "{key}");
        }
    }
    let f = read.expect("the client reads the figures");
    assert_eq!(
        f,
        ClockFigures {
            sequence: f.sequence,
            source: TimeSource::System,
            ready: true,
            follower: None,
            service: None,
        }
    );
    assert!(f.sequence > 2, "{f:?}");
}

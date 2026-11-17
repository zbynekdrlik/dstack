// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! A running device asked for its bridge figures over the info port, as
//! dcontroller asks a dasio bridge device (dstack #391): with figures the
//! reply comes back to the asking socket and reads with the strict walker
//! (`conformance/bridge_figures.rs`) and with the client; a device that
//! holds none never answers.
//!
//! The device runs on loopback (dstack #366), one at a time (each binds
//! the Dante ports on that address).

#![cfg(any(target_os = "linux", windows))]

#[path = "conformance/bridge_figures.rs"]
mod walk;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dstack::daemon::{Options, Place};
use dstack::info::client::{self, Ask, InfoError};
use dstack::info::{OwnFigures, UnderrunCause};

use dstack::host::{self, ClockSource, Signals};

const TEST_DEVICE: &str = include_str!("../../../config/test-device.toml");
/// One device at a time: each binds the Dante ports on the same address.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());
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

/// The test device's address: loopback (dstack #366).
fn test_ipv4() -> Ipv4Addr {
    Ipv4Addr::LOCALHOST
}

/// Runs a device, with bridge figures when `bridged`, and asks it from a
/// socket of our own: the raw replies the walker read (by the raw request
/// for up to `BOUND`, or `quiet` for a device without figures), the
/// client's read, and the device's board info read after them (the proof
/// its info server ran meanwhile).
fn ask_device(
    bridged: bool,
    quiet: Duration,
) -> (
    Vec<walk::Reply>,
    Result<dstack::info::BridgeFigures, InfoError>,
    Result<client::Board, InfoError>,
) {
    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let ip = test_ipv4();
    let opts = Options {
        config: config_in(dir.path()),
        place: Place::Ip(ip),
        control: dir.path().join("control.sock"),
    };
    let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("the device set up");
    let ours = dstack::cmc::device_id(ip, prepared.config().process_id);
    let late = Arc::new(AtomicU64::new(7));
    if bridged {
        let media = prepared.media();
        let seen = Arc::clone(&late);
        media.bridge_figures().set_source(move || OwnFigures {
            to_asio_target_us: 3000,
            from_asio_target_us: 1333,
            to_asio_converter_delay_us: 250,
            from_asio_converter_delay_us: 167,
            late_callbacks: seen.load(Ordering::Relaxed),
            late_bins: [1, 3, 5, 7, 9, 11, 13, 15],
        });
        media
            .bridge_figures()
            .set_underruns(5, 3, UnderrunCause::Bridge, UnderrunCause::Driver);
    }
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
    request.extend_from_slice(&[0x07, 0x31, 0xd5, 0xb1, 0, 0, 0, 0]);
    let to = SocketAddrV4::new(ip, PORT);
    let mut replies = Vec::new();
    let mut buf = [0u8; 2048];
    let wait = if bridged { BOUND } else { quiet };
    let start = Instant::now();
    let mut next = start;
    // Until a reply shows the source's second value (a resend's reply in
    // flight may still show the first).
    let changed = |r: &[walk::Reply]| r.last().is_some_and(|x| x.get("late_callbacks") == 8);
    while !changed(&replies) && start.elapsed() < wait {
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
        late.store(8, Ordering::Relaxed);
    }
    let client_sock = client::unicast_socket(ip).unwrap();
    let read = client::bridge_figures(
        &client_sock,
        ip,
        Some(ours),
        if bridged { BOUND } else { quiet },
    );
    let board = client::board_info(ip, ip, Some(ours), BOUND);
    let stopped = host.stop(Duration::from_secs(5));
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
    (replies, read, board)
}

#[test]
fn a_running_bridge_device_answers_its_figures_to_the_asker() {
    let (replies, read, board) = ask_device(true, BOUND);
    assert!(board.is_ok(), "{board:?}");
    assert!(replies.len() >= 2, "replies within {BOUND:?}: {replies:?}");
    let first = &replies[0];
    for (key, want) in [
        ("to_asio_cause", 1),
        ("from_asio_cause", 2),
        ("to_asio_target_us", 3000),
        ("from_asio_target_us", 1333),
        ("to_asio_converter_delay_us", 250),
        ("from_asio_converter_delay_us", 167),
        ("to_asio_underruns", 5),
        ("from_asio_underruns", 3),
        ("late_callbacks", 7),
        ("late_250us", 1),
        ("late_2000us", 15),
    ] {
        assert_eq!(first.get(key), want, "{key}");
    }
    // Read at each request: a later reply has the source's new value, each
    // its own sequence (from 1), all the same run.
    let last = replies.last().unwrap();
    assert_eq!(last.get("late_callbacks"), 8);
    for (k, r) in replies.iter().enumerate() {
        assert_eq!(r.get("sequence"), k as u64 + 1);
        assert_eq!(r.get("run_start_unix_ms"), first.get("run_start_unix_ms"));
    }
    assert!(first.get("run_start_unix_ms") > 1_700_000_000_000);
    let f = read.expect("the client reads the figures");
    assert_eq!(
        (f.to_asio_cause, f.from_asio_cause),
        (UnderrunCause::Bridge, UnderrunCause::Driver)
    );
    assert_eq!((f.to_asio_underruns, f.from_asio_underruns), (5, 3));
    assert_eq!(f.late_bins, [1, 3, 5, 7, 9, 11, 13, 15]);
    assert!(f.sequence > last.get("sequence") as u32, "{f:?}");
}

#[test]
fn a_running_device_without_a_bridge_never_answers() {
    let (replies, read, board) = ask_device(false, Duration::from_secs(2));
    // Its info server answered all along: the board info comes.
    assert!(board.is_ok(), "{board:?}");
    assert!(replies.is_empty(), "{replies:?}");
    assert_eq!(
        read,
        Err(InfoError::Timeout {
            ask: Ask::BridgeFigures
        })
    );
}

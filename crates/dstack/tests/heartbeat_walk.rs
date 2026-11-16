//! Whole devices' heartbeats walked as Dante Controller and dcontroller
//! walk them (dstack #349): a running device on the test-only system
//! clock, heard on the heartbeat group 224.0.0.233:8708 like any other
//! device on the network, and every heartbeat read with the strict
//! walker written from hardware captures (`conformance/walk.rs`), never
//! with dstack's own reader. dstack #345 shipped a peaks record whose
//! length left out its padding: every device with a TX+RX channel count
//! not a multiple of 4 (dlisten's 0+2) lost its lateness records, while
//! the tests' devices had aligned counts and were read back with our own
//! parser.
//!
//! The device runs on the box's routed IPv4 address, as in
//! `tests/system_clock.rs` (one device at a time: each binds the Dante
//! ports on that address).

#![cfg(any(target_os = "linux", windows))]

#[path = "conformance/walk.rs"]
mod walk;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use dstack::daemon::{Options, Place};
use dstack::device::DeviceConfig;
use dstack::host::{self, ClockSource, Signals};

const TEST_DEVICE: &str = include_str!("../../../config/test-device.toml");
/// One device at a time: each binds the Dante ports on the same address.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// How long the device may take to send a heartbeat with its records.
const BOUND: Duration = Duration::from_secs(10);
/// The heartbeat group and port (Dante's; written here, not taken from
/// the crate, so a changed constant there cannot hide).
const GROUP: Ipv4Addr = Ipv4Addr::new(224, 0, 0, 233);
const PORT: u16 = 8708;

/// The test device's config with `tx` and `rx` channels (`T<n>`, `R<n>`),
/// a 10 ms latency and its state file in `dir`.
fn config_in(dir: &Path, tx: usize, rx: usize) -> PathBuf {
    let head: Vec<&str> = TEST_DEVICE
        .lines()
        .take_while(|l| !l.starts_with("rx = ["))
        .filter(|l| !l.starts_with("state_file"))
        .map(|l| {
            if l.starts_with("latency_ns") {
                "latency_ns = 10000000"
            } else {
                l
            }
        })
        .collect();
    let list = |prefix: &str, n: usize| {
        (1..=n)
            .map(|k| format!("\"{prefix}{k}\""))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let text = format!(
        "{}\nstate_file = {:?}\nrx = [{}]\ntx = [{}]\n",
        head.join("\n"),
        dir.join("state.toml").display().to_string(),
        list("R", rx),
        list("T", tx)
    );
    let cfg = DeviceConfig::from_toml(&text).expect("the config reads");
    assert_eq!((cfg.tx.len(), cfg.rx.len()), (tx, rx));
    let path = dir.join("device.toml");
    std::fs::write(&path, &text).unwrap();
    path
}

/// The box's IPv4 address on its default route (sends nothing).
fn routed_ipv4() -> Ipv4Addr {
    let probe = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).expect("a UDP socket");
    probe
        .connect((Ipv4Addr::new(192, 0, 2, 1), 9))
        .expect("a default route on this box");
    match probe.local_addr().expect("the routed address") {
        SocketAddr::V4(a) if !a.ip().is_loopback() && !a.ip().is_unspecified() => *a.ip(),
        other => panic!("no routed IPv4 address on this box: {other}"),
    }
}

/// A socket on the heartbeat group, joined on `ip`'s network, sharing the
/// port with any other listener on the box.
fn heartbeat_socket(ip: Ipv4Addr) -> UdpSocket {
    let sock = socket2::Socket::new(
        socket2::Domain::IPV4,
        socket2::Type::DGRAM,
        Some(socket2::Protocol::UDP),
    )
    .unwrap();
    sock.set_reuse_address(true).unwrap();
    #[cfg(unix)]
    sock.set_reuse_port(true).unwrap();
    sock.bind(&SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, PORT).into())
        .unwrap();
    sock.join_multicast_v4(&GROUP, &ip).unwrap();
    let sock: UdpSocket = sock.into();
    sock.set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    sock
}

/// Runs a device with `tx` and `rx` channels and returns its first
/// heartbeats with records (at least two), each walked strictly.
fn walked_beats(tx: usize, rx: usize) -> Vec<walk::Heartbeat> {
    let _one = ONE_DEVICE.lock().unwrap_or_else(|e| e.into_inner());
    let dir = tempfile::tempdir().unwrap();
    let ip = routed_ipv4();
    let opts = Options {
        config: config_in(dir.path(), tx, rx),
        place: Place::Ip(ip),
        control: dir.path().join("control.sock"),
    };
    let sock = heartbeat_socket(ip);
    let prepared = host::prepare_with(&opts, &[], ClockSource::System).expect("the device set up");
    let ours = dstack::cmc::device_id(ip, prepared.config().process_id);
    let stop = Arc::new(AtomicBool::new(false));
    let host = prepared
        .start(Signals::Report(Arc::clone(&stop)))
        .expect("the device started");
    let mut buf = [0u8; 2048];
    let mut beats = Vec::new();
    let mut refused = Vec::new();
    let start = Instant::now();
    while beats.len() < 2 && refused.is_empty() && start.elapsed() < BOUND {
        let Ok((n, SocketAddr::V4(from))) = sock.recv_from(&mut buf) else {
            continue;
        };
        let d = &buf[..n];
        // Our device's heartbeats only: its address, its id, past the
        // bare header it sends before its clock is ready.
        if *from.ip() != ip
            || d.len() <= walk::HEADER
            || d[8..16] != ours
            || d[0..2] != [0xff, 0xfe]
        {
            continue;
        }
        match walk::heartbeat(d) {
            Ok(beat) => beats.push(beat),
            Err(e) => refused.push(e),
        }
    }
    let stopped = host.stop(Duration::from_secs(5));
    assert!(
        refused.is_empty(),
        "{tx}+{rx}: our heartbeat breaks the hardware's format: {refused:?}"
    );
    assert!(
        beats.len() >= 2,
        "{tx}+{rx}: {} heartbeats with records within {BOUND:?}",
        beats.len()
    );
    assert!(matches!(stopped, host::Stopped::Done), "{stopped:?}");
    beats
}

/// What a heartbeat must hold for a reader that walks it: the peaks for
/// every channel, then the lateness and the late counts it is read for.
fn check(beats: &[walk::Heartbeat], tx: usize, rx: usize) {
    for b in beats {
        let kinds: Vec<u16> = b.records.iter().map(|&(k, _)| k).collect();
        assert_eq!(kinds, [0x8001, 0x8002, 0x8003, 0x8004], "{tx}+{rx}: {b:?}");
        let (tx_peaks, rx_peaks) = b.peaks.as_ref().expect("the peaks");
        assert_eq!((tx_peaks.len(), rx_peaks.len()), (tx, rx), "{b:?}");
        let (rate, lateness) = b.lateness.as_ref().expect("the lateness record");
        assert_eq!(*rate, 96_000, "{b:?}");
        let late = b.late.as_ref().expect("the late record");
        assert!(
            !lateness.is_empty() && lateness.len() == late.len(),
            "{b:?}"
        );
    }
}

/// dlisten's shape: 2 RX channels, no TX (the device dcontroller showed no
/// graph for, dstack #345).
#[test]
fn a_device_with_2_rx_channels_heartbeats_in_the_hardware_s_format() {
    check(&walked_beats(0, 2), 0, 2);
}

/// An odd TX+RX total: the peaks record's padding is 3 bytes.
#[test]
fn a_device_with_an_odd_channel_total_heartbeats_in_the_hardware_s_format() {
    check(&walked_beats(3, 2), 3, 2);
}

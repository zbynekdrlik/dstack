// SPDX-License-Identifier: MIT OR Apache-2.0

//! The driver side's buffer switch with its to-ASIO pull (dstack #307)
//! under RealtimeSanitizer: it runs on the ASIO driver's thread, the one
//! with the deadline. The `rtsan` CI job builds this test with a pinned
//! nightly, `-Zsanitizer=realtime`, `-Zbuild-std` and `--cfg iem_rtsan`;
//! the switch's part after the pull (`bridge::AsioSide`'s `serve`: the
//! from-ASIO FIFO write, the hand-over's age, the to-ASIO player), the
//! pull's ring read and FIFO write (dlink's `bridge::pull`) and its drain's
//! packet placing (dstack's `media::rx::intake::place`) then carry
//! `#[sanitize(realtime = "nonblocking")]`, and any allocation, lock or
//! blocking syscall inside them aborts the test binary. The sockets' reads
//! and the clock read stay outside the attribute (system calls by nature,
//! non-blocking). Built normally (every other job), the same workload runs
//! without the sanitizer. The from-ASIO side the TX thread runs (dstack
//! #364, `bridge::FromAsio`'s `process`) carries the attribute too.
#![cfg_attr(iem_rtsan, feature(sanitize))]

use std::net::{Ipv4Addr, UdpSocket};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use dlink::bridge::RxPull;
use dstack::clock::{ManualClock, PtpClock};
use dstack::media::ring::Ring;
use dstack::media::rx::{RxPlan, RxRoute, RxShared};
use dstack::media::tx::ns_of;
use dstack_asio::bridge::{Bridge, Spec};
use dstack_asio::{Block, Opened, Process, Sample};

const RATE: u32 = 48_000;
/// Ten years of a grandmaster's uptime at 48 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * RATE as u64;
/// The device's latency P, samples.
const P: u64 = 96;
const FPP: usize = 32;
const BUFFER: usize = 256;
const PROBE: &str = "DSTACK_ASIO_RTSAN_PROBE";

#[test]
fn the_driver_s_switch_with_its_pull_is_realtime_safe() {
    let rings: Arc<[Ring]> = (0..1).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, rings).unwrap());
    let plan = RxPlan {
        src: Some(Ipv4Addr::LOCALHOST),
        bps: 3,
        nslots: 1,
        latency: P as u32,
        delay: 0,
        block: 128,
        live: false,
        generation: 1,
        group: None,
    };
    rx.publish(
        |slot| if slot == 0 { plan } else { RxPlan::default() },
        |_| Some(RxRoute { flow: 0, slot: 0 }),
    );
    let mut bridge = Bridge::new(Spec {
        dante_rate: RATE,
        block_frames: 128,
        to_asio: 1,
        from_asio: 1,
        app_buffer_ns: None,
        to_asio_app_buffer_ns: None,
        from_asio_app_buffer_ns: None,
        converter_taps: None,
    })
    .unwrap();
    let mut dante = bridge.dante_side().unwrap();
    let clock = Arc::new(ManualClock::new(ns_of(T + P, RATE)));
    let pull = RxPull::new(
        rx.app_intake().expect("the app's claim"),
        Arc::clone(&clock) as Arc<dyn PtpClock>,
        Arc::clone(&rx),
        vec![0],
        bridge.to_asio_fifo().unwrap(),
        P,
        RATE,
    )
    .expect("the pull");
    let stats = pull.stats();
    bridge.pull_to_asio(pull).unwrap();
    let opened = Opened {
        rate: f64::from(RATE),
        buffer_frames: BUFFER as u32,
        driver_inputs: 1,
        driver_outputs: 1,
        inputs: vec![0],
        outputs: vec![0],
        input_sample: Some(Sample::Int32),
        output_sample: Sample::Int32,
    };
    let mut asio = bridge.asio_side(&opened).unwrap();
    let mut pump_out = vec![0.0; 128];
    dante.process(&mut Block::new(128, &[], &mut pump_out));
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let dest = rx.local_addr(0).unwrap();
    let mut packet = [0u8; 1500];
    let input = vec![0.25; BUFFER];
    let mut out = vec![0.0; BUFFER];
    // The first switch's pull only starts there (reads nothing).
    asio.process(&mut Block::new(BUFFER, &input, &mut out));
    let mut played = 0;
    for k in 0..50u64 {
        let at = T + k * BUFFER as u64;
        for p in 0..(BUFFER / FPP) as u64 {
            let (secs, sub) = dstack::clock::stamp(at + p * FPP as u64, RATE);
            let n = dstack::media::packet::write(&mut packet, secs, sub, 1, 3, FPP, |_, _| 1 << 29);
            sender.send_to(&packet[..n], dest).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
        clock.sleep_until(ns_of(at + BUFFER as u64 + P, RATE));
        asio.process(&mut Block::new(BUFFER, &input, &mut out));
        if out.iter().all(|&x| (x - 0.25).abs() < 1e-3) {
            played += 1;
        }
    }
    assert_eq!(stats.frames(), 50 * BUFFER as u64);
    assert!(played >= 30, "it played the flow: {played} switches");
    let c = rx.lane(0);
    assert_eq!((c.packets, c.unplaced, c.late), (400, 0, 0), "{c:?}");
}

/// The from-ASIO side the TX thread runs (dstack #364, `FromAsio`'s
/// `process`, rtsan-marked: the block age's take, the switch's take of a
/// driver open's player and the player's fill), through priming into
/// playing, a reopen's player taken and a skipped stretch (a resync).
#[test]
fn the_tx_thread_s_from_asio_fill_is_realtime_safe() {
    let mut bridge = Bridge::new(Spec {
        dante_rate: RATE,
        block_frames: 128,
        to_asio: 0,
        from_asio: 1,
        app_buffer_ns: None,
        to_asio_app_buffer_ns: None,
        from_asio_app_buffer_ns: None,
        converter_taps: None,
    })
    .unwrap();
    let mut from = bridge.pull_from_asio().unwrap();
    let _dante = bridge.dante_side().unwrap();
    let opened = Opened {
        rate: f64::from(RATE),
        buffer_frames: BUFFER as u32,
        driver_inputs: 1,
        driver_outputs: 0,
        inputs: vec![0],
        outputs: vec![],
        input_sample: Some(Sample::Int32),
        output_sample: Sample::Int32,
    };
    let input = vec![0.25; BUFFER];
    let mut none = [0.0; 0];
    let mut tx = vec![0.0; 128];
    let mut start = 1_000_000u64;
    let mut played = 0;
    for open in 0..2 {
        let mut asio = bridge.asio_side(&opened).unwrap();
        if open == 1 {
            // A skipped stretch before the reopen's first fill.
            start += 48_000;
        }
        for _ in 0..60 {
            asio.process(&mut Block::new(BUFFER, &input, &mut none));
            for _ in 0..BUFFER / 128 {
                from.process(&mut Block::new(128, &[], &mut tx).at(start));
                start += 128;
                if tx.iter().all(|&x| (x - 0.25).abs() < 1e-3) {
                    played += 1;
                }
            }
        }
    }
    // The reopen's handover freed the idle player; the first open's came
    // back at the reopen's take.
    assert_eq!(bridge.collect(), 1, "the first open's player came back");
    assert!(
        played >= 150,
        "it played the driver's input: {played} fills"
    );
}

/// Allocates in a real-time context: a violation under the sanitizer.
#[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
fn nonblocking_alloc(n: usize) -> usize {
    vec![0u8; n].len()
}

/// Run by `rtsan_detects_a_violation` in a child process; on its own it
/// does nothing.
#[test]
fn probe() {
    if std::env::var_os(PROBE).is_some() {
        assert_eq!(nonblocking_alloc(64), 64);
    }
}

#[test]
fn rtsan_detects_a_violation() {
    let out = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "probe", "--nocapture", "--test-threads", "1"])
        .env(PROBE, "1")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    if cfg!(iem_rtsan) {
        assert!(!out.status.success(), "the allocation went unnoticed");
        assert!(stderr.contains("RealtimeSanitizer"), "{stderr}");
    } else {
        assert!(out.status.success(), "{stderr}");
    }
}

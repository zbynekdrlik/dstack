// SPDX-License-Identifier: MIT OR Apache-2.0

//! The RX pull (dstack #307) under RealtimeSanitizer: it runs on an
//! output's callback, the thread with the deadline. The `rtsan` CI job
//! builds this test with a pinned nightly, `-Zsanitizer=realtime`,
//! `-Zbuild-std` and `--cfg iem_rtsan`; the pull's ring read and FIFO
//! write (`bridge::pull`'s `read`) and its drain's packet placing
//! (dstack's `media::rx::intake::place`) then carry
//! `#[sanitize(realtime = "nonblocking")]`, and any allocation, lock or
//! blocking syscall inside them aborts the test binary. The sockets' reads
//! and the clock read stay outside the attribute (system calls by nature,
//! non-blocking). Built normally (every other job), the same workload runs
//! without the sanitizer.
#![cfg_attr(iem_rtsan, feature(sanitize))]

use std::net::{Ipv4Addr, UdpSocket};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use dlink::bridge::{Fifo, RxPull, fifo_for};
use dstack::clock::{ManualClock, PtpClock};
use dstack::media::ring::Ring;
use dstack::media::rx::{RxPlan, RxRoute, RxShared};
use dstack::media::tx::ns_of;

const RATE: u32 = 48_000;
/// Ten years of a grandmaster's uptime at 48 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * RATE as u64;
/// The device's latency P, samples.
const P: u64 = 96;
const FPP: usize = 32;
const PROBE: &str = "DLINK_RTSAN_PROBE";

#[test]
fn the_rx_pull_is_realtime_safe() {
    let rings: Arc<[Ring]> = (0..2).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    let plan = RxPlan {
        src: Some(Ipv4Addr::LOCALHOST),
        bps: 3,
        nslots: 2,
        latency: P as u32,
        delay: 0,
        block: 128,
        live: false,
        generation: 1,
        group: None,
    };
    rx.publish(
        |slot| if slot == 0 { plan } else { RxPlan::default() },
        |c| Some(RxRoute { flow: 0, slot: c }),
    );
    let intake = rx.app_intake().expect("the app's claim");
    let clock = Arc::new(ManualClock::new(ns_of(T + P, RATE)));
    let fifo: Arc<Fifo> = Arc::new(fifo_for(2, RATE));
    let mut pull = RxPull::new(
        intake,
        Arc::clone(&clock) as Arc<dyn PtpClock>,
        Arc::clone(&rx),
        vec![0, 1],
        Arc::clone(&fifo),
        P,
        RATE,
    );
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let dest = rx.local_addr(0).unwrap();
    let mut packet = [0u8; 1500];
    let mut out = vec![0.0f64; 2 * 480];
    assert_eq!(pull.pull(), 0, "the start");
    let mut written = 0;
    for k in 0..50u64 {
        let at = T + k * 480;
        for p in 0..15u64 {
            let (secs, sub) = dstack::clock::stamp(at + p * FPP as u64, RATE);
            let n = dstack::media::packet::write(&mut packet, secs, sub, 2, 3, FPP, |f, c| {
                (f as i32 + 1 + c as i32) << 8
            });
            sender.send_to(&packet[..n], dest).unwrap();
        }
        std::thread::sleep(Duration::from_millis(5));
        clock.sleep_until(ns_of(at + 480 + P, RATE));
        written += pull.pull();
        assert!(fifo.pop_planar(&mut out, 480), "{} held", fifo.len());
    }
    assert_eq!(written, 50 * 480);
    // Each pull drains a whole period's packets at once (read up to a
    // period after their stamps: counted late, placed all the same).
    let c = rx.lane(0);
    assert_eq!((c.packets, c.refused, c.unplaced), (750, 0, 0), "{c:?}");
    assert_eq!(rings[1].counters().written, 50 * 480);
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

// SPDX-License-Identifier: MIT OR Apache-2.0

//! `RxPull::pull` allocates nothing (dstack #307): it runs on the output's
//! callback, the thread with the deadline. Each pull drains the flow's
//! socket (packets waiting there), reads the rings into the FIFO (a read
//! of a whole callback in one push), waits on a clock behind its last read and
//! starts again after a gap past the FIFO, all under `assert_no_alloc`
//! (warn mode: the violation count must stay zero). Its own test binary,
//! because it installs the global allocator; the first test proves the
//! detector sees an allocation.

use std::net::{Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use dlink::bridge::{Fifo, RxPull, fifo_for};
use dstack::clock::{ManualClock, PtpClock};
use dstack::media::ring::Ring;
use dstack::media::rx::{RxPlan, RxRoute, RxShared};
use dstack::media::tx::ns_of;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: u32 = 48_000;
/// A grandmaster's uptime of ten years at 48 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * RATE as u64;
/// The device's latency P, samples.
const P: u64 = 96;
/// Frames a packet.
const FPP: usize = 32;
/// A little over one sample at [`RATE`], ns.
const ONE_SAMPLE_NS: i64 = 20_834;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1u8; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

/// Sends `count` packets of [`FPP`] frames from position `at` to flow slot
/// 0's socket, then waits until they are there.
fn send(sender: &UdpSocket, rx: &RxShared, at: u64, count: u64) {
    let mut out = [0u8; 1500];
    for k in 0..count {
        let (secs, sub) = dstack::clock::stamp(at + k * FPP as u64, RATE);
        let n = dstack::media::packet::write(&mut out, secs, sub, 1, 3, FPP, |f, _| {
            (f as i32 + 1) << 8
        });
        sender
            .send_to(&out[..n], rx.local_addr(0).unwrap())
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(30));
}

#[test]
fn a_pull_allocates_nothing() {
    let rings: Arc<[Ring]> = (0..2).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
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
        |c| (c == 0).then_some(RxRoute { flow: 0, slot: 0 }),
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
    let stats = pull.stats();
    let mut out = vec![0.0f64; 2 * 4096];
    let mut written = 0;
    reset_violation_count();
    // The start, then pieces of 480 frames (two of the reader's pieces)
    // whose packets wait in the socket, read by the pull's own drain.
    assert_no_alloc(|| written += pull.pull());
    for k in 0..20u64 {
        let at = T + k * 480;
        send(&sender, &rx, at, 15);
        clock.sleep_until(ns_of(at + 480 + P, RATE));
        assert_no_alloc(|| {
            written += pull.pull();
            // The same clock: an empty read.
            written += pull.pull();
            // The clock a sample behind the last read: it waits.
            clock.re_anchor(-ONE_SAMPLE_NS);
            assert_eq!(pull.pull(), 0);
            clock.re_anchor(ONE_SAMPLE_NS);
            assert!(fifo.pop_planar(&mut out[..2 * 480], 480));
        });
    }
    // A gap past the FIFO's capacity: started again at now.
    clock.sleep_until(ns_of(T + 20 * 480 + P + 10 * RATE as u64, RATE));
    assert_no_alloc(|| written += pull.pull());
    assert_eq!(
        violation_count(),
        0,
        "the pull allocated (wrote {written} frames)"
    );
    assert_eq!(written, 20 * 480);
    assert_eq!(stats.restarts(), 1);
    assert_eq!(rings[0].counters().written, 20 * 480);
    assert_eq!(rings[0].counters().holes, 0);
}

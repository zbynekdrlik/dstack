// SPDX-License-Identifier: MIT OR Apache-2.0

//! The driver side's buffer switch allocates nothing with its to-ASIO pull
//! (dstack #307): each switch drains the flow's socket, reads the rings
//! into the to-ASIO FIFO and plays it through the player, and takes the
//! driver's inputs into the from-ASIO FIFO, all under `assert_no_alloc`
//! (warn mode: the violation count must stay zero), through priming into
//! playing and a gap past the FIFO (a restart). Its own test binary, because it installs the global
//! allocator; the first test proves the detector sees an allocation.

use std::net::{Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use dlink::bridge::RxPull;
use dstack::clock::{ManualClock, PtpClock};
use dstack::media::ring::Ring;
use dstack::media::rx::{RxPlan, RxRoute, RxShared};
use dstack::media::tx::ns_of;
use dstack_asio::bridge::{Bridge, Spec};
use dstack_asio::{Block, Opened, Process, Sample};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

const RATE: u32 = 48_000;
/// A grandmaster's uptime of ten years at 48 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * RATE as u64;
/// The device's latency P, samples.
const P: u64 = 96;
/// Frames a packet, and the driver's buffer.
const FPP: usize = 32;
const BUFFER: usize = 256;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1u8; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

/// Sends flow slot 0 the packets of `BUFFER` frames from position `at`, a
/// quarter of full scale, then waits until they are there.
fn send(sender: &UdpSocket, rx: &RxShared, at: u64) {
    let mut out = [0u8; 1500];
    for k in 0..(BUFFER / FPP) as u64 {
        let (secs, sub) = dstack::clock::stamp(at + k * FPP as u64, RATE);
        let n = dstack::media::packet::write(&mut out, secs, sub, 1, 3, FPP, |_, _| 1 << 29);
        sender
            .send_to(&out[..n], rx.local_addr(0).unwrap())
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(5));
}

#[test]
fn the_driver_s_switch_with_its_pull_allocates_nothing() {
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
    })
    .unwrap();
    // The pump's side takes the from-ASIO player at each open.
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
    let input = vec![0.25; BUFFER];
    let mut out = vec![0.0; BUFFER];
    reset_violation_count();
    let mut at = T;
    let mut played = 0;
    for _ in 0..60 {
        send(&sender, &rx, at);
        at += BUFFER as u64;
        clock.sleep_until(ns_of(at + P, RATE));
        assert_no_alloc(|| asio.process(&mut Block::new(BUFFER, &input, &mut out)));
        if out.iter().all(|&x| (x - 0.25).abs() < 1e-3) {
            played += 1;
        }
    }
    // A gap past the FIFO: the pull starts again at now.
    at += 48_000;
    clock.sleep_until(ns_of(at + P, RATE));
    assert_no_alloc(|| asio.process(&mut Block::new(BUFFER, &input, &mut out)));
    assert_eq!(violation_count(), 0, "allocations in the driver's switch");
    assert!(played >= 40, "it played the flow: {played} switches");
    assert_eq!(stats.restarts(), 1, "the gap");
    assert!(stats.frames() >= 59 * BUFFER as u64, "{}", stats.frames());
}

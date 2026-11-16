//! A stalled RX helper thread and the samples the pump reads (dstack #326,
//! strih-snv 15-16.11.2026): under OBS's load the scheduler left the
//! normal-priority RX thread behind by up to 17.5 ms, more than the 13 ms
//! between a packet's arrival and the pump's read of its samples, and the
//! thread then wrote them behind the reader (`written_late`, the ring's
//! `late`, and as many holes).
//!
//! Real threads on loopback: a sender sends a flow in real time to our RX
//! slot, the RX thread and the engine's pump run on the media clock, and
//! the RX thread is stalled 20 ms at random moments (`stall_thread`), past
//! the 8 ms receive slack. In the normal profile the pump drains the flow
//! sockets itself, so no sample is late and none is a hole; in the
//! low-latency profile (the RX thread writes, on real-time priority in the
//! field) the same stalls land samples behind the reader, which proves the
//! stalls bite.

use std::net::{Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use dstack::clock::{self, PtpClock, SkewedClock, TestClockPpm};
use dstack::device::{DeviceConfig, TimingProfile};
use dstack::media::packet;
use dstack::media::ring::{Counters, Ring};
use dstack::media::rx::{self, RxCounts, RxPlan, RxRoute, RxShared};
use dstack::media::tx::{Scheduling, TxShared};
use dstack::stream::{self, Map, Pump, PumpStats, Timing};
use dstack_audio_io::{Block, Process};

const RATE: u32 = 48_000;
/// Frames a packet.
const FPP: u64 = 16;
/// The pump's block, its playout point (the receive slack, 8 ms) and its
/// margin, samples.
const BLOCK: u64 = 64;
const PLAYOUT: u64 = 384;
const MARGIN: u64 = 288;
/// How long the RX thread stalls each time: past the slack.
const STALL: Duration = Duration::from_millis(20);
const STALLS: usize = 6;
/// The flow's length.
const RUN: Duration = Duration::from_secs(3);
/// How early the sender sends a packet, samples (4 ms): a sender thread
/// late by less than this plus the slack still lands in time, so only the
/// RX side decides the counts.
const LEAD: u64 = 192;

/// The app's processor: hears the inputs, sends nothing.
struct Listen;

impl Process for Listen {
    fn process(&mut self, _block: &mut Block<'_>) {}
}

/// The ring's and the slot's counts after a flow whose RX thread stalled
/// [`STALLS`] times, under `profile`.
fn stalled_run(profile: TimingProfile) -> (Counters, RxCounts) {
    let mut cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    cfg.sample_rate = RATE;
    cfg.block = BLOCK;
    cfg.timing = profile;
    let rings: Arc<[Ring]> = (0..cfg.rx.len())
        .map(|_| Ring::new())
        .collect::<Vec<_>>()
        .into();
    let shared = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    shared.publish(
        |slot| {
            if slot == 0 {
                RxPlan {
                    src: Some(Ipv4Addr::LOCALHOST),
                    bps: 3,
                    nslots: 1,
                    latency: PLAYOUT as u32,
                    delay: 0,
                    block: BLOCK as u16,
                    live: false,
                    generation: 1,
                    group: None,
                }
            } else {
                RxPlan::default()
            }
        },
        |rx| (rx == 0).then_some(RxRoute { flow: 0, slot: 0 }),
    );
    shared.set_device_latency(PLAYOUT * 1_000_000_000 / u64::from(RATE));
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
    let timing = Timing {
        block: BLOCK,
        playout: PLAYOUT,
        margin: MARGIN,
    };
    let stats = Arc::new(PumpStats::new(timing, RATE));
    let map = Map {
        rx: vec![0],
        tx: Vec::new(),
    };
    let pump = Pump::new(Listen, map, timing, 24, Arc::clone(&shared), tx, stats);
    let media: Arc<dyn PtpClock> = Arc::new(SkewedClock::new(
        clock::app_monotonic_ns(),
        0,
        TestClockPpm(0.0),
    ));
    let rx_thread = rx::spawn(
        Arc::clone(&media),
        Arc::clone(&shared),
        None,
        Arc::new(Scheduling::new()),
    )
    .unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let pump = stream::spawn(Arc::clone(&media), pump, Arc::clone(&stop)).unwrap();

    // The sender: each packet [`LEAD`] before its last frame is due, in
    // real time.
    let sender = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let dest = shared.local_addr(0).unwrap();
    let samples = |ns: u64| clock::to_samples(ns, RATE);
    let mut next = samples(media.now_ns().unwrap()) / FPP * FPP;
    let mut out = [0u8; 1500];
    // The stalls' moments: spread over the run after a settled start,
    // jittered by a fixed LCG.
    let mut seed = 0x2b7e_1516_28ae_d2a6u64;
    let stalls: Vec<Duration> = (0..STALLS)
        .map(|k| {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            Duration::from_millis(500 + 400 * k as u64 + (seed >> 33) % 200)
        })
        .collect();
    let began = Instant::now();
    let mut stalled = 0;
    while began.elapsed() < RUN {
        if stalls.get(stalled).is_some_and(|&at| began.elapsed() >= at) {
            shared.stall_thread(u64::try_from(STALL.as_nanos()).unwrap());
            stalled += 1;
        }
        let now = samples(media.now_ns().unwrap());
        while next + FPP <= now + LEAD {
            let (secs, sub) = clock::stamp(next, RATE);
            let n = packet::write(&mut out, secs, sub, 1, 3, FPP as usize, |f, _| {
                (((next + f as u64) % 65_536) << 8) as i32
            });
            sender.send_to(&out[..n], dest).unwrap();
            next += FPP;
        }
        std::thread::sleep(Duration::from_micros(250));
    }
    assert_eq!(stalled, STALLS);
    // The pump stops before the flow does: its last reads find the flow.
    stop.store(true, Ordering::Release);
    pump.join().unwrap();
    shared.stop();
    rx_thread.join().unwrap();
    (shared.rings()[0].counters(), shared.lane(0))
}

#[test]
fn a_stalled_rx_thread_loses_no_sample_in_the_normal_profile() {
    let (ring, lane) = stalled_run(TimingProfile::Normal);
    // About 9000 packets in 3 s.
    assert!(lane.packets > 8_000, "{lane:?}");
    assert_eq!(
        (ring.late, ring.holes, lane.late_at_read),
        (0, 0, 0),
        "{ring:?} {lane:?}"
    );
}

#[test]
fn a_stalled_rx_thread_writes_behind_the_reader_when_it_writes() {
    // The control: the same stalls with the RX thread as the writer.
    let (ring, lane) = stalled_run(TimingProfile::LowLatency);
    assert!(lane.packets > 8_000, "{lane:?}");
    assert!(ring.late > 0, "{ring:?} {lane:?}");
    assert!(lane.late_at_read > 0, "{ring:?} {lane:?}");
}

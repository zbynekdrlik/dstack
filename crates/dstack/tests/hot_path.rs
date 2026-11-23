//! The media hot path allocates nothing (Gen 3 phase B, plan Tasks 3, 4
//! and 5): a packet written and read, a ring written and read, every TX
//! source and both checkers, the TX thread's passes (the plans, the
//! sources, the keepalives, the packets and their sends) and the RX
//! thread's (the wait on every flow socket, the plans and routes, the
//! receive, the placing in the rings, the keepalives), under
//! `assert_no_alloc` (warn mode: the violation count must stay zero). Phase
//! C adds the engine's pump (its media steps and blocks: the RX rings read,
//! the processor, the engine rings written, the stats and histograms) and
//! the TX passes that send the engine rings. The TX passes are timed and
//! their sleeps read as the TX thread does (dstack #359), and the TX pull
//! the TX thread fills before it sends (dstack #363), and the pump's late
//! wakes with their own sleep's oversleep (dstack #370). The first test
//! proves the detector sees an allocation. Its own test binary, because it installs
//! the global allocator; every buffer, ring, checker, socket, the
//! transmitter, the receiver and the pump are built before the detector
//! runs.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use dstack::clock;
use dstack::device::DeviceConfig;
use dstack::flows::control::FlowRequest;
use dstack::media::bypass::{FrameSender, Neighbour};
use dstack::media::check::{CounterCheck, SineCheck};
use dstack::media::packet::{self, MEDIA_HEADER};
use dstack::media::ring::Ring;
use dstack::media::rx::{Receiver, RxPlan, RxRoute, RxShared};
use dstack::media::source::Source;
use dstack::media::tx::{HostNic, Transmitter, TxFill, TxShared, ns_of};
use dstack::stream::{BLOCK, Map, Pump, PumpStats, Timing, WORK_MARGIN};
use dstack_audio_io::{Block, Process};

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

/// Ten years of a grandmaster's uptime at 96 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * 96_000;
const RATE: u32 = 96_000;
const FPP: usize = 32;
const SLOTS: usize = 8;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1u8; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

#[test]
fn the_media_path_does_not_allocate() {
    let rings: Vec<Ring> = (0..SLOTS).map(|_| Ring::new()).collect();
    let mut sources = [
        Source::Silence,
        Source::Counter,
        Source::Sine {
            hz: 1_000.0,
            dbfs: -20.0,
        },
        Source::Echo { rx: 0, offset: 192 },
        Source::Echo { rx: 1, offset: 229 },
        Source::Echo { rx: 99, offset: 0 },
        Source::Counter,
        Source::Sine {
            hz: 997.0,
            dbfs: 0.0,
        },
    ];
    let mut counter = CounterCheck::new(u64::from(RATE) * 10);
    let mut sine = SineCheck::new(1_000.0, RATE, 24);
    let mut frames = [[0i32; FPP]; SLOTS];
    let mut out = [0u8; 1500];
    let mut channel = [0i32; FPP];
    let mut written = 0usize;
    let mut holes = 0usize;
    reset_violation_count();
    assert_no_alloc(|| {
        for p in 0..2_000u64 {
            let stamp = T + p * FPP as u64;
            // TX: each source fills its slot, the packet is written.
            for (src, frame) in sources.iter_mut().zip(frames.iter_mut()) {
                holes += src.fill(stamp, RATE, frame, &rings, None).holes;
            }
            let (secs, sub) = (stamp / u64::from(RATE), stamp % u64::from(RATE));
            let n = packet::write(&mut out, secs as u32, sub as u32, SLOTS, 3, FPP, |f, s| {
                frames[s][f]
            });
            assert_eq!(n, MEDIA_HEADER + FPP * SLOTS * 3);
            // RX: the header, then each slot's samples into its ring.
            let (secs, sub) = packet::read_header(&out[..n]).unwrap();
            let pos = u64::from(secs) * u64::from(RATE) + u64::from(sub);
            for (slot, ring) in rings.iter().enumerate() {
                packet::read_frames(&out[..n], SLOTS, 3, |f, s, v| {
                    if s == slot {
                        channel[f] = v;
                    }
                })
                .unwrap();
                written += ring.write_at(pos, &channel).written;
            }
            // The checkers read the counter's and the sine's rings back,
            // two packets behind (written by then).
            if p >= 2 {
                let back = stamp - 2 * FPP as u64;
                rings[1].read_at(back, &mut channel);
                counter.check(back, &channel);
                rings[2].read_at(back, &mut channel);
                sine.check(back, &channel);
            }
        }
    });
    assert_eq!(violation_count(), 0, "the media path allocated");
    assert_eq!(written, 2_000 * FPP * SLOTS);
    assert!(holes > 0, "the echoes ran, the missing RX channel too");
    let c = counter.stats();
    assert_eq!((c.mismatch, c.dropout, c.slips), (0, 0, 0), "{c:?}");
    assert!(c.checked > 60_000, "{c:?}");
    let s = sine.stats();
    assert_eq!(s.discontinuities, 0, "{s:?}");
    assert!(s.checked > 60_000, "{s:?}");
}

/// A receiver's socket on loopback and its address.
fn receiver() -> (UdpSocket, SocketAddrV4) {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    s.set_nonblocking(true).unwrap();
    let SocketAddr::V4(at) = s.local_addr().unwrap() else {
        panic!("an IPv4 socket");
    };
    (s, at)
}

#[test]
fn the_tx_thread_s_passes_do_not_allocate() {
    let cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    let rings: Arc<[Ring]> = (0..2).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let shared = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
    assert!(shared.set_source(0, Source::Counter));
    assert!(shared.set_source(1, Source::Echo { rx: 0, offset: 37 }));
    assert!(shared.set_source(
        2,
        Source::Sine {
            hz: 1_000.0,
            dbfs: -20.0
        }
    ));
    let (a, at_a) = receiver();
    let (b, at_b) = receiver();
    {
        let mut c = shared.lock();
        for (rx, channels, bits, fpp) in [
            (at_a, vec![Some(1), Some(2), Some(3)], 24, 32),
            (at_b, vec![Some(3), None, Some(1)], 32, 16),
        ] {
            let req = FlowRequest {
                rx_host: "peer-device".to_string(),
                rx_flow_name: "flow-1".to_string(),
                sample_rate: RATE,
                bits,
                channels,
                fpp,
                rx,
            };
            c.table.add(&req, shared.now_ns()).unwrap();
        }
        shared.publish(&c);
    }
    // A keepalive waiting on the first flow's socket.
    a.send_to(&[0x13, 0x37], shared.local_addr(0).unwrap())
        .unwrap();
    let mut tx = Transmitter::new(Arc::clone(&shared));
    let start = T / u64::from(RATE) * 1_000_000_000;
    // The thread CPU read's TSC rate known first (Windows, dstack #485:
    // two reads 100 ms apart, once a process), so every pass is measured.
    if dstack::stream::split::thread_cpu_ns().is_none() {
        std::thread::sleep(std::time::Duration::from_nanos(
            dstack::stream::split::tsc::CALIBRATION_NS + 10_000_000,
        ));
    }
    reset_violation_count();
    let base = std::time::Instant::now();
    assert_no_alloc(|| {
        // 100 ms in passes 250 µs apart: three reads of the plans, the
        // sources and the keepalives, and every packet in between; each
        // pass timed and each sleep's return read as the TX thread's loop
        // does (dstack #359).
        for i in 0..400 {
            let now = Some(start + i * 250_000);
            tx.timed_step(now, 0, || {
                (
                    u64::try_from(base.elapsed().as_nanos()).unwrap_or(u64::MAX),
                    dstack::stream::split::thread_cpu_ns(),
                )
            });
            let woke = Some(start + (i + 1) * 250_000);
            tx.slept(tx.wake_ns(now), now, woke, true);
        }
    });
    assert_eq!(violation_count(), 0, "the TX passes allocated");
    let thread = shared.tx_thread();
    assert!(thread.pass_max_us > 0, "{thread:?}");
    assert_eq!(
        thread.pass_max_cpu_us.is_some(),
        cfg!(any(
            target_os = "linux",
            all(windows, target_arch = "x86_64")
        )),
        "{thread:?}"
    );
    let (la, lb) = (shared.lane(0), shared.lane(1));
    // 100 ms at 96 kHz: 300 packets of 32 frames, 600 of 16, less the first.
    assert!((295..=300).contains(&la.packets), "{la:?}");
    assert!((595..=600).contains(&lb.packets), "{lb:?}");
    assert_eq!((la.send_errors, lb.send_errors), (0, 0));
    assert_eq!(la.keepalives, 1);
    assert!(la.holes > 0, "the echo of an empty ring");
    let mut buf = [0u8; 2048];
    assert_eq!(a.recv(&mut buf).unwrap(), MEDIA_HEADER + 3 * 3 * 32);
    assert_eq!(b.recv(&mut buf).unwrap(), MEDIA_HEADER + 3 * 4 * 16);
}

/// dstack #1: a send the socket refuses goes through the retry decision
/// in the TX pass without allocating; a lasting refusal (Linux refuses a
/// send to port 0) is not retried and counts as a send error.
#[cfg(target_os = "linux")]
#[test]
fn a_refused_send_in_the_tx_pass_does_not_allocate() {
    let cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    let rings: Arc<[Ring]> = (0..1).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let shared = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
    assert!(shared.set_source(0, Source::Counter));
    {
        let mut c = shared.lock();
        let req = FlowRequest {
            rx_host: "peer-device".to_string(),
            rx_flow_name: "flow-1".to_string(),
            sample_rate: RATE,
            bits: 24,
            channels: vec![Some(1)],
            fpp: 32,
            rx: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0),
        };
        c.table.add(&req, shared.now_ns()).unwrap();
        shared.publish(&c);
    }
    let mut tx = Transmitter::new(Arc::clone(&shared));
    let start = T / u64::from(RATE) * 1_000_000_000;
    reset_violation_count();
    assert_no_alloc(|| {
        for i in 0..40 {
            tx.step(Some(start + i * 250_000), 0);
        }
    });
    assert_eq!(violation_count(), 0, "the refused sends allocated");
    let lane = shared.lane(0);
    assert_eq!(lane.packets, 0, "{lane:?}");
    assert!(lane.send_errors > 0, "{lane:?}");
    assert_eq!(lane.send_retries, 0, "{lane:?}");
}

/// A host frame sender that takes every second frame and counts its calls
/// and frames (atomics only: it runs inside the detector).
struct Counting {
    calls: AtomicU64,
    frames: AtomicU64,
}

impl FrameSender for Counting {
    fn send(&self, frames: &[&[u8]], sent: &mut [bool]) -> std::io::Result<()> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.frames
            .fetch_add(frames.len() as u64, Ordering::Relaxed);
        for (i, s) in sent.iter_mut().enumerate() {
            *s = i % 2 == 0;
        }
        Ok(())
    }
}

/// dstack #1: a TX pass through a host frame sender (the batch of frames,
/// the one call, the frames it did not take sent on the UDP socket)
/// allocates nothing.
#[test]
fn the_tx_passes_through_a_host_sender_do_not_allocate() {
    let cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    let rings: Arc<[Ring]> = (0..1).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let shared = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
    assert!(shared.set_source(0, Source::Counter));
    let (a, at_a) = receiver();
    let (b, at_b) = receiver();
    {
        let mut c = shared.lock();
        for rx in [at_a, at_b] {
            let req = FlowRequest {
                rx_host: "peer-device".to_string(),
                rx_flow_name: "flow-1".to_string(),
                sample_rate: RATE,
                bits: 24,
                channels: vec![Some(1)],
                fpp: 32,
                rx,
            };
            c.table.add(&req, shared.now_ns()).unwrap();
        }
        shared.publish(&c);
    }
    let sender = Arc::new(Counting {
        calls: AtomicU64::new(0),
        frames: AtomicU64::new(0),
    });
    let nic = HostNic {
        ifindex: 3,
        mac: [2, 0, 0, 0, 0, 1],
        name: "test0".to_string(),
    };
    assert!(shared.use_frame_sender(sender.clone(), nic));
    shared.set_neighbours(&[Neighbour {
        ip: Ipv4Addr::LOCALHOST,
        mac: [0, 0x1d, 0xc1, 1, 2, 3],
        ifindex: 3,
    }]);
    let mut tx = Transmitter::new(Arc::clone(&shared));
    let start = T / u64::from(RATE) * 1_000_000_000;
    reset_violation_count();
    assert_no_alloc(|| {
        for i in 0..400 {
            tx.step(Some(start + i * 250_000), 0);
        }
    });
    assert_eq!(violation_count(), 0, "the host sender's passes allocated");
    let (la, lb) = (shared.lane(0), shared.lane(1));
    let frames = sender.frames.load(Ordering::Relaxed);
    assert!((295..=300).contains(&la.packets), "{la:?}");
    assert!((295..=300).contains(&lb.packets), "{lb:?}");
    assert_eq!(frames, la.packets + lb.packets, "both flows to the sender");
    let calls = sender.calls.load(Ordering::Relaxed);
    assert!(calls > 0 && calls <= 400, "at most a call a pass: {calls}");
    let direct = la.direct + lb.direct;
    assert!(
        direct >= frames / 2 && direct < frames,
        "{direct} of {frames}"
    );
    // The frames not taken went out on the flows' sockets (sent whole; the
    // loopback receivers' buffers may not hold them all).
    assert_eq!((la.send_errors, lb.send_errors), (0, 0));
    let mut buf = [0u8; 2048];
    let got = [&a, &b].map(|s| s.recv(&mut buf).ok());
    assert!(got.contains(&Some(MEDIA_HEADER + 3 * 32)), "{got:?}");
}

#[test]
fn the_rx_thread_s_passes_do_not_allocate() {
    let rings: Arc<[Ring]> = (0..4).map(|_| Ring::new()).collect::<Vec<_>>().into();
    let shared = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, rings).unwrap());
    let plan = RxPlan {
        src: Some(Ipv4Addr::LOCALHOST),
        bps: 3,
        nslots: 2,
        latency: 192,
        delay: 0,
        block: 0,
        live: true,
        generation: 1,
        group: None,
    };
    // RX 1 and 3 from slot 0, RX 2 from slot 1, RX 4 from nothing.
    shared.publish(
        |slot| if slot == 0 { plan } else { RxPlan::default() },
        |rx| match rx {
            0 | 2 => Some(RxRoute { flow: 0, slot: 0 }),
            1 => Some(RxRoute { flow: 0, slot: 1 }),
            _ => None,
        },
    );
    let mut rx = Receiver::new(Arc::clone(&shared)).unwrap();
    let (transmitter, _) = receiver();
    let dest = shared.local_addr(0).unwrap();
    // 300 packets of 32 frames, 24-bit, two slots: 100 ms of audio.
    let packets: Vec<Vec<u8>> = (0..300u64)
        .map(|k| {
            let mut out = vec![0u8; 1500];
            let (secs, sub) = clock::stamp(T + 32 * k, RATE);
            let n = packet::write(&mut out, secs, sub, 2, 3, FPP, |f, s| {
                ((f as i32 + 1) << 8) * if s == 0 { 1 } else { -1 }
            });
            out.truncate(n);
            out
        })
        .collect();
    let now = ns_of(T + 32 * 300 + 64, RATE);
    reset_violation_count();
    assert_no_alloc(|| {
        // A pass a millisecond: 300 ms, one keepalive due at 250 ms.
        for (k, p) in packets.iter().enumerate() {
            transmitter.send_to(p, dest).unwrap();
            rx.wait(Duration::ZERO).unwrap();
            rx.pass(Some(now), k as u64 * 1_000_000);
        }
    });
    assert_eq!(violation_count(), 0, "the RX passes allocated");
    let c = shared.lane(0);
    assert_eq!((c.packets, c.refused, c.stray), (300, 0, 0), "{c:?}");
    // The packets' judge ran inside the passes. dstack #350: on a box with
    // kernel receive stamps (Linux) each age is the one at the arrival, the
    // age at the pass less its real wake, so the figures are bounded by the
    // ages at the pass; the arrival ages and the read waits add up to them
    // exactly.
    let at_pass: u64 = (0..300u64).map(|k| now - ns_of(T + 32 * k, RATE)).sum();
    assert_eq!(c.lateness_sum_ns + c.read_wait_sum_ns, at_pass, "{c:?}");
    assert_eq!(c.read_wait_count, 300);
    let peak = shared.take_max_lateness(0);
    // `read_wait_peak_us` is truncated to whole us (31 stands for up to
    // 31.999 us), and the peak lateness to whole samples: take the wait's
    // upper end, rounded up to samples, plus the peak's own sample.
    let waited = ((c.read_wait_peak_us + 1) * u64::from(RATE)).div_ceil(1_000_000) + 1;
    assert!(
        peak <= 32 * 300 + 64 && peak + waited >= 32 * 300 + 64,
        "{peak} {c:?}"
    );
    let least = shared.last_min_lateness_ns(0).unwrap();
    assert!(least <= now - ns_of(T + 32 * 299, RATE), "{least}");
    // dstack #342: every placed packet's age summed and counted inside the
    // passes too.
    assert_eq!(c.lateness_count, 300);
    assert_eq!(shared.last_lateness_mean_ns(0).map(|ns| ns > 0), Some(true));
    assert_eq!(c.keepalives, 1);
    let mut got = [0i32; FPP];
    for (ring, sign) in [(0, 1), (1, -1), (2, 1)] {
        assert_eq!(
            shared.rings()[ring].read_at(T + 32 * 299, &mut got).holes,
            0
        );
        assert_eq!(got[FPP - 1], ((FPP as i32) << 8) * sign);
    }
    assert_eq!(shared.rings()[3].read_at(T, &mut got).start_holes, FPP);
    let mut buf = [0u8; 16];
    assert_eq!(transmitter.recv(&mut buf).unwrap(), 2);
}

#[test]
fn the_pump_s_drain_of_the_rx_flows_does_not_allocate() {
    // dstack #326: in the normal profile the pump drains the flow sockets
    // before each block: the read, the parse, the judge, the ring writes.
    let mut cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    cfg.timing = dstack::device::TimingProfile::Normal;
    let rings: Arc<[Ring]> = (0..cfg.rx.len())
        .map(|_| Ring::new())
        .collect::<Vec<_>>()
        .into();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    let plan = RxPlan {
        src: Some(Ipv4Addr::LOCALHOST),
        bps: 3,
        nslots: 2,
        latency: 96,
        delay: 0,
        block: BLOCK as u16,
        live: true,
        generation: 1,
        group: None,
    };
    rx.publish(
        |slot| if slot == 0 { plan } else { RxPlan::default() },
        |c| (c < 2).then_some(RxRoute { flow: 0, slot: c }),
    );
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
    let timing = Timing::new(BLOCK, 96, WORK_MARGIN);
    let stats = Arc::new(PumpStats::new(timing, RATE));
    let map = Map {
        rx: vec![0, 1],
        tx: Vec::new(),
    };
    let mut pump = Pump::new(
        Copy,
        map,
        timing,
        24,
        Arc::clone(&rx),
        tx,
        Arc::clone(&stats),
    );
    assert!(pump.drains());
    let (transmitter, _) = receiver();
    let dest = rx.local_addr(0).unwrap();
    // 300 packets of 32 frames, one a block.
    let packets: Vec<Vec<u8>> = (0..300u64)
        .map(|k| {
            let mut out = vec![0u8; 1500];
            let (secs, sub) = clock::stamp(T + 32 * k, RATE);
            let n = packet::write(&mut out, secs, sub, 2, 3, FPP, |f, s| {
                ((f as i32 + 1) << 8) * if s == 0 { 1 } else { -1 }
            });
            out.truncate(n);
            out
        })
        .collect();
    // A free block first: the drain takes the flow on (its first drain
    // drops what waited before it).
    pump.free();
    let next = RxPlan {
        generation: 2,
        ..plan
    };
    let mut now = T + BLOCK + 96;
    reset_violation_count();
    assert_no_alloc(|| {
        for (k, p) in packets.iter().enumerate() {
            // Halfway, a new flow in the slot: its rings restart and the
            // packet that waited for it is dropped, still with no
            // allocation.
            if k == 150 {
                rx.publish(
                    |slot| if slot == 0 { next } else { RxPlan::default() },
                    |c| (c < 2).then_some(RxRoute { flow: 0, slot: c }),
                );
            }
            transmitter.send_to(p, dest).unwrap();
            // The packet in its socket before the block's drain.
            std::thread::sleep(Duration::from_micros(200));
            now = pump.media(now, 0);
        }
    });
    assert_eq!(violation_count(), 0, "the pump's drain allocated");
    let v = stats.view();
    assert_eq!((v.blocks, v.rebootstraps), (301, 0), "{v:?}");
    let c = rx.lane(0);
    assert_eq!(
        (c.packets, c.refused, c.stray, c.late),
        (299, 0, 1, 0),
        "{c:?}"
    );
    let ring = rx.rings()[1].counters();
    assert_eq!((ring.holes, ring.late), (0, 0), "{ring:?}");
}

#[test]
fn the_pump_s_late_wakes_with_their_oversleep_do_not_allocate() {
    // dstack #370: a late wake goes into the recent ring with its wall
    // time and its own sleep's oversleep, re-bootstrap or not.
    let cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    let rings: Arc<[Ring]> = (0..cfg.rx.len())
        .map(|_| Ring::new())
        .collect::<Vec<_>>()
        .into();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
    let timing = Timing::new(BLOCK, 96, WORK_MARGIN);
    let stats = Arc::new(PumpStats::new(timing, RATE));
    let mut pump = Pump::new(Copy, Map::default(), timing, 24, rx, tx, Arc::clone(&stats));
    let mut now = pump.media(T, 0);
    reset_violation_count();
    assert_no_alloc(|| {
        // 40 wakes, each 2 blocks late (every fourth past W), each after
        // a sleep that returned that late.
        for k in 0..40u64 {
            let late = if k % 4 == 3 {
                WORK_MARGIN + 1
            } else {
                2 * BLOCK
            };
            let target = ns_of(now, RATE);
            pump.slept(
                target,
                Some(target - 50_000),
                Some(ns_of(now + late, RATE)),
                true,
            );
            now = pump.media(now + late, 0);
        }
    });
    assert_eq!(violation_count(), 0, "a late wake's record allocated");
    let recent = stats.wakes().recent();
    assert_eq!(stats.wakes().late_wakes(), 40);
    assert_eq!(recent.len(), 32);
    for w in &recent {
        let us = if w.rebootstrap { 1_010 } else { 666 };
        assert_eq!((w.behind_us, w.oversleep_us), (us, Some(us)), "{recent:?}");
    }
    assert_eq!(recent.iter().filter(|w| w.rebootstrap).count(), 8);
}

/// Output j is input j (the first 24 of 32), through a stack buffer.
struct Copy;

/// The pump's blocks that carried a play time (dstack #295).
static STAMPED: AtomicU64 = AtomicU64::new(0);

impl Process for Copy {
    fn process(&mut self, block: &mut Block<'_>) {
        if block.play_at().is_some() {
            STAMPED.fetch_add(1, Ordering::Relaxed);
        }
        let mut x = [0.0f64; BLOCK as usize];
        for ch in 0..block.outputs() {
            x.copy_from_slice(block.input(ch));
            block.output(ch).copy_from_slice(&x);
        }
    }
}

/// The counter of position `p` on the 24-bit grid.
fn counter(p: u64) -> i32 {
    ((p % 65_536) << 8) as i32
}

#[test]
fn the_engine_pump_and_its_tx_passes_do_not_allocate() {
    const INPUTS: usize = 32;
    const OUTPUTS: usize = 24;
    let mut cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    cfg.tx = (1..=OUTPUTS).map(|n| format!("TX {n}")).collect();
    assert_eq!(cfg.rx.len(), INPUTS);
    let rings: Arc<[Ring]> = (0..INPUTS).map(|_| Ring::new()).collect::<Vec<_>>().into();
    for (rx, ring) in rings.iter().enumerate() {
        let v: Vec<i32> = (T - 1_000..T + 11_000)
            .map(|p| counter(p + rx as u64))
            .collect();
        ring.write_at(T - 1_000, &v);
    }
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    rx.publish(
        |_| RxPlan::default(),
        |c| {
            Some(RxRoute {
                flow: 0,
                slot: c % 8,
            })
        },
    );
    let engine: Vec<usize> = (0..OUTPUTS).collect();
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &engine).unwrap());
    for c in 0..OUTPUTS {
        assert!(tx.set_source(c, Source::Engine));
    }
    let timing = Timing::new(BLOCK, 96, WORK_MARGIN);
    let stats = Arc::new(PumpStats::new(timing, RATE));
    let map = Map {
        rx: (0..INPUTS).collect(),
        tx: engine.clone(),
    };
    let mut pump = Pump::new(
        Copy,
        map,
        timing,
        24,
        Arc::clone(&rx),
        Arc::clone(&tx),
        Arc::clone(&stats),
    );
    // A receiver of TX 1…8 and one of TX 17…24.
    let (a, at_a) = receiver();
    let (b, at_b) = receiver();
    {
        let mut c = tx.lock();
        for (to, first) in [(at_a, 1), (at_b, 17)] {
            let req = FlowRequest {
                rx_host: "peer-device".to_string(),
                rx_flow_name: "flow-1".to_string(),
                sample_rate: RATE,
                bits: 24,
                channels: (first..first + 8).map(Some).collect(),
                fpp: 32,
                rx: to,
            };
            c.table.add(&req, tx.now_ns()).unwrap();
        }
        tx.publish(&c);
    }
    let mut transmitter = Transmitter::new(Arc::clone(&tx));
    let start = ns_of(T, RATE);
    let mut now = T;
    let media = clock::ManualClock::new(start);
    reset_violation_count();
    assert_no_alloc(|| {
        // 300 blocks, one a media step, waking where the pump asks; each
        // wake reads the media and the app's clock first, as the pump
        // thread does (dstack #295), so every block carries a play time.
        for _ in 0..300 {
            // The sleep timed as the pump thread does (dstack #370).
            let target = ns_of(now, RATE);
            let asked = clock::PtpClock::now_ns(&media);
            clock::PtpClock::sleep_until(&media, target);
            pump.slept(target, asked, clock::PtpClock::now_ns(&media), true);
            pump.read_clocks(&media, clock::app_monotonic_ns);
            now = pump.media(now, 0);
        }
        // 100 ms of TX passes 250 µs apart over what the pump wrote, with
        // a bounded escape's last pass every 10 ms (dstack #1, #141).
        for i in 0..400 {
            let at = start + i * 250_000;
            transmitter.step(Some(at), 0);
            if i % 40 == 20 {
                let _ = transmitter.before_wait(|| (Some(at + 100_000), 0), || true, || false);
            }
        }
    });
    assert_eq!(violation_count(), 0, "the pump or the TX passes allocated");
    let v = stats.view();
    assert_eq!((v.blocks, v.late, v.rebootstraps), (300, 0, 0), "{v:?}");
    assert_eq!(STAMPED.load(Ordering::Relaxed), 300, "every block stamped");
    let (la, lb) = (tx.lane(0), tx.lane(1));
    assert_eq!(la.rebootstraps + lb.rebootstraps, 0, "{la:?} {lb:?}");
    assert!((295..=300).contains(&la.packets), "{la:?}");
    assert!((295..=300).contains(&lb.packets), "{lb:?}");
    // Only the packets before the pump's first output (D = 224 after its
    // first input) carry holes.
    assert!(la.holes <= 8 * 4 * 32, "{la:?}");
    // The last packet to TX 1…8: slot k is RX k + 1 D earlier.
    let mut buf = [0u8; 2048];
    let mut last = None;
    while let Ok(n) = a.recv(&mut buf) {
        last = Some(buf[..n].to_vec());
    }
    let p = last.expect("packets to the first receiver");
    let (secs, sub) = packet::read_header(&p).unwrap();
    let t = u64::from(secs) * u64::from(RATE) + u64::from(sub);
    let mut first = [0i32; 8];
    packet::read_frames(&p, 8, 3, |f, s, x| {
        if f == 0 {
            first[s] = x;
        }
    })
    .unwrap();
    for (k, &x) in first.iter().enumerate() {
        assert_eq!(x, counter(t - 224 + k as u64), "slot {k} at {t}");
    }
    assert!(b.recv(&mut buf).is_ok());
}

/// Output j at position p is the counter of p + j.
struct Counter;

impl Process for Counter {
    fn process(&mut self, block: &mut Block<'_>) {
        let start = block.start().unwrap_or(0);
        for ch in 0..block.outputs() {
            for (f, y) in block.output(ch).iter_mut().enumerate() {
                *y = dstack::stream::to_f64(counter(start + f as u64 + ch as u64));
            }
        }
    }
}

/// dstack #363: the TX thread's passes with a TX pull fill each due block
/// into the engine rings (the processor, the quantizing, the writes) and
/// then send, without allocating, a bounded escape's last pass too.
#[test]
fn the_tx_passes_with_a_tx_pull_do_not_allocate() {
    let cfg = DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml");
    let timing = Timing::of(&cfg);
    let none: Arc<[Ring]> = Arc::from(Vec::<Ring>::new());
    let tx =
        Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Arc::clone(&none), 1, &[0, 1]).unwrap());
    assert!(tx.set_source(0, Source::Engine));
    assert!(tx.set_source(1, Source::Engine));
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, none).unwrap());
    let fill = TxFill::new(Box::new(Counter), vec![0, 1], timing, 24, rx);
    assert!(tx.claim_fill(fill).is_ok());
    let (a, at_a) = receiver();
    {
        let mut c = tx.lock();
        let req = FlowRequest {
            rx_host: "peer-device".to_string(),
            rx_flow_name: "flow-1".to_string(),
            sample_rate: RATE,
            bits: 24,
            channels: vec![Some(1), Some(2)],
            fpp: 32,
            rx: at_a,
        };
        c.table.add(&req, tx.now_ns()).unwrap();
        tx.publish(&c);
    }
    let mut transmitter = Transmitter::new(Arc::clone(&tx));
    let start = ns_of(T, RATE);
    reset_violation_count();
    assert_no_alloc(|| {
        for i in 0..400 {
            let at = start + i * 250_000;
            transmitter.step(Some(at), 0);
            if i % 40 == 20 {
                let _ = transmitter.before_wait(|| (Some(at + 100_000), 0), || true, || false);
            }
        }
    });
    assert_eq!(
        violation_count(),
        0,
        "the TX passes with a TX pull allocated"
    );
    let v = tx.tx_fill().expect("the TX pull's counts");
    assert_eq!(
        (v.blocks, v.rebootstraps, v.writes_after_send),
        (300, 0, 0),
        "{v:?}"
    );
    let lane = tx.lane(0);
    assert!((295..=300).contains(&lane.packets), "{lane:?}");
    // Only the first W (96) of each channel's stamps, before the first
    // block's, as the pump's bootstrap leaves them.
    assert_eq!(lane.holes, 2 * 96, "{lane:?}");
    // The last packet: TX 2 is the counter one ahead, D = 224 after its
    // input.
    let mut buf = [0u8; 2048];
    let mut last = None;
    while let Ok(n) = a.recv(&mut buf) {
        last = Some(buf[..n].to_vec());
    }
    let p = last.expect("packets to the receiver");
    let (secs, sub) = packet::read_header(&p).unwrap();
    let t = u64::from(secs) * u64::from(RATE) + u64::from(sub);
    let mut first = [0i32; 2];
    packet::read_frames(&p, 2, 3, |f, s, x| {
        if f == 0 {
            first[s] = x;
        }
    })
    .unwrap();
    assert_eq!(first, [counter(t - 224), counter(t - 224 + 1)], "at {t}");
}

//! The pump as the RX rings' writer in the normal profile (dstack #326): it
//! drains the flow sockets just before each block reads the rings.

use super::*;

use std::net::UdpSocket;

use crate::clock::ManualClock;
use crate::device::TimingProfile;
use crate::media::packet;

/// A pump of `profile` over RX 1 (from slot 0 of a 32-frame flow from
/// loopback, slot 0 of the flow) into TX 1's engine ring.
pub(super) fn flow_rig(profile: TimingProfile) -> Rig {
    let mut cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml");
    cfg.timing = profile;
    let rings: Arc<[Ring]> = (0..cfg.rx.len()).map(|_| Ring::new()).collect();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    rx.publish(
        |slot| {
            if slot == 0 {
                RxPlan {
                    src: Some(Ipv4Addr::LOCALHOST),
                    bps: 3,
                    nslots: 1,
                    latency: P as u32,
                    delay: 0,
                    block: B as u16,
                    live: false,
                    generation: 1,
                    group: None,
                }
            } else {
                RxPlan::default()
            }
        },
        |c| (c == 0).then_some(RxRoute { flow: 0, slot: 0 }),
    );
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[0]).unwrap());
    let stats = Arc::new(PumpStats::new(TIMING, RATE));
    let map = Map {
        rx: vec![0],
        tx: vec![0],
    };
    let mut pump = Pump::new(
        Copy::default(),
        map,
        TIMING,
        24,
        Arc::clone(&rx),
        Arc::clone(&tx),
        Arc::clone(&stats),
    );
    // The flow taken on before any packet (a new flow's first drain drops
    // what waited before it).
    pump.drain(None);
    Rig {
        pump,
        rx,
        tx,
        stats,
    }
}

/// Sends the 32 frames from position `pos` to slot 0, counter-valued, and
/// waits for them to be in its socket.
pub(super) fn send(r: &Rig, t: &UdpSocket, pos: u64) {
    let mut out = [0u8; 1500];
    let (secs, sub) = crate::clock::stamp(pos, RATE);
    let n = packet::write(&mut out, secs, sub, 1, 3, B as usize, |f, _| {
        counter(pos + f as u64)
    });
    t.send_to(&out[..n], r.rx.local_addr(0).unwrap()).unwrap();
    std::thread::sleep(Duration::from_millis(20));
}

#[test]
fn a_normal_profile_pump_writes_the_rx_rings_and_a_low_latency_one_does_not() {
    let r = flow_rig(TimingProfile::Normal);
    assert!(r.pump.drains());
    assert!(r.rx.pump_writes());
    let r = flow_rig(TimingProfile::LowLatency);
    assert!(!r.pump.drains());
    assert!(!r.rx.pump_writes());
    // The RX thread claimed the rings first: a normal pump leaves them.
    let mut cfg = DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml");
    cfg.timing = TimingProfile::Normal;
    let rings: Arc<[Ring]> = (0..cfg.rx.len()).map(|_| Ring::new()).collect();
    let rx = Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::clone(&rings)).unwrap());
    let _thread = crate::media::rx::Receiver::new(Arc::clone(&rx)).unwrap();
    let tx = Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, rings, 1, &[]).unwrap());
    let pump = Pump::new(
        Copy::default(),
        Map::default(),
        TIMING,
        24,
        Arc::clone(&rx),
        tx,
        Arc::new(PumpStats::new(TIMING, RATE)),
    );
    assert!(!pump.drains());
    assert!(!rx.pump_writes());
}

#[test]
fn the_pump_drains_the_flow_just_before_the_block_that_reads_it() {
    let mut r = flow_rig(TimingProfile::Normal);
    let t = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    // Block T's input arrives, then its due time: read whole.
    send(&r, &t, T);
    let now = T + B + P;
    r.pump.media(now, 0);
    assert_eq!(r.stats.view().blocks, 1);
    let ring = r.rx.rings()[0].counters();
    assert_eq!(
        (ring.written, ring.holes, ring.late),
        (B, 0, 0),
        "written before the read: {ring:?}"
    );
    assert!(r.pump.processor.loudest > 0.0, "the block heard it");
    // Block T + B's input never came before its read: holes.
    r.pump.media(now + B, 0);
    let ring = r.rx.rings()[0].counters();
    assert_eq!((ring.holes, ring.late), (B, 0), "{ring:?}");
    // It comes after the read, with block T + 2B's: the next block's drain
    // writes the late one behind the reader, its own in time.
    send(&r, &t, T + B);
    send(&r, &t, T + 2 * B);
    r.pump.media(now + 2 * B, 0);
    let ring = r.rx.rings()[0].counters();
    assert_eq!((ring.holes, ring.late), (B, B), "{ring:?}");
    assert_eq!(r.rx.lane(0).late_at_read, 1);
    // A catch-up of two blocks: the first drain writes both blocks' input.
    send(&r, &t, T + 3 * B);
    send(&r, &t, T + 4 * B);
    r.pump.media(now + 4 * B, 0);
    assert_eq!(r.stats.view().blocks, 5);
    let ring = r.rx.rings()[0].counters();
    assert_eq!((ring.holes, ring.late), (B, B), "{ring:?}");
    let mut got = [0i32; B as usize];
    r.rx.rings()[0].read_at(T + 4 * B, &mut got);
    assert_eq!(got[7], counter(T + 4 * B + 7));
}

#[test]
fn a_low_latency_pump_reads_no_flow_socket() {
    let mut r = flow_rig(TimingProfile::LowLatency);
    let t = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    send(&r, &t, T);
    r.pump.media(T + B + P, 0);
    assert_eq!(r.rx.lane(0).packets, 0);
    assert_eq!(r.rx.rings()[0].counters().written, 0);
}

#[test]
fn the_drain_judges_its_packets_at_the_wake_s_clock_read() {
    let mut r = flow_rig(TimingProfile::Normal);
    // Synthetic times against the kernel's real receive stamps: the
    // arrival is the age at the read only without a stamp (dstack #350).
    r.rx.set_stamps(false);
    let t = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let now = T + B + P;
    // The wake read media now 7 µs into sample `now`: the judge sees it.
    let read = ns_of(now, RATE) + 7_000;
    assert_eq!(to_samples(read, RATE), now);
    send(&r, &t, T);
    assert_eq!(
        r.pump.read_clocks(&ManualClock::new(read), || 0),
        Some(read)
    );
    r.pump.media(now, 0);
    r.rx.take_max_lateness(0);
    assert_eq!(r.rx.last_min_lateness_ns(0), Some(read - ns_of(T, RATE)));
    // A media step with no read of its own: `now` itself.
    send(&r, &t, T + B);
    r.pump.media(now + B, 0);
    r.rx.take_max_lateness(0);
    assert_eq!(
        r.rx.last_min_lateness_ns(0),
        Some(ns_of(now + B, RATE) - ns_of(T + B, RATE))
    );
    // A read of another sample is not this step's.
    send(&r, &t, T + 2 * B);
    r.pump.read_clocks(&ManualClock::new(read), || 0);
    r.pump.media(now + 2 * B, 0);
    r.rx.take_max_lateness(0);
    assert_eq!(
        r.rx.last_min_lateness_ns(0),
        Some(ns_of(now + 2 * B, RATE) - ns_of(T + 2 * B, RATE))
    );
}

#[test]
fn a_free_block_drains_the_flow_as_unplaced() {
    let mut r = flow_rig(TimingProfile::Normal);
    let t = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    send(&r, &t, T);
    r.pump.free();
    let c = r.rx.lane(0);
    assert_eq!((c.packets, c.unplaced), (1, 1), "{c:?}");
    assert_eq!(r.rx.rings()[0].counters().written, 0);
}

#[test]
fn a_late_pump_counts_late_only_the_packets_it_already_read_past() {
    // dstack #477 (presenter-listen, 21.11.2026: 40 late packets of
    // 0.26-0.34 ms arrivals at P 5 ms, in the seconds of pump wakes 4-6 ms
    // late): a packet is late when the reader already read its position,
    // and the pump reads by its schedule, not by now. A wake late by
    // `behind` reads first the block due `behind` ago, so what came since
    // the last drain is in time though now is past P + B of it.
    let mut r = flow_rig(TimingProfile::Normal);
    r.rx.set_stamps(false);
    let t = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let now = T + B + P;
    send(&r, &t, T);
    r.pump.media(now, 0);
    // Block T + B's input arrives in time; the pump's next wake comes two
    // blocks late (within the margin: three blocks run, none skipped). Now
    // is P + 3B past its first frame, the reader's position still T + B.
    send(&r, &t, T + B);
    let behind = 2 * B;
    const _: () = assert!(2 * B < WORK_MARGIN);
    r.pump.media(now + B + behind, 0);
    assert_eq!(r.stats.view().blocks, 4);
    let c = r.rx.lane(0);
    assert_eq!((c.packets, c.late, c.late_at_read), (2, 0, 0), "{c:?}");
    let ring = r.rx.rings()[0].counters();
    assert_eq!((ring.written, ring.late), (2 * B, 0), "{ring:?}");
    // Block T + 2B's input comes only now, after its block was read (a
    // hole): truly late, counted, at an on-time wake.
    send(&r, &t, T + 2 * B);
    r.pump.media(now + 4 * B, 0);
    let c = r.rx.lane(0);
    assert_eq!((c.packets, c.late, c.late_at_read), (3, 1, 1), "{c:?}");
}

#[test]
fn a_late_pump_keeps_a_hole_open_until_the_block_it_reads_next() {
    // dstack #477: a hole is lost once the reader read past it, judged by
    // the block the pump reads next, never by now less P less a block.
    let mut r = flow_rig(TimingProfile::Normal);
    r.rx.set_stamps(false);
    let t = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    let now = T + B + P;
    // Block T and the packet after a missing one (T + B): a hole opens.
    send(&r, &t, T);
    send(&r, &t, T + 2 * B);
    r.pump.media(now, 0);
    // The missing packet comes before a wake two blocks late, which reads
    // block T + B first: it fills its hole in time.
    send(&r, &t, T + B);
    r.pump.media(now + 3 * B, 0);
    let c = r.rx.lane(0);
    assert_eq!(
        (c.packets, c.gaps, c.reordered, c.late, c.late_at_read),
        (3, 0, 1, 0, 0),
        "{c:?}"
    );
    // The ring read blocks T to T + 3B: only T + 3B, never sent, is a hole.
    let ring = r.rx.rings()[0].counters();
    assert_eq!(
        (ring.written, ring.holes, ring.late),
        (3 * B, B, 0),
        "{ring:?}"
    );
}

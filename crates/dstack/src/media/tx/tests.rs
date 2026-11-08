use super::*;

use std::sync::atomic::AtomicU64;
use std::time::Duration;

use crate::flows::control::split;
use crate::media::packet::{read_frames, read_header};
use crate::wire::tests::Rng;

const TEST_DEVICE: &str = include_str!("../../../../../config/test-device.toml");
const RATE: u32 = 96_000;
const FPP: u64 = 32;
/// Ten years of a grandmaster's uptime, ns.
const T: u64 = 10 * 365 * 86_400 * 1_000_000_000;
const MS: u64 = 1_000_000;
const PEER_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 11);

/// The test device: 16 TX channels at 96 kHz, 24 bits, a 1 ms latency
/// (96 samples, the schedules' `max_lag`).
fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(TEST_DEVICE).expect("config/test-device.toml")
}

fn req(rx: SocketAddrV4) -> FlowRequest {
    FlowRequest {
        rx_host: "peer-device".to_string(),
        rx_flow_name: "flow-1".to_string(),
        sample_rate: RATE,
        bits: 24,
        channels: vec![Some(1), Some(2)],
        fpp: FPP as u16,
        rx,
    }
}

fn peer(port: u16) -> SocketAddrV4 {
    SocketAddrV4::new(PEER_IP, port)
}

// ---- the table ----

#[test]
fn flows_take_slots_in_turn_with_cookies_from_the_seed() {
    let mut t = TxTable::new(1);
    let handles: Vec<Handle> = (0..3)
        .map(|i| t.add(&req(peer(4000 + i)), 0).unwrap().1)
        .collect();
    // The cookies are SplitMix64's top 16 bits, from the seed on.
    let mut rng = Rng::new(1);
    for (slot, h) in handles.iter().enumerate() {
        assert_eq!(split(h), (slot as u32, (rng.next() >> 48) as u16));
    }
    assert_eq!(t.stop(&handles[1]), Ok(1));
    // The sequence goes on past the freed slot, then wraps to it.
    assert_eq!(t.add(&req(peer(4100)), 0).unwrap().0, 3);
    for i in 4..MAX_FLOWS {
        assert_eq!(t.add(&req(peer(4100 + i as u16)), 0).unwrap().0, i);
    }
    assert_eq!(t.add(&req(peer(4200)), 0).unwrap().0, 1);
    assert_eq!(t.add(&req(peer(4201)), 0), Err(status::TOO_MANY_TX_FLOWS));
    assert_eq!(t.flows().count(), MAX_FLOWS);
    // Another seed, other cookies.
    let mut other = TxTable::new(2);
    assert_ne!(other.add(&req(peer(4000)), 0).unwrap().1, handles[0]);
}

#[test]
fn a_datagram_at_3_9_s_keeps_the_flow_none_for_4_s_expires_it() {
    let s = 1_000_000_000;
    let mut t = TxTable::new(1);
    t.add(&req(peer(4000)), 0).unwrap();
    t.add(&req(peer(4001)), 0).unwrap();
    t.keepalive(0, 39 * s / 10);
    // Slot 1 was never heard: it expires 4 s after its request, slot 0 4 s
    // after its datagram.
    assert_eq!(t.expire(4 * s - 1), 0);
    assert_eq!(t.expire(4 * s), 0b10);
    assert_eq!(t.expire(39 * s / 10 + 4 * s - 1), 0);
    assert_eq!(t.expire(39 * s / 10 + 4 * s), 0b01);
    // Expired once: not again.
    assert_eq!(t.expire(100 * s), 0);
    assert!(t.flows().all(|(_, f)| f.expired));
    // A time before the last datagram never expires a flow.
    let mut t = TxTable::new(1);
    t.add(&req(peer(4000)), 10 * s).unwrap();
    assert_eq!(t.expire(5 * s), 0);
}

#[test]
fn a_datagram_after_expiry_revives_nothing() {
    let mut t = TxTable::new(1);
    t.add(&req(peer(4000)), 0).unwrap();
    t.expire(KEEPALIVE_TIMEOUT_NS);
    t.keepalive(0, KEEPALIVE_TIMEOUT_NS + 1);
    let f = t.flow(0).unwrap();
    assert_eq!((f.expired, f.heard_ns), (true, 0));
    assert_eq!(t.plan(0), Plan::default());
    // Empty or missing slots are no flow.
    t.keepalive(1, 5);
    t.keepalive(999, 5);
    assert_eq!(t.flow(1), None);
    assert_eq!(t.flow(999), None);
}

#[test]
fn slot_of_finds_a_destination_expired_or_not() {
    let mut t = TxTable::new(1);
    assert_eq!(t.slot_of(peer(4000)), None);
    t.add(&req(peer(4000)), 0).unwrap();
    t.add(&req(peer(4001)), 0).unwrap();
    assert_eq!(t.slot_of(peer(4001)), Some(1));
    assert_eq!(t.slot_of(peer(4002)), None);
    t.expire(KEEPALIVE_TIMEOUT_NS);
    assert_eq!(t.slot_of(peer(4000)), Some(0));
}

#[test]
fn get_finds_a_flow_by_its_slot_and_cookie_only() {
    let mut t = TxTable::new(1);
    let (_, h) = t.add(&req(peer(4000)), 0).unwrap();
    let (slot, cookie) = split(&h);
    assert_eq!(t.get(&h).map(|(s, f)| (s, f.dest)), Some((0, peer(4000))));
    assert!(t.get(&control::handle(slot, cookie ^ 1)).is_none());
    assert!(t.get(&control::handle(5, cookie)).is_none());
    assert!(t.get(&control::handle(u32::MAX, cookie)).is_none());
    assert_eq!(
        t.update(&control::handle(5, cookie), &[], 0),
        Err(status::FLOW_NOT_FOUND)
    );
}

#[test]
fn a_plan_sends_only_a_live_flow_that_can_be_sent() {
    let mut t = TxTable::new(1);
    assert_eq!(t.plan(0), Plan::default());
    assert_eq!(t.plan(999), Plan::default());
    t.add(
        &FlowRequest {
            channels: vec![Some(3), None, Some(16)],
            bits: 32,
            fpp: 16,
            ..req(peer(4000))
        },
        0,
    )
    .unwrap();
    assert_eq!(
        t.plan(0),
        Plan {
            dest: Some(peer(4000)),
            fpp: 16,
            bps: 4,
            nch: 3,
            channels: [3, 0, 16, 0, 0, 0, 0, 0],
        }
    );
    t.expire(KEEPALIVE_TIMEOUT_NS);
    assert_eq!(t.plan(0), Plan::default());
    // The table itself takes anything; the plan sends only what can be
    // sent (the server refuses the rest before).
    for bad in [
        FlowRequest {
            fpp: 0,
            ..req(peer(5000))
        },
        FlowRequest {
            bits: 8,
            ..req(peer(5001))
        },
        FlowRequest {
            channels: vec![None; 9],
            ..req(peer(5002))
        },
    ] {
        let (slot, _) = t.add(&bad, 0).unwrap();
        assert_eq!(t.plan(slot), Plan::default(), "{bad:?}");
    }
}

#[test]
fn sendable_bounds_slots_widths_frames_and_the_datagram() {
    for (nch, bps, fpp, ok) in [
        (8, 4, 45, true),
        (8, 4, 46, false),
        (8, 3, 60, true),
        (8, 3, 61, false),
        (7, 3, 61, true),
        (9, 2, 1, false),
        (0, 2, 1, true),
        (1, 1, 1, false),
        (1, 2, 1, true),
        (1, 4, 1, true),
        (1, 5, 1, false),
        (1, 4, 0, false),
        (1, 2, 256, true),
        (1, 2, 257, false),
    ] {
        assert_eq!(sendable(nch, bps, fpp), ok, "{nch} {bps} {fpp}");
    }
}

// ---- what the TX thread reads ----

#[test]
fn a_plan_round_trips_through_its_words() {
    let plan = Plan {
        dest: Some(SocketAddrV4::new(Ipv4Addr::new(10, 1, 2, 3), 0xbeef)),
        fpp: 0x1234,
        bps: 3,
        nch: 8,
        channels: [1, 2, 3, 4, 0xfff5, 6, 7, 8],
    };
    let w = plan.words();
    assert_eq!(w[0], 0x0a01_0203_beef_1234);
    assert_eq!(w[1], (1 << 63) + 0x0308);
    assert_eq!(w[2], 0x0004_0003_0002_0001);
    assert_eq!(w[3], 0x0008_0007_0006_fff5);
    assert_eq!(Plan::from_words(w), plan);
    assert_eq!(Plan::default().words(), [0; 4]);
    assert_eq!(Plan::from_words([0; 4]), Plan::default());
    // A word set without the live bit is no plan.
    assert_eq!(
        Plan::from_words([w[0], 0x0308, w[2], w[3]]),
        Plan::default()
    );
}

#[test]
fn a_new_receiver_fpp_or_width_restarts_the_schedule_channels_do_not() {
    let base = Plan {
        dest: Some(peer(4000)),
        fpp: 32,
        bps: 3,
        nch: 2,
        channels: [1, 2, 0, 0, 0, 0, 0, 0],
    };
    assert!(!base.restarts(&base));
    let channels = Plan {
        nch: 3,
        channels: [5, 0, 7, 0, 0, 0, 0, 0],
        ..base
    };
    assert!(!channels.restarts(&base));
    for changed in [
        Plan {
            dest: Some(peer(4001)),
            ..base
        },
        Plan { dest: None, ..base },
        Plan { fpp: 16, ..base },
        Plan { bps: 4, ..base },
    ] {
        assert!(changed.restarts(&base), "{changed:?}");
        assert!(base.restarts(&changed), "{changed:?}");
    }
}

#[test]
fn every_source_survives_its_words() {
    for s in [
        Source::Silence,
        Source::Counter,
        Source::Sine {
            hz: 997.5,
            dbfs: -23.25,
        },
        Source::Echo {
            rx: 31,
            offset: 229,
        },
        Source::Echo {
            rx: 0,
            offset: u32::MAX,
        },
        Source::Engine,
    ] {
        assert_eq!(source_from(source_words(s)), s);
    }
    assert_eq!(source_words(Source::Counter), [1, 0, 0]);
    assert_eq!(source_words(Source::Engine), [4, 0, 0]);
    assert_eq!(source_from([9, 1, 2]), Source::Silence);
    assert_eq!(source_from([5, 0, 0]), Source::Silence);
}

#[test]
fn a_cell_reads_whole_writes_and_counts_its_versions() {
    let cell = Cell::<3>::new();
    assert_eq!(cell.read(), Some((0, [0; 3])));
    cell.write([1, 2, 3]);
    assert_eq!(cell.version(), 2);
    assert_eq!(cell.read(), Some((2, [1, 2, 3])));
    assert_eq!(cell.peek(), [1, 2, 3]);
    cell.write([4, 5, 6]);
    assert_eq!(cell.read(), Some((4, [4, 5, 6])));
    // A write under way (an odd version): the reader gives up, never
    // blocks, never reads half of it.
    cell.seq.store(5, Ordering::SeqCst);
    assert_eq!(cell.read(), None);
}

#[test]
fn a_reader_never_sees_a_torn_write() {
    let cell = Arc::new(Cell::<4>::new());
    let writer = {
        let cell = Arc::clone(&cell);
        std::thread::spawn(move || {
            for k in 1..=200_000u64 {
                cell.write([k; 4]);
            }
        })
    };
    let mut last = 0;
    let mut reads = 0;
    while !writer.is_finished() || reads == 0 {
        if let Some((v, w)) = cell.read() {
            assert!(w.iter().all(|&x| x == w[0]), "torn: {w:?}");
            assert!(v >= last, "versions only grow");
            assert_eq!(u64::from(v), 2 * w[0], "the version of that write");
            last = v;
            reads += 1;
        }
    }
    writer.join().unwrap();
    assert_eq!(cell.read(), Some((400_000, [200_000; 4])));
}

// ---- the schedule ----

#[test]
fn the_first_stamp_is_the_first_multiple_of_fpp_after_now() {
    for (now, next) in [
        (0, 32),
        (1, 32),
        (31, 32),
        (32, 64),
        (33, 64),
        (1_000_007, 1_000_032),
    ] {
        let s = Schedule::bootstrap(now, 32, 96);
        assert_eq!((s.next, s.fpp, s.max_lag), (next, 32, 96), "{now}");
    }
    // fpp 0 reads as 1: no division by zero.
    assert_eq!(Schedule::bootstrap(5, 0, 96).next, 6);
}

#[test]
fn nothing_is_sent_before_stamp_plus_fpp() {
    let mut s = Schedule::bootstrap(1_000, 32, 96);
    let mut stamps = Vec::new();
    for now in 1_000..2_000 {
        while let Some(t) = s.due(now) {
            assert_eq!(now, t + FPP, "sent the first moment its last sample passed");
            stamps.push(t);
        }
    }
    assert_eq!(stamps.first(), Some(&1_024));
    assert!(stamps.windows(2).all(|w| w[1] == w[0] + FPP));
    assert_eq!(stamps.len(), (2_000 - 1_056 - 1) / 32 + 1);
    assert_eq!(s.take_rebootstraps(), 0);
}

#[test]
fn a_lag_beyond_the_latency_restarts_once() {
    // Due at 1 056; 96 samples late is still sent.
    let mut s = Schedule::bootstrap(1_000, 32, 96);
    assert_eq!(s.due(1_056 + 96), Some(1_024));
    assert_eq!(s.take_rebootstraps(), 0);
    // 97 late: started again on the next multiple after now, nothing sent.
    let mut s = Schedule::bootstrap(1_000, 32, 96);
    assert_eq!(s.due(1_056 + 97), None);
    assert_eq!(s.next, 1_184);
    assert_eq!(s.due(1_153), None);
    assert_eq!(s.take_rebootstraps(), 1);
    assert_eq!(s.take_rebootstraps(), 0);
    assert_eq!(s.due(1_184 + 32), Some(1_184));
}

#[test]
fn a_clock_jump_back_restarts_once() {
    // Due at 1 000 064: a clock 192 000 samples before that is a jump.
    let mut s = Schedule::bootstrap(1_000_000, 32, 96);
    assert_eq!(s.due(808_064), None);
    assert_eq!(
        s.take_rebootstraps(),
        0,
        "exactly the jump's size is no jump"
    );
    assert_eq!(s.due(808_063), None);
    assert_eq!(s.take_rebootstraps(), 1);
    assert_eq!(s.next, 808_064);
}

#[test]
fn a_realign_counts_and_starts_on_a_multiple_of_fpp() {
    let mut s = Schedule::bootstrap(0, 32, 96);
    s.realign(1_000);
    s.realign(2_000);
    assert_eq!((s.next, s.take_rebootstraps()), (2_016, 2));
}

#[test]
fn wake_ns_is_the_first_ns_the_next_packet_is_due() {
    for rate in [44_100, 48_000, 88_200, 96_000, 192_000] {
        for next in [0, 31, 1_000_003, 96_000 * 86_400 * 3_650] {
            let s = Schedule {
                next,
                fpp: 32,
                max_lag: 96,
                rebootstraps: 0,
            };
            let w = s.wake_ns(rate, TX_MARGIN_NS);
            assert!(
                send_clock(w, rate, TX_MARGIN_NS) >= next + 32,
                "{rate} {next}"
            );
            assert!(
                send_clock(w - 1, rate, TX_MARGIN_NS) < next + 32,
                "{rate} {next}"
            );
        }
    }
}

#[test]
fn ns_of_and_send_clock_round_the_safe_way() {
    assert_eq!(ns_of(0, RATE), 0);
    assert_eq!(ns_of(96_000, RATE), 1_000_000_000);
    assert_eq!(ns_of(1, RATE), 10_417, "10 416.67 rounded up");
    assert_eq!(ns_of(2, 0), 2_000_000_000, "a rate of 0 reads as 1");
    assert_eq!(ns_of(u64::MAX, 1), u64::MAX, "saturates");
    assert_eq!(send_clock(0, RATE, TX_MARGIN_NS), 0);
    assert_eq!(send_clock(TX_MARGIN_NS + 10_416, RATE, TX_MARGIN_NS), 0);
    assert_eq!(send_clock(TX_MARGIN_NS + 10_417, RATE, TX_MARGIN_NS), 1);
}

#[test]
fn packets_a_second_are_rate_over_fpp() {
    for (rate, fpp, per_second) in [
        (96_000, 32, 3_000),
        (48_000, 16, 3_000),
        (96_000, 256, 375),
        (192_000, 1, 192_000),
    ] {
        // The TX loop: wake when the next packet is due, send what is due.
        // The packets sent in one second, from 0.1 s after the start.
        let start = T;
        let window = start + 100 * MS..start + 1_100 * MS;
        let mut s = Schedule::bootstrap(send_clock(start, rate, TX_MARGIN_NS), fpp, 96);
        let mut now_ns = start;
        let mut sent = 0;
        let mut wakes = 0;
        while now_ns < window.end {
            while s.due(send_clock(now_ns, rate, TX_MARGIN_NS)).is_some() {
                if window.contains(&now_ns) {
                    sent += 1;
                }
            }
            now_ns = s.wake_ns(rate, TX_MARGIN_NS);
            wakes += 1;
            assert!(wakes < 400_000, "the loop advances");
        }
        let expected: i64 = per_second;
        assert!(
            (sent as i64 - expected).abs() <= 1,
            "{rate} Hz {fpp}: {sent} packets"
        );
        assert_eq!(s.take_rebootstraps(), 0);
    }
}

// ---- the shared state ----

fn shared_with(rings: Vec<Ring>) -> Arc<TxShared> {
    Arc::new(TxShared::new(cfg(), Ipv4Addr::LOCALHOST, rings.into(), 1, &[]).unwrap())
}

/// Adds a flow and hands it to the TX side; returns its slot and handle.
fn add(shared: &TxShared, r: &FlowRequest) -> (usize, Handle) {
    let mut c = shared.lock();
    let added = c.table.add(r, shared.now_ns()).unwrap();
    shared.publish(&c);
    added
}

#[test]
fn sync_keeps_a_heard_flow_alive_and_expires_a_silent_one() {
    let shared = shared_with(Vec::new());
    let s = 1_000_000_000;
    {
        let mut c = shared.lock();
        c.table.add(&req(peer(4000)), 0).unwrap();
        c.table.add(&req(peer(4001)), 0).unwrap();
        shared.publish(&c);
    }
    // Slot 0's receiver is heard (the TX thread's count moved).
    shared.lanes[0].keepalives.fetch_add(1, Ordering::Relaxed);
    assert_eq!(shared.sync(39 * s / 10), 0);
    assert_eq!(shared.sync(4 * s), 0b10);
    assert_eq!(shared.plans[1].read().unwrap().1, [0; 4], "handed over");
    assert_ne!(shared.plans[0].read().unwrap().1, [0; 4]);
    // No count moved since: slot 0 expires 4 s after it was heard.
    assert_eq!(shared.sync(79 * s / 10 - 1), 0);
    assert_eq!(shared.sync(79 * s / 10), 0b01);
    assert_eq!(shared.plans[0].read().unwrap().1, [0; 4]);
}

#[test]
fn publish_writes_only_the_plans_that_changed() {
    let shared = shared_with(Vec::new());
    let versions = |s: &TxShared| -> Vec<u32> { s.plans.iter().map(Cell::version).collect() };
    shared.publish(&shared.lock());
    assert_eq!(versions(&shared), vec![0; MAX_FLOWS]);
    let (slot, _) = add(&shared, &req(peer(4000)));
    let after = versions(&shared);
    assert_eq!(after[slot], 2);
    assert!(after.iter().enumerate().all(|(i, &v)| i == slot || v == 0));
    shared.publish(&shared.lock());
    assert_eq!(versions(&shared), after);
}

#[test]
fn sources_are_silent_at_start_and_set_at_runtime() {
    let shared = shared_with(Vec::new());
    assert_eq!(shared.source(0), Some(Source::Silence));
    assert_eq!(shared.source(15), Some(Source::Silence));
    assert_eq!(shared.source(16), None, "16 TX channels");
    assert!(shared.set_source(15, Source::Counter));
    assert_eq!(shared.source(15), Some(Source::Counter));
    assert!(!shared.set_source(16, Source::Counter));
}

/// The TX state with engine rings on `engine` (TX channel indices).
fn shared_with_engine(rings: Vec<Ring>, engine: &[usize]) -> Arc<TxShared> {
    Arc::new(TxShared::new(cfg(), Ipv4Addr::LOCALHOST, rings.into(), 1, engine).unwrap())
}

#[test]
fn engine_is_refused_for_a_channel_without_an_engine_ring() {
    let shared = shared_with_engine(Vec::new(), &[1, 15]);
    let has: Vec<bool> = (0..17).map(|tx| shared.engine_ring(tx).is_some()).collect();
    let mut want = vec![false; 17];
    want[1] = true;
    want[15] = true;
    assert_eq!(has, want, "TX 2 and TX 16 only; TX 17 does not exist");
    assert!(!shared.set_source(0, Source::Engine));
    assert_eq!(shared.source(0), Some(Source::Silence), "unchanged");
    assert!(shared.set_source(1, Source::Engine));
    assert_eq!(shared.source(1), Some(Source::Engine));
    assert!(shared.set_source(15, Source::Engine));
    assert!(!shared.set_source(16, Source::Engine));
    // A channel with an engine ring takes any other source too, and back.
    assert!(shared.set_source(1, Source::Counter));
    assert_eq!(shared.source(1), Some(Source::Counter));
    assert!(shared.set_source(1, Source::Engine));
    assert_eq!(shared.source(1), Some(Source::Engine));
    // Each ring is its own.
    shared.engine_ring(1).unwrap().put_at(T, &[5 << 8]);
    let mut got = [0; 1];
    assert_eq!(
        shared.engine_ring(15).unwrap().read_at(T, &mut got).holes,
        1
    );
    // The standalone device has none.
    let none = shared_with(Vec::new());
    assert!((0..16).all(|tx| none.engine_ring(tx).is_none()));
    assert!(!none.set_source(0, Source::Engine));
}

#[test]
fn an_engine_channel_past_the_tx_count_is_refused() {
    for engine in [&[16][..], &[0, 99]] {
        let e = TxShared::new(cfg(), Ipv4Addr::LOCALHOST, Vec::new().into(), 1, engine)
            .expect_err("refused");
        assert_eq!(e.kind(), io::ErrorKind::InvalidInput, "{engine:?}");
        assert_eq!(
            e.to_string(),
            "an engine channel past the device's 16 TX channels"
        );
    }
    // The last channel is the device's own.
    assert!(TxShared::new(cfg(), Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[15]).is_ok());
}

#[test]
fn each_slot_has_its_own_socket_counts_and_the_stop_flag() {
    let rings: Arc<[Ring]> = vec![Ring::new(), Ring::new()].into();
    let shared = TxShared::new(cfg(), Ipv4Addr::LOCALHOST, Arc::clone(&rings), 1, &[]).unwrap();
    let mut ports: Vec<u16> = (0..MAX_FLOWS)
        .map(|s| shared.local_addr(s).unwrap().port())
        .collect();
    assert!(
        (0..MAX_FLOWS).all(
            |s| shared.local_addr(s).unwrap().ip() == std::net::IpAddr::V4(Ipv4Addr::LOCALHOST)
        )
    );
    ports.sort_unstable();
    ports.dedup();
    assert_eq!(ports.len(), MAX_FLOWS);
    assert!(shared.local_addr(MAX_FLOWS).is_err());
    assert_eq!(shared.lane(MAX_FLOWS), LaneCounts::default());
    assert!(
        Arc::ptr_eq(shared.rings(), &rings),
        "the RX rings themselves"
    );
    assert!(!shared.stopping());
    shared.stop();
    assert!(shared.stopping());
    let a = shared.now_ns();
    std::thread::sleep(Duration::from_millis(2));
    assert!(shared.now_ns() >= a + 2 * MS);
}

// ---- the transmitter on loopback ----

/// A transmitter, its receiver's socket and a stranger's.
struct Rig {
    shared: Arc<TxShared>,
    tx: Transmitter,
    rx: UdpSocket,
    stranger: UdpSocket,
}

fn socket() -> UdpSocket {
    let s = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    s.set_nonblocking(true).unwrap();
    s
}

fn v4(s: &UdpSocket) -> SocketAddrV4 {
    match s.local_addr().unwrap() {
        SocketAddr::V4(a) => a,
        SocketAddr::V6(_) => panic!("an IPv4 socket"),
    }
}

impl Rig {
    fn new(rings: Vec<Ring>) -> Self {
        Rig::with(shared_with(rings))
    }

    /// A rig whose TX channels at the indices `engine` have engine rings.
    fn with_engine(engine: &[usize]) -> Self {
        Rig::with(shared_with_engine(Vec::new(), engine))
    }

    fn with(shared: Arc<TxShared>) -> Self {
        Rig {
            tx: Transmitter::new(Arc::clone(&shared)),
            shared,
            rx: socket(),
            stranger: socket(),
        }
    }

    /// A flow of `channels` to the rig's receiver.
    fn flow(&self, channels: Vec<Option<u16>>) -> (usize, Handle) {
        add(
            &self.shared,
            &FlowRequest {
                channels,
                ..req(v4(&self.rx))
            },
        )
    }
}

/// Every datagram waiting on `s`.
fn drain(s: &UdpSocket) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut buf = [0u8; 2048];
    while let Ok(n) = s.recv(&mut buf) {
        out.push(buf[..n].to_vec());
    }
    out
}

fn stamp_of(p: &[u8]) -> u64 {
    let (secs, sub) = read_header(p).unwrap();
    u64::from(secs) * u64::from(RATE) + u64::from(sub)
}

/// Slot `slot`'s samples of a packet of `nch` 24-bit slots.
fn slot_samples(p: &[u8], nch: usize, slot: usize) -> Vec<i32> {
    let mut v = Vec::new();
    read_frames(p, nch, 3, |_, s, x| {
        if s == slot {
            v.push(x);
        }
    })
    .unwrap();
    v
}

fn counter(t: u64, n: usize) -> Vec<i32> {
    (0..n as u64)
        .map(|i| (((t + i) % 65_536) << 8) as i32)
        .collect()
}

/// The packets of `fpp` frames sent by stepping every 500 µs from
/// `T + from_us` to `T + to_us`, each checked to have left in time: never
/// before its last sample + the margin, and none due left behind.
fn run_fpp(rig: &mut Rig, from_us: u64, to_us: u64, fpp: u64) -> Vec<Vec<u8>> {
    let mut all = Vec::new();
    let mut at = from_us;
    while at <= to_us {
        let now = T + at * 1_000;
        rig.tx.step(Some(now), 0);
        let got = drain(&rig.rx);
        if let Some(last) = got.last() {
            let t = stamp_of(last);
            assert!(ns_of(t + fpp, RATE) + TX_MARGIN_NS <= now, "never early");
            assert!(
                ns_of(t + 2 * fpp, RATE) + TX_MARGIN_NS > now,
                "none due left"
            );
        }
        all.extend(got);
        at += 500;
    }
    all
}

/// [`run_fpp`] of 32-frame packets.
fn run(rig: &mut Rig, from_us: u64, to_us: u64) -> Vec<Vec<u8>> {
    run_fpp(rig, from_us, to_us, FPP)
}

#[test]
fn a_flow_carries_its_sources_samples_stamped_t_to_its_receiver_only() {
    let mut rig = Rig::new(Vec::new());
    assert!(rig.shared.set_source(0, Source::Counter));
    let (slot, _) = rig.flow(vec![Some(1), Some(2), None]);
    let packets = run(&mut rig, 0, 20_000);
    // 20 ms at 96 kHz, 32 frames a packet: 60, less the first.
    assert!((58..=60).contains(&packets.len()), "{}", packets.len());
    let first = stamp_of(&packets[0]);
    for (k, p) in packets.iter().enumerate() {
        assert_eq!(p.len(), 9 + 3 * 3 * 32);
        assert_eq!(p[0], 2);
        let t = stamp_of(p);
        assert_eq!(t, first + k as u64 * FPP, "every stamp, in turn");
        assert_eq!(t % FPP, 0);
        assert_eq!(
            slot_samples(p, 3, 0),
            counter(t, 32),
            "TX 1's counter at T…T+31"
        );
        assert_eq!(slot_samples(p, 3, 1), vec![0; 32], "TX 2 is silent");
        assert_eq!(slot_samples(p, 3, 2), vec![0; 32], "the empty slot");
    }
    assert!(drain(&rig.stranger).is_empty());
    let lane = rig.shared.lane(slot);
    assert_eq!(
        (
            lane.packets,
            lane.send_errors,
            lane.rebootstraps,
            lane.holes
        ),
        (packets.len() as u64, 0, 0, 0)
    );
}

#[test]
fn a_sent_channel_s_peak_is_kept_until_taken() {
    let mut rig = Rig::new(Vec::new());
    assert!(rig.shared.set_source(0, Source::Counter));
    assert!(rig.shared.set_source(
        2,
        Source::Sine {
            hz: 1_000.0,
            dbfs: -20.0
        }
    ));
    rig.flow(vec![Some(1), Some(2), Some(3)]);
    // Nothing sent yet: no peak.
    assert_eq!(rig.shared.take_peak(0), 0);
    let packets = run(&mut rig, 0, 20_000);
    // The counter's largest sample sent, read back from the packets.
    let loudest = packets
        .iter()
        .flat_map(|p| slot_samples(p, 3, 0))
        .map(i32::unsigned_abs)
        .max()
        .unwrap();
    assert!(loudest > 0);
    assert_eq!(rig.shared.take_peak(0), loudest);
    assert_eq!(rig.shared.take_peak(0), 0, "taken");
    // TX 2 is silent; TX 3's sine is at most -20 dBFS and near it.
    assert_eq!(rig.shared.take_peak(1), 0);
    let sine = rig.shared.take_peak(2);
    let cap = (f64::from(i32::MAX) * 0.1) as u32;
    assert!((cap - cap / 100..=cap).contains(&sine), "{sine}");
    // A channel no flow sends, and one the device does not have.
    assert_eq!(rig.shared.take_peak(3), 0);
    assert_eq!(rig.shared.take_peak(99), 0);
}

#[test]
fn nothing_is_sent_before_the_clock_is_ready_but_keepalives_are_read() {
    let mut rig = Rig::new(Vec::new());
    let (slot, _) = rig.flow(vec![Some(1)]);
    let to = rig.shared.local_addr(slot).unwrap();
    for _ in 0..5 {
        rig.tx.step(None, 0);
        assert_eq!(rig.tx.wake_ns(None), 0, "a clock that is not ready naps");
    }
    rig.rx.send_to(&[0x13, 0x37], to).unwrap();
    rig.tx.step(None, 0);
    assert!(drain(&rig.rx).is_empty());
    assert_eq!(rig.shared.lane(slot).keepalives, 1);
    // Ready: the flow starts.
    assert!(!run(&mut rig, 0, 2_000).is_empty());
}

#[test]
fn echo_sends_the_rx_sample_offset_earlier_and_counts_its_holes() {
    let ring = Ring::new();
    let n0 = send_clock(T, RATE, TX_MARGIN_NS);
    // RX 1 holds positions up to n0 + 600: the echo of later ones are holes.
    let base = n0 - 1_000;
    let values: Vec<i32> = (base..n0 + 600)
        .map(|p| ((p % 65_536) << 8) as i32)
        .collect();
    ring.write_at(base, &values);
    let mut rig = Rig::new(vec![ring]);
    assert!(rig.shared.set_source(0, Source::Echo { rx: 0, offset: 37 }));
    let (slot, _) = rig.flow(vec![Some(1)]);
    let packets = run(&mut rig, 0, 10_000);
    let mut holes = 0;
    for p in &packets {
        let t = stamp_of(p);
        let got = slot_samples(p, 1, 0);
        for (f, &x) in got.iter().enumerate() {
            let pos = t + f as u64 - 37;
            if pos < n0 + 600 {
                assert_eq!(x, ((pos % 65_536) << 8) as i32, "TX T is RX T − 37");
            } else {
                assert_eq!(x, 0, "a hole is silence");
                holes += 1;
            }
        }
    }
    assert!(holes > 0);
    assert_eq!(rig.shared.lane(slot).holes, holes);
}

/// The engine's samples a test puts in: the counter of each position, a
/// sign apart from the counter source's.
fn engine_value(p: u64) -> i32 {
    -(((p % 65_536) << 8) as i32) - 256
}

#[test]
fn an_engine_source_sends_its_engine_ring_at_the_stamp() {
    let mut rig = Rig::with_engine(&[1]);
    let n0 = send_clock(T, RATE, TX_MARGIN_NS);
    let base = n0 - 1_000;
    let values: Vec<i32> = (base..n0 + 3_000).map(engine_value).collect();
    rig.shared.engine_ring(1).unwrap().put_at(base, &values);
    assert!(rig.shared.set_source(0, Source::Counter));
    assert!(rig.shared.set_source(1, Source::Engine));
    // TX 2 (the engine's), TX 1 (the counter) and TX 3 (silent, no ring).
    let (slot, _) = rig.flow(vec![Some(2), Some(1), Some(3)]);
    let packets = run(&mut rig, 0, 20_000);
    assert!((58..=60).contains(&packets.len()), "{}", packets.len());
    for p in &packets {
        let t = stamp_of(p);
        let want: Vec<i32> = (t..t + FPP).map(engine_value).collect();
        assert_eq!(slot_samples(p, 3, 0), want, "TX 2 at T is its ring at T");
        assert_eq!(slot_samples(p, 3, 1), counter(t, 32), "TX 1's counter");
        assert_eq!(slot_samples(p, 3, 2), vec![0; 32], "TX 3 is silent");
    }
    let lane = rig.shared.lane(slot);
    assert_eq!((lane.holes, lane.send_errors), (0, 0), "{lane:?}");
    // TX 2's peak is what it sent.
    let loudest = packets
        .iter()
        .flat_map(|p| slot_samples(p, 3, 0))
        .map(i32::unsigned_abs)
        .max()
        .unwrap();
    assert_eq!(rig.shared.take_peak(1), loudest);
}

/// The TX loop's step before its wait (dstack #1, #141): no escape unless
/// asked; an escape's last pass at the fresh read sends what is due by
/// then and nothing ahead; a clock not ready or re-anchored since the pass
/// puts the escape off, unless it is overdue.
#[test]
fn before_wait_escapes_only_when_asked_and_puts_off_a_re_anchored_one() {
    let mut rig = Rig::new(Vec::new());
    assert!(rig.shared.set_source(0, Source::Counter));
    let (slot, _) = rig.flow(vec![Some(1)]);
    rig.tx.step(Some(T), 0);
    let never_read = || -> (Option<u64>, u32) { panic!("no read without an escape") };
    let never = || -> bool { panic!("no defer") };
    // No escape: nothing read, nothing sent, no defer.
    assert!(!rig.tx.before_wait(never_read, || false, never));
    assert!(drain(&rig.rx).is_empty());
    // An escape at a read 1 ms later: the packets due by then go out,
    // each at or after its due time.
    let at = T + MS;
    assert!(rig.tx.before_wait(|| (Some(at), 0), || true, never));
    let sent: Vec<u64> = drain(&rig.rx).iter().map(|p| stamp_of(p)).collect();
    let n0 = send_clock(T, RATE, TX_MARGIN_NS);
    let first = n0 - n0 % FPP + FPP;
    let due: Vec<u64> = (0..)
        .map(|k| first + k * FPP)
        .take_while(|&t| ns_of(t + FPP, RATE) + TX_MARGIN_NS <= at)
        .collect();
    assert_eq!(due.len(), 2, "{due:?}");
    assert_eq!(sent, due);
    // The clock re-anchored since the pass: put off, nothing sent.
    let mut deferred = 0;
    let put_off = || {
        deferred += 1;
        true
    };
    assert!(!rig.tx.before_wait(|| (Some(at + MS), 1), || true, put_off));
    assert_eq!((drain(&rig.rx).len(), deferred), (0, 1));
    // Overdue: escapes anyway. No clock: the same.
    assert!(rig.tx.before_wait(|| (Some(at + MS), 1), || true, || false));
    assert!(rig.tx.before_wait(|| (None, 0), || true, || false));
    assert!(drain(&rig.rx).is_empty());
    assert_eq!(rig.shared.lane(slot).rebootstraps, 0);
}

/// dstack #141: around a bounded escape TX sends no packet before its due
/// time (stamp + fpp + margin). An UltimoX2 receiver (ESI planet 22) reads
/// a packet that arrives before its media time as latency plus that early
/// time: band-asio-dante's send ahead of up to 2.14 ms once a second read
/// 3.5 ms there, the same device without it 0.77 ms.
#[test]
fn an_escape_sends_no_packet_before_its_due_time() {
    let mut rig = Rig::new(Vec::new());
    assert!(rig.shared.set_source(0, Source::Counter));
    let (slot, _) = rig.flow(vec![Some(1)]);
    rig.tx.step(Some(T), 0);
    assert!(drain(&rig.rx).is_empty(), "nothing due yet at T");
    assert!(rig.tx.before_wait(|| (Some(T), 0), || true, || false));
    let sent = drain(&rig.rx);
    let early: Vec<u64> = sent
        .iter()
        .map(|p| stamp_of(p))
        .filter(|&t| ns_of(t + FPP, RATE) + TX_MARGIN_NS > T)
        .collect();
    assert!(early.is_empty(), "sent before their due time: {early:?}");
    assert_eq!(rig.shared.lane(slot).rebootstraps, 0);
}

#[test]
fn an_engine_hole_is_silence_counted_in_the_lane() {
    let mut rig = Rig::with_engine(&[0, 2]);
    let n0 = send_clock(T, RATE, TX_MARGIN_NS);
    // TX 1's ring holds positions up to n0 + 600; TX 3's nothing at all.
    let base = n0 - 1_000;
    let values: Vec<i32> = (base..n0 + 600).map(engine_value).collect();
    rig.shared.engine_ring(0).unwrap().put_at(base, &values);
    assert!(rig.shared.set_source(0, Source::Engine));
    assert!(rig.shared.set_source(2, Source::Engine));
    let (slot, _) = rig.flow(vec![Some(1), Some(3)]);
    let packets = run(&mut rig, 0, 10_000);
    let mut holes = 0;
    for p in &packets {
        let t = stamp_of(p);
        for (f, &x) in slot_samples(p, 2, 0).iter().enumerate() {
            let pos = t + f as u64;
            if pos < n0 + 600 {
                assert_eq!(x, engine_value(pos), "TX 1 at {pos}");
            } else {
                assert_eq!(x, 0, "a hole is silence");
                holes += 1;
            }
        }
        assert_eq!(slot_samples(p, 2, 1), vec![0; 32], "TX 3: all holes");
        holes += 32;
    }
    assert!(holes > 32 * packets.len() as u64, "TX 1 ran out too");
    assert_eq!(rig.shared.lane(slot).holes, holes);
}

#[test]
fn a_re_anchor_realigns_every_flow_and_counts_it() {
    let mut rig = Rig::new(Vec::new());
    let (a, _) = rig.flow(vec![Some(1)]);
    let (b, _) = add(
        &rig.shared,
        &FlowRequest {
            fpp: 16,
            ..req(v4(&rig.stranger))
        },
    );
    // The first epoch seen is no change.
    rig.tx.step(Some(T), 7);
    rig.tx.step(Some(T + MS), 7);
    assert!(!drain(&rig.rx).is_empty());
    // Re-anchored 0.3 ms back: both start again on a multiple of their fpp.
    rig.tx.step(Some(T + 200_000), 8);
    assert_eq!(rig.shared.lane(a).rebootstraps, 1);
    assert_eq!(rig.shared.lane(b).rebootstraps, 1);
    rig.tx.step(Some(T + 1_000_000), 8);
    let after = drain(&rig.rx);
    assert!(!after.is_empty());
    assert!(after.iter().all(|p| stamp_of(p).is_multiple_of(FPP)));
    assert!(
        drain(&rig.stranger)
            .iter()
            .all(|p| stamp_of(p).is_multiple_of(16))
    );
    // The same epoch again: nothing more.
    rig.tx.step(Some(T + 1_500_000), 8);
    assert_eq!(rig.shared.lane(a).rebootstraps, 1);
}

/// iemmixer and SongPlayer (6.11.2026, v0.6.43; dstack #1): after dsync's
/// coordinated date step the clock re-anchored, every flow started again
/// on the fpp grid after now, and a receiver saw 2 packets (64 frames)
/// missing. Every device steps by the same time, so the receivers expect
/// the stamps to go on at the old ones + the step: 1.6 s is 153 600
/// samples at 96 kHz, a whole number of packets at fpp 32 and 16.
const DATE_STEP_NS: i64 = 1_600_000_000;
const DATE_STEP: u64 = 153_600;

/// The clock's anchor after one re-anchor of `step_ns` into `epoch`.
fn stepped(epoch: u32, step_ns: i64) -> crate::clock::Anchor {
    crate::clock::Anchor {
        epoch,
        re_anchor: Some(crate::clock::ReAnchor { epoch, step_ns }),
    }
}

/// The stamps' steps: each packet's stamp less the one before.
fn stamp_steps(packets: &[Vec<u8>]) -> Vec<u64> {
    packets
        .windows(2)
        .map(|w| stamp_of(&w[1]).wrapping_sub(stamp_of(&w[0])))
        .collect()
}

#[test]
fn a_re_anchor_by_whole_packets_shifts_every_flow_and_loses_no_packet() {
    let mut rig = Rig::with_engine(&[1]);
    let n0 = send_clock(T, RATE, TX_MARGIN_NS);
    // The engine ring as the pump writes it: one line of positions across
    // the step (the pump shifts its own schedule by the same step).
    let base = n0 - 1_000;
    let values: Vec<i32> = (base..base + 6_000).map(engine_value).collect();
    rig.shared.engine_ring(1).unwrap().put_at(base, &values);
    assert!(rig.shared.set_source(0, Source::Counter));
    assert!(rig.shared.set_source(1, Source::Engine));
    let (a, _) = rig.flow(vec![Some(1), Some(2)]);
    let (b, _) = add(
        &rig.shared,
        &FlowRequest {
            fpp: 16,
            ..req(v4(&rig.stranger))
        },
    );
    let (mut to_a, mut to_b) = (Vec::new(), Vec::new());
    for us in (0..=10_000).step_by(500) {
        rig.tx.step(Some(T + us * 1_000), 7);
        to_a.extend(drain(&rig.rx));
        to_b.extend(drain(&rig.stranger));
    }
    let before = to_a.len();
    // The step lands between two passes.
    for us in (10_500..=20_000).step_by(500) {
        let now = T + DATE_STEP_NS as u64 + us * 1_000;
        rig.tx.step(Some(now), stepped(8, DATE_STEP_NS));
        to_a.extend(drain(&rig.rx));
        to_b.extend(drain(&rig.stranger));
    }
    for (packets, fpp) in [(&to_a, FPP), (&to_b, 16)] {
        let steps = stamp_steps(packets);
        assert_eq!(
            steps.iter().filter(|&&d| d == fpp + DATE_STEP).count(),
            1,
            "fpp {fpp}: one step of a packet + the date step"
        );
        assert!(
            steps.iter().all(|&d| d == fpp || d == fpp + DATE_STEP),
            "fpp {fpp}: no packet missing: {steps:?}"
        );
    }
    assert!((58..=60).contains(&to_a.len()), "{}", to_a.len());
    for (i, p) in to_a.iter().enumerate() {
        let t = stamp_of(p);
        assert_eq!(
            slot_samples(p, 2, 0),
            counter(t, 32),
            "the counter names its stamp"
        );
        // After the step the engine's samples go on where they were.
        let line = if i < before { t } else { t - DATE_STEP };
        let want: Vec<i32> = (line..line + FPP).map(engine_value).collect();
        assert_eq!(slot_samples(p, 2, 1), want, "packet {i} at {t}");
    }
    for slot in [a, b] {
        let lane = rig.shared.lane(slot);
        assert_eq!((lane.rebootstraps, lane.holes), (0, 0), "{lane:?}");
    }
}

/// A TX thread that starts after the engine line moved (the pump saw a
/// re-anchor first) reads the engine's ring on the line.
#[test]
fn a_tx_that_starts_late_reads_the_engine_on_its_line() {
    let mut rig = Rig::with_engine(&[0]);
    rig.shared
        .engine_line
        .shift_at(crate::clock::Anchor::from(1), RATE, 32);
    rig.shared
        .engine_line
        .shift_at(stepped(2, DATE_STEP_NS), RATE, 32);
    let n0 = send_clock(T, RATE, TX_MARGIN_NS);
    let base = n0 - DATE_STEP - 1_000;
    let values: Vec<i32> = (base..base + 6_000).map(engine_value).collect();
    rig.shared.engine_ring(0).unwrap().put_at(base, &values);
    assert!(rig.shared.set_source(0, Source::Engine));
    let (slot, _) = rig.flow(vec![Some(1)]);
    let mut packets = Vec::new();
    for us in (0..=10_000).step_by(500) {
        rig.tx.step(Some(T + us * 1_000), 2);
        packets.extend(drain(&rig.rx));
    }
    assert!(packets.len() > 25, "{}", packets.len());
    for p in &packets {
        let line = stamp_of(p) - DATE_STEP;
        let want: Vec<i32> = (line..line + FPP).map(engine_value).collect();
        assert_eq!(slot_samples(p, 1, 0), want);
    }
    assert_eq!(rig.shared.lane(slot).holes, 0);
}

/// One shift for the whole device (review of dstack #1's fix): a step off
/// the block grid (the date step + 16 samples) moves a fpp-16 flow by the
/// same whole blocks as the pump and the engine line (153 632), so its
/// engine samples go on with none skipped or repeated.
#[test]
fn a_flow_on_a_finer_grid_shifts_with_the_block_and_its_samples_go_on() {
    let step_ns = 1_600_166_667; // 153 616 samples at 96 kHz
    let shift = 153_632;
    let mut rig = Rig::with_engine(&[0]);
    let n0 = send_clock(T, RATE, TX_MARGIN_NS);
    let base = n0 - 1_000;
    let values: Vec<i32> = (base..base + 6_000).map(engine_value).collect();
    rig.shared.engine_ring(0).unwrap().put_at(base, &values);
    assert!(rig.shared.set_source(0, Source::Engine));
    let (slot, _) = add(
        &rig.shared,
        &FlowRequest {
            fpp: 16,
            channels: vec![Some(1)],
            ..req(v4(&rig.rx))
        },
    );
    let mut packets = Vec::new();
    for us in (0..=5_000).step_by(500) {
        rig.tx.step(Some(T + us * 1_000), 1);
        packets.extend(drain(&rig.rx));
    }
    let before = packets.len();
    for us in (5_500..=10_000).step_by(500) {
        rig.tx
            .step(Some(T + step_ns as u64 + us * 1_000), stepped(2, step_ns));
        packets.extend(drain(&rig.rx));
    }
    let steps = stamp_steps(&packets);
    assert!(
        steps.iter().all(|&d| d == 16 || d == 16 + shift),
        "{steps:?}"
    );
    assert_eq!(steps.iter().filter(|&&d| d == 16 + shift).count(), 1);
    for (i, p) in packets.iter().enumerate() {
        let t = stamp_of(p);
        let line = if i < before { t } else { t - shift };
        let want: Vec<i32> = (line..line + 16).map(engine_value).collect();
        assert_eq!(slot_samples(p, 1, 0), want, "packet {i}");
    }
    let lane = rig.shared.lane(slot);
    assert_eq!((lane.rebootstraps, lane.holes), (0, 0), "{lane:?}");
}

/// A flow whose fpp does not divide the device's shift (fpp 64, a shift of
/// one block of 32) starts again, counted, so its stamps stay on its grid;
/// a fpp-32 flow beside it moves.
#[test]
fn a_flow_whose_fpp_does_not_divide_the_shift_starts_again() {
    let mut rig = Rig::new(Vec::new());
    let (a, _) = rig.flow(vec![Some(1)]);
    let (b, _) = add(
        &rig.shared,
        &FlowRequest {
            fpp: 64,
            ..req(v4(&rig.stranger))
        },
    );
    for us in (0..=3_000).step_by(500) {
        rig.tx.step(Some(T + us * 1_000), 1);
    }
    drain(&rig.rx);
    drain(&rig.stranger);
    let step_ns = 333_333; // 32 samples at 96 kHz
    let now = T + 3_500_000 + step_ns as u64;
    rig.tx.step(Some(now), stepped(2, step_ns));
    assert_eq!(rig.shared.lane(a).rebootstraps, 0);
    assert_eq!(rig.shared.lane(b).rebootstraps, 1);
    rig.tx.step(Some(now + 2 * MS), stepped(2, step_ns));
    let after = drain(&rig.stranger);
    assert!(!after.is_empty());
    assert!(after.iter().all(|p| stamp_of(p).is_multiple_of(64)));
}

/// A pass that reads the clock's new time with its old epoch (the clock
/// moves k before the epoch): the jump starts the flow again at the new
/// time, and the next pass, at the new epoch, keeps it there instead of
/// shifting it a whole date step ahead (1.6 s of silence).
#[test]
fn a_time_read_before_its_epoch_restarts_once_and_is_not_shifted_again() {
    let mut rig = Rig::new(Vec::new());
    let (slot, _) = rig.flow(vec![Some(1)]);
    for us in (0..=3_000).step_by(500) {
        rig.tx.step(Some(T + us * 1_000), 1);
    }
    drain(&rig.rx);
    let new = T + DATE_STEP_NS as u64 + 3_500_000;
    rig.tx.step(Some(new), 1);
    assert_eq!(rig.shared.lane(slot).rebootstraps, 1);
    let mut packets = Vec::new();
    for us in (500..=3_000).step_by(500) {
        rig.tx
            .step(Some(new + us * 1_000), stepped(2, DATE_STEP_NS));
        packets.extend(drain(&rig.rx));
    }
    assert_eq!(rig.shared.lane(slot).rebootstraps, 1);
    assert!(packets.len() >= 7, "sending on at once: {}", packets.len());
    assert!(stamp_steps(&packets).iter().all(|&d| d == FPP));
}

#[test]
fn a_re_anchor_moves_a_schedule_up_to_its_latency_and_a_packet_past_now() {
    let now = 1_000_000;
    let fresh = || Schedule::bootstrap(now - 64, 32, 96);
    // Bootstrapped at now − 64: next = now − 32.
    assert_eq!(fresh().next, now - 32);
    let mut s = fresh();
    assert_eq!(s.re_anchor(Some(160), now, 32), Shift::Moved);
    assert_eq!((s.next, s.take_rebootstraps()), (now + 128, 0));
    let mut s = fresh();
    assert_eq!(
        s.re_anchor(Some(192), now, 32),
        Shift::Kept,
        "now + 160 is past it"
    );
    assert_eq!((s.next, s.take_rebootstraps()), (now - 32, 0));
    let mut s = fresh();
    assert_eq!(s.re_anchor(Some(-320), now, 32), Shift::Moved);
    assert_eq!(s.next, now - 352);
    let mut s = fresh();
    assert_eq!(
        s.re_anchor(Some(16), now, 32),
        Shift::Restarted,
        "off its grid"
    );
    assert_eq!((s.next, s.take_rebootstraps()), (now + 32, 1));
    let mut s = fresh();
    assert_eq!(s.re_anchor(None, now, 32), Shift::Restarted);
    assert_eq!(s.take_rebootstraps(), 1);
}

/// A latency set apart from the block (smaller than it): the room for a
/// shift is the block, since the rest of a step on the block grid can leave
/// the stamp up to half a block ahead.
#[test]
fn a_re_anchor_s_room_is_at_least_the_block() {
    let now = 1_000_000;
    // fpp 32, a latency of 24 samples (0.25 ms at 96 kHz), block 64.
    let fresh = || Schedule::bootstrap(now - 64, 32, 24);
    let mut s = fresh();
    assert_eq!(s.re_anchor(Some(128), now, 64), Shift::Moved);
    assert_eq!(s.next, now + 96);
    let mut s = fresh();
    assert_eq!(s.re_anchor(Some(160), now, 64), Shift::Kept);
    assert_eq!(s.next, now - 32);
}

/// Each re-anchor is one info line with what it did to the flows.
#[test]
fn a_re_anchor_logs_what_it_did_to_the_flows() {
    crate::test_log::undecided();
    let mut rig = Rig::new(Vec::new());
    rig.flow(vec![Some(1)]);
    add(
        &rig.shared,
        &FlowRequest {
            fpp: 64,
            ..req(v4(&rig.stranger))
        },
    );
    for us in (0..=3_000).step_by(500) {
        rig.tx.step(Some(T + us * 1_000), 1);
    }
    let log = crate::test_log::capture_at(tracing::Level::INFO, || {
        rig.tx
            .step(Some(T + 3_500_000 + 333_333), stepped(2, 333_333));
        rig.tx.step(Some(T + 4_000_000 + 333_333), 2);
        rig.tx
            .step(Some(T + 4_500_000 + 333_333), crate::clock::Anchor::from(3));
    });
    let lines: Vec<&str> = log.lines().filter(|l| l.contains(shift::TX_LINE)).collect();
    assert_eq!(lines.len(), 2, "{log}");
    assert!(
        lines[0].contains("moved=1") && lines[0].contains("restarted=1"),
        "{log}"
    );
    assert!(
        lines[0].contains("kept=0") && lines[0].contains("shift=Some(32)"),
        "{log}"
    );
    assert!(
        lines[1].contains("moved=0") && lines[1].contains("restarted=2"),
        "{log}"
    );
}

/// A re-anchor whose step is not a whole number of packets (a local k
/// correction of 50 µs, 4.8 samples at 96 kHz; the date step plus it) keeps
/// the stamps going: the schedule moves by the step rounded to its grid and
/// the rest is a lag or a wait well inside the latency. Starting again
/// instead would drop up to a packet and never land nearer the clock.
#[test]
fn a_re_anchor_off_the_grid_shifts_by_the_nearest_whole_packets() {
    for (step_ns, shift) in [
        (50_000, 0),
        (-50_000, 0),
        (DATE_STEP_NS + 50_000, DATE_STEP),
    ] {
        let mut rig = Rig::new(Vec::new());
        let (slot, _) = rig.flow(vec![Some(1)]);
        let mut packets = Vec::new();
        for us in (0..=5_000).step_by(500) {
            rig.tx.step(Some(T + us * 1_000), 3);
            packets.extend(drain(&rig.rx));
        }
        for us in (5_500..=10_000).step_by(500) {
            let now = (T + us * 1_000).wrapping_add_signed(step_ns);
            rig.tx.step(Some(now), stepped(4, step_ns));
            packets.extend(drain(&rig.rx));
        }
        let steps = stamp_steps(&packets);
        assert!(
            steps
                .iter()
                .all(|&d| d == FPP || (shift > 0 && d == FPP + shift)),
            "{step_ns}: {steps:?}"
        );
        assert_eq!(
            steps.iter().filter(|&&d| d == FPP + shift).count(),
            if shift > 0 { 1 } else { steps.len() }
        );
        assert_eq!(rig.shared.lane(slot).rebootstraps, 0, "{step_ns}");
    }
}

/// A step the media threads cannot use still starts every flow again,
/// counted: the clock knows no step, its step is for another epoch, or two
/// re-anchors came between two passes.
#[test]
fn a_re_anchor_without_a_usable_step_realigns_and_counts_it() {
    let unusable = [
        crate::clock::Anchor::from(8),
        crate::clock::Anchor {
            epoch: 8,
            re_anchor: Some(crate::clock::ReAnchor {
                epoch: 7,
                step_ns: DATE_STEP_NS,
            }),
        },
        stepped(9, DATE_STEP_NS),
    ];
    for anchor in unusable {
        let mut rig = Rig::new(Vec::new());
        let (slot, _) = rig.flow(vec![Some(1)]);
        rig.tx.step(Some(T), 7);
        rig.tx.step(Some(T + MS), 7);
        assert!(!drain(&rig.rx).is_empty());
        let now = T + DATE_STEP_NS as u64 + 2 * MS;
        rig.tx.step(Some(now), anchor);
        assert_eq!(rig.shared.lane(slot).rebootstraps, 1, "{anchor:?}");
        rig.tx.step(Some(now + MS), anchor);
        let after = drain(&rig.rx);
        assert!(!after.is_empty());
        assert!(
            stamp_of(&after[0]) > send_clock(now, RATE, TX_MARGIN_NS),
            "{anchor:?}"
        );
    }
}

#[test]
fn a_lag_past_the_latency_restarts_once_and_sends_no_burst() {
    let mut rig = Rig::new(Vec::new());
    let (slot, _) = rig.flow(vec![Some(1)]);
    let before = run(&mut rig, 0, 2_000);
    assert!(!before.is_empty());
    // 50 ms without a pass: nothing of them is sent, the schedule starts
    // again (counted) and goes on from there.
    rig.tx.step(Some(T + 52_000_000), 0);
    assert!(drain(&rig.rx).is_empty(), "no burst of late packets");
    assert_eq!(rig.shared.lane(slot).rebootstraps, 1);
    let after = run(&mut rig, 52_500, 60_000);
    assert!(!after.is_empty());
    assert!(stamp_of(&after[0]) >= send_clock(T + 52_000_000, RATE, TX_MARGIN_NS));
    assert_eq!(rig.shared.lane(slot).rebootstraps, 1);
}

#[test]
fn keepalives_count_only_the_receiver_s_datagrams_at_most_64_a_read() {
    let mut rig = Rig::new(Vec::new());
    let (slot, _) = rig.flow(vec![Some(1)]);
    let to = rig.shared.local_addr(slot).unwrap();
    for _ in 0..3 {
        rig.rx.send_to(&[0x13, 0x37], to).unwrap();
    }
    for _ in 0..2 {
        rig.stranger.send_to(&[0x13, 0x37], to).unwrap();
    }
    rig.tx.step(Some(T), 0);
    assert_eq!(rig.shared.lane(slot).keepalives, 3);
    // A flood: 64 a read, the rest at the next.
    for _ in 0..100 {
        rig.rx.send_to(&[0], to).unwrap();
    }
    rig.tx.step(Some(T + 10 * MS), 0);
    assert_eq!(
        rig.shared.lane(slot).keepalives,
        3,
        "not before the next read"
    );
    rig.tx.step(Some(T + EVENTS_EVERY_NS), 0);
    assert_eq!(rig.shared.lane(slot).keepalives, 3 + 64);
    rig.tx.step(Some(T + 2 * EVENTS_EVERY_NS), 0);
    assert_eq!(rig.shared.lane(slot).keepalives, 3 + 100);
    // An expired flow's datagrams are not read (inferno).
    rig.shared.sync(0);
    assert_eq!(rig.shared.sync(KEEPALIVE_TIMEOUT_NS), 1 << slot);
    rig.rx.send_to(&[0], to).unwrap();
    rig.tx.step(Some(T + 3 * EVENTS_EVERY_NS), 0);
    rig.tx.step(Some(T + 4 * EVENTS_EVERY_NS), 0);
    assert_eq!(rig.shared.lane(slot).keepalives, 3 + 100);
}

#[test]
fn a_stopped_flow_stops_sending_at_the_next_read_of_the_plans() {
    let mut rig = Rig::new(Vec::new());
    let (_, h) = rig.flow(vec![Some(1)]);
    assert!(!run(&mut rig, 0, 2_000).is_empty());
    {
        let mut c = rig.shared.lock();
        c.table.stop(&h).unwrap();
        rig.shared.publish(&c);
    }
    // Until the next read it sends on; from the read at 33 ms, nothing.
    assert!(!run(&mut rig, 2_500, 32_500).is_empty());
    assert!(run(&mut rig, 33_000, 60_000).is_empty());
}

#[test]
fn changed_channels_keep_the_schedule_and_a_new_fpp_restarts_it() {
    let mut rig = Rig::new(Vec::new());
    assert!(rig.shared.set_source(2, Source::Counter));
    let (slot, h) = rig.flow(vec![Some(1)]);
    let before = run(&mut rig, 0, 32_500);
    {
        let mut c = rig.shared.lock();
        c.table.update(&h, &[Some(3), None], 0).unwrap();
        rig.shared.publish(&c);
    }
    let after = run(&mut rig, 33_000, 40_000);
    // The stamps go on without a gap; the packets carry the new slots.
    assert_eq!(stamp_of(&after[0]), stamp_of(before.last().unwrap()) + FPP);
    assert_eq!(after[0].len(), 9 + 2 * 3 * 32);
    assert_eq!(
        slot_samples(&after[0], 2, 0),
        counter(stamp_of(&after[0]), 32)
    );
    // The same receiver asks for 16 frames: a new schedule, uncounted.
    {
        let mut c = rig.shared.lock();
        c.table
            .add(
                &FlowRequest {
                    fpp: 16,
                    ..req(v4(&rig.rx))
                },
                0,
            )
            .unwrap();
        rig.shared.publish(&c);
    }
    let again = run_fpp(&mut rig, 66_000, 70_000, 16);
    assert!(again.iter().all(|p| p.len() == 9 + 2 * 3 * 16));
    assert!(
        again
            .windows(2)
            .all(|w| stamp_of(&w[1]) == stamp_of(&w[0]) + 16)
    );
    assert_eq!(rig.shared.lane(slot).rebootstraps, 0);
}

#[test]
fn the_plans_are_read_at_once_when_the_clock_goes_back() {
    let mut rig = Rig::new(Vec::new());
    rig.tx.step(Some(T), 0);
    rig.flow(vec![Some(1)]);
    // The same time again: the next read is exactly 33 ms ahead, not yet.
    rig.tx.step(Some(T), 0);
    rig.tx.step(Some(T + MS), 0);
    assert!(drain(&rig.rx).is_empty());
    // 1 ns before the last read: the clock went back, read at once.
    rig.tx.step(Some(T - 1), 0);
    rig.tx.step(Some(T + MS), 0);
    assert!(!drain(&rig.rx).is_empty());
}

#[test]
fn wake_ns_is_the_next_packet_or_the_next_read() {
    let mut rig = Rig::new(Vec::new());
    rig.tx.step(Some(T), 0);
    assert_eq!(rig.tx.wake_ns(Some(T)), T + EVENTS_EVERY_NS);
    rig.flow(vec![Some(1)]);
    rig.tx.step(Some(T + EVENTS_EVERY_NS), 0);
    let w = rig.tx.wake_ns(Some(T + EVENTS_EVERY_NS));
    assert!(
        w > T + EVENTS_EVERY_NS && w < T + EVENTS_EVERY_NS + MS,
        "{}",
        w - T
    );
    // 1 ns early: nothing; at it: one packet.
    rig.tx.step(Some(w - 1), 0);
    assert!(drain(&rig.rx).is_empty());
    rig.tx.step(Some(w), 0);
    assert_eq!(drain(&rig.rx).len(), 1);
    assert_eq!(rig.tx.wake_ns(None), 0);
}

#[test]
fn the_devices_tx_margin_sets_when_a_packet_leaves_and_its_latency_the_lag() {
    // #72 phase F: a margin of 30 µs instead of 250 µs, a latency of 0.25 ms.
    let cfg = DeviceConfig {
        tx_margin_ns: 30_000,
        tx_latency_ns: Some(250_000),
        ..cfg()
    };
    let shared =
        Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    let mut rig = Rig::with(shared);
    assert_eq!(rig.tx.max_lag, 24, "0.25 ms at 96 kHz");
    rig.tx.step(Some(T), 0);
    rig.flow(vec![Some(1)]);
    rig.tx.step(Some(T + EVENTS_EVERY_NS), 0);
    let w = rig.tx.wake_ns(Some(T + EVENTS_EVERY_NS));
    // 1 ns early: nothing; at it: one packet, 30 µs after its last sample.
    rig.tx.step(Some(w - 1), 0);
    assert!(drain(&rig.rx).is_empty());
    rig.tx.step(Some(w), 0);
    let sent = drain(&rig.rx);
    assert_eq!(sent.len(), 1);
    assert_eq!(w, ns_of(stamp_of(&sent[0]) + FPP, RATE) + 30_000);
}

// Linux refuses a send to port 0; Windows sends it.
#[cfg(target_os = "linux")]
#[test]
fn a_send_the_socket_refuses_is_counted() {
    let mut rig = Rig::new(Vec::new());
    // Port 0 (the server refuses it; the table itself takes it): sendto
    // fails.
    let (slot, _) = add(&rig.shared, &req(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)));
    rig.tx.step(Some(T), 0);
    rig.tx.step(Some(T + MS), 0);
    let lane = rig.shared.lane(slot);
    assert_eq!(lane.packets, 0);
    assert!(lane.send_errors > 0);
}

// ---- the thread ----

/// A clock whose time moves only to the times the thread sleeps to, up to
/// a limit the test sets; at the limit the thread parks (1 ms of real
/// time a sleep).
#[derive(Debug, Default)]
struct StepClock {
    now: AtomicU64,
    limit: AtomicU64,
    ready: AtomicBool,
    parked: AtomicU64,
}

impl PtpClock for StepClock {
    fn now_ns(&self) -> Option<u64> {
        self.ready
            .load(Ordering::SeqCst)
            .then(|| self.now.load(Ordering::SeqCst))
    }

    fn sleep_until(&self, ptp_ns: u64) {
        let ready = self.ready.load(Ordering::SeqCst);
        let limit = self.limit.load(Ordering::SeqCst);
        if ready && ptp_ns <= limit {
            self.now.fetch_max(ptp_ns, Ordering::SeqCst);
            return;
        }
        if ready {
            self.now.fetch_max(limit, Ordering::SeqCst);
        }
        self.parked.fetch_add(1, Ordering::SeqCst);
        std::thread::sleep(Duration::from_millis(1));
    }

    fn epoch(&self) -> u32 {
        0
    }
}

impl StepClock {
    fn at(t: u64) -> Arc<Self> {
        let c = StepClock::default();
        c.now.store(t, Ordering::SeqCst);
        c.limit.store(t, Ordering::SeqCst);
        c.ready.store(true, Ordering::SeqCst);
        Arc::new(c)
    }

    /// Lets time run to `t` and waits (at most 5 s) until the thread made
    /// a whole pass there.
    fn run_to(&self, t: u64) {
        let parked = self.parked.load(Ordering::SeqCst);
        self.limit.store(t, Ordering::SeqCst);
        let deadline = Instant::now() + Duration::from_secs(5);
        // The third park: one may have read the old limit, the second
        // comes before the pass at the new one.
        while self.parked.load(Ordering::SeqCst) < parked + 3 {
            assert!(Instant::now() < deadline, "the TX thread made no pass");
            std::thread::sleep(Duration::from_micros(200));
        }
    }
}

fn join_within(h: JoinHandle<()>, limit: Duration) {
    let deadline = Instant::now() + limit;
    while !h.is_finished() {
        assert!(Instant::now() < deadline, "the TX thread did not end");
        std::thread::sleep(Duration::from_millis(1));
    }
    h.join().unwrap();
}

#[test]
fn the_tx_thread_sends_rate_over_fpp_packets_a_second_never_early_and_ends_on_stop() {
    let shared = shared_with(Vec::new());
    assert!(shared.set_source(0, Source::Counter));
    let rx = socket();
    let (slot, _) = add(
        &shared,
        &FlowRequest {
            channels: vec![Some(1)],
            ..req(v4(&rx))
        },
    );
    let clock = StepClock::at(T);
    let thread = spawn(Arc::clone(&clock) as Arc<dyn PtpClock>, Arc::clone(&shared)).unwrap();
    let mut stamps = Vec::new();
    for k in 1..=100 {
        let limit = T + k * 10 * MS;
        clock.run_to(limit);
        for p in drain(&rx) {
            let t = stamp_of(&p);
            assert!(ns_of(t + FPP, RATE) + TX_MARGIN_NS <= limit, "never early");
            assert_eq!(slot_samples(&p, 1, 0), counter(t, 32));
            stamps.push(t);
        }
        let last = *stamps.last().expect("packets every 10 ms");
        assert!(
            ns_of(last + 2 * FPP, RATE) + TX_MARGIN_NS > limit,
            "none due left"
        );
    }
    // One second: 3 000 packets less the first, every stamp in turn.
    assert!((2_998..=3_000).contains(&stamps.len()), "{}", stamps.len());
    assert!(stamps.windows(2).all(|w| w[1] == w[0] + FPP));
    assert_eq!(shared.lane(slot).packets, stamps.len() as u64);
    shared.stop();
    join_within(thread, Duration::from_secs(2));
}

/// dstack #141: while the TX thread blocks its bounded escape, the relay
/// thread sends its packets at their times, never one early; the TX
/// thread goes on from there with every stamp in turn.
#[test]
fn the_relay_thread_sends_the_tx_packets_while_the_tx_thread_blocks() {
    let rig = Rig::new(Vec::new());
    assert!(rig.shared.set_source(0, Source::Counter));
    let (slot, _) = rig.flow(vec![Some(1)]);
    let clock = StepClock::at(T);
    let relay = relay::Relay::new();
    let mut tx = Transmitter::new(Arc::clone(&rig.shared));
    tx.step(Some(T), 0);
    let end = T + 2 * MS;
    let mut tx = std::thread::scope(|scope| {
        scope.spawn(|| run_relay(clock.as_ref(), &relay));
        let mut misses = 0;
        let mut blocked = 0;
        let tx = escape_through(
            &relay,
            tx,
            &mut misses,
            Duration::from_secs(5),
            || {
                blocked += 1;
                clock.run_to(end);
            },
            || panic!("the relay took over"),
        );
        assert_eq!((misses, blocked), (0, 1));
        relay.stop();
        tx
    });
    let sent: Vec<u64> = drain(&rig.rx).iter().map(|p| stamp_of(p)).collect();
    let n0 = send_clock(T, RATE, TX_MARGIN_NS);
    let first = n0 - n0 % FPP + FPP;
    let due: Vec<u64> = (0..)
        .map(|k| first + k * FPP)
        .take_while(|&t| ns_of(t + FPP, RATE) + TX_MARGIN_NS <= end)
        .collect();
    assert_eq!(due.len(), 5, "{due:?}");
    assert_eq!(
        sent, due,
        "the relay sent every packet due in the block, none early"
    );
    // Back with the TX thread: the next stamps follow.
    tx.step(Some(end + MS), 0);
    let after: Vec<u64> = drain(&rig.rx).iter().map(|p| stamp_of(p)).collect();
    assert_eq!(after.first(), Some(&(due[4] + FPP)));
    assert!(after.windows(2).all(|w| w[1] == w[0] + FPP));
    assert_eq!(rig.shared.lane(slot).rebootstraps, 0);
}

/// A relay that does not take over in time: the escape is put off (no
/// block), or, overdue, blocks uncovered; the TX thread keeps its
/// transmitter; warned at the first miss and every 100th.
#[test]
fn an_escape_whose_relay_does_not_take_over_is_put_off_and_warned_rarely() {
    crate::test_log::undecided();
    let rig = Rig::new(Vec::new());
    let relay = relay::Relay::new();
    let mut held = Some(Transmitter::new(Arc::clone(&rig.shared)));
    let mut misses = 0;
    let mut blocks = 0;
    let warnings = crate::test_log::capture_warnings(|| {
        for k in 0..101 {
            let tx = held.take().expect("the TX thread holds it");
            held = Some(escape_through(
                &relay,
                tx,
                &mut misses,
                Duration::ZERO,
                || blocks += 1,
                || k % 2 == 0,
            ));
        }
    });
    let mut tx = held.expect("kept");
    assert_eq!(misses, 101);
    assert_eq!(blocks, 50, "the overdue ones block uncovered");
    assert_eq!(
        warnings.matches("did not take over in time").count(),
        2,
        "{warnings}"
    );
    // The transmitter still works.
    assert!(rig.shared.set_source(0, Source::Counter));
    rig.flow(vec![Some(1)]);
    tx.step(Some(T), 0);
    tx.step(Some(T + MS), 0);
    assert_eq!(drain(&rig.rx).len(), 2);
}

#[test]
fn the_tx_thread_sends_nothing_before_the_clock_is_ready() {
    let shared = shared_with(Vec::new());
    let rx = socket();
    add(&shared, &req(v4(&rx)));
    let clock = StepClock::at(T);
    clock.ready.store(false, Ordering::SeqCst);
    let thread = spawn(Arc::clone(&clock) as Arc<dyn PtpClock>, Arc::clone(&shared)).unwrap();
    // It naps on a clock that is not ready.
    let parked = clock.parked.load(Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(5);
    while clock.parked.load(Ordering::SeqCst) < parked + 20 {
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(drain(&rx).is_empty());
    clock.ready.store(true, Ordering::SeqCst);
    clock.run_to(T + 5 * MS);
    assert!(!drain(&rx).is_empty());
    shared.stop();
    join_within(thread, Duration::from_secs(2));
}

/// The CPUs the calling thread may run on.
#[cfg(target_os = "linux")]
fn affinity() -> Vec<usize> {
    // SAFETY: an all-zero cpu_set_t is valid; the call fills it; pid 0 is
    // the calling thread.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        assert_eq!(
            libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set),
            0
        );
        (0..crate::device::MAX_CPU)
            .filter(|&c| libc::CPU_ISSET(c, &set))
            .collect()
    }
}

#[cfg(target_os = "linux")]
#[test]
fn a_thread_pins_itself_to_one_cpu_and_a_missing_cpu_is_refused() {
    std::thread::spawn(|| {
        assert!(pin_to_cpu(0), "CPU 0 exists on every runner");
        assert_eq!(affinity(), vec![0]);
        assert!(!pin_to_cpu(1023), "no runner has 1024 CPUs");
        assert!(!pin_to_cpu(crate::device::MAX_CPU), "past CPU_SETSIZE");
        assert_eq!(
            affinity(),
            vec![0],
            "a refused CPU leaves the thread where it was"
        );
    })
    .join()
    .unwrap();
}

#[cfg(target_os = "linux")]
#[test]
fn place_pins_a_thread_to_the_configured_cpu_and_leaves_it_without_one() {
    std::thread::spawn(|| {
        let before = affinity();
        place("test thread", None);
        assert_eq!(
            affinity(),
            before,
            "no CPU in the config: the thread stays where it was"
        );
        place("test thread", Some(0));
        assert_eq!(affinity(), vec![0], "pinned to the configured CPU");
        place("test thread", Some(1023));
        assert_eq!(
            affinity(),
            vec![0],
            "a CPU the kernel refuses leaves it where it was"
        );
    })
    .join()
    .unwrap();
}

#[test]
fn on_windows_tx_runs_above_rx_and_the_pump() {
    assert_eq!(windows_thread_priority(TX_PRIORITY), WINDOWS_TIME_CRITICAL);
    assert_eq!(
        windows_thread_priority(TX_PRIORITY + 1),
        WINDOWS_TIME_CRITICAL
    );
    assert_eq!(
        windows_thread_priority(crate::media::rx::RX_PRIORITY),
        WINDOWS_HIGHEST
    );
    assert_eq!(
        windows_thread_priority(crate::stream::ENGINE_PRIORITY),
        WINDOWS_HIGHEST
    );
    assert_eq!(windows_thread_priority(TX_PRIORITY - 1), WINDOWS_HIGHEST);
    // Windows' own values for the two levels.
    assert_eq!((WINDOWS_TIME_CRITICAL, WINDOWS_HIGHEST), (15, 2));
}

/// dstack #90: the normal profile's threads ask THREAD_PRIORITY_HIGHEST on
/// Windows whatever their real-time number, real-time keeps TX above RX
/// and the pump, and normal asks nothing.
#[test]
fn windows_asks_highest_under_the_normal_profile_and_nothing_at_normal() {
    use crate::clock::ThreadPriority as Asked;
    for p in [
        TX_PRIORITY,
        crate::media::rx::RX_PRIORITY,
        crate::stream::ENGINE_PRIORITY,
    ] {
        assert_eq!(
            windows_priority_for(Asked::Highest, p),
            Some(WINDOWS_HIGHEST)
        );
        assert_eq!(windows_priority_for(Asked::Normal, p), None);
    }
    assert_eq!(
        windows_priority_for(Asked::Realtime, TX_PRIORITY),
        Some(WINDOWS_TIME_CRITICAL)
    );
    assert_eq!(
        windows_priority_for(Asked::Realtime, crate::media::rx::RX_PRIORITY),
        Some(WINDOWS_HIGHEST)
    );
}

#[cfg(windows)]
#[test]
fn a_windows_media_thread_gets_its_priority_and_its_cpu() {
    use windows_sys::Win32::System::Threading::{GetCurrentThread, GetThreadPriority};
    std::thread::spawn(|| {
        use crate::clock::ThreadPriority as Asked;
        assert!(
            ask_priority(Asked::Realtime, TX_PRIORITY),
            "Windows gives TX TIME_CRITICAL"
        );
        // SAFETY: the calling thread's pseudo handle.
        assert_eq!(
            unsafe { GetThreadPriority(GetCurrentThread()) },
            WINDOWS_TIME_CRITICAL
        );
        assert!(
            ask_priority(Asked::Realtime, crate::media::rx::RX_PRIORITY),
            "and RX HIGHEST"
        );
        // SAFETY: as above.
        assert_eq!(
            unsafe { GetThreadPriority(GetCurrentThread()) },
            WINDOWS_HIGHEST
        );
        // The normal profile (dlink, dstack #90): HIGHEST, in the normal
        // class, without privilege.
        assert!(
            !ask_priority(Asked::Normal, TX_PRIORITY),
            "normal asks nothing"
        );
        // SAFETY: as above.
        assert_eq!(
            unsafe { GetThreadPriority(GetCurrentThread()) },
            WINDOWS_HIGHEST,
            "and leaves the level as it was"
        );
        assert!(ask_priority(Asked::Realtime, TX_PRIORITY));
        assert!(
            ask_priority(Asked::Highest, TX_PRIORITY),
            "the normal profile gets HIGHEST"
        );
        // SAFETY: as above.
        assert_eq!(
            unsafe { GetThreadPriority(GetCurrentThread()) },
            WINDOWS_HIGHEST
        );
        assert!(pin_to_cpu(0), "CPU 0 exists on every runner");
        assert!(!pin_to_cpu(64), "past the first processor group");
    })
    .join()
    .unwrap();
}

// ---- how late packets leave (problem A: the 30.10 TX stall) ----

#[test]
fn a_lateness_falls_in_the_bin_of_its_lower_edge() {
    for (ns, bin) in [
        (0, 0),
        (249_999, 0),
        (250_000, 1),
        (499_999, 1),
        (500_000, 2),
        (999_999, 2),
        (1_000_000, 3),
        (2_000_000, 4),
        (4_999_999, 4),
        (5_000_000, 5),
        (u64::MAX, 5),
    ] {
        assert_eq!(late_bin(ns), bin, "{ns} ns");
    }
    assert_eq!(LATE_BINS, LATE_EDGES_NS.len() + 1);
}

/// The first packet a fresh flow sends after a step at `now`: its due
/// PTP ns (its last sample plus the margin).
fn first_due_ns(now: u64) -> u64 {
    let s = send_clock(now, RATE, TX_MARGIN_NS);
    let first = s - s % FPP + FPP;
    ns_of(first + FPP, RATE) + TX_MARGIN_NS
}

#[test]
fn packets_sent_on_time_are_in_the_first_bin() {
    let mut rig = Rig::new(Vec::new());
    let (slot, _) = rig.flow(vec![Some(1), Some(2)]);
    // Steps every 500 µs of PTP time; each step's own (real) time adds to
    // the lateness of what it sends, so the bound carries the longest.
    let mut longest = 0u64;
    let mut at = T;
    while at <= T + 20 * MS {
        let began = Instant::now();
        rig.tx.step(Some(at), 0);
        longest = longest.max(nanos(began.elapsed()));
        at += 500_000;
    }
    let packets = drain(&rig.rx).len() as u64;
    let lane = rig.shared.lane(slot);
    assert_eq!(lane.packets, packets);
    assert!((58..=60).contains(&packets), "{packets}");
    assert_eq!(lane.late_bins.iter().sum::<u64>(), lane.packets);
    assert!(
        lane.late_max_ns < 500_000 + longest + 1_000,
        "{lane:?}, longest step {longest} ns"
    );
}

/// Catalog: the 30.10 stall on the FOH PC. The TX thread is not scheduled
/// for ~5.5 ms, inside a 10 ms latency: no restart, every packet goes out,
/// a receiver sees them late (Dante Controller's latency tab), and before
/// v0.1.2 every one of our counters read 0. Now the lane shows it.
#[test]
fn a_stall_within_the_latency_is_counted_as_late_packets() {
    let mut cfg = cfg();
    cfg.latency_ns = 10 * MS;
    let shared =
        Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    let mut rig = Rig::with(shared);
    let (slot, _) = rig.flow(vec![Some(1), Some(2)]);
    let start = T;
    rig.tx.step(Some(start), 0);
    let due = first_due_ns(start);
    let began = Instant::now();
    rig.tx.step(Some(due + 5_500_000), 0);
    let took = nanos(began.elapsed());
    let lane = rig.shared.lane(slot);
    assert_eq!(lane.rebootstraps, 0, "inside the latency: no restart");
    // 5.5 ms of 333 µs packets: 17 out at once, the first 5.5 ms late.
    assert!((16..=18).contains(&lane.packets), "{lane:?}");
    assert_eq!(lane.late_bins.iter().sum::<u64>(), lane.packets);
    assert!(
        lane.late_bins[5] >= 1,
        "one at 5 ms past due or more: {lane:?}"
    );
    // The first packet left 5.5 ms past due, plus the step's own time.
    assert!(
        (5_500_000..5_500_000 + took + 1_000).contains(&lane.late_max_ns),
        "{lane:?}, step took {took} ns"
    );
    assert!(lane.send_max_ns < lane.late_max_ns, "{lane:?}");
}

/// Catalog: gen3 at 250 µs and the IEM card at 250 µs (31.10.2026). The
/// bins measure from the packet's due time (its last sample plus the
/// margin) and their first edge is 250 µs, so a packet a receiver calls
/// late (past the latency since its stamp) counted as on time: the box
/// read late_max 204 µs while the FOH console read 770 µs and pops. The
/// lane counts each packet's age since its stamp against the advertised
/// latency.
#[test]
fn a_packet_older_than_the_latency_since_its_stamp_counts_as_over_latency() {
    let mut cfg = cfg();
    cfg.tx_latency_ns = Some(500_000);
    let shared =
        Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    let mut rig = Rig::with(shared);
    let (slot, _) = rig.flow(vec![Some(1), Some(2)]);
    // The receiver states a latency under ours: it plays at ours.
    rig.shared.set_receiver_latency(slot, v4(&rig.rx), 250_000);
    let start = T;
    rig.tx.step(Some(start), 0);
    // 50 µs past due: well under the first bin's edge, but 333 µs (fpp 32) +
    // the 250 µs margin + 50 µs since the stamp, past the 500 µs latency.
    let due = first_due_ns(start);
    let began = Instant::now();
    rig.tx.step(Some(due + 50_000), 0);
    let took = nanos(began.elapsed());
    let lane = rig.shared.lane(slot);
    // The bins measure from the due time: 50 µs plus the step's own time
    // (a shared Windows runner once took 256 µs to send, 2.11.2026), in
    // the first bin whenever the step is quick, never more than that late.
    assert!(
        (50_000..50_000 + took + 1_000).contains(&lane.late_max_ns),
        "{lane:?}, step took {took} ns"
    );
    let bin = LATE_EDGES_NS
        .iter()
        .position(|&edge| lane.late_max_ns < edge)
        .unwrap_or(LATE_EDGES_NS.len());
    assert_eq!(lane.late_bins[bin], lane.packets, "{lane:?}");
    if took < LATE_EDGES_NS[0] - 50_000 {
        assert_eq!(
            lane.late_bins[0], lane.packets,
            "the bins call it on time: {lane:?}"
        );
    }
    assert!(lane.over_latency >= 1, "{lane:?}");
    let age = ns_of(FPP, RATE) + TX_MARGIN_NS + 50_000;
    assert!(
        (age..age + took + 1_000).contains(&lane.age_max_ns),
        "{lane:?}, step took {took} ns"
    );
}

/// dstack #1: a receiver's stated latency counts for its flow only while
/// the slot still sends to it; a reused slot starts unknown.
#[test]
fn a_receiver_latency_holds_only_for_the_destination_it_was_read_for() {
    let shared = shared_with(Vec::new());
    let a = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 2), 4000);
    let b = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 2), 4001);
    let c = SocketAddrV4::new(Ipv4Addr::new(127, 0, 0, 3), 4000);
    assert_eq!(shared.receiver_latency_ns(0, a), 0, "never read");
    shared.set_receiver_latency(0, a, 10_000_000);
    assert_eq!(shared.receiver_latency_ns(0, a), 10_000_000);
    assert_eq!(shared.receiver_latency_ns(0, b), 0, "another port");
    assert_eq!(shared.receiver_latency_ns(0, c), 0, "another address");
    assert_eq!(shared.receiver_latency_ns(1, a), 0, "another slot");
    shared.set_receiver_latency(MAX_FLOWS, a, 1);
    assert_eq!(shared.receiver_latency_ns(MAX_FLOWS, a), 0, "no such slot");
}

#[test]
fn a_packet_at_the_latency_still_plays_and_one_ns_more_is_late() {
    assert!(!over_latency(250_000, 250_000));
    assert!(over_latency(250_001, 250_000));
    assert!(!over_latency(0, 250_000));
}

/// Catalog: gen3 on the N100 box (31.10.2026, v0.4.9). The RX and TX
/// threads share the io core; the box received 7 stage flows (168 k
/// packets/s at fpp 4). At one SCHED_FIFO priority the TX thread waits
/// until the RX thread yields: age_max 239/247 µs at 250 µs, card peaks
/// over 400 µs. TX must preempt RX, and both stay below dsync's 50.
#[test]
fn the_tx_thread_preempts_the_rx_thread_and_both_stay_below_dsync() {
    const DSYNC_PRIORITY: i32 = 50;
    let (tx, rx, engine) = (
        TX_PRIORITY,
        crate::media::rx::RX_PRIORITY,
        crate::stream::ENGINE_PRIORITY,
    );
    assert!(tx > rx, "TX {tx} must be above RX {rx}");
    assert!(tx < DSYNC_PRIORITY, "TX {tx} below dsync");
    assert!(rx > engine, "RX {rx} above the engine pump {engine}");
}

/// Catalog: gen3 on the N100 box (31.10.2026). Dante hardware marks its
/// audio DSCP EF; ours went out unmarked, behind everything a switch with
/// QoS ranks higher, and in the kernel's default band behind bulk data.
/// Every TX flow socket marks EF, and on Linux takes priority 6
/// (pfifo_fast's first band).
#[cfg(target_os = "linux")]
#[test]
fn every_tx_flow_socket_marks_its_packets_ef() {
    let shared = TxShared::new(cfg(), Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap();
    for s in shared.sockets.iter() {
        let r = socket2::SockRef::from(s);
        assert_eq!(r.tos_v4().unwrap(), u32::from(DSCP_EF) << 2);
        assert_eq!(r.priority().unwrap(), SOCKET_PRIORITY);
    }
}

// ---- past the qdisc ----

#[test]
fn a_slot_takes_its_neighbour_s_mac_from_the_arp_table() {
    let shared = shared_with(Vec::new());
    let (slot, _) = add(&shared, &req(peer(4000)));
    let (other, _) = add(&shared, &req(peer(4001)));
    let mac = [0, 0x1d, 0xc1, 0xaa, 0xbb, 0xcc];
    let table = format!(
        "IP address HW type Flags HW address Mask Device\n\
         {PEER_IP} 0x1 0x2 00:1d:c1:aa:bb:cc * dante0\n"
    );
    assert_eq!(shared.slot_mac(slot), None, "none before the tick");
    let c = shared.lock();
    shared.set_macs(&c, &table, "dante0");
    assert_eq!(shared.slot_mac(slot), Some(mac));
    assert_eq!(shared.slot_mac(other), Some(mac), "the same receiver");
    assert_eq!(shared.slot_mac(other + 1), None, "a free slot");
    assert_eq!(shared.slot_mac(MAX_FLOWS), None, "no such slot");
    // No socket past the qdisc (the tests have no CAP_NET_RAW): nothing
    // goes direct, whatever the MAC.
    if shared.direct.is_none() {
        assert!(shared.direct_mac(slot).is_none());
    }
    // An entry on another interface, or gone: back through the UDP socket.
    let version = shared.macs[slot].version();
    shared.set_macs(&c, &table, "eth9");
    assert_eq!(shared.slot_mac(slot), None);
    shared.set_macs(&c, "", "dante0");
    assert_eq!(shared.slot_mac(slot), None);
    assert_eq!(
        shared.macs[slot].version(),
        version.wrapping_add(2),
        "an unchanged MAC is not written again"
    );
}

#[test]
fn a_direct_packet_leaves_from_its_slot_s_port() {
    let shared = shared_with(Vec::new());
    for slot in [0, 7, MAX_FLOWS - 1] {
        assert_eq!(
            shared.ports[slot],
            shared.local_addr(slot).unwrap().port(),
            "slot {slot}"
        );
    }
    assert_eq!(shared.ip, Ipv4Addr::LOCALHOST);
}

#[test]
fn a_send_counts_a_packet_a_direct_one_or_an_error() {
    let stats = LaneStats::default();
    count_send(&stats, &Ok(false), 100);
    count_send(&stats, &Ok(true), 200);
    count_send(&stats, &Ok(true), 300);
    count_send(
        &stats,
        &Err(io::Error::from(io::ErrorKind::WouldBlock)),
        400,
    );
    assert_eq!(stats.packets.load(Ordering::Relaxed), 3);
    // The sent packets' wire bytes only: datagrams plus 42 header bytes each.
    assert_eq!(stats.bytes.load(Ordering::Relaxed), 600 + 3 * 42);
    assert_eq!(stats.direct.load(Ordering::Relaxed), 2);
    assert_eq!(stats.send_errors.load(Ordering::Relaxed), 1);
    // A refusal without an OS code reads -1; one with a code keeps it.
    assert_eq!(stats.last_os_error.load(Ordering::Relaxed), -1);
    count_send(&stats, &Err(io::Error::from_raw_os_error(10055)), 50);
    assert_eq!(stats.last_os_error.load(Ordering::Relaxed), 10055);
    assert_eq!(stats.send_errors.load(Ordering::Relaxed), 2);
}

/// dstack #1: the packets sent on a retry reach the lane's counts.
#[test]
fn a_lanes_retries_reach_its_counts() {
    let shared = shared_with(Vec::new());
    shared.lanes[2].send_retries.fetch_add(5, Ordering::Relaxed);
    assert_eq!(shared.lane(2).send_retries, 5);
    assert_eq!(shared.lane(1).send_retries, 0);
}

#[test]
fn the_udp_path_sends_the_packet_after_the_headroom() {
    let shared = shared_with(Vec::new());
    let rx = UdpSocket::bind("127.0.0.1:0").unwrap();
    let SocketAddr::V4(dest) = rx.local_addr().unwrap() else {
        unreachable!()
    };
    let mut buf = [0u8; HEADROOM + 4];
    buf[HEADROOM..].copy_from_slice(&[1, 2, 3, 4]);
    let mut id = 9;
    let src = SocketAddrV4::new(Ipv4Addr::LOCALHOST, shared.ports[0]);
    let sent = send_one(None, &shared.sockets[0], &mut buf, src, dest, &mut id, 4);
    assert!(matches!(sent, Ok(false)), "{sent:?}");
    assert_eq!(id, 9, "no IPv4 ID taken");
    rx.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    let mut got = [0u8; 16];
    let (n, from) = rx.recv_from(&mut got).unwrap();
    assert_eq!(&got[..n], [1, 2, 3, 4]);
    assert_eq!(from.port(), shared.ports[0]);
}

/// What `f` logs at WARN or above on this thread, as text.
fn warned(f: impl FnOnce()) -> String {
    logged(tracing::Level::WARN, f)
}

/// What `f` logs at `level` and above.
fn logged(level: tracing::Level, f: impl FnOnce()) -> String {
    let buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));
    let sink = Arc::clone(&buf);
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || Writer(Arc::clone(&sink)))
        .with_ansi(false)
        .with_max_level(level)
        .finish();
    crate::test_log::undecided();
    tracing::subscriber::with_default(subscriber, f);
    String::from_utf8(buf.lock().unwrap().clone()).unwrap()
}

struct Writer(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Writer {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// dstack #1: SongPlayer's flow read 24 late at the FOH card on 1.11 with
/// no line on our side. The late watch logs our late packets each second.
#[test]
fn the_late_watch_logs_a_flow_s_late_packets_with_their_peak() {
    let mut cfg = cfg();
    cfg.tx_latency_ns = Some(500_000);
    let shared =
        Arc::new(TxShared::new(cfg, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    let mut rig = Rig::with(shared);
    let (slot, _) = rig.flow(vec![Some(1), Some(2)]);
    // The receiver states a latency under ours: it plays at ours.
    rig.shared.set_receiver_latency(slot, v4(&rig.rx), 250_000);
    let mut watch = crate::media::late::LateWatch::new();
    let quiet = warned(|| watch.watch(0, &rig.shared));
    assert_eq!(quiet, "", "the first look learns");
    rig.tx.step(Some(T), 0);
    rig.tx.step(Some(first_due_ns(T) + 50_000), 0);
    let lane = rig.shared.lane(slot);
    assert!(lane.over_latency >= 1, "{lane:?}");
    // Inside the second: nothing yet, the peak kept.
    assert_eq!(
        warned(|| watch.watch(crate::media::late::EVERY_NS - 1, &rig.shared)),
        ""
    );
    let log = warned(|| watch.watch(crate::media::late::EVERY_NS, &rig.shared));
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains(" WARN "), "{log}");
    assert!(
        log.contains(&format!(
            "slot={slot} packets={} age_peak_us={} receiver_latency_us=250",
            lane.over_latency,
            lane.age_max_ns / 1_000
        )),
        "{log}"
    );
    assert_eq!(lane.receiver_latency_ns, 250_000);
    assert!(
        log.contains("older than the latency their receiver plays at"),
        "{log}"
    );
    // The peak went with the look; the lane's maximum stays.
    assert_eq!(rig.shared.take_age_peak(slot), 0);
    assert_eq!(rig.shared.lane(slot).age_max_ns, lane.age_max_ns);
    assert_eq!(rig.shared.take_age_peak(MAX_FLOWS), 0, "no such slot");
    // The next second, no new late packet: quiet.
    assert_eq!(
        warned(|| watch.watch(2 * crate::media::late::EVERY_NS, &rig.shared)),
        ""
    );
}

#[test]
fn a_device_latency_set_live_moves_the_advertised_latency_and_the_late_count() {
    // No tx_latency_ns: our TX advertises the device latency, so a latency
    // Dante Controller sets moves it, and the TX thread counts lateness
    // against the new one from its next events pass.
    let mut follows = cfg();
    follows.tx_latency_ns = None;
    follows.latency_ns = 2_000_000;
    let shared =
        Arc::new(TxShared::new(follows, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    assert_eq!(shared.advertised_latency_ns(), 2_000_000);
    let mut rig = Rig::with(Arc::clone(&shared));
    let (slot, _) = rig.flow(vec![Some(1), Some(2)]);
    // The receiver states a latency under ours: it plays at ours.
    rig.shared.set_receiver_latency(slot, v4(&rig.rx), 250_000);
    // Set after the TX thread's state was made with 2 ms: its next events
    // pass takes it.
    shared.set_device_latency(500_000);
    assert_eq!(shared.advertised_latency_ns(), 500_000);
    rig.tx.step(Some(T), 0);
    // 50 us past due: 333 us + 250 us margin + 50 us since the stamp, past
    // the new 500 us (under the old 2 ms).
    rig.tx.step(Some(first_due_ns(T) + 50_000), 0);
    assert!(
        rig.shared.lane(slot).over_latency >= 1,
        "{:?}",
        rig.shared.lane(slot)
    );
    // A config's own tx_latency_ns holds: the device latency does not move
    // it.
    let mut fixed = cfg();
    fixed.tx_latency_ns = Some(700_000);
    let shared =
        Arc::new(TxShared::new(fixed, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap());
    shared.set_device_latency(5_000_000);
    assert_eq!(shared.advertised_latency_ns(), 700_000);
}

/// dlink (#68): under the normal profile a thread at normal priority is
/// what was asked, said at info; THREAD_PRIORITY_HIGHEST on Windows
/// (dstack #90) likewise; a warning only when a raised priority was asked
/// and refused.
#[test]
fn a_thread_s_priority_is_granted_highest_normal_by_profile_or_refused() {
    use crate::clock::ThreadPriority as Asked;
    assert_eq!(priority_outcome(Asked::Realtime, true), Priority::Granted);
    assert_eq!(priority_outcome(Asked::Realtime, false), Priority::Refused);
    assert_eq!(priority_outcome(Asked::Highest, true), Priority::Highest);
    assert_eq!(priority_outcome(Asked::Highest, false), Priority::Refused);
    assert_eq!(priority_outcome(Asked::Normal, false), Priority::Normal);
    assert_eq!(priority_outcome(Asked::Normal, true), Priority::Normal);
}

#[test]
fn the_bytes_sent_are_every_slot_s_wire_bytes() {
    let shared = shared_with(Vec::new());
    assert_eq!(shared.bytes_sent(), 0);
    count_send(&shared.lanes[0], &Ok(false), 100);
    count_send(&shared.lanes[3], &Ok(false), 58);
    assert_eq!(shared.bytes_sent(), 100 + 58 + 2 * WIRE_OVERHEAD);
}

/// dasio#1: a flow's failed sends are logged once a second with the OS
/// error, from the lanes' own counts.
#[test]
fn the_send_watch_logs_a_flow_s_failed_sends_with_the_os_error() {
    let shared = shared_with(Vec::new());
    let mut watch = crate::media::late::SendWatch::new();
    assert_eq!(
        warned(|| watch.watch(0, &shared)),
        "",
        "the first look learns"
    );
    count_send(
        &shared.lanes[2],
        &Err(io::Error::from_raw_os_error(10055)),
        50,
    );
    count_send(
        &shared.lanes[2],
        &Err(io::Error::from_raw_os_error(10055)),
        50,
    );
    let log = warned(|| watch.watch(crate::media::late::EVERY_NS, &shared));
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains(" WARN "), "{log}");
    assert!(
        log.contains("slot=2 errors=2 retried=0 os_error=10055"),
        "{log}"
    );
    assert_eq!(
        warned(|| watch.watch(2 * crate::media::late::EVERY_NS, &shared)),
        "",
        "quiet second"
    );
}

/// dstack #1: packets that went out on a retry are logged at info, once a
/// second, never as a warning (none was lost).
#[test]
fn the_send_watch_logs_retried_sends_at_info() {
    let shared = shared_with(Vec::new());
    let mut watch = crate::media::late::SendWatch::new();
    assert_eq!(logged(tracing::Level::INFO, || watch.watch(0, &shared)), "");
    count_send(
        &shared.lanes[1],
        &Err(io::Error::from_raw_os_error(10055)),
        50,
    );
    let errors = shared.lane(1).send_errors;
    shared.lanes[1].send_retries.fetch_add(3, Ordering::Relaxed);
    let log = logged(tracing::Level::INFO, || {
        watch.watch(crate::media::late::EVERY_NS, &shared)
    });
    assert_eq!(errors, 1);
    // The lost one and the retries: one warning.
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains(" WARN "), "{log}");
    assert!(log.contains("errors=1 retried=3"), "{log}");
    // Retries alone: info, not a warning.
    shared.lanes[1].send_retries.fetch_add(2, Ordering::Relaxed);
    let log = logged(tracing::Level::INFO, || {
        watch.watch(2 * crate::media::late::EVERY_NS, &shared)
    });
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(log.contains(" INFO "), "{log}");
    assert!(log.contains("slot=1 retried=2 os_error=10055"), "{log}");
    assert!(log.contains("went out on a retry"), "{log}");
    assert_eq!(
        warned(|| watch.watch(3 * crate::media::late::EVERY_NS, &shared)),
        "",
        "quiet second"
    );
}

/// dasio#1: a 16–32 ms stall of the NIC's send completions filled the
/// default send buffers and every send failed. Each flow socket asks for
/// [`SEND_BUFFER_BYTES`]; the kernel grants as far as `wmem_max`.
#[cfg(target_os = "linux")]
#[test]
fn the_send_buffer_is_raised_as_far_as_the_kernel_allows() {
    let max: usize = std::fs::read_to_string("/proc/sys/net/core/wmem_max")
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    // The kernel doubles what it grants (its bookkeeping), up to wmem_max.
    assert_eq!(
        shared_with(Vec::new()).send_buffer(),
        2 * SEND_BUFFER_BYTES.min(max)
    );
    // It covers more than a 50 ms stall of the widest flow: 8 channels of
    // 32 bits at 192 kHz.
    const _: () = assert!(SEND_BUFFER_BYTES > 192_000 * 8 * 4 / 20);
}

/// Windows (the FOH PC of dasio#1) takes the size as asked.
#[cfg(windows)]
#[test]
fn the_send_buffer_is_what_was_asked_on_windows() {
    assert_eq!(shared_with(Vec::new()).send_buffer(), SEND_BUFFER_BYTES);
}

/// `flow_slots` (dstack #1): a table of 2 slots grants two receivers and
/// refuses a third; the device opens only that many TX sockets.
#[test]
fn the_table_and_the_sockets_hold_only_the_device_s_flow_slots() {
    let mut t = TxTable::with_slots(7, 2);
    let (a, _) = t.add(&req(peer(4301)), 0).unwrap();
    let (b, _) = t.add(&req(peer(4302)), 0).unwrap();
    assert!(a < 2 && b < 2 && a != b, "{a} {b}");
    assert_eq!(t.add(&req(peer(4303)), 0), Err(status::TOO_MANY_TX_FLOWS));
    let mut few = cfg();
    few.flow_slots = 3;
    let s = TxShared::new(few, Ipv4Addr::LOCALHOST, Vec::new().into(), 1, &[]).unwrap();
    assert_eq!(s.sockets.len(), 3);
    assert!(s.ports[..3].iter().all(|&p| p != 0));
    assert!(s.ports[3..].iter().all(|&p| p == 0));
    assert_eq!(shared_with(Vec::new()).sockets.len(), MAX_FLOWS);
}

/// dasio (FOH, 3.11.2026): a flow of packets longer than our latency (DVS's
/// fpp 60 at 0.5 ms) is late only for a receiver below fpp + margin + the
/// send slack; its packets count as over latency only past that.
#[test]
fn a_flow_s_latency_floor_is_its_packet_when_longer_than_ours() {
    // 60 frames at 96 kHz: 625 µs, + 250 µs margin + the send slack.
    let floor = 625_000 + 250_000 + crate::device::TX_SEND_SLACK_NS;
    assert_eq!(flow_latency_ns(500_000, 60, 96_000, 250_000), floor);
    // A packet inside our latency: ours stands.
    assert_eq!(flow_latency_ns(1_000_000, 8, 96_000, 250_000), 1_000_000);
    assert!(!over_latency(
        floor,
        flow_latency_ns(500_000, 60, 96_000, 250_000)
    ));
    assert!(over_latency(
        floor + 1,
        flow_latency_ns(500_000, 60, 96_000, 250_000)
    ));
}

/// iemmixer (4.11.2026, v0.6.29 live): the Service -> Internal switch took
/// every TX flow's re-bootstraps 0 -> 1 (4 of 4). A seamless handover
/// (v0.6.31) keeps the epoch and steps the clock by at most
/// `clock::linked::HANDOVER_NS` (3 samples at 96 kHz): even at the least
/// advertised latency (0.25 ms, 24 samples) and fpp 16, a schedule stepped
/// either way at any point of a packet starts no schedule again and sends
/// every stamp once, in order.
#[test]
fn a_seamless_handover_step_restarts_no_tx_schedule() {
    use crate::clock::linked::HANDOVER_NS;
    let step = clock::to_samples(HANDOVER_NS.unsigned_abs(), 96_000) + 1;
    for forward in [true, false] {
        for into in 0..16 {
            let mut s = Schedule::bootstrap(1_000, 16, 24);
            let mut stamps = Vec::new();
            for now in 1_000..1_200 {
                while let Some(t) = s.due(now) {
                    stamps.push(t);
                }
            }
            let at = 1_200 + into;
            let from = if forward { at + step } else { at - step };
            for now in from..from + 200 {
                while let Some(t) = s.due(now) {
                    stamps.push(t);
                }
            }
            assert_eq!(s.take_rebootstraps(), 0, "forward {forward} into {into}");
            assert!(
                stamps.windows(2).all(|w| w[1] == w[0] + 16),
                "forward {forward} into {into}: {stamps:?}"
            );
        }
    }
}

// SPDX-License-Identifier: MIT OR Apache-2.0

//! The TX thread's passes with a TX pull allocate nothing (dstack #363):
//! each pass fills the due blocks through the [`TxPull`] (its player, a
//! switch to a new player taken mid-run, and `Rated`'s TX converter when
//! the app's rate is not the device's) into the engine rings, then sends
//! the flow's packets, under `assert_no_alloc` (warn mode: the violation
//! count must stay zero). Its own test binary, because it installs the
//! global allocator; the first test proves the detector sees an
//! allocation.

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::Arc;

use assert_no_alloc::{AllocDisabler, assert_no_alloc, reset_violation_count, violation_count};
use dlink::Rated;
use dlink::bridge::{AppBuffer, Fifo, Player, Stats, TxPull, chunk_ns, fifo_for};
use dstack::device::DeviceConfig;
use dstack::flows::control::FlowRequest;
use dstack::media::ring::Ring;
use dstack::media::rx::RxShared;
use dstack::media::source::Source;
use dstack::media::tx::{Transmitter, TxFill, TxShared, ns_of};
use dstack::stream::Timing;
use dstack_audio_io::Process;

#[global_allocator]
static ALLOCATOR: AllocDisabler = AllocDisabler;

/// The test device's rate (96 kHz, block 32, 1 ms latency).
const RATE: u32 = 96_000;
/// Ten years of a grandmaster's uptime at 96 kHz, as a position.
const T: u64 = 10 * 365 * 86_400 * 96_000;

#[test]
fn the_detector_sees_an_allocation() {
    reset_violation_count();
    let v = assert_no_alloc(|| vec![1u8; 4]);
    assert!(violation_count() > 0);
    assert_eq!(v.len(), 4);
}

fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(include_str!("../../../config/test-device.toml"))
        .expect("config/test-device.toml")
}

/// A player of `fifo` at `app` Hz for the device's TX.
fn player(fifo: &Arc<Fifo>, app: u32, stats: &Arc<Stats>) -> Player {
    let buffer = AppBuffer::new(10_000_000, chunk_ns(32, RATE));
    Player::new(
        Arc::clone(fifo),
        app,
        app,
        buffer,
        buffer.chunk_ns(),
        Arc::clone(stats),
    )
    .expect("the player")
}

/// 100 ms of TX passes 250 µs apart with a fill of TX 1 and 2 by `fill`
/// (wrapped by `wrap`), the app writing 10 ms into `fifo` at `app` Hz
/// every 40 passes, a new player handed over half way; returns the fill's
/// blocks.
fn passes(app: u32, wrap: impl FnOnce(TxPull) -> Box<dyn Process>) -> u64 {
    let cfg = cfg();
    let timing = Timing::of(&cfg);
    let shared = Arc::new(
        TxShared::new(
            cfg,
            Ipv4Addr::LOCALHOST,
            Arc::from(Vec::<Ring>::new()),
            1,
            &[0, 1],
        )
        .unwrap(),
    );
    assert!(shared.set_source(0, Source::Engine));
    assert!(shared.set_source(1, Source::Engine));
    let rx =
        Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::from(Vec::<Ring>::new())).unwrap());
    let fifo = Arc::new(fifo_for(2, app));
    let stats: Arc<Stats> = Arc::default();
    let (pull, mut switch) = TxPull::switched(player(&fifo, app, &stats), 2);
    let next = player(&fifo, app, &stats);
    let fill = TxFill::new(wrap(pull), vec![0, 1], timing, 24, rx);
    assert!(shared.claim_fill(fill).is_ok());
    let receiver = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    receiver.set_nonblocking(true).unwrap();
    let SocketAddr::V4(to) = receiver.local_addr().unwrap() else {
        panic!("an IPv4 socket");
    };
    {
        let mut c = shared.lock();
        let req = FlowRequest {
            rx_host: "peer-device".to_string(),
            rx_flow_name: "flow-1".to_string(),
            sample_rate: RATE,
            bits: 24,
            channels: vec![Some(1), Some(2)],
            fpp: 32,
            rx: SocketAddrV4::new(*to.ip(), to.port()),
        };
        c.table.add(&req, shared.now_ns()).unwrap();
        shared.publish(&c);
    }
    let mut tx = Transmitter::new(Arc::clone(&shared));
    let start = ns_of(T, RATE);
    let frames = app as usize / 100;
    let mut next = Some(next);
    reset_violation_count();
    for i in 0..400u64 {
        if i % 40 == 0 {
            fifo.push_with(frames, |f, c| ((f + c) as f64 * 0.001).sin() * 0.25);
        }
        if i == 200 {
            assert_eq!(switch.send(next.take().unwrap()), Ok(()));
        }
        let now = Some(start + i * 250_000);
        assert_no_alloc(|| tx.step(now, 0));
    }
    assert_eq!(
        violation_count(),
        0,
        "the TX passes with a TX pull allocated"
    );
    assert_eq!(switch.collect(), 1, "the replaced player came back");
    let lane = shared.lane(0);
    assert!(lane.packets >= 295, "{lane:?}");
    assert!(stats.played() > 0);
    let v = shared.tx_fill().unwrap();
    assert_eq!(
        (v.rebootstraps, v.writes_after_send, v.faults),
        (0, 0, 0),
        "{v:?}"
    );
    v.blocks
}

#[test]
fn the_tx_passes_with_a_tx_pull_allocate_nothing() {
    // The 4 blocks of the start at T (from the one stamped T), then one
    // each 32 samples up to 99.75 ms.
    assert_eq!(passes(RATE, |p| Box::new(p)), 303);
}

#[test]
fn the_tx_passes_with_a_converting_tx_pull_allocate_nothing() {
    let blocks = passes(48_000, |p| {
        Box::new(Rated::new(p, RATE, 48_000, 32, 0, 2).expect("the converter"))
    });
    assert_eq!(blocks, 303);
}

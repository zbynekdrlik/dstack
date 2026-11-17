// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

use super::*;

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, UdpSocket};

use crate::device::DeviceConfig;
use crate::flows::control::FlowRequest;
use crate::media::packet::{read_frames, read_header};
use crate::media::source::Source;
use crate::media::tx::{TX_MARGIN_NS, Transmitter, TxShared};

const RATE: u32 = 96_000;
/// Ten years of a grandmaster's uptime at 96 kHz, as a position (a
/// multiple of the block).
const T: u64 = 10 * 365 * 86_400 * 96_000;
/// The test device's block, P and W (`config/test-device.toml`: 1 ms
/// latency, the default block and work margin), and D = B + P + W.
const B: u64 = 32;
const P: u64 = 96;
const W: u64 = 96;
const D: u64 = B + P + W;
/// The flows' lag bound at the test device's 1 ms latency.
const MAX_LAG: u64 = 96;

fn cfg() -> DeviceConfig {
    DeviceConfig::from_toml(include_str!("../../../../../../config/test-device.toml"))
        .expect("config/test-device.toml")
}

#[test]
fn the_test_device_s_timing_is_the_one_these_tests_name() {
    let t = Timing::of(&cfg());
    assert_eq!((t.block, t.playout, t.margin, t.offset()), (B, P, W, D));
}

/// What [`Ramp`] writes at input position `pos` on output `c`, as the
/// ring then holds it (24 bits, left-justified).
fn ramp(pos: u64, c: usize) -> i32 {
    (((pos % 4096) + 1000 * c as u64) << 8) as i32
}

/// Output `c` at the block's position `start + f` is [`ramp`]; counts its
/// calls and discontinuities, and panics in the block at `panic_at`.
#[derive(Default)]
struct Ramp {
    calls: Arc<AtomicU64>,
    discontinuities: Arc<AtomicU64>,
    panic_at: Option<u64>,
}

impl Process for Ramp {
    fn process(&mut self, block: &mut Block<'_>) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let start = block.start().expect("a block with a start");
        assert_eq!(block.inputs(), 0);
        if self.panic_at == Some(start) {
            panic!("the app's TX processor panics");
        }
        for c in 0..block.outputs() {
            for (f, y) in block.output(c).iter_mut().enumerate() {
                *y = f64::from(ramp(start + f as u64, c) >> 8) / f64::from(1u32 << 23);
            }
        }
    }

    fn discontinuity(&mut self) {
        self.discontinuities.fetch_add(1, Ordering::Relaxed);
    }
}

fn rx() -> Arc<RxShared> {
    Arc::new(RxShared::new(RATE, Ipv4Addr::LOCALHOST, Arc::from(Vec::<Ring>::new())).unwrap())
}

/// Engine rings on TX 1 and 3, none on TX 2.
fn engine() -> Vec<Option<Ring>> {
    vec![Some(Ring::new()), None, Some(Ring::new())]
}

/// A fill of `ramp` on TX 1 and 3 (and its counts).
fn fill_of(ramp: Ramp) -> (TxFill, Arc<AtomicU64>, Arc<AtomicU64>) {
    let (calls, disc) = (Arc::clone(&ramp.calls), Arc::clone(&ramp.discontinuities));
    let fill = TxFill::new(Box::new(ramp), vec![0, 2], Timing::of(&cfg()), 24, rx());
    (fill, calls, disc)
}

/// `n` samples of `ring` from position `at`.
fn read(ring: &Ring, at: u64, n: usize) -> (Vec<i32>, usize) {
    let mut out = vec![0x5555_5500; n];
    let holes = ring.read_at(at, &mut out).all_holes();
    (out, holes)
}

fn ramps(from: u64, n: u64, c: usize) -> Vec<i32> {
    (from..from + n).map(|p| ramp(p, c)).collect()
}

#[test]
fn a_due_block_is_filled_at_the_pump_s_stamp_on_each_channel() {
    let (mut fill, calls, disc) = fill_of(Ramp::default());
    let engine = engine();
    assert_eq!(fill.wake_ns(RATE), None, "no schedule yet");
    // The first block's input ends at the playout point: due at T.
    fill.fill_due(T, MAX_LAG, &engine, 0);
    let first = T - B - P;
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(disc.load(Ordering::Relaxed), 1, "the start");
    let ring = engine[0].as_ref().unwrap();
    assert_eq!(read(ring, first + D, B as usize), (ramps(first, B, 0), 0));
    let ring = engine[2].as_ref().unwrap();
    assert_eq!(read(ring, first + D, B as usize), (ramps(first, B, 1), 0));
    // The next block is due a block later, at the pump's time.
    assert_eq!(fill.wake_ns(RATE), Some(ns_of(T + B, RATE)));
    fill.fill_due(T + B - 1, MAX_LAG, &engine, 0);
    assert_eq!(calls.load(Ordering::Relaxed), 1, "not due yet");
    fill.fill_due(T + B, MAX_LAG, &engine, 0);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    let ring = engine[0].as_ref().unwrap();
    assert_eq!(
        read(ring, first + B + D, B as usize),
        (ramps(first + B, B, 0), 0)
    );
    let v = fill.stats().view();
    assert_eq!(
        v,
        TxFillView {
            blocks: 2,
            ..TxFillView::default()
        }
    );
    assert_eq!(disc.load(Ordering::Relaxed), 1, "one start only");
}

#[test]
fn the_stamp_follows_the_engine_line() {
    let (mut fill, _, _) = fill_of(Ramp::default());
    let engine = engine();
    fill.fill_due(T, MAX_LAG, &engine, 64);
    let first = T - B - P;
    let ring = engine[0].as_ref().unwrap();
    assert_eq!(
        read(ring, first + D - 64, B as usize),
        (ramps(first, B, 0), 0)
    );
}

#[test]
fn a_late_tx_thread_catches_up_within_its_reach_and_starts_again_past_it() {
    let (mut fill, calls, disc) = fill_of(Ramp::default());
    let engine = engine();
    fill.fill_due(T, MAX_LAG, &engine, 0);
    // The reach is W + the latency: 192 samples behind, every block in
    // between is filled, none lost.
    let reach = W + MAX_LAG;
    fill.fill_due(T + reach, MAX_LAG, &engine, 0);
    let filled = 1 + reach / B;
    assert_eq!(calls.load(Ordering::Relaxed), filled);
    let first = T - B - P;
    let ring = engine[0].as_ref().unwrap();
    let n = filled * B;
    assert_eq!(read(ring, first + D, n as usize), (ramps(first, n, 0), 0));
    assert_eq!(fill.stats().view().rebootstraps, 0);
    // A sample past the reach behind the next block: started again at
    // now, the blocks between skipped, the processor told.
    let next_due = T + reach + B;
    fill.fill_due(next_due + reach + 1, MAX_LAG, &engine, 0);
    let v = fill.stats().view();
    assert_eq!((v.rebootstraps, v.writes_after_send), (1, 0), "{v:?}");
    assert!(v.skipped >= reach / B, "{v:?}");
    // The processor hears it before its next block.
    let wake = fill.wake_ns(RATE).unwrap();
    fill.fill_due(crate::clock::to_samples(wake, RATE), MAX_LAG, &engine, 0);
    assert_eq!(disc.load(Ordering::Relaxed), 2);
}

#[test]
fn a_clock_gone_back_far_starts_again() {
    let (mut fill, _, disc) = fill_of(Ramp::default());
    let engine = engine();
    fill.fill_due(T, MAX_LAG, &engine, 0);
    fill.fill_due(T - 2 * crate::media::tx::DISCONTINUITY, MAX_LAG, &engine, 0);
    assert_eq!(fill.stats().view().rebootstraps, 1);
    assert_eq!(disc.load(Ordering::Relaxed), 2);
}

#[test]
fn a_new_latency_or_lag_bound_starts_the_schedule_again_at_the_new_stamp() {
    let rx = rx();
    let ramp = Ramp::default();
    let disc = Arc::clone(&ramp.discontinuities);
    let mut fill = TxFill::new(
        Box::new(ramp),
        vec![0],
        Timing::of(&cfg()),
        24,
        Arc::clone(&rx),
    );
    let engine = engine();
    fill.fill_due(T, MAX_LAG, &engine, 0);
    // The same lag bound and latency: no restart.
    fill.fill_due(T + B, MAX_LAG, &engine, 0);
    assert_eq!(fill.stats().view().rebootstraps, 0);
    // Dante Controller sets 2 ms: P 192.
    rx.set_device_latency(2_000_000);
    let now = T + 10 * B;
    fill.fill_due(now, MAX_LAG, &engine, 0);
    let v = fill.stats().view();
    assert_eq!((v.rebootstraps, v.skipped), (1, 0), "{v:?}");
    assert_eq!(disc.load(Ordering::Relaxed), 2);
    // The new start's block is due at now and stamped at the new D.
    let p = 192;
    let start = (now - p - B).div_ceil(B) * B;
    let ring = engine[0].as_ref().unwrap();
    assert_eq!(
        read(ring, start + B + p + W, B as usize),
        (ramps(start, B, 0), 0)
    );
    // A new lag bound (the advertised latency) starts it again too.
    fill.fill_due(now + B, MAX_LAG + 1, &engine, 0);
    assert_eq!(fill.stats().view().rebootstraps, 2);
}

#[test]
fn a_clock_not_ready_starts_the_schedule_afresh_uncounted() {
    let (mut fill, _, disc) = fill_of(Ramp::default());
    let engine = engine();
    fill.fill_due(T, MAX_LAG, &engine, 0);
    fill.idle();
    assert_eq!(fill.wake_ns(RATE), None);
    fill.fill_due(T + 100 * B, MAX_LAG, &engine, 0);
    assert_eq!(fill.stats().view().rebootstraps, 0);
    assert_eq!(disc.load(Ordering::Relaxed), 2);
}

#[test]
fn a_re_anchor_moves_the_schedule_by_the_shift_or_starts_it_again() {
    let (mut fill, calls, disc) = fill_of(Ramp::default());
    let engine = engine();
    // No schedule yet: nothing to move.
    fill.re_anchor(None, T);
    assert_eq!(fill.stats().view().rebootstraps, 0);
    fill.fill_due(T, MAX_LAG, &engine, 0);
    // A step of two blocks forward, known: moved, nothing lost.
    fill.re_anchor(Some(2 * B as i64), T + 2 * B);
    assert_eq!(fill.wake_ns(RATE), Some(ns_of(T + 3 * B, RATE)));
    assert_eq!(fill.stats().view().rebootstraps, 0);
    // An unknown step: started again, counted, the processor told.
    fill.re_anchor(None, T + 2 * B);
    assert_eq!(fill.stats().view().rebootstraps, 1);
    fill.fill_due(T + 2 * B, MAX_LAG, &engine, 0);
    assert_eq!(disc.load(Ordering::Relaxed), 2);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
}

#[test]
fn a_panicking_processor_is_silent_from_then_on_and_counted() {
    let first = T - B - P;
    let (mut fill, calls, _) = fill_of(Ramp {
        panic_at: Some(first + B),
        ..Ramp::default()
    });
    let engine = engine();
    fill.fill_due(T, MAX_LAG, &engine, 0);
    fill.fill_due(T + 2 * B, MAX_LAG, &engine, 0);
    assert_eq!(calls.load(Ordering::Relaxed), 2, "never called after it");
    let ring = engine[0].as_ref().unwrap();
    assert_eq!(read(ring, first + D, B as usize), (ramps(first, B, 0), 0));
    let silent = vec![0; 2 * B as usize];
    assert_eq!(read(ring, first + B + D, 2 * B as usize), (silent, 0));
    let v = fill.stats().view();
    assert_eq!((v.blocks, v.faults), (3, 1), "{v:?}");
}

#[test]
fn a_block_written_after_its_stamps_were_read_is_counted() {
    let (mut fill, _, _) = fill_of(Ramp::default());
    let engine = engine();
    let first = T - B - P;
    // TX 3's stamps read (sent) before the block is filled.
    let (_, holes) = read(engine[2].as_ref().unwrap(), first + D, 4);
    assert_eq!(holes, 4);
    fill.fill_due(T, MAX_LAG, &engine, 0);
    assert_eq!(fill.stats().view().writes_after_send, 1);
}

#[test]
fn the_debug_form_names_the_channels() {
    let (fill, _, _) = fill_of(Ramp::default());
    assert!(format!("{fill:?}").contains("channels: [0, 2]"));
}

// ---- the TX thread ----

/// The TX state of the test device with an engine ring on TX 1.
fn shared() -> Arc<TxShared> {
    Arc::new(
        TxShared::new(
            cfg(),
            Ipv4Addr::LOCALHOST,
            Arc::from(Vec::<Ring>::new()),
            1,
            &[0],
        )
        .unwrap(),
    )
}

fn one_channel(ramp: Ramp) -> TxFill {
    TxFill::new(Box::new(ramp), vec![0], Timing::of(&cfg()), 24, rx())
}

#[test]
fn a_fill_is_claimed_once_before_the_tx_thread_and_only_on_engine_rings() {
    let shared = shared();
    assert_eq!(shared.tx_fill(), None);
    // TX 2 has no engine ring.
    let no_ring = TxFill::new(
        Box::new(Ramp::default()),
        vec![1],
        Timing::of(&cfg()),
        24,
        rx(),
    );
    assert!(shared.claim_fill(no_ring).is_err());
    assert_eq!(shared.tx_fill(), None);
    assert!(shared.claim_fill(one_channel(Ramp::default())).is_ok());
    assert_eq!(shared.tx_fill(), Some(TxFillView::default()));
    assert!(
        shared.claim_fill(one_channel(Ramp::default())).is_err(),
        "once"
    );
    let tx = Transmitter::new(Arc::clone(&shared));
    assert!(tx.fill.is_some(), "the TX thread took it");
    // Built: a later claim is refused.
    let other = Arc::new(
        TxShared::new(
            cfg(),
            Ipv4Addr::LOCALHOST,
            Arc::from(Vec::<Ring>::new()),
            1,
            &[0],
        )
        .unwrap(),
    );
    let built = Transmitter::new(Arc::clone(&other));
    assert!(built.fill.is_none());
    assert!(other.claim_fill(one_channel(Ramp::default())).is_err());
    assert_eq!(other.tx_fill(), None);
}

fn stamp_of(p: &[u8]) -> u64 {
    let (secs, sub) = read_header(p).unwrap();
    u64::from(secs) * u64::from(RATE) + u64::from(sub)
}

/// The TX thread fills each block before the pass that sends its stamps,
/// whatever its own lateness within the latency: no pump at all, and every
/// packet after the first block carries the processor's audio.
#[test]
fn the_tx_thread_fills_before_it_sends_even_when_it_runs_late() {
    let shared = shared();
    assert!(shared.set_source(0, Source::Engine));
    assert!(shared.claim_fill(one_channel(Ramp::default())).is_ok());
    let rx = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
    rx.set_nonblocking(true).unwrap();
    let SocketAddr::V4(to) = rx.local_addr().unwrap() else {
        panic!("an IPv4 socket");
    };
    {
        let mut c = shared.lock();
        let req = FlowRequest {
            rx_host: "peer-device".to_string(),
            rx_flow_name: "flow-1".to_string(),
            sample_rate: RATE,
            bits: 24,
            channels: vec![Some(1)],
            fpp: 32,
            rx: SocketAddrV4::new(*to.ip(), to.port()),
        };
        c.table.add(&req, shared.now_ns()).unwrap();
        shared.publish(&c);
    }
    let mut tx = Transmitter::new(Arc::clone(&shared));
    let start = ns_of(T, RATE);
    tx.step(Some(start), 0);
    // The next wake is the fill's next block (the pump's due time).
    assert_eq!(tx.wake_ns(Some(start)), ns_of(T + B, RATE));
    // Passes 0.9 ms apart (a TX thread late by up to 86 samples on each
    // packet, within its 1 ms latency; the pump's due times passed by as
    // much): every block is still filled before its stamps go out.
    let mut packets = Vec::new();
    let mut buf = [0u8; 2048];
    for i in 1..=30u64 {
        tx.step(Some(start + i * 900_000), 0);
        while let Ok(n) = rx.recv(&mut buf) {
            packets.push(buf[..n].to_vec());
        }
    }
    let written = T - B - P + D;
    let mut checked = 0;
    for p in &packets {
        let t = stamp_of(p);
        assert!(ns_of(t + 32, RATE) + TX_MARGIN_NS <= start + 30 * 1_000_000);
        let mut got = Vec::new();
        read_frames(p, 1, 3, |_, _, x| got.push(x)).unwrap();
        if t >= written {
            assert_eq!(got, ramps(t - D, 32, 0), "the processor's audio at {t}");
            checked += 1;
        }
    }
    assert!(
        checked >= 70,
        "{checked} packets checked of {}",
        packets.len()
    );
    let lane = shared.lane(0);
    assert_eq!(lane.rebootstraps, 0, "{lane:?}");
    // Only the stamps before the first block's are holes.
    assert!(lane.holes <= written - T + 32, "{lane:?}");
    let v = shared.tx_fill().unwrap();
    assert_eq!(
        (v.rebootstraps, v.writes_after_send, v.faults),
        (0, 0, 0),
        "{v:?}"
    );
    // 27 ms at 96 kHz: 81 blocks of 32 after the first.
    assert_eq!(v.blocks, 82, "{v:?}");
}

/// While the clock is not ready the TX thread fills nothing; at a re-anchor
/// it moves the fill with the flows.
#[test]
fn the_tx_thread_idles_the_fill_without_a_clock_and_moves_it_at_a_re_anchor() {
    let shared = shared();
    let ramp = Ramp::default();
    let calls = Arc::clone(&ramp.calls);
    assert!(shared.claim_fill(one_channel(ramp)).is_ok());
    let mut tx = Transmitter::new(Arc::clone(&shared));
    tx.step(None, 0);
    assert_eq!(calls.load(Ordering::Relaxed), 0);
    let start = ns_of(T, RATE);
    tx.step(Some(start), 0);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    tx.step(None, 0);
    assert_eq!(tx.wake_ns(None), 0);
    assert!(tx.fill.as_ref().unwrap().wake_ns(RATE).is_none(), "idle");
    tx.step(Some(start + 1_000_000), 0);
    // A new epoch whose step is unknown: the fill starts again, counted.
    tx.step(Some(start + 2_000_000), 1);
    let v = shared.tx_fill().unwrap();
    assert_eq!(v.rebootstraps, 1, "{v:?}");
}

#[test]
fn the_engine_rings_are_the_pump_s_and_the_pull_s_channels_once_each() {
    assert_eq!(engine_channels(&[0, 2], &[1, 3], 4), Ok(vec![0, 2, 1, 3]));
    assert_eq!(engine_channels(&[], &[3], 4), Ok(vec![3]));
    assert_eq!(engine_channels(&[3], &[], 4), Ok(vec![3]));
    assert_eq!(
        engine_channels(&[0], &[4], 4),
        Err("the app's TX channel 5 is past the device's 4 TX channels".to_string())
    );
    assert_eq!(
        engine_channels(&[4], &[0], 4),
        Err("the app's TX channel 5 is past the device's 4 TX channels".to_string())
    );
    assert_eq!(
        engine_channels(&[1], &[0, 1], 4),
        Err("the app's TX channel 2 is named twice".to_string())
    );
    assert_eq!(
        engine_channels(&[], &[2, 2], 4),
        Err("the app's TX channel 3 is named twice".to_string())
    );
}

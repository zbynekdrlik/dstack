// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #403 (#402's guard): [`Block::play_at`] against the wire stamp of
//! the audio it carries, on loopback. A sender (a 96 kHz app, no converter)
//! codes each sample's wire stamp into its audio (a ramp: the media sample
//! index mod 2^20); a receiver shaped like stream-obs (a 48 kHz app on a
//! 96 kHz device, block 128, P 13.041666 ms) records each block's
//! `play_at` and the stamp its first sample decodes to. `L = play_at -
//! (that stamp + P)` in the app's clock is then fixed by the terms:
//!
//! - (c) RX, the hand-off: B + the converter's RX delay (`play_at` adds it);
//! - the converter's delay once more: the app block's first sample holds
//!   the audio that delay before the block's start (a ramp through the
//!   half-band's linear phase comes out shifted by exactly its delay);
//! - the flow's read delay, when the transmitter's latency is over ours:
//!   the flow is read at its latency, rounded up to whole blocks
//!   (`Manager::read_at`).
//!
//! #402 measured it on v0.6.144 and v0.6.170 alike: 2.583 ms at a 9 ms
//! transmitter, 30.583 ms at a 40 ms one (the flow read 28.0 ms later,
//! `delay 2688`), the same in every block. Both are pinned here to the
//! microsecond, derived from `Config::split`; a change in any term (or in
//! how `play_at` or the read follow them) fails one of the two.
//!
//! The coding is checked on the sender's own wire first: a raw flow from
//! it to a socket of the test carries each packet's stamp in its samples.

use std::net::{SocketAddr, SocketAddrV4, UdpSocket};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use dlink::{Block, Clock, Config, Link, Map, Place, Ports, Process, SubStatus};
use dstack::clock::{app_monotonic_ns, latency_samples};
use dstack::flows::client::Client;
use dstack::flows::control::FlowRequest;
use dstack::media::packet;

/// Loopback (dstack #366): nothing this test sends leaves the box.
fn test_ipv4() -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::LOCALHOST
}

/// Each test here runs two devices on the same ports.
static ONE_DEVICE: Mutex<()> = Mutex::new(());

/// The network's rate (both devices) and the receiver's app rate.
const RATE: u32 = 96_000;
const APP_RATE: u32 = 48_000;
/// The receiver's block, device frames (stream-obs: dlink's default).
const BLOCK: u64 = 128;
/// The ramp's period, samples (10.9 s at 96 kHz): its values are exact in
/// 24 bits (4 LSB a sample).
const WRAP: u64 = 1 << 20;
/// Samples kept off the ramp's wrap: the converter's filter smears it.
const WRAP_MARGIN: f64 = 3000.0;
/// Blocks set aside after a discontinuity: the converter starts again.
const AFTER_RESTART: u32 = 32;
/// How far an app sample may sit off its block's ramp, samples at the
/// network's rate (measured: about 1e-6).
const LINE_TOLERANCE: f64 = 0.05;
/// #402's measurement (issuecomment-6975644042), µs: `L` at a transmitter
/// latency under ours (9 ms) and over it (40 ms).
const MEASURED_UNDER_US: u64 = 2583;
const MEASURED_OVER_US: u64 = 30583;
/// How far the median `L` may sit off the terms (measured on dev1: within
/// 0.25 µs), and any one block's: the pump's offset filter follows a torn
/// clock pair by up to 133 ns a 1.33 ms wake (100 ppm), so a starved
/// runner may lean a few µs; still far under the converter's 625 µs and a
/// block's 1333 µs, the least a term moves `L` by.
const MEDIAN_TOLERANCE_NS: f64 = 1_000.0;
const BLOCK_TOLERANCE_NS: f64 = 20_000.0;
/// The audio recorded once the flow runs.
const RECORD: Duration = Duration::from_secs(2);
const SETTLE: Duration = Duration::from_millis(1500);
/// A bound on every wait, so a fault fails in seconds.
const BOUND: Duration = Duration::from_secs(20);

/// Sample index `w`'s code: (w mod 2^20) / 2^21 - 0.25.
fn code(w: u64) -> f64 {
    (w % WRAP) as f64 / (2 * WRAP) as f64 - 0.25
}

/// [`code`] back, as a fraction of samples (the converter's output lies
/// between the codes).
fn decode(x: f64) -> f64 {
    (x + 0.25) * (2 * WRAP) as f64
}

/// Whether `x` (one app block) is the coded ramp throughout: each sample
/// `step` network samples after the one before. A ring hole (a packet that
/// came after its read on a loaded box, silence) or the converter's smear
/// of one is not, and says nothing about `play_at`.
fn on_ramp(x: &[f64], step: f64) -> bool {
    let Some(&last) = x.last() else {
        return false;
    };
    let end = decode(last);
    x.iter()
        .enumerate()
        .all(|(i, &v)| (decode(v) - (end - step * (x.len() - 1 - i) as f64)).abs() < LINE_TOLERANCE)
}

/// A 24-bit wire sample (`read_frames`' word >> 8) back to its index mod
/// 2^20: `code(w) * 2^23 = 4 (w mod 2^20) - 2^21`.
fn wire_index(v: i32) -> u64 {
    (i64::from(v) + (1 << 21)).div_euclid(4) as u64
}

/// The sender: each output sample is its own wire stamp, coded. The pump
/// sends block `s`'s output at stamps `s + D` (`offset`, set once the
/// pump runs; silence before).
struct Ramp {
    offset: Arc<AtomicU64>,
}

impl Process for Ramp {
    fn process(&mut self, block: &mut Block<'_>) {
        let d = self.offset.load(Ordering::Relaxed);
        let start = block.start();
        let out = block.output(0);
        match start {
            Some(s) if d > 0 => {
                for (i, y) in out.iter_mut().enumerate() {
                    *y = code(s + i as u64 + d);
                }
            }
            _ => out.fill(0.0),
        }
    }
}

/// One received app block.
#[derive(Debug, Clone, Copy)]
struct Rec {
    /// The app's start, app frames.
    start: u64,
    play_at: i64,
    /// Its first input sample.
    first: f64,
    /// The whole block is the ramp ([`on_ramp`]).
    on_ramp: bool,
}

/// The receiver: records each block once `on`, but none within
/// [`AFTER_RESTART`] blocks of a discontinuity.
struct Stamps {
    on: Arc<AtomicBool>,
    recs: Arc<Mutex<Vec<Rec>>>,
    restarts: Arc<AtomicU64>,
    set_aside: u32,
}

impl Process for Stamps {
    fn process(&mut self, block: &mut Block<'_>) {
        if self.set_aside > 0 {
            self.set_aside -= 1;
            return;
        }
        if !self.on.load(Ordering::Relaxed) {
            return;
        }
        let (Some(start), Some(play_at)) = (block.start(), block.play_at()) else {
            return;
        };
        let input = block.input(0);
        let first = input.first().copied().unwrap_or(0.0);
        let on_ramp = on_ramp(input, f64::from(RATE) / f64::from(APP_RATE));
        if let Ok(mut v) = self.recs.try_lock() {
            v.push(Rec {
                start,
                play_at,
                first,
                on_ramp,
            });
        }
    }

    fn discontinuity(&mut self) {
        self.restarts.fetch_add(1, Ordering::Relaxed);
        self.set_aside = AFTER_RESTART;
    }
}

/// The receiver's media clock less the app's, ns: the read pair with the
/// least gap of 200 (on the TEST system clock the media clock IS the app's
/// clock plus a constant, so the bracket is the error).
fn media_less_app(link: &Link) -> (f64, i64) {
    let mut best: Option<(i64, f64)> = None;
    for _ in 0..200 {
        let before = app_monotonic_ns();
        let media = link.media().clock.now_ns().expect("a media clock");
        let after = app_monotonic_ns();
        let gap = after - before;
        let k = media as f64 - (before as f64 + after as f64) / 2.0;
        if best.is_none_or(|(g, _)| gap < g) {
            best = Some((gap, k));
        }
    }
    let (gap, k) = best.expect("200 reads");
    (k, gap)
}

/// A raw flow from the sender to a socket of ours: every sample of every
/// packet must be its own stamp, coded (the ramp IS the wire stamp).
/// Returns the packets checked.
fn wire_carries_its_stamps() -> Result<u64, String> {
    let ip = test_ipv4();
    let audio = UdpSocket::bind((ip, 0)).unwrap();
    audio
        .set_read_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let SocketAddr::V4(at) = audio.local_addr().unwrap() else {
        panic!("IPv4")
    };
    let req = FlowRequest {
        rx_host: "stamp-check".to_string(),
        rx_flow_name: "stamps".to_string(),
        sample_rate: RATE,
        bits: 24,
        channels: vec![Some(1)],
        fpp: 32,
        rx: at,
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let flows = SocketAddrV4::new(ip, dstack::flows::control::PORT);
    rt.block_on(Client::new(ip).request_flow(flows, &req))
        .map_err(|e| format!("the sender's raw flow: {e:?}"))?;
    let mut buf = [0u8; 2048];
    let (mut checked, mut silent) = (0u64, 0u64);
    let start = Instant::now();
    while checked < 300 && !BOUND.saturating_sub(start.elapsed()).is_zero() {
        let Ok(n) = audio.recv(&mut buf) else {
            continue;
        };
        let (secs, sub) = packet::read_header(&buf[..n]).ok_or("a short packet")?;
        let stamp = u64::from(secs) * u64::from(RATE) + u64::from(sub);
        let mut words = Vec::with_capacity(32);
        packet::read_frames(&buf[..n], 1, 3, |_, _, x| words.push(x >> 8))
            .map_err(|_| "a ragged packet")?;
        // A TX hole (a late pump on a loaded box) goes out as silence.
        if words.iter().all(|&v| v == 0) {
            silent += 1;
            continue;
        }
        for (i, &v) in words.iter().enumerate() {
            let want = (stamp + i as u64) % WRAP;
            if wire_index(v) != want {
                return Err(format!(
                    "packet stamped {stamp}: frame {i} carries {} (word {v}), its stamp is {want}",
                    wire_index(v)
                ));
            }
        }
        checked += 1;
    }
    if checked < 300 {
        return Err(format!(
            "{checked} packets within {BOUND:?} ({silent} silent)"
        ));
    }
    Ok(checked)
}

/// What one transmitter latency gave.
#[derive(Debug)]
struct Measured {
    /// Per block, `play_at - (stamp + P)`, ns.
    ls: Vec<f64>,
    /// Blocks recorded, and set aside: not the ramp throughout (a ring
    /// hole, silence), or near its wrap.
    recorded: usize,
    off_ramp: usize,
    near_wrap: usize,
    restarts: u64,
    expected_ns: f64,
    /// The flow's read as the terms give it (`playout`, `delay`), samples.
    read: (u64, u64),
    status: String,
}

/// The two devices, the transmitter at `tx_latency_ns`; the expected `L`
/// from the splits.
fn run(tx_latency_ns: u64) -> Measured {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(test_ipv4());
    let tx_config = Config::new("dlink-stamp-tx", dir.path().join("tx.toml"))
        .tx(["Out 1"])
        .sample_rate(RATE)
        .network_latency_ns(1_000_000)
        .receive_buffer_ns(tx_latency_ns - 1_000_000);
    let rx_config = Config::new("dlink-stamp-rx", dir.path().join("rx.toml"))
        .rx(["In 1"])
        .sample_rate(APP_RATE)
        .network_rate(RATE)
        .block(BLOCK)
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .subscribe(1, "Out 1", "dlink-stamp-tx");
    let tx = tx_config.split().expect("the sender's split");
    let rx = rx_config.split().expect("the receiver's split");
    assert_eq!(tx.latency_ns, tx_latency_ns, "the transmitter's latency");
    let p = latency_samples(rx.latency_ns, RATE);
    let advertised = latency_samples(tx.latency_ns, RATE);
    assert_eq!(latency_samples(rx.block_ns, RATE), BLOCK);
    // The flow's read: its latency (the larger) less ours, in whole blocks.
    let delay = advertised.saturating_sub(p).div_ceil(BLOCK) * BLOCK;
    let expected_ns =
        (rx.rx_handoff_ns + rx.rx_convert_ns) as f64 + delay as f64 * 1e9 / f64::from(RATE);

    let offset = Arc::new(AtomicU64::new(0));
    let sender = Link::start_on(
        &tx_config,
        &place,
        Map {
            rx: vec![],
            tx: vec![0],
        },
        Ramp {
            offset: Arc::clone(&offset),
        },
        Clock::System,
    )
    .expect("the sender starts");
    let d = sender.media().engine().expect("the sender's pump").offset;
    offset.store(d, Ordering::Relaxed);
    // The ring is written that far ahead: the stamps from here on are coded.
    std::thread::sleep(Duration::from_millis(300));
    if let Err(why) = wire_carries_its_stamps() {
        let status = sender.status();
        let _ = sender.stop();
        panic!("the sender's wire: {why}; status {status}");
    }

    let on = Arc::new(AtomicBool::new(false));
    let recs = Arc::new(Mutex::new(Vec::with_capacity(200_000)));
    let restarts = Arc::new(AtomicU64::new(0));
    let receiver = Link::start_on(
        &rx_config,
        &place,
        Map {
            rx: vec![0],
            tx: vec![],
        },
        Stamps {
            on: Arc::clone(&on),
            recs: Arc::clone(&recs),
            restarts: Arc::clone(&restarts),
            set_aside: 0,
        },
        Clock::System,
    )
    .expect("the receiver starts");
    let start = Instant::now();
    while receiver.rx_channels()[0].status != Some(SubStatus::Receiving)
        && !BOUND.saturating_sub(start.elapsed()).is_zero()
    {
        std::thread::sleep(Duration::from_millis(50));
    }
    let channel = receiver.rx_channels()[0].clone();
    if channel.status != Some(SubStatus::Receiving) {
        let status = receiver.status();
        let _ = receiver.stop();
        let _ = sender.stop();
        panic!("the receiver never received within {BOUND:?}: {channel:?}; status {status}");
    }
    std::thread::sleep(SETTLE);
    let (k_before, gap_before) = media_less_app(&receiver);
    on.store(true, Ordering::Relaxed);
    std::thread::sleep(RECORD);
    on.store(false, Ordering::Relaxed);
    let (k_after, gap_after) = media_less_app(&receiver);
    let status = receiver.status();
    let rx_pump = receiver.media().engine().expect("the receiver's pump");
    let recs = recs.lock().unwrap().clone();
    let _ = receiver.stop();
    let _ = sender.stop();
    // The TEST system clock is the app's plus a constant (no skew).
    assert!(
        (k_after - k_before).abs() < 1_000.0,
        "the receiver's media clock less the app's moved: {k_before} -> {k_after} ns (read gaps {gap_before}, {gap_after} ns)"
    );
    assert_eq!(rx_pump.playout, p, "the receiver's P");
    let k = (k_before + k_after) / 2.0;

    let ratio = f64::from(RATE) / f64::from(APP_RATE);
    let p_ns = p as f64 * 1e9 / f64::from(RATE);
    let mut ls = Vec::with_capacity(recs.len());
    let (mut off_ramp, mut near_wrap) = (0, 0);
    for r in &recs {
        if !r.on_ramp {
            off_ramp += 1;
            continue;
        }
        let w = decode(r.first);
        if !(WRAP_MARGIN..WRAP as f64 - WRAP_MARGIN).contains(&w) {
            near_wrap += 1;
            continue;
        }
        // The stamp's whole value: the period nearest the block's start.
        let near = r.start as f64 * ratio;
        let periods = ((near - w) / WRAP as f64).round();
        let stamp = periods * WRAP as f64 + w;
        let due_app = stamp * 1e9 / f64::from(RATE) + p_ns - k;
        ls.push(r.play_at as f64 - due_app);
    }
    Measured {
        ls,
        recorded: recs.len(),
        off_ramp,
        near_wrap,
        restarts: restarts.load(Ordering::Relaxed),
        expected_ns,
        read: (p + delay, delay),
        status,
    }
}

/// `L` at a transmitter latency of `tx_latency_ns`: the terms give the
/// `measured_us` of #402, and every block sits on them.
fn pinned(tx_latency_ns: u64, measured_us: u64) {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let m = run(tx_latency_ns);
    let mut sorted = m.ls.clone();
    sorted.sort_by(f64::total_cmp);
    let (min, max) = (sorted.first().copied(), sorted.last().copied());
    let median = sorted.get(sorted.len() / 2).copied();
    eprintln!(
        "tx {} ms: L expected {:.0} ns; min {min:?} median {median:?} max {max:?} over {} blocks ({} recorded, {} off the ramp, {} near the wrap, {} restarts)",
        tx_latency_ns as f64 / 1e6,
        m.expected_ns,
        sorted.len(),
        m.recorded,
        m.off_ramp,
        m.near_wrap,
        m.restarts
    );
    assert_eq!(
        (m.expected_ns / 1_000.0).round() as u64,
        measured_us,
        "the terms no longer give #402's measured L"
    );
    // The receiver's status states the same read of the flow.
    let (playout, delay) = m.read;
    let read = format!("\"playout\":{playout},\"delay\":{delay},");
    assert!(
        m.status.contains(&read),
        "the flow's read {read} not in the status {}",
        m.status
    );
    // 2 s of 64-frame app blocks at 48 kHz is 1500; the wrap, and ring
    // holes on a loaded box (the status's `holes`), may take some.
    assert!(
        sorted.len() >= 1_000,
        "only {} blocks judged ({} recorded, {} off the ramp, {} near the wrap, {} restarts); status {}",
        sorted.len(),
        m.recorded,
        m.off_ramp,
        m.near_wrap,
        m.restarts,
        m.status
    );
    let median = median.expect("blocks judged");
    assert!(
        (median - m.expected_ns).abs() <= MEDIAN_TOLERANCE_NS,
        "the median L {median:.0} ns is off the terms' {:.0} ns by over {MEDIAN_TOLERANCE_NS} ns (min {min:?}, max {max:?}); status {}",
        m.expected_ns,
        m.status
    );
    let off: Vec<f64> = sorted
        .iter()
        .copied()
        .filter(|l| (l - m.expected_ns).abs() > BLOCK_TOLERANCE_NS)
        .collect();
    assert!(
        off.is_empty(),
        "{} of {} blocks off the terms' L {:.0} ns by over {BLOCK_TOLERANCE_NS} ns (min {min:?}, max {max:?}); status {}",
        off.len(),
        sorted.len(),
        m.expected_ns,
        m.status
    );
}

/// A transmitter latency under ours (9 ms against 13.041666 ms): the flow
/// is read at our P, `L` = (c) RX + the converter's delay = 2.583 ms.
#[test]
fn play_at_is_the_wire_stamp_plus_the_rx_path_under_our_latency() {
    pinned(9_000_000, MEASURED_UNDER_US);
}

/// A transmitter latency over ours (40 ms): the flow is read at its
/// latency in whole blocks (28.0 ms later, `delay 2688`), and `play_at`
/// follows the read: 30.583 ms.
#[test]
fn play_at_follows_a_flow_read_later_at_a_transmitter_latency_over_ours() {
    pinned(40_000_000, MEASURED_OVER_US);
}

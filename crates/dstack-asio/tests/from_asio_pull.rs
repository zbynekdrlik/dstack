// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #364 (part of #346): an ASIO bridge's ASIO -> Dante side must not
//! depend on the pump's lateness. A bridge device (pulled both ways, as a
//! dasio bridge instance runs: `Link::start_pulling_with_tx` with the
//! bridge's to-ASIO FIFO and its from-ASIO pull) runs the bridge's pump
//! side while its pump stalls 10 to 20 ms at random: inside a block that
//! woke in time (its outputs then come after the TX thread sent their
//! stamps: `writes_after_send`) and past W (6 ms, a pump re-bootstrap). A
//! "driver" thread runs the bridge's driver side on a steady ASIO period
//! (256 frames at 48 kHz), its input a constant, as a driver's buffer
//! switch does. A dlink RX device hears the constant with no sample off
//! the level, the bridge device's TX lanes count no hole, its pump no
//! write after send, and the from-ASIO player no underrun. A file of its
//! own, on loopback (dstack #366) and its own system clock; our device
//! hears itself (the tests' rule: our device only). A second case runs a
//! bridge from ASIO only (`Link::start_tx_pulling`), and both read the TX
//! thread's fill (status `tx_pull`): every block filled, no restart, no
//! write after send, no fault. On a loaded dev1 run it
//! real-time (`DSTACK_TEST_LOOPBACK=1 chrt -f 10 <binary>`,
//! `.claude/rules/tests.md`).
//!
//! The driver thread is the from-ASIO FIFO's writer, witnessed as dlink's
//! `tests/tx_pull.rs` witnesses its app thread (dstack #376, dlink's
//! `tests/writer/witness.rs`): a judged window in which it stalled is set
//! aside and the next judged, up to [`witness::WINDOWS`], and a failure
//! prints `verdict RUNNER|OURS`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::{Clock, Config, Link, Map, Place, Ports, Stopped, SubStatus};
use dstack_asio::bridge::{Bridge, DanteSide, Spec};
use dstack_asio::{Block, Opened, Process, Sample};

#[path = "../../dlink/tests/writer/witness.rs"]
mod witness;
use witness::{Witness, judge, verdict};

/// One device pair at a time: each binds the Dante ports on loopback.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The test device's address: loopback, so none of its mDNS, info or
/// heartbeat multicasts leaves the box (dstack #366: this workspace is a
/// test run, which refuses a device on a site address).
fn test_ipv4() -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::LOCALHOST
}

/// The level the driver's input carries.
const LEVEL: f64 = 0.5;
/// The network's and the driver's rate.
const RATE: u32 = 48_000;
/// The driver's buffer: 256 frames at 48 kHz, 5.333 ms.
const BUFFER: usize = 256;
/// The settling after the receiver reads Receiving (priming, the
/// converter's filters), then the window judged.
const SETTLE: Duration = Duration::from_secs(2);
const JUDGED: Duration = Duration::from_secs(8);

/// The bridge's pump side under a pump that stalls 10 to 20 ms (at random)
/// about every 150 blocks (about 0.4 s of 128-frame blocks at 48 kHz):
/// more than W (6 ms) and the TX slack, inside the block.
struct Stalling {
    side: DanteSide,
    seed: u64,
    stalls: Arc<AtomicU64>,
}

impl Stalling {
    /// The next pseudo-random number (an LCG: no dependency for this).
    fn next(&mut self) -> u64 {
        self.seed = self
            .seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        self.seed >> 33
    }
}

impl Process for Stalling {
    fn process(&mut self, block: &mut Block<'_>) {
        self.side.process(block);
        if self.next().is_multiple_of(150) {
            let ms = 10 + self.next() % 11;
            self.stalls.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

/// The receiver's processor: counts input 0's samples at the level and
/// off it while `judging`.
struct Listen {
    judging: Arc<AtomicBool>,
    on: Arc<AtomicU64>,
    off: Arc<AtomicU64>,
}

impl Process for Listen {
    fn process(&mut self, block: &mut Block<'_>) {
        if !self.judging.load(Ordering::Relaxed) {
            return;
        }
        let (mut on, mut off) = (0, 0);
        for &x in block.input(0) {
            if (x - LEVEL).abs() < 1e-2 {
                on += 1;
            } else {
                off += 1;
            }
        }
        self.on.fetch_add(on, Ordering::Relaxed);
        self.off.fetch_add(off, Ordering::Relaxed);
    }
}

/// The counts of one judged window.
#[derive(Debug)]
struct Judged {
    /// The receiver's samples at the level and off it.
    on: u64,
    off: u64,
    /// The bridge device's TX holes and its pump's writes after send.
    holes: u64,
    after_send: u64,
    /// The pump's stalls, and the pump's view at the window's end.
    stalls: u64,
    sent: String,
    /// The TX fill's counts at the window's start and end.
    before: dstack::media::tx::TxFillView,
    after: dstack::media::tx::TxFillView,
}

/// Every TX slot's holes of `link`.
fn tx_holes(link: &Link) -> u64 {
    (0..dstack::media::tx::MAX_FLOWS)
        .map(|s| link.media().tx.lane(s).holes)
        .sum()
}

/// The bridge device started as a dasio bridge instance starts it pulled
/// (`to_asio` channels to ASIO: both ways, `Link::start_pulling_with_tx`;
/// none: `Link::start_tx_pulling`), its pump running `side` under the
/// stalls; the to-ASIO pull handed over when there is one.
fn start_bridge(
    bridge: &mut Bridge,
    config: &Config,
    place: &Place,
    to_asio: usize,
    stalls: &Arc<AtomicU64>,
) -> Link {
    let from = bridge.pull_from_asio().expect("the from-ASIO pull");
    let side = Stalling {
        side: bridge.dante_side().expect("the pump's side"),
        seed: 364,
        stalls: Arc::clone(stalls),
    };
    if to_asio == 0 {
        let map = Map {
            rx: vec![],
            tx: vec![0],
        };
        return Link::start_tx_pulling_on(config, place, map, side, from, Clock::System)
            .expect("the bridge device started");
    }
    let fifo = bridge.to_asio_fifo().expect("the to-ASIO FIFO");
    let map = Map {
        rx: vec![0],
        tx: vec![0],
    };
    let (link, pull) =
        Link::start_pulling_with_tx_on(config, place, map, side, fifo, from, Clock::System)
            .expect("the bridge device started");
    bridge
        .pull_to_asio(pull)
        .expect("the bridge takes the to-ASIO pull");
    link
}

/// The stalled-pump run (module doc) of a bridge with `to_asio` channels to
/// ASIO (0 or 1) and one from it.
fn a_stalled_pump_against(to_asio: usize) {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(test_ipv4());
    let rx: &[&str] = if to_asio == 0 { &[] } else { &["In 1"] };
    let bridge_config = Config::new("asio-from-tx", dir.path().join("tx.toml"))
        .rx(rx.iter().copied())
        .tx(["Out 1"])
        .sample_rate(RATE);
    let rx_config = Config::new("asio-from-rx", dir.path().join("rx.toml"))
        .rx(["In 1"])
        .sample_rate(RATE)
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .subscribe(1, "Out 1", "asio-from-tx");
    let opened = Opened {
        rate: f64::from(RATE),
        buffer_frames: BUFFER as u32,
        driver_inputs: 1,
        driver_outputs: 1,
        inputs: vec![0],
        outputs: (0..to_asio as u32).collect(),
        input_sample: Some(Sample::Float32),
        output_sample: Sample::Float32,
    };
    let mut bridge = Bridge::new(Spec {
        dante_rate: RATE,
        block_frames: dlink::BLOCK as u32,
        to_asio,
        from_asio: 1,
        app_buffer_ns: None,
    })
    .expect("the bridge");
    let stats = bridge.from_asio_stats();
    let stalls = Arc::new(AtomicU64::new(0));
    let sender = start_bridge(&mut bridge, &bridge_config, &place, to_asio, &stalls);
    let mut side = bridge.asio_side(&opened).expect("the driver's side");
    // The driver thread: a buffer switch every ASIO period, whatever the
    // pump does, from the device's start (a driver opened at the bridge's
    // start), each switch's lateness on the witness.
    let period = Duration::from_nanos(BUFFER as u64 * 1_000_000_000 / u64::from(RATE));
    let witness = Arc::new(Witness::new(
        period,
        &bridge.app_buffer(&opened),
        Arc::clone(&stats),
    ));
    let running = Arc::new(AtomicBool::new(true));
    let driver = {
        let (running, witness) = (Arc::clone(&running), Arc::clone(&witness));
        std::thread::spawn(move || {
            let input = vec![LEVEL; BUFFER];
            let mut out = vec![0f64; BUFFER * to_asio];
            let mut next = Instant::now();
            while running.load(Ordering::Relaxed) {
                next += period;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                side.process(&mut Block::new(BUFFER, &input, &mut out));
                witness.wrote(next);
            }
        })
    };
    let judging = Arc::new(AtomicBool::new(false));
    let (on, off) = (Arc::new(AtomicU64::new(0)), Arc::new(AtomicU64::new(0)));
    let receiver = Link::start_on(
        &rx_config,
        &place,
        Map {
            rx: vec![0],
            tx: vec![],
        },
        Listen {
            judging: Arc::clone(&judging),
            on: Arc::clone(&on),
            off: Arc::clone(&off),
        },
        Clock::System,
    )
    .expect("the receiving device started");
    let end = Instant::now() + Duration::from_secs(15);
    while receiver.rx_channels()[0].status != Some(SubStatus::Receiving) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    std::thread::sleep(SETTLE);
    let pump = || sender.media().engine().expect("the bridge device's pump");
    // The TX thread's fill (dstack #363's status `tx_pull`): the RED's
    // pump-run pull had none, so it is read where there is one.
    let fill = || sender.media().tx.tx_fill().unwrap_or_default();
    let (judged, window, aside) = judge(&witness, || {
        let (holes, after_send, stalled, before, on0, off0) = (
            tx_holes(&sender),
            pump().writes_after_send,
            stalls.load(Ordering::Relaxed),
            fill(),
            on.load(Ordering::Relaxed),
            off.load(Ordering::Relaxed),
        );
        judging.store(true, Ordering::Relaxed);
        std::thread::sleep(JUDGED);
        judging.store(false, Ordering::Relaxed);
        let sent = pump();
        Judged {
            on: on.load(Ordering::Relaxed) - on0,
            off: off.load(Ordering::Relaxed) - off0,
            holes: tx_holes(&sender) - holes,
            after_send: sent.writes_after_send - after_send,
            stalls: stalls.load(Ordering::Relaxed) - stalled,
            sent: format!("{sent:?}"),
            before,
            after: fill(),
        }
    });
    let Judged {
        on,
        off,
        holes,
        after_send,
        stalls,
        sent,
        before,
        after,
    } = judged;
    let filled = sender.media().tx.tx_fill();
    let channel = receiver.rx_channels().remove(0);
    running.store(false, Ordering::Relaxed);
    driver.join().expect("the driver thread");
    assert_eq!((sender.ended(), receiver.ended()), (None, None));
    assert!(matches!(receiver.stop(), Stopped::Done));
    assert!(matches!(sender.stop(), Stopped::Done));
    assert!(stalls >= 10, "only {stalls} pump stalls: {sent}");
    assert!(
        on + off >= 7 * u64::from(RATE),
        "{on} on, {off} off: {channel:?}"
    );
    // A writer stall makes no TX hole, write after send, fill restart or
    // fault: every window's, set aside or judged.
    let aside_counts: Vec<[u64; 5]> = aside
        .iter()
        .map(|(j, _)| {
            [
                j.holes,
                j.after_send,
                j.after.rebootstraps - j.before.rebootstraps,
                j.after.writes_after_send - j.before.writes_after_send,
                j.after.faults,
            ]
        })
        .collect();
    assert!(
        aside_counts.iter().all(|c| *c == [0; 5]),
        "the TX holes, writes after send, fill restarts, fill writes after send and faults of the windows set aside: {aside_counts:?}, {aside:?}"
    );
    let underruns = window.underruns;
    let fill_counts = [
        ("TX holes", holes),
        ("writes after send", after_send),
        (
            "the fill's restarts",
            after.rebootstraps - before.rebootstraps,
        ),
        (
            "the fill's writes after send",
            after.writes_after_send - before.writes_after_send,
        ),
        ("faults", after.faults),
    ];
    assert_eq!(
        (off, holes, after_send, underruns),
        (0, 0, 0, 0),
        "samples off the level, the bridge device's TX holes, its pump's writes after send and the from-ASIO underruns with {stalls} pump stalls: {on} on, the bridge device's pump {sent}, its TX pull {filled:?}, {channel:?}; {}, the driver thread's stall bound {} us, its record {window:?}; windows set aside (the driver thread stalled) {aside:?}",
        verdict(&window, &fill_counts),
        witness.stall_us(),
    );
    // The TX thread filled every block of the judged window (128 frames at
    // 48 kHz), and never started again, wrote after a send or faulted.
    assert!(
        filled.is_some(),
        "the TX thread fills the from-ASIO channel"
    );
    let blocks = after.blocks - before.blocks;
    let expected = JUDGED.as_secs() * u64::from(RATE) / dlink::BLOCK;
    assert!(
        blocks + 50 >= expected && blocks <= expected + 50,
        "{blocks} blocks filled, about {expected} due: {after:?}"
    );
    assert_eq!(
        (
            after.rebootstraps - before.rebootstraps,
            after.writes_after_send - before.writes_after_send,
            after.faults
        ),
        (0, 0, 0),
        "the TX pull's restarts, writes after send and faults: {after:?}"
    );
}

/// A dasio bridge both ways (to ASIO pulled by the driver, #307; from ASIO
/// by the TX thread).
#[test]
fn a_stalled_pump_leaves_the_from_asio_bridge_whole() {
    a_stalled_pump_against(1);
}

/// A bridge from ASIO only (`Link::start_tx_pulling`).
#[test]
fn a_stalled_pump_leaves_a_from_asio_only_bridge_whole() {
    a_stalled_pump_against(0);
}

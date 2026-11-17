// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #363: the TX pull on a whole device, read from its own counts. A
//! dlink TX device carries an app's constant through a [`TxPull`] while
//! its pump stalls 10 to 20 ms at random (as `tests/tx_pull.rs`), at the
//! device's rate and converted (a 48 kHz app on a 96 kHz device: the pull
//! behind `Rated`'s TX converter, obs's shape). A dlink RX device hears the
//! constant with no sample off the level; the sender's TX lanes count no
//! hole, its bridge no underrun, and the TX thread's fill (status
//! `tx_pull`) no restart, no write after send and no fault in the judged
//! window, while it filled every block (the pump's own counts no longer
//! see the pulled channels). Its own file, on the box's routed address
//! and its own system clock; on a loaded dev1 run it real-time (`chrt -f
//! 10`, `.claude/rules/tests.md`).

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::bridge::{Player, Stats, TxPull, fifo_for};
use dlink::{Block, Clock, Config, Link, Map, Place, Ports, Process, Stopped, SubStatus};

/// One device pair at a time: each binds the Dante ports on the routed
/// address.
static ONE_DEVICE: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn routed_ipv4() -> std::net::Ipv4Addr {
    let probe = UdpSocket::bind("0.0.0.0:0").unwrap();
    probe.connect("192.0.2.1:9").unwrap();
    match probe.local_addr().expect("the routed address") {
        SocketAddr::V4(a) => *a.ip(),
        other => panic!("no routed IPv4 address on this box: {other}"),
    }
}

const LEVEL: f64 = 0.5;
/// The app's rate.
const APP: u32 = 48_000;
const PERIOD: Duration = Duration::from_millis(10);
const SETTLE: Duration = Duration::from_secs(2);
const JUDGED: Duration = Duration::from_secs(8);

/// A pump that stalls 10 to 20 ms about every 150 blocks.
struct Stalling {
    seed: u64,
    stalls: Arc<AtomicU64>,
}

impl Process for Stalling {
    fn process(&mut self, _block: &mut Block<'_>) {
        self.seed = self
            .seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        if (self.seed >> 33).is_multiple_of(150) {
            let ms = 10 + (self.seed >> 40) % 11;
            self.stalls.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

/// Counts input 0's samples at the level and off it while `judging`.
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
        let off = block
            .input(0)
            .iter()
            .filter(|&&x| (x - LEVEL).abs() >= 1e-2)
            .count() as u64;
        self.off.fetch_add(off, Ordering::Relaxed);
        self.on
            .fetch_add(block.frames() as u64 - off, Ordering::Relaxed);
    }
}

fn tx_holes(link: &Link) -> u64 {
    (0..dstack::media::tx::MAX_FLOWS)
        .map(|s| link.media().tx.lane(s).holes)
        .sum()
}

/// The stalled-pump run with the app at [`APP`] on a device at `network`
/// Hz.
fn stalled_pump_at(network: u32, name: &str) {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(routed_ipv4());
    let tx_name = format!("{name}-tx");
    let tx_config = Config::new(tx_name.as_str(), dir.path().join("tx.toml"))
        .tx(["Out 1"])
        .sample_rate(APP)
        .network_rate(network)
        .app_buffer(10_000_000, None);
    let rx_config = Config::new(format!("{name}-rx"), dir.path().join("rx.toml"))
        .rx(["In 1"])
        .sample_rate(network)
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .subscribe(1, "Out 1", tx_name.as_str());
    let buffer = tx_config.bridge_buffer().expect("the app buffer");
    let fifo = Arc::new(fifo_for(1, APP));
    let stats = Arc::new(Stats::default());
    let player = Player::new(
        Arc::clone(&fifo),
        APP,
        APP,
        buffer,
        buffer.chunk_ns(),
        Arc::clone(&stats),
    )
    .expect("the player");
    let writing = Arc::new(AtomicBool::new(true));
    // The app thread's latest write past its schedule, us: an underrun
    // with this near the bridge's foot (the target less half a write) is
    // the test's own thread starved on a loaded box, not the stack.
    let app_late_us = Arc::new(AtomicU64::new(0));
    let app = {
        let (fifo, writing, late) = (
            Arc::clone(&fifo),
            Arc::clone(&writing),
            Arc::clone(&app_late_us),
        );
        std::thread::spawn(move || {
            let mut next = Instant::now();
            while writing.load(Ordering::Relaxed) {
                next += PERIOD;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                let us = Instant::now().saturating_duration_since(next).as_micros();
                late.fetch_max(u64::try_from(us).unwrap_or(u64::MAX), Ordering::Relaxed);
                fifo.push_with(APP as usize / 100, |_, _| LEVEL);
            }
        })
    };
    let stalls = Arc::new(AtomicU64::new(0));
    let sender = Link::start_tx_pulling_on(
        &tx_config,
        &place,
        Map {
            rx: vec![],
            tx: vec![0],
        },
        Stalling {
            seed: u64::from(network),
            stalls: Arc::clone(&stalls),
        },
        TxPull::new(player, 1),
        Clock::System,
    )
    .expect("the sending device started");
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
    let fill = || sender.media().tx.tx_fill().expect("the TX pull's counts");
    let (holes, before, underruns, stalled) = (
        tx_holes(&sender),
        fill(),
        stats.underruns(),
        stalls.load(Ordering::Relaxed),
    );
    judging.store(true, Ordering::Relaxed);
    std::thread::sleep(JUDGED);
    judging.store(false, Ordering::Relaxed);
    let holes = tx_holes(&sender) - holes;
    let after = fill();
    let underruns = stats.underruns() - underruns;
    let stalls = stalls.load(Ordering::Relaxed) - stalled;
    let channel = receiver.rx_channels().remove(0);
    let status = sender.status();
    let (on, off) = (on.load(Ordering::Relaxed), off.load(Ordering::Relaxed));
    writing.store(false, Ordering::Relaxed);
    app.join().expect("the app thread");
    assert_eq!((sender.ended(), receiver.ended()), (None, None));
    assert!(matches!(receiver.stop(), Stopped::Done));
    assert!(matches!(sender.stop(), Stopped::Done));
    assert!(stalls >= 10, "only {stalls} pump stalls at {network} Hz");
    assert!(
        on + off >= 7 * u64::from(network),
        "{on} on, {off} off at {network} Hz: {channel:?}"
    );
    assert!(status.contains("\"tx_pull\":{"), "{status}");
    // The device's blocks (128 frames at its rate) in the judged window,
    // every one filled.
    let blocks = after.blocks - before.blocks;
    let expected = JUDGED.as_secs() * u64::from(network) / 128;
    assert!(
        blocks + 50 >= expected && blocks <= expected + 50,
        "{blocks} blocks filled at {network} Hz, about {expected} due: {after:?}"
    );
    assert_eq!(
        (
            off,
            holes,
            underruns,
            after.rebootstraps - before.rebootstraps,
            after.writes_after_send - before.writes_after_send,
            after.faults
        ),
        (0, 0, 0, 0, 0, 0),
        "samples off the level, TX holes, bridge underruns, the fill's restarts, writes after send and faults at {network} Hz with {stalls} pump stalls: {on} on, {after:?}, {channel:?}, the app thread's latest write {} us late; the player's recentres {} and resyncs {}; the sender's status {status}",
        app_late_us.load(Ordering::Relaxed),
        stats.recentres(),
        stats.resyncs(),
    );
}

#[test]
fn a_stalled_pump_leaves_the_tx_pull_whole_at_the_device_s_rate() {
    stalled_pump_at(APP, "dlink-txcount-48");
}

#[test]
fn a_stalled_pump_leaves_a_converted_tx_pull_whole() {
    stalled_pump_at(96_000, "dlink-txcount-96");
}

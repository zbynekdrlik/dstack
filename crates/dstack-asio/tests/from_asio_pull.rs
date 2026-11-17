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
//! hears itself (the tests' rule: our device only). On a loaded dev1 run it
//! real-time (`DSTACK_TEST_LOOPBACK=1 chrt -f 10 <binary>`,
//! `.claude/rules/tests.md`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::{Clock, Config, Link, Map, Place, Ports, Stopped, SubStatus};
use dstack_asio::bridge::{Bridge, DanteSide, Spec};
use dstack_asio::{Block, Opened, Process, Sample};

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

/// Every TX slot's holes of `link`.
fn tx_holes(link: &Link) -> u64 {
    (0..dstack::media::tx::MAX_FLOWS)
        .map(|s| link.media().tx.lane(s).holes)
        .sum()
}

#[test]
fn a_stalled_pump_leaves_the_from_asio_bridge_whole() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(test_ipv4());
    let bridge_config = Config::new("asio-from-tx", dir.path().join("tx.toml"))
        .rx(["In 1"])
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
        outputs: vec![0],
        input_sample: Some(Sample::Float32),
        output_sample: Sample::Float32,
    };
    let mut bridge = Bridge::new(Spec {
        dante_rate: RATE,
        block_frames: dlink::BLOCK as u32,
        to_asio: 1,
        from_asio: 1,
        app_buffer_ns: None,
    })
    .expect("the bridge");
    let fifo = bridge.to_asio_fifo().expect("the to-ASIO FIFO");
    let stats = bridge.from_asio_stats();
    let from = bridge.pull_from_asio().expect("the from-ASIO pull");
    let stalls = Arc::new(AtomicU64::new(0));
    let (sender, pull) = Link::start_pulling_with_tx_on(
        &bridge_config,
        &place,
        Map {
            rx: vec![0],
            tx: vec![0],
        },
        Stalling {
            side: bridge.dante_side().expect("the pump's side"),
            seed: 364,
            stalls: Arc::clone(&stalls),
        },
        fifo,
        from,
        Clock::System,
    )
    .expect("the bridge device started");
    bridge
        .pull_to_asio(pull)
        .expect("the bridge takes the to-ASIO pull");
    let mut side = bridge.asio_side(&opened).expect("the driver's side");
    // The driver thread: a buffer switch every ASIO period, whatever the
    // pump does, from the device's start (a driver opened at the bridge's
    // start). It keeps its latest lateness for the message.
    let running = Arc::new(AtomicBool::new(true));
    let late_us = Arc::new(AtomicU64::new(0));
    let driver = {
        let (running, late_us) = (Arc::clone(&running), Arc::clone(&late_us));
        std::thread::spawn(move || {
            let period = Duration::from_nanos(BUFFER as u64 * 1_000_000_000 / u64::from(RATE));
            let input = vec![LEVEL; BUFFER];
            let mut out = vec![0f64; BUFFER];
            let mut next = Instant::now();
            while running.load(Ordering::Relaxed) {
                next += period;
                match next.checked_duration_since(Instant::now()) {
                    Some(wait) => std::thread::sleep(wait),
                    None => {
                        let us = u64::try_from(next.elapsed().as_micros()).unwrap_or(u64::MAX);
                        late_us.fetch_max(us, Ordering::Relaxed);
                    }
                }
                side.process(&mut Block::new(BUFFER, &input, &mut out));
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
    let (holes, after_send, underruns, stalled) = (
        tx_holes(&sender),
        pump().writes_after_send,
        stats.underruns(),
        stalls.load(Ordering::Relaxed),
    );
    late_us.store(0, Ordering::Relaxed);
    judging.store(true, Ordering::Relaxed);
    std::thread::sleep(JUDGED);
    judging.store(false, Ordering::Relaxed);
    let holes = tx_holes(&sender) - holes;
    let sent = pump();
    let after_send = sent.writes_after_send - after_send;
    let underruns = stats.underruns() - underruns;
    let stalls = stalls.load(Ordering::Relaxed) - stalled;
    let channel = receiver.rx_channels().remove(0);
    let (on, off) = (on.load(Ordering::Relaxed), off.load(Ordering::Relaxed));
    running.store(false, Ordering::Relaxed);
    driver.join().expect("the driver thread");
    assert_eq!((sender.ended(), receiver.ended()), (None, None));
    assert!(matches!(receiver.stop(), Stopped::Done));
    assert!(matches!(sender.stop(), Stopped::Done));
    assert!(stalls >= 10, "only {stalls} pump stalls: {sent:?}");
    assert!(
        on + off >= 7 * u64::from(RATE),
        "{on} on, {off} off: {channel:?}"
    );
    assert_eq!(
        (off, holes, after_send, underruns),
        (0, 0, 0, 0),
        "samples off the level, the bridge device's TX holes, its pump's writes after send and the from-ASIO underruns with {stalls} pump stalls: {on} on, the driver thread's latest switch {} us late, the bridge device's pump {sent:?}, {channel:?}",
        late_us.load(Ordering::Relaxed)
    );
}

// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #307: an RX bridge whose consumer has its own deadline (dlisten's
//! output callback) must not depend on the pump's lateness. A dlink TX
//! device sends a constant; a dlink RX device bridges it to an "output"
//! thread on a steady 10 ms deadline, while its pump stalls 10 to 20 ms at
//! random. The output plays the constant with no bridge underrun and no
//! sample off the level. A file of its own (its process sets the normal
//! profile), on the box's routed address and its own system clock; our
//! device hears itself (the tests' rule: our device only).

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::bridge::{Player, Stats, fifo_for};
use dlink::{Block, Clock, Config, Link, Map, Place, Ports, Process, Stopped, SubStatus};

/// The box's IPv4 address on its default route (connecting a UDP socket
/// picks the route and sends nothing).
fn routed_ipv4() -> std::net::Ipv4Addr {
    let probe = UdpSocket::bind("0.0.0.0:0").unwrap();
    probe.connect("192.0.2.1:9").unwrap();
    match probe.local_addr().expect("the routed address") {
        SocketAddr::V4(a) => *a.ip(),
        other => panic!("no routed IPv4 address on this box: {other}"),
    }
}

/// The level the sender plays.
const LEVEL: f64 = 0.5;
/// The app's (and the network's) rate.
const RATE: u32 = 48_000;
/// The output's period: 10 ms, dlisten's WASAPI period.
const PERIOD: Duration = Duration::from_millis(10);
const PERIOD_FRAMES: usize = 480;
/// The output's settling (priming, the converter's filters), then the
/// window judged.
const SETTLE: Duration = Duration::from_secs(2);
const JUDGED: Duration = Duration::from_secs(8);

/// [`LEVEL`] on output 0.
struct Constant;

impl Process for Constant {
    fn process(&mut self, block: &mut Block<'_>) {
        block.output(0).fill(LEVEL as f32 as f64);
    }
}

/// A pump that stalls 10 to 20 ms (at random) about every 150 blocks
/// (about 0.4 s of 128-frame blocks at 48 kHz): a loaded PC's late wakes,
/// and past the RX-only device's W (15 ms) a pump restart.
struct Stalling {
    seed: u64,
    calls: u64,
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
    fn process(&mut self, _block: &mut Block<'_>) {
        self.calls += 1;
        if self.next().is_multiple_of(150) {
            let ms = 10 + self.next() % 11;
            self.stalls.fetch_add(1, Ordering::Relaxed);
            std::thread::sleep(Duration::from_millis(ms));
        }
    }
}

/// What the output thread saw over the judged window.
#[derive(Debug, Default)]
struct Heard {
    /// Samples at the level and off it.
    on: u64,
    off: u64,
    /// The bridge's underruns in the judged window.
    underruns: u64,
}

#[test]
fn a_stalled_pump_leaves_the_output_s_bridge_whole() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(routed_ipv4());
    let tx_config = Config::new("dlink-pull-tx", dir.path().join("tx.toml"))
        .tx(["Out 1"])
        .sample_rate(RATE)
        .work_margin_ns(12_000_000);
    let rx_config = Config::new("dlink-pull-rx", dir.path().join("rx.toml"))
        .rx(["In 1"])
        .sample_rate(RATE)
        .app_buffer(10_000_000, None)
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .subscribe(1, "Out 1", "dlink-pull-tx");
    let buffer = rx_config.bridge_buffer().expect("the app buffer");
    let sender = Link::start_on(
        &tx_config,
        &place,
        Map {
            rx: vec![],
            tx: vec![0],
        },
        Constant,
        Clock::System,
    )
    .expect("the sending device started");
    let fifo = Arc::new(fifo_for(1, RATE));
    let stalls = Arc::new(AtomicU64::new(0));
    let (receiver, mut pull) = Link::start_pulling_on(
        &rx_config,
        &place,
        Map {
            rx: vec![0],
            tx: vec![],
        },
        Stalling {
            seed: 307,
            calls: 0,
            stalls: Arc::clone(&stalls),
        },
        Arc::clone(&fifo),
        Clock::System,
    )
    .expect("the receiving device started");
    let end = Instant::now() + Duration::from_secs(15);
    while receiver.rx_channels()[0].status != Some(SubStatus::Receiving) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    let stats = Arc::new(Stats::default());
    let mut player = Player::new(
        Arc::clone(&fifo),
        RATE,
        RATE,
        buffer,
        buffer.chunk_ns(),
        Arc::clone(&stats),
    )
    .expect("the player");
    // The output thread: a steady deadline every PERIOD, as a sound card's
    // callback, whatever the pump does.
    let output = {
        let stats = Arc::clone(&stats);
        std::thread::spawn(move || {
            let mut out = vec![0f32; PERIOD_FRAMES];
            let mut heard = Heard::default();
            let began = Instant::now();
            let mut next = began;
            let mut judging = None;
            loop {
                next += PERIOD;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                pull.pull();
                player.fill_with(&mut out, 1, |x| x as f32);
                let elapsed = began.elapsed();
                if elapsed < SETTLE {
                    continue;
                }
                let before = *judging.get_or_insert_with(|| stats.underruns());
                if elapsed > SETTLE + JUDGED {
                    heard.underruns = stats.underruns() - before;
                    break;
                }
                for &x in &out {
                    if (f64::from(x) - LEVEL).abs() < 1e-2 {
                        heard.on += 1;
                    } else {
                        heard.off += 1;
                    }
                }
            }
            heard
        })
    };
    let heard = output.join().expect("the output thread");
    let channel = receiver.rx_channels().remove(0);
    let pump = receiver.media().engine().expect("the receiver's pump");
    assert_eq!((sender.ended(), receiver.ended()), (None, None));
    assert!(matches!(receiver.stop(), Stopped::Done));
    assert!(matches!(sender.stop(), Stopped::Done));
    let stalls = stalls.load(Ordering::Relaxed);
    assert!(stalls >= 10, "only {stalls} pump stalls: {pump:?}");
    assert!(
        heard.on + heard.off >= 7 * u64::from(RATE),
        "{heard:?}: {channel:?}"
    );
    assert_eq!(
        (heard.underruns, heard.off),
        (0, 0),
        "bridge underruns and samples off the level with {stalls} pump stalls: {heard:?}, {pump:?}, {channel:?}"
    );
}

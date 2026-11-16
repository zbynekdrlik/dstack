// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #307, step 2: an ASIO bridge's Dante -> ASIO side must not depend
//! on the pump's lateness. A dlink TX device sends a constant; a dlink RX
//! device runs the bridge's pump side ([`Bridge::dante_side`]) while its
//! pump stalls 10 to 20 ms at random, and a "driver" thread runs the
//! bridge's driver side ([`Bridge::asio_side`]) on a steady ASIO period
//! (256 frames at 48 kHz), as a driver's buffer switch does. The driver's
//! output plays the constant with no to-ASIO underrun and no sample off the
//! level. A file of its own (its process sets the normal profile), on the
//! box's routed address and its own system clock; our device hears itself
//! (the tests' rule: our device only).
//!
//! The driver thread records its own scheduling gaps on the app's clock
//! (dstack #307, a windows-2025 failure of v0.6.155: 2 underruns and 2584
//! samples off with no ring hole and no pull restart). A gap over two
//! periods is the runner not running the "driver": the switches it then
//! makes back to back, to keep its clock, take what the late switch's pull
//! brought and the player had skipped as an excess (by design), and run
//! the FIFO dry. A 20 ms stall of this thread every 700 switches, with no
//! pump stall, reproduced that run on dev1 to the sample (2 underruns,
//! 2584 off). A real card glitches there too: such a window says nothing
//! about the bridge. So a judged window in which the driver thread stalled
//! is set aside (kept for the message) and the next one is judged, up to
//! [`WINDOWS`]; the asserts on the judged window are unchanged, and a
//! failure names the driver's stalls before each underrun (`verdict`).

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::{Clock, Config, Link, Map, Place, Ports, Stopped, SubStatus};
use dstack_asio::bridge::{Bridge, DanteSide, Spec};
use dstack_asio::{Block, Opened, Process, Sample};

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
/// The network's and the driver's rate.
const RATE: u32 = 48_000;
/// The driver's buffer: 256 frames at 48 kHz, 5.333 ms.
const BUFFER: usize = 256;
/// The driver's settling (priming, the converter's filters), then the
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

/// The bridge's pump side under a pump that stalls 10 to 20 ms (at random)
/// about every 150 blocks (about 0.4 s of 128-frame blocks at 48 kHz): a
/// loaded PC's late wakes, and past the RX-only device's W a pump restart.
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

/// What the driver thread saw over the judged window.
#[derive(Debug, Default)]
struct Heard {
    /// Samples at the level and off it.
    on: u64,
    off: u64,
    /// The to-ASIO player's underruns in the judged window.
    underruns: u64,
    /// The FIFO's depth, frames, summed over two looks a buffer switch (its
    /// top, a buffer over the depth after the switch, and its foot, the
    /// depth after it), and the looks.
    depth: u64,
    looks: u64,
    /// The driver thread's own scheduling in the window: its largest gap
    /// between two switches and every gap over [`DRIVER_STALL_US`] with the
    /// switch it ended at (µs of the app's clock), and the switch at which
    /// each underrun was counted.
    largest_gap_us: u64,
    stalls: Vec<(u64, u64)>,
    underrun_at: Vec<u64>,
}

/// A gap between two of the driver thread's switches over this is a stall
/// of the thread, µs: two periods (the card's next switch was due).
const DRIVER_STALL_US: u64 = 2 * BUFFER as u64 * 1_000_000 / RATE as u64;

/// The judged windows the driver thread may need: one whose driver thread
/// never stalled is judged, a stalled one is set aside.
const WINDOWS: usize = 4;

/// How many switches after a stall of the driver thread an underrun is its
/// own: the late switch's excess skip and the switches made back to back
/// after it.
const CAUSE_SWITCHES: u64 = 8;

/// A failure's verdict: `RUNNER` when every underrun follows a stall of
/// the driver thread within [`CAUSE_SWITCHES`] (the runner did not run
/// it); else `OURS` (the bridge lost audio while its consumer ran on
/// time).
fn verdict(heard: &Heard) -> String {
    let caused = |at: u64| {
        heard
            .stalls
            .iter()
            .filter(move |&&(end, _)| end <= at && at - end <= CAUSE_SWITCHES)
    };
    let causes: Vec<String> = heard
        .underrun_at
        .iter()
        .map(|&at| {
            let before: Vec<String> = caused(at)
                .map(|&(end, us)| format!("{us} us, {} switches before", at - end))
                .collect();
            format!("underrun at switch {at}: driver stalls {before:?}")
        })
        .collect();
    let runner = !heard.underrun_at.is_empty()
        && heard
            .underrun_at
            .iter()
            .all(|&at| caused(at).next().is_some());
    let who = if runner {
        "RUNNER (the driver thread was not run)"
    } else {
        "OURS"
    };
    format!("verdict {who}: {causes:?}")
}

impl Heard {
    /// The FIFO's mean depth over the judged window, ms.
    fn mean_depth_ms(&self) -> f64 {
        self.depth as f64 / self.looks.max(1) as f64 * 1e3 / f64::from(RATE)
    }
}

#[test]
fn a_stalled_pump_leaves_the_to_asio_bridge_whole() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(routed_ipv4());
    let tx_config = Config::new("asio-pull-tx", dir.path().join("tx.toml"))
        .tx(["Out 1"])
        .sample_rate(RATE)
        .work_margin_ns(12_000_000);
    let rx_config = Config::new("asio-pull-rx", dir.path().join("rx.toml"))
        .rx(["In 1"])
        .sample_rate(RATE)
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .subscribe(1, "Out 1", "asio-pull-tx");
    let opened = Opened {
        rate: f64::from(RATE),
        buffer_frames: BUFFER as u32,
        driver_inputs: 0,
        driver_outputs: 1,
        inputs: vec![],
        outputs: vec![0],
        input_sample: None,
        output_sample: Sample::Float32,
    };
    let mut bridge = Bridge::new(Spec {
        dante_rate: RATE,
        block_frames: dlink::BLOCK as u32,
        to_asio: 1,
        from_asio: 0,
        app_buffer_ns: None,
    })
    .expect("the bridge");
    let buffer = bridge.app_buffer(&opened);
    let fifo = bridge.to_asio_fifo().expect("the to-ASIO FIFO");
    let stats = bridge.to_asio_stats();
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
    let stalls = Arc::new(AtomicU64::new(0));
    let (receiver, pull) = Link::start_pulling_on(
        &rx_config,
        &place,
        Map {
            rx: vec![0],
            tx: vec![],
        },
        Stalling {
            side: bridge.dante_side().expect("the pump's side"),
            seed: 307,
            stalls: Arc::clone(&stalls),
        },
        Arc::clone(&fifo),
        Clock::System,
    )
    .expect("the receiving device started");
    let pulled = pull.stats();
    bridge
        .pull_to_asio(pull)
        .expect("the bridge takes the pull");
    let mut side = bridge.asio_side(&opened).expect("the driver's side");
    // The driver thread: a buffer switch every ASIO period, whatever the
    // pump does. It runs from the device's start, as a driver opened at
    // the bridge's start does. The window judged starts SETTLE after the
    // channel reads Receiving.
    let receiving = Arc::new(AtomicU64::new(0));
    let driver = {
        let (stats, receiving, fifo) = (
            Arc::clone(&stats),
            Arc::clone(&receiving),
            Arc::clone(&fifo),
        );
        std::thread::spawn(move || {
            let period = Duration::from_nanos(BUFFER as u64 * 1_000_000_000 / u64::from(RATE));
            let mut out = vec![0f64; BUFFER];
            let mut heard = Heard::default();
            let mut aside = Vec::new();
            let began = Instant::now();
            let mut next = began;
            let mut since = None;
            let mut window: Option<(Instant, u64)> = None;
            let mut last_ns = dstack::clock::app_monotonic_ns();
            let mut switch = 0u64;
            let mut seen = stats.underruns();
            loop {
                next += period;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                side.process(&mut Block::new(BUFFER, &[], &mut out));
                let foot = fifo.len();
                switch += 1;
                let now_ns = dstack::clock::app_monotonic_ns();
                let gap_us = u64::try_from(now_ns - last_ns).unwrap_or(0) / 1_000;
                last_ns = now_ns;
                if receiving.load(Ordering::Acquire) == 0 {
                    if began.elapsed() > Duration::from_secs(15) {
                        return (heard, aside);
                    }
                    continue;
                }
                if since.get_or_insert_with(Instant::now).elapsed() < SETTLE {
                    continue;
                }
                let (start, before) = match window {
                    Some(w) => w,
                    None => {
                        // A window starts: its underruns are counted from
                        // here.
                        seen = stats.underruns();
                        *window.insert((Instant::now(), seen))
                    }
                };
                heard.largest_gap_us = heard.largest_gap_us.max(gap_us);
                if gap_us > DRIVER_STALL_US {
                    heard.stalls.push((switch, gap_us));
                }
                let underruns = stats.underruns();
                if underruns > seen {
                    heard.underrun_at.push(switch);
                    seen = underruns;
                }
                if start.elapsed() > JUDGED {
                    heard.underruns = underruns - before;
                    if heard.stalls.is_empty() || aside.len() + 1 == WINDOWS {
                        break;
                    }
                    // The runner stalled the driver thread: set aside.
                    aside.push(std::mem::take(&mut heard));
                    window = None;
                    continue;
                }
                heard.depth += (2 * foot + BUFFER) as u64;
                heard.looks += 2;
                for &x in &out {
                    if (x - LEVEL).abs() < 1e-2 {
                        heard.on += 1;
                    } else {
                        heard.off += 1;
                    }
                }
            }
            (heard, aside)
        })
    };
    let end = Instant::now() + Duration::from_secs(15);
    while receiver.rx_channels()[0].status != Some(SubStatus::Receiving) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    receiving.store(1, Ordering::Release);
    let (heard, aside) = driver.join().expect("the driver thread");
    let channel = receiver.rx_channels().remove(0);
    let pump = receiver.media().engine().expect("the receiver's pump");
    let sent = sender.media().engine().expect("the sender's pump");
    let ring = receiver.media().rx.rings()[0].counters();
    let late: u64 = (0..dstack::media::rx::MAX_FLOWS)
        .map(|s| receiver.media().rx.lane(s).late)
        .sum();
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
        "to-ASIO underruns and samples off the level with {stalls} pump stalls, {}: {heard:?}, {pump:?}, {channel:?}, the ring {ring:?}, the pull's restarts {}, the sender's pump {sent:?}; windows set aside (the driver thread stalled) {aside:?}",
        verdict(&heard),
        pulled.restarts()
    );
    // The driver's side drains every buffer switch: a packet is late only
    // past P and the time since the last drain (one ASIO period).
    assert_eq!(late, 0, "packets counted late: {channel:?}");
    // No latency added: pulled, the mean of the FIFO's top (after the pull)
    // and foot (after the switch's take) is the whole wait from a sample's
    // due time (its position + P) to its playing; the bound is the target
    // (the ASIO buffer + one pump block) + one pump block, under the
    // pump-written path's own wait (its FIFO mean + half a block in the
    // ring).
    let target_ms = buffer.target_ns() as f64 / 1e6;
    let block_ms = buffer.block_ns() as f64 / 1e6;
    let mean = heard.mean_depth_ms();
    assert!(
        mean > target_ms - 1.0 && mean < target_ms + block_ms,
        "mean wait {mean:.2} ms against the target {target_ms:.3} ms + a block {block_ms:.3} ms: {heard:?}"
    );
}

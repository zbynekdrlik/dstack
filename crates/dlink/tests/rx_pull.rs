// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #307: an RX bridge whose consumer has its own deadline (dlisten's
//! output callback) must not depend on the pump's lateness. A dlink TX
//! device sends a constant; a dlink RX device bridges it to an "output"
//! thread on a steady 10 ms deadline, while its pump stalls 10 to 20 ms at
//! random. The output plays the constant with no bridge underrun and no
//! sample off the level. A file of its own (its process sets the normal
//! profile), on loopback (dstack #366) and its own system clock; our
//! device hears itself (the tests' rule: our device only).
//!
//! The output thread is witnessed (`tests/consumer/witness.rs`, dstack
//! #395/#396): each callback's lateness against its absolute schedule, the
//! ring's holes never written, the sender's TX schedule restarts (dstack
//! #401) and its pump restarts with the silence they sent (dstack #436). A
//! late output thread (the runner not running it: its catch-up callbacks
//! run the FIFO dry, a windows-2025 failure of PR #394's run: 2 underruns,
//! 2578 off, no ring hole), a sender whose TX thread slept past its
//! latency (PR #406's windows-2025 run: 520 off, 0 underruns, 512 holes
//! never written) or whose pump woke more than W late (its TX thread sends
//! the positions the restart left unwritten as silence) says nothing about
//! the bridge: a
//! judged window that lost audio to either is set aside (kept for the
//! message) and the next one judged, up to [`witness::WINDOWS`]; the
//! asserts on the judged window are unchanged, and a failure prints
//! `verdict RUNNER|SENDER|OURS`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::bridge::{Player, Stats, fifo_for};
use dlink::{Block, Clock, Config, Link, Map, Place, Ports, Process, Stopped, SubStatus};

#[path = "tx_restart/restarts.rs"]
mod restarts;
#[path = "consumer/witness.rs"]
mod witness;
use witness::{Names, Played, Window, Witness, late_us, verdict};

/// The test device's address: loopback, so none of its mDNS, info or
/// heartbeat multicasts leaves the box (dstack #366: this workspace is a
/// test run, which refuses a device on a site address).
fn test_ipv4() -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::LOCALHOST
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

/// How many callbacks after a stall of the output thread an underrun is
/// its own: the late callback's pull brings the stall's audio, the player
/// skips the excess, and the first callback made back to back after it
/// runs the FIFO dry (counted once, then re-primed: a 25 to 40 ms stall's
/// underrun came 1 callback after it on dev1); 4 is margin.
const CAUSE_CALLBACKS: u64 = 4;

/// The output thread in a verdict.
const NAMES: Names = Names {
    thread: "output thread",
    call: "callback",
    calls: "callbacks",
};

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

#[test]
fn a_stalled_pump_leaves_the_output_s_bridge_whole() {
    restarts::listen();
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(test_ipv4());
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
    // The ring's holes never written (holes the sender left), and the
    // sender's TX side and pump, for the witness.
    let ring_media = Arc::clone(receiver.media());
    let mut witness = Witness::new(
        PERIOD,
        CAUSE_CALLBACKS,
        JUDGED,
        stats.underruns(),
        move || {
            let ring = ring_media.rx.rings()[0].counters();
            ring.holes.saturating_sub(ring.late)
        },
        Some(witness::Sender {
            tx: Arc::clone(&sender.media().tx),
            pump: sender.media().engine_stats().expect("the sender's pump"),
            rate: RATE,
        }),
    );
    let stall_us = witness.stall_us();
    // The output thread: a steady deadline every PERIOD, as a sound card's
    // callback, whatever the pump does. It runs from the device's start,
    // as dlisten's output does: its pull is the rings' only drain, so the
    // flow's audio is heard (and kept alive) only through it. The window
    // judged starts SETTLE after the channel reads Receiving.
    let receiving = Arc::new(AtomicU64::new(0));
    let pulled = pull.stats();
    let output = {
        let (stats, receiving, fifo) = (
            Arc::clone(&stats),
            Arc::clone(&receiving),
            Arc::clone(&fifo),
        );
        std::thread::spawn(move || {
            let mut out = vec![0f32; PERIOD_FRAMES];
            let began = Instant::now();
            let mut next = began;
            let mut since = None;
            loop {
                next += PERIOD;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                // How late this callback woke past its due time.
                let late_us = late_us(next);
                pull.pull();
                let top = fifo.len();
                player.fill_with(&mut out, 1, |x| x as f32);
                let foot = fifo.len();
                let underruns = stats.underruns();
                let heard = receiving.load(Ordering::Acquire) != 0;
                if !heard && began.elapsed() > Duration::from_secs(15) {
                    return None;
                }
                let judging = heard && since.get_or_insert_with(Instant::now).elapsed() >= SETTLE;
                let on = out
                    .iter()
                    .filter(|&&x| (f64::from(x) - LEVEL).abs() < 1e-2)
                    .count() as u64;
                // The FIFO's depth after the pull (the sawtooth's top) and
                // after the fill (its foot).
                let played = Played {
                    on,
                    off: PERIOD_FRAMES as u64 - on,
                    depth: (top + foot) as u64,
                    looks: 2,
                };
                if let Some(judged) =
                    witness.called(Instant::now(), late_us, underruns, judging, played)
                {
                    return Some(judged);
                }
            }
        })
    };
    let end = Instant::now() + Duration::from_secs(15);
    while receiver.rx_channels()[0].status != Some(SubStatus::Receiving) && Instant::now() < end {
        std::thread::sleep(Duration::from_millis(50));
    }
    receiving.store(1, Ordering::Release);
    let (heard, aside) = output
        .join()
        .expect("the output thread")
        .unwrap_or_else(|| (Window::default(), Vec::new()));
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
        "bridge underruns and samples off the level with {stalls} pump stalls, {}: {heard:?}, the output thread's stall bound {stall_us} us past its floor, {pump:?}, {channel:?}, the ring {ring:?}, the pull's restarts {}, the sender's pump {sent:?}; windows set aside (lost to the runner) {aside:?}",
        verdict(&heard, CAUSE_CALLBACKS, NAMES),
        pulled.restarts()
    );
    // The pull drains every callback: a packet is late only past P and the
    // time since the last drain (a callback's 10 ms), never on the pump's
    // block.
    assert_eq!(late, 0, "packets counted late: {channel:?}");
    // No latency added. A sample is due at its position + P; it then waits
    // in the ring until it is read, and in the FIFO until it is played.
    // Pulled, the ring wait is the time to the next callback and the FIFO
    // holds its foot between callbacks, so the mean of the top (after the
    // pull) and the foot is the whole wait. Written by the pump (dlisten up
    // to v0.6.152), the wait is the FIFO's mean plus half a pump block in
    // the ring. Measured on dev1 without stalls at this target (12.667 ms:
    // the period + one pump block): 13.63 ms + 1.33 ms = 14.96 ms written
    // by the pump, 14.34 ms pulled. The bound is the target + one pump
    // block (15.33 ms), under the pump path's own wait.
    let target_ms = buffer.target_ns() as f64 / 1e6;
    let block_ms = buffer.block_ns() as f64 / 1e6;
    let mean = heard.mean_depth_ms(RATE);
    assert!(
        mean > target_ms - 1.0 && mean < target_ms + block_ms,
        "mean wait {mean:.2} ms against the target {target_ms:.3} ms + a block {block_ms:.3} ms: {heard:?}"
    );
}

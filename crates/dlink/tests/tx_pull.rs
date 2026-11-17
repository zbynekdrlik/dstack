// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #363 (part of #346): a TX bridge whose app writes on its own
//! clock (dsend's capture, OBS's mix) must not depend on the pump's
//! lateness. A dlink TX device carries an app's constant through a
//! [`TxPull`] (an app thread writes it into the FIFO every 10 ms) while
//! the device's pump stalls 10 to 20 ms at random: inside a block that
//! woke in time (its outputs then come after the TX thread sent their
//! stamps: `writes_after_send`) and past W (6 ms, a pump re-bootstrap). A
//! dlink RX device hears the constant with no sample off the level, the
//! sender's TX lanes count no hole and its bridge no underrun. A file of
//! its own, on loopback (dstack #366) and its own system clock; our
//! device hears itself (the tests' rule: our device only). On a loaded
//! dev1 run it real-time (`chrt -f 10`, `.claude/rules/tests.md`).
//!
//! The app thread records how late each of its writes lands (dstack
//! #376, v0.6.166 windows-2025: 1 underrun, 390 off, the TX side clean):
//! a judged window in which it stalled is set aside and the next judged,
//! up to
//! [`witness::WINDOWS`], and a failure prints `verdict RUNNER|OURS`
//! (`tests/writer/witness.rs`).

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use dlink::bridge::{Player, Stats, TxPull, fifo_for};
use dlink::{Block, Clock, Config, Link, Map, Place, Ports, Process, Stopped, SubStatus};

#[path = "writer/witness.rs"]
mod witness;
use witness::{Witness, judge, verdict};

/// The test device's address: loopback, so none of its mDNS, info or
/// heartbeat multicasts leaves the box (dstack #366: this workspace is a
/// test run, which refuses a device on a site address).
fn test_ipv4() -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::LOCALHOST
}

/// The level the app writes.
const LEVEL: f64 = 0.5;
/// The app's (and the network's) rate.
const RATE: u32 = 48_000;
/// The app's writes: 10 ms, dsend's WASAPI packet.
const PERIOD: Duration = Duration::from_millis(10);
const PERIOD_FRAMES: usize = 480;
/// The settling after the receiver reads Receiving (priming, the
/// converter's filters), then the window judged.
const SETTLE: Duration = Duration::from_secs(2);
const JUDGED: Duration = Duration::from_secs(8);

/// A pump that stalls 10 to 20 ms (at random) about every 150 blocks
/// (about 0.4 s of 128-frame blocks at 48 kHz): more than W (6 ms) and
/// the TX slack, inside the block.
struct Stalling {
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
    fn process(&mut self, _block: &mut Block<'_>) {
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
    /// The sender's TX holes and its pump's writes after send.
    holes: u64,
    after_send: u64,
    /// The pump's stalls, and the pump's view at the window's end.
    stalls: u64,
    sent: String,
}

/// Every TX slot's holes of `link`.
fn tx_holes(link: &Link) -> u64 {
    (0..dstack::media::tx::MAX_FLOWS)
        .map(|s| link.media().tx.lane(s).holes)
        .sum()
}

#[test]
fn a_stalled_pump_leaves_the_tx_bridge_s_audio_whole() {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(test_ipv4());
    let tx_config = Config::new("dlink-txpull-tx", dir.path().join("tx.toml"))
        .tx(["Out 1"])
        .sample_rate(RATE)
        .app_buffer(10_000_000, None);
    let rx_config = Config::new("dlink-txpull-rx", dir.path().join("rx.toml"))
        .rx(["In 1"])
        .sample_rate(RATE)
        .ports(Ports {
            arc: 4441,
            cmc: 8801,
            flows: 4456,
        })
        .process_id(1)
        .subscribe(1, "Out 1", "dlink-txpull-tx");
    let buffer = tx_config.bridge_buffer().expect("the app buffer");
    let fifo = Arc::new(fifo_for(1, RATE));
    let stats = Arc::new(Stats::default());
    let player = Player::new(
        Arc::clone(&fifo),
        RATE,
        RATE,
        buffer,
        buffer.chunk_ns(),
        Arc::clone(&stats),
    )
    .expect("the player");
    // The app: a steady write every PERIOD on its own clock, whatever the
    // device does, each write's lateness on the witness.
    let witness = Arc::new(Witness::new(PERIOD, &buffer, Arc::clone(&stats)));
    let writing = Arc::new(AtomicBool::new(true));
    let app = {
        let (fifo, writing, witness) = (
            Arc::clone(&fifo),
            Arc::clone(&writing),
            Arc::clone(&witness),
        );
        std::thread::spawn(move || {
            let mut next = Instant::now();
            while writing.load(Ordering::Relaxed) {
                next += PERIOD;
                if let Some(wait) = next.checked_duration_since(Instant::now()) {
                    std::thread::sleep(wait);
                }
                fifo.push_with(PERIOD_FRAMES, |_, _| LEVEL);
                witness.wrote(next);
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
            seed: 363,
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
    let pump = || sender.media().engine().expect("the sender's pump");
    let (judged, window, aside) = judge(&witness, || {
        let (holes, after_send, stalled, on0, off0) = (
            tx_holes(&sender),
            pump().writes_after_send,
            stalls.load(Ordering::Relaxed),
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
        }
    });
    let Judged {
        on,
        off,
        holes,
        after_send,
        stalls,
        sent,
    } = judged;
    let channel = receiver.rx_channels().remove(0);
    writing.store(false, Ordering::Relaxed);
    app.join().expect("the app thread");
    assert_eq!((sender.ended(), receiver.ended()), (None, None));
    assert!(matches!(receiver.stop(), Stopped::Done));
    assert!(matches!(sender.stop(), Stopped::Done));
    assert!(stalls >= 10, "only {stalls} pump stalls: {sent}");
    assert!(
        on + off >= 7 * u64::from(RATE),
        "{on} on, {off} off: {channel:?}"
    );
    // A writer stall makes no TX hole and no write after send: every
    // window's, set aside or judged.
    let aside_counts: Vec<(u64, u64)> =
        aside.iter().map(|(j, _)| (j.holes, j.after_send)).collect();
    assert!(
        aside_counts.iter().all(|&c| c == (0, 0)),
        "the TX holes and writes after send of the windows set aside: {aside_counts:?}, {aside:?}"
    );
    let underruns = window.underruns;
    assert_eq!(
        (off, holes, after_send, underruns),
        (0, 0, 0, 0),
        "samples off the level, the sender's TX holes, its pump's writes after send and its bridge's underruns with {stalls} pump stalls: {on} on, the sender's pump {sent}, {channel:?}; {}, the app thread's stall bound {} us, its record {window:?}; windows set aside (the app thread stalled) {aside:?}",
        verdict(
            &window,
            &[("TX holes", holes), ("writes after send", after_send)]
        ),
        witness.stall_us(),
    );
}

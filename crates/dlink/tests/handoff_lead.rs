// SPDX-License-Identifier: MIT OR Apache-2.0

//! dstack #476: every RX block reaches the app a stated lead H before its
//! [`Block::play_at`] (the owner's ROZHODNUTÉ of 21.11.2026: H = 30 % of
//! P, rounded down to whole samples, taken out of the receive buffer (b);
//! P, `play_at` and P + (c) unchanged). `play_at` is the time the app
//! PLAYS the block (OBS stamps its audio with it), so a block handed at or
//! after it is late for an app that plays by it.
//!
//! A receiver shaped like stream-obs and strih-snv's OBS (a 48 kHz app on
//! a 96 kHz device, block 128, RX only, the defaults' P 13.041666 ms)
//! records, per block, the app's clock (`clock::app_monotonic_ns`) at its
//! processor's entry and the block's `play_at`. The lead is `play_at` less
//! that time. Before #476 the pump was due AT `play_at` and handed every
//! block after it (this test on v0.6.209 on dev1: every block late, p50
//! -104 µs on time; with the 2.5 ms nap down to -1.47 ms).
//!
//! Judged per block: never before its due time (a lead at most H: the pump
//! never runs a block early), and every block before its `play_at` (a lead
//! above 0), on time and with the processor napping 2.5 ms (inside H)
//! every 40th block, which makes the pump wake late for the blocks after
//! it. The median sits within [`MEDIAN_BELOW_H`] under H: the wake, the
//! drain, the ring read and the converter before the call. A block late
//! past its `play_at` counts against the code unless the pump itself woke
//! later than H in the window, which no lead can make up: that run prints
//! `RUNNER`. The window's slowest wake is its own (#476, main CI
//! 45630237191's coverage job: 76 late blocks, passed on rerun): the main
//! thread merges the pump's recent late wakes every [`WAKE_LOOK`] while it
//! records and keeps the latest whose wall time is inside the window. The
//! pump's `wake_behind_max_us` is since its start, so a stall before the
//! window hid any stall inside it that was smaller.
//!
//! Load-sensitive: judge it under `DSTACK_TEST_LOOPBACK=1 chrt -f 10` on
//! a loaded dev1.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use dlink::{Block, Clock, Config, Link, Map, Place, Ports, Process, SubStatus};
use dstack::clock::{app_monotonic_ns, latency_samples};

/// Loopback (dstack #366): nothing this test sends leaves the box.
fn test_ipv4() -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::LOCALHOST
}

/// Each test here runs two devices on the same ports.
static ONE_DEVICE: Mutex<()> = Mutex::new(());

const RATE: u32 = 96_000;
const APP_RATE: u32 = 48_000;
const BLOCK: u64 = 128;
/// Blocks set aside after a discontinuity (the pump started again).
const AFTER_RESTART: u32 = 32;
const RECORD: Duration = Duration::from_secs(2);
const SETTLE: Duration = Duration::from_millis(1000);
const BOUND: Duration = Duration::from_secs(20);
/// The nap: 2.5 ms, inside H (3.906 ms at this P), every 40th block.
const NAP: Duration = Duration::from_micros(2500);
const NAP_EVERY: u64 = 40;
/// How far under H the median lead may sit, ns: the pump's wake, its
/// socket drain, the ring read and the converter before the processor
/// (about 0.13 ms on dev1).
const MEDIAN_BELOW_H: i64 = 600_000;
/// How often the main thread merges the pump's recent late wakes while it
/// records: the pump keeps the last 32 (`dstack::stream::wake::RECENT`),
/// and a nap every [`NAP_EVERY`] blocks gives a few a nap.
const WAKE_LOOK: Duration = Duration::from_millis(5);
/// How far over H a block's lead may read, ns: the pump's play-time
/// filter follows the media-less-app offset a few µs a wake at most.
const OVER_H: i64 = 20_000;

/// H, the decided lead, samples at the device's rate: 30 % of P, rounded
/// down to whole samples (the owner, 21.11.2026). Derived here, not read
/// from the stack, so the test states the contract on its own.
fn lead_of(p: u64) -> u64 {
    p * 3 / 10
}

/// A constant on the sender's output.
struct Tone;

impl Process for Tone {
    fn process(&mut self, block: &mut Block<'_>) {
        block.output(0).fill(0.25);
    }
}

/// One block as the app received it.
#[derive(Debug, Clone, Copy)]
struct Rec {
    /// The app's clock at the processor's entry, ns.
    at: i64,
    play_at: i64,
}

/// The receiver: records each block once `on` (none within
/// [`AFTER_RESTART`] of a discontinuity), and sleeps `nap` every
/// [`NAP_EVERY`]th block, so the pump wakes that late (less the blocks'
/// own time) for the blocks after it.
struct Handoff {
    on: Arc<AtomicBool>,
    recs: Arc<Mutex<Vec<Rec>>>,
    restarts: Arc<AtomicU64>,
    set_aside: u32,
    calls: u64,
    nap: Option<Duration>,
}

impl Process for Handoff {
    fn process(&mut self, block: &mut Block<'_>) {
        let at = app_monotonic_ns();
        self.calls += 1;
        let on = self.on.load(Ordering::Relaxed);
        if self.set_aside > 0 {
            self.set_aside -= 1;
        } else if on
            && let Some(play_at) = block.play_at()
            && let Ok(mut v) = self.recs.try_lock()
        {
            v.push(Rec { at, play_at });
        }
        if let Some(nap) = self.nap
            && on
            && self.calls.is_multiple_of(NAP_EVERY)
        {
            std::thread::sleep(nap);
        }
    }

    fn discontinuity(&mut self) {
        self.restarts.fetch_add(1, Ordering::Relaxed);
        self.set_aside = AFTER_RESTART;
    }
}

struct Measured {
    /// Per block, `play_at` less its receipt, ns.
    leads: Vec<i64>,
    /// H, ns.
    lead_ns: i64,
    restarts: u64,
    rebootstraps: u64,
    /// The pump's latest wake since its start, before and after the
    /// window, µs (shown only: a stall before the window keeps it high).
    wake_before_us: u64,
    wake_after_us: u64,
    /// The window's own latest wake, µs: the largest `behind_us` of the
    /// pump's late wakes timed inside it (0: none was a block late).
    window_wake_us: u64,
    /// Late wakes in the window that no merge saw (the pump's recent list
    /// moved past them between two looks).
    wakes_unseen: u64,
    status: String,
}

fn run(nap: Option<Duration>) -> Measured {
    let dir = tempfile::tempdir().unwrap();
    let place = Place::Ip(test_ipv4());
    let tx_config = Config::new("dlink-lead-tx", dir.path().join("tx.toml"))
        .tx(["Out 1"])
        .sample_rate(RATE);
    let rx_config = Config::new("dlink-lead-rx", dir.path().join("rx.toml"))
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
        .subscribe(1, "Out 1", "dlink-lead-tx");
    let rx = rx_config.split().expect("the receiver's split");
    let p = latency_samples(rx.latency_ns, RATE);
    let lead_ns = i64::try_from(lead_of(p) * 1_000_000_000 / u64::from(RATE)).unwrap();
    let sender = Link::start_on(
        &tx_config,
        &place,
        Map {
            rx: vec![],
            tx: vec![0],
        },
        Tone,
        Clock::System,
    )
    .expect("the sender starts");
    let on = Arc::new(AtomicBool::new(false));
    let recs = Arc::new(Mutex::new(Vec::with_capacity(10_000)));
    let restarts = Arc::new(AtomicU64::new(0));
    let receiver = Link::start_on(
        &rx_config,
        &place,
        Map {
            rx: vec![0],
            tx: vec![],
        },
        Handoff {
            on: Arc::clone(&on),
            recs: Arc::clone(&recs),
            restarts: Arc::clone(&restarts),
            set_aside: 0,
            calls: 0,
            nap,
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
    let before = receiver.media().engine().expect("the pump");
    let pump = receiver.media().engine_stats().expect("the pump");
    let mut wakes = Wakes::new(pump.wakes().late_wakes());
    let from_ms = unix_ms();
    on.store(true, Ordering::Relaxed);
    let recording = Instant::now();
    while !RECORD.saturating_sub(recording.elapsed()).is_zero() {
        std::thread::sleep(WAKE_LOOK);
        wakes.merge(pump.wakes().late_wakes(), &pump.wakes().recent());
    }
    on.store(false, Ordering::Relaxed);
    wakes.merge(pump.wakes().late_wakes(), &pump.wakes().recent());
    let to_ms = unix_ms();
    let after = receiver.media().engine().expect("the pump");
    let status = receiver.status();
    let recs = recs.lock().unwrap().clone();
    let _ = receiver.stop();
    let _ = sender.stop();
    Measured {
        leads: recs.iter().map(|r| r.play_at - r.at).collect(),
        lead_ns,
        restarts: restarts.load(Ordering::Relaxed),
        rebootstraps: after.rebootstraps - before.rebootstraps,
        wake_before_us: before.wake_behind_max_us,
        wake_after_us: after.wake_behind_max_us,
        window_wake_us: wakes.latest_within(from_ms, to_ms),
        wakes_unseen: wakes.unseen(),
        status,
    }
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The pump's late wakes merged over the window (each: wall ms, behind
/// µs, re-bootstrapped), and the count since its start at each merge.
struct Wakes {
    seen: BTreeSet<(u64, u64, bool)>,
    /// The count at the window's open.
    from: u64,
    /// The count at the last merge.
    count: u64,
}

impl Wakes {
    fn new(count: u64) -> Self {
        Wakes {
            seen: BTreeSet::new(),
            from: count,
            count,
        }
    }

    /// Merges the pump's `recent` late wakes, `count` since its start.
    fn merge(&mut self, count: u64, recent: &[dstack::stream::wake::LateWake]) {
        self.count = self.count.max(count);
        self.seen.extend(
            recent
                .iter()
                .map(|w| (w.unix_ms, w.behind_us, w.rebootstrap)),
        );
    }

    /// The latest wake timed within `from_ms..=to_ms`, µs.
    fn latest_within(&self, from_ms: u64, to_ms: u64) -> u64 {
        self.seen
            .iter()
            .filter(|w| (from_ms..=to_ms).contains(&w.0))
            .map(|w| w.1)
            .max()
            .unwrap_or(0)
    }

    /// The late wakes since the window opened that no merge saw (about:
    /// two wakes alike in all three fields count as one; wakes merged from
    /// before the window are not counted as seen in it).
    fn unseen(&self) -> u64 {
        let seen = self.seen.len() as u64;
        (self.count - self.from).saturating_sub(seen)
    }
}

/// The run's lateness is the runner's when the pump itself woke later
/// than H inside the window (`window_wake_us`, its own slowest wake): no
/// lead covers a wake past it.
fn runner(window_wake_us: u64, lead_ns: i64) -> bool {
    i64::try_from(window_wake_us).unwrap_or(i64::MAX) * 1_000 > lead_ns
}

fn pct(sorted: &[i64], p: f64) -> i64 {
    let i = ((sorted.len() - 1) as f64 * p).round() as usize;
    sorted[i]
}

/// Every block before its `play_at` and never before its due time, the
/// median within [`MEDIAN_BELOW_H`] under H.
fn judged(what: &str, m: &Measured) {
    let mut s = m.leads.clone();
    s.sort_unstable();
    assert!(
        s.len() >= 1_000,
        "{what}: only {} blocks; status {}",
        s.len(),
        m.status
    );
    let h = m.lead_ns;
    let late = s.iter().filter(|&&l| l <= 0).count();
    let early = s.iter().filter(|&&l| l > h + OVER_H).count();
    // The pump itself woke later than H in the window: no lead covers it.
    let runner = runner(m.window_wake_us, h);
    let line = format!(
        "{what}: H {h} ns; lead (play_at - receipt) over {} blocks, us: min {:.1} p1 {:.1} p10 {:.1} p50 {:.1} p90 {:.1} max {:.1}; late {late} early {early}; restarts {} rebootstraps {}; pump wake max {} -> {} us, in the window {} us ({} late wakes unseen){}",
        s.len(),
        s[0] as f64 / 1e3,
        pct(&s, 0.01) as f64 / 1e3,
        pct(&s, 0.10) as f64 / 1e3,
        pct(&s, 0.50) as f64 / 1e3,
        pct(&s, 0.90) as f64 / 1e3,
        s[s.len() - 1] as f64 / 1e3,
        m.restarts,
        m.rebootstraps,
        m.wake_before_us,
        m.wake_after_us,
        m.window_wake_us,
        m.wakes_unseen,
        if runner && late > 0 { " RUNNER" } else { "" },
    );
    eprintln!("{line}");
    assert_eq!(
        early, 0,
        "a block handed before its due time (a lead over H): {line}"
    );
    let p50 = pct(&s, 0.50);
    assert!(
        (h - MEDIAN_BELOW_H..=h).contains(&p50),
        "the median block is not handed H before its play_at: {line}"
    );
    assert!(
        late == 0 || runner,
        "a block reached the app at or after its play_at: {line}"
    );
}

#[test]
fn every_block_reaches_the_app_h_before_its_play_at() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    judged("on time", &run(None));
}

#[test]
fn a_pump_late_within_h_still_hands_every_block_before_its_play_at() {
    let _one = ONE_DEVICE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    judged("2.5 ms nap every 40th block", &run(Some(NAP)));
}

/// A window's blocks for [`judged`] without a device: `late` blocks at
/// or after their `play_at`, the rest at the median; the pump's
/// since-start max `since_start_us` before and after (a stall before the
/// window), and the window's own latest wake.
fn shaped(late: usize, since_start_us: u64, window_wake_us: u64) -> Measured {
    let lead_ns = 3_906_250;
    let mut leads = vec![lead_ns - 100_000; 1_200 - late];
    leads.extend(std::iter::repeat_n(-1_000, late));
    Measured {
        leads,
        lead_ns,
        restarts: 0,
        rebootstraps: 0,
        wake_before_us: since_start_us,
        wake_after_us: since_start_us,
        window_wake_us,
        wakes_unseen: 0,
        status: String::new(),
    }
}

/// #476 (main CI 45630237191): a stall before the window kept the pump's
/// since-start max at 8 ms; a 5 ms wake inside the window (past H) made
/// the late blocks. The window's own wake excuses them; the old judge
/// (the since-start max rising across the window) failed this run. A
/// window whose own wakes stayed within H still fails on a late block.
#[test]
fn the_judge_reads_the_window_s_own_slowest_wake() {
    judged(
        "a stall past H inside the window",
        &shaped(76, 8_000, 5_000),
    );
    let within_h = std::panic::catch_unwind(|| {
        judged("wakes within H", &shaped(76, 8_000, 2_000));
    });
    assert!(
        within_h.is_err(),
        "a late block with every wake within H passed"
    );
    judged("no late block", &shaped(0, 8_000, 2_000));
    assert!(!runner(3_906, 3_906_250) && runner(3_907, 3_906_250));
}

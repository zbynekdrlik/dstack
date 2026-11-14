// SPDX-License-Identifier: MIT OR Apache-2.0

//! The release gate's soak (dstack #304, the owner's request of
//! 14.11.2026): the whole path an app on its own clock runs, for minutes,
//! with the clocks apart and the pump waking late, so a TX or RX timing
//! regression (dstack #303: v0.6.132 shipped green while a TX bridge
//! re-primed about 0.7 times a second in the field) fails here, before a
//! tag is released.
//!
//! The path is dlink's real one, OBS's shape (stream-snv's settings): a
//! 48 kHz app on a 96 kHz device ([`Rated`], block 128), two TX and two RX
//! channels, driven by dstack's own [`Pump`] on its [`Blocks`] schedule.
//! Inside, the app's [`Process`] fills the TX block from the TX bridge
//! ([`Player`] over the [`Fifo`] the app writes) and hands each RX block to
//! the RX bridge (the [`Fifo`] the app's [`Player`] reads, dlisten's
//! direction), recording the block's [`Block::play_at`] (dstack #295, the
//! stamp OBS plays RX by).
//!
//! - **Simulated time** (10 minutes a case, seconds of wall time): the
//!   media clock is a [`ManualClock`]; the app's clock runs ±6 ppm off it
//!   (both signs); the app writes and reads 1024 frames a tick with ±2 ms
//!   of tick jitter; the pump wakes late on a seeded schedule (most under
//!   1 ms, one in a hundred 2 to 8 ms, all inside W, set to 9 ms here so
//!   8 ms is inside). One case makes one wake late past W: the schedule
//!   starts again with 8 blocks skipped (a rebootstrap, #302's field
//!   event). One case is an irregular writer (dstack #303 design v2): the
//!   app's thread stalls 10 to 25 ms now and then and catches up in a
//!   burst, the real-time run's failure at load ~51 made deterministic.
//! - **Real time** (one minute of wall time): the same path on the pump's
//!   own thread ([`stream::spawn`]) on the box's monotonic clock, the app on
//!   the test's thread at real 1024-frame ticks.
//!
//! **The release gate** is the simulated cases, the tests named `gate_*`
//! (`.github/workflows/release.yml` runs them before a tag's release).
//! The real-time case stays out of it: on a shared runner it measures the
//! runner's load (main's decision on #304).
//!
//! The pass criteria (the design on #304, the main's decisions of 14.11)
//! are hard asserts, never loosened to pass: see [`Soak::check`]. The RX
//! bridge under the field's 2 to 8 ms late wakes is the one criterion not
//! asserted, by name ([`Wakes::Field`], dstack #307).

use std::f64::consts::TAU;
use std::sync::{Arc, Mutex, PoisonError};

use dlink::bridge::{self, AppBuffer, Fifo, Player, Stats};
use dlink::{Block, Config, Map, Process, Rated};
use dstack::clock::{Anchor, ManualClock, PtpClock, to_samples};
use dstack::media::ring::Ring;
use dstack::media::rx::RxShared;
use dstack::media::tx::{TxShared, ns_of};
use dstack::stream::{Blocks, Pump, PumpStats, PumpView, Timing};

/// The app's rate and the device's (stream-snv: OBS at 48 kHz, the site's
/// 96 kHz network).
const APP_RATE: u32 = 48_000;
const DEVICE_RATE: u32 = 96_000;
/// The device's block (dlink's default) and the app block it makes.
const BLOCK: u64 = 128;
const APP_BLOCK: u64 = 64;
/// OBS's audio tick, app frames.
const CHUNK: usize = 1024;
/// The channels each way (OBS's program L/R; the RX pair).
const CHANNELS: usize = 2;
/// W: inside it the 8 ms late wakes the owner named (#304).
const W_NS: u64 = 9_000_000;
/// The media clock's start (1000 s) and the app clock's reading there.
const T0: u64 = 1_000_000_000_000;
const APP0: i64 = 7_000_000_000;
const SECOND: u64 = 1_000_000_000;
const MS: u64 = 1_000_000;
/// A case's simulated time.
const SOAK_NS: u64 = 600 * SECOND;
/// The tail over which the correction must have settled.
const SETTLED_NS: u64 = 300 * SECOND;
/// The app's tick jitter, each side.
const JITTER_NS: f64 = 2e6;
/// The fault: one wake this late (past W, 7.875 blocks: 8 blocks skipped)
/// at this simulated time.
const FAULT_LATE_NS: u64 = 10_500_000;
const FAULT_AT_NS: u64 = 240 * SECOND;
const FAULT_SKIPPED: u64 = 8;
/// After the fault, the time the bridges have to settle again.
const RESETTLE_NS: u64 = 30 * SECOND;
/// A play time's tolerance against the app's clock, ns.
const PLAY_AT_TOL_NS: i64 = 1_000;
/// The irregular writer (dstack #303 design v2): the app's thread stalls
/// this long, uniform, ns, ...
const STALL_MIN_NS: f64 = 10e6;
const STALL_MAX_NS: f64 = 25e6;
/// ... once in this many ns, uniform, and then writes (and reads) the
/// ticks it missed at once, a catch-up burst.
const STALL_EVERY_MIN_NS: f64 = 2e9;
const STALL_EVERY_MAX_NS: f64 = 6e9;
/// A TX gap belongs to a stall up to this long after the stall's end.
const STALL_GAP_GRACE_NS: u64 = 100 * MS;
/// The TX correction's band is judged on the median of this many seconds
/// (the main's decision on #304: a one-second spike is allowed, a ramp is
/// not).
const MEDIAN_SECONDS: usize = 30;

const _: () = assert!(FAULT_LATE_NS > W_NS);
const _: () = assert!(FAULT_AT_NS + RESETTLE_NS + SETTLED_NS <= SOAK_NS);
const _: () = assert!(MEDIAN_SECONDS as u64 * SECOND <= SETTLED_NS);

/// splitmix64: a seeded, reproducible schedule with no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }

    /// Uniform in [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Uniform in [lo, hi).
    fn between(&mut self, lo: f64, hi: f64) -> f64 {
        lo + (hi - lo) * self.unit()
    }

    /// A pump wake's lateness, ns: most under 1 ms; on the field's schedule
    /// one in a hundred 2 to 8 ms (#304).
    fn late_ns(&mut self, wakes: Wakes) -> u64 {
        if wakes == Wakes::Field && self.unit() < 0.01 {
            (2e6 + 6e6 * self.unit()) as u64
        } else {
            (1e6 * self.unit()) as u64
        }
    }
}

/// The pump's wakes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wakes {
    /// The field's (#304): one in a hundred 2 to 8 ms late. The RX bridge's
    /// target does not count the pump's lateness, so at +6 ppm its depth
    /// reaches the floor now and then (14 RX dropouts in 10 min on every
    /// version since v0.6.126). That is dstack #307, an owner latency
    /// decision (`app_buffer` is an owner-fixed term, #247): until it is
    /// decided the RX bridge is NOT judged under these wakes. It is judged,
    /// strictly, under [`Wakes::Short`].
    Field,
    /// Every wake under 1 ms late: inside what the RX bridge's target
    /// covers, so the RX criterion is asserted.
    Short,
}

/// The app's thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Writer {
    /// OBS's ticks with their jitter.
    Ticks,
    /// The same, stalling now and then (`STALL_*`), each stall followed by
    /// a catch-up burst of the ticks it held.
    Stalling,
}

/// One app block as the app saw it: its start (app frames) and play time.
type Mark = (Option<u64>, Option<i64>);

/// The app's processor at its own rate (inside [`Rated`]): OBS's
/// dante-input shape.
struct App {
    /// The TX bridge: the app's own writes, converted to the device's
    /// clock.
    tx: Player,
    /// Interleaved TX frames of one block.
    out: Vec<f64>,
    /// The RX bridge's FIFO (the app's own player reads it).
    rx: Arc<Fifo>,
    /// Each block's start and play time, for the test.
    marks: Arc<Mutex<Vec<Mark>>>,
}

impl Process for App {
    fn process(&mut self, block: &mut Block<'_>) {
        let frames = block.frames();
        let out = &mut self.out[..frames * CHANNELS];
        self.tx.fill_with(out, CHANNELS, |x| x);
        for c in 0..CHANNELS {
            for (f, y) in block.output(c).iter_mut().enumerate() {
                *y = out[f * CHANNELS + c];
            }
        }
        let input = [block.input(0), block.input(1)];
        self.rx.push_with(frames, |f, c| input[c][f]);
        self.marks
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push((block.start(), block.play_at()));
    }
}

/// The device's config: stream-snv's shape (#303).
fn config(dir: &std::path::Path) -> Config {
    Config::new("soak", dir.join("state.toml"))
        .rx(["In L", "In R"])
        .tx(["Out L", "Out R"])
        .sample_rate(APP_RATE)
        .network_rate(DEVICE_RATE)
        .work_margin_ns(W_NS)
        .flow_slots(1)
        .app_buffer(bridge::chunk_ns(CHUNK as u64, APP_RATE), None)
}

/// The path, built as dlink builds it: the pump, and the app's ends of
/// its bridges.
struct Path {
    pump: Pump<Rated<App>>,
    ends: Ends,
}

/// The app's ends of the bridges and the counts the test reads.
struct Ends {
    pump_stats: Arc<PumpStats>,
    timing: Timing,
    /// The app's TX writes.
    tx_fifo: Arc<Fifo>,
    tx_stats: Arc<Stats>,
    /// The app's RX reads.
    rx_player: Player,
    rx_stats: Arc<Stats>,
    marks: Arc<Mutex<Vec<Mark>>>,
    _dir: tempfile::TempDir,
}

fn path() -> Path {
    let dir = tempfile::tempdir().unwrap();
    let config = config(dir.path());
    let cfg = config.device().expect("the soak's config");
    let buffer: AppBuffer = config.bridge_buffer().expect("an app buffer");
    let timing = Timing::of(&cfg);
    assert_eq!(timing.block, BLOCK);
    assert_eq!(timing.margin, to_samples(W_NS, DEVICE_RATE));

    let tx_fifo = Arc::new(bridge::fifo_for(CHANNELS, APP_RATE));
    let tx_stats = Arc::new(Stats::default());
    let tx = Player::new(
        Arc::clone(&tx_fifo),
        APP_RATE,
        APP_RATE,
        buffer,
        Arc::clone(&tx_stats),
    )
    .unwrap();
    let rx_fifo = Arc::new(bridge::fifo_for(CHANNELS, APP_RATE));
    let rx_stats = Arc::new(Stats::default());
    let rx_player = Player::new(
        Arc::clone(&rx_fifo),
        APP_RATE,
        APP_RATE,
        buffer,
        Arc::clone(&rx_stats),
    )
    .unwrap();
    let marks = Arc::new(Mutex::new(Vec::new()));
    let app = App {
        tx,
        out: vec![0.0; 4 * CHUNK * CHANNELS],
        rx: rx_fifo,
        marks: Arc::clone(&marks),
    };
    let rated = Rated::new(
        app,
        DEVICE_RATE,
        APP_RATE,
        BLOCK as usize,
        CHANNELS,
        CHANNELS,
    )
    .unwrap();

    let rings: Arc<[Ring]> = (0..cfg.rx.len()).map(|_| Ring::new()).collect();
    let rx = Arc::new(
        RxShared::with_slots(
            DEVICE_RATE,
            std::net::Ipv4Addr::LOCALHOST,
            Arc::clone(&rings),
            None,
            1,
        )
        .unwrap(),
    );
    let tx_shared = Arc::new(
        TxShared::new(
            cfg.clone(),
            std::net::Ipv4Addr::LOCALHOST,
            rings,
            1,
            &[0, 1],
        )
        .unwrap(),
    );
    let pump_stats = Arc::new(PumpStats::new(timing, DEVICE_RATE));
    let pump = Pump::new(
        rated,
        Map {
            rx: vec![0, 1],
            tx: vec![0, 1],
        },
        timing,
        24,
        rx,
        tx_shared,
        Arc::clone(&pump_stats),
    );
    Path {
        pump,
        ends: Ends {
            pump_stats,
            timing,
            tx_fifo,
            tx_stats,
            rx_player,
            rx_stats,
            marks,
            _dir: dir,
        },
    }
}

/// The app's tick: `CHUNK` frames of a 1 kHz tone written to TX, `CHUNK`
/// frames read from RX.
struct Ticker {
    written: u64,
    rx_out: Vec<f64>,
}

impl Ticker {
    fn new() -> Self {
        Ticker {
            written: 0,
            rx_out: vec![0.0; CHUNK * CHANNELS],
        }
    }

    fn tick(&mut self, p: &mut Ends) {
        let at = self.written;
        p.tx_fifo.push_with(CHUNK, |f, _| {
            0.25 * (TAU * 1000.0 * (at + f as u64) as f64 / f64::from(APP_RATE)).sin()
        });
        self.written += CHUNK as u64;
        p.rx_player.fill_with(&mut self.rx_out, CHANNELS, |x| x);
    }
}

/// The bridges' counts at one moment.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Counts {
    at_ns: u64,
    tx_underruns: u64,
    tx_recentres: u64,
    rx_underruns: u64,
    rx_recentres: u64,
    tx_ppm: f64,
    rx_ppm: f64,
}

impl Counts {
    fn of(p: &Ends, at_ns: u64) -> Self {
        Counts {
            at_ns,
            tx_underruns: p.tx_stats.underruns(),
            tx_recentres: p.tx_stats.recentres(),
            rx_underruns: p.rx_stats.underruns(),
            rx_recentres: p.rx_stats.recentres(),
            tx_ppm: p.tx_stats.ppm(),
            rx_ppm: p.rx_stats.ppm(),
        }
    }

    /// The TX side's gap counters (dropouts and re-centres; a floor deficit
    /// counts in both, so only a change of the sum is read, as one gap).
    fn tx_sum(&self) -> u64 {
        self.tx_underruns + self.tx_recentres
    }

    /// The RX side's.
    fn rx_sum(&self) -> u64 {
        self.rx_underruns + self.rx_recentres
    }
}

/// The gaps in `at` (media times) within [from, to].
fn gaps_within(at: &[u64], from: u64, to: u64) -> usize {
    at.iter().filter(|&&t| t >= from && t <= to).count()
}

/// The median of `values` (not empty).
fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    let n = values.len();
    if n % 2 == 1 {
        values[n / 2]
    } else {
        f64::midpoint(values[n / 2 - 1], values[n / 2])
    }
}

/// One simulated case.
struct Soak {
    drift_ppm: f64,
    fault: bool,
    wakes: Wakes,
    writer: Writer,
    /// The irregular writer's schedule: the next stall (start, length) on
    /// the app's clock, ns, and every stall drawn, on the media clock.
    stall_rng: Rng,
    next_stall: (f64, f64),
    stalls: Vec<(u64, u64)>,
    /// The media times of each side's gaps (a dropout or a re-centre, one
    /// a step of the simulation) after priming.
    tx_gap_at: Vec<u64>,
    rx_gap_at: Vec<u64>,
    /// The counts once both bridges ran (priming done), and each second's.
    primed: Option<Counts>,
    seconds: Vec<Counts>,
    /// The counts just before the fault's wake.
    before_fault: Option<Counts>,
    /// The play-time checks: the first block's offset from the app's clock,
    /// the largest deviation from it, the largest step error, the starts
    /// that jumped.
    offset0: Option<i64>,
    offset_err_max: i64,
    step_err_max: i64,
    last_mark: Option<(u64, i64)>,
    jumps: Vec<u64>,
    marks: u64,
    view: PumpView,
}

impl Soak {
    fn new(drift_ppm: f64, fault: bool, wakes: Wakes, writer: Writer) -> Self {
        let mut stall_rng = Rng(0x0303_57a1 ^ drift_ppm.to_bits());
        let next_stall = (
            APP0 as f64 + stall_rng.between(STALL_EVERY_MIN_NS, STALL_EVERY_MAX_NS),
            stall_rng.between(STALL_MIN_NS, STALL_MAX_NS),
        );
        let mut soak = Soak {
            drift_ppm,
            fault,
            wakes,
            writer,
            stall_rng,
            next_stall,
            stalls: Vec::new(),
            tx_gap_at: Vec::new(),
            rx_gap_at: Vec::new(),
            primed: None,
            seconds: Vec::new(),
            before_fault: None,
            offset0: None,
            offset_err_max: 0,
            step_err_max: 0,
            last_mark: None,
            jumps: Vec::new(),
            marks: 0,
            view: PumpView::default(),
        };
        soak.record_stall();
        soak
    }

    fn label(&self) -> String {
        format!(
            "drift {:+} ppm, fault {}, wakes {:?}, writer {:?}",
            self.drift_ppm, self.fault, self.wakes, self.writer
        )
    }

    /// The app's clock at media time `media_ns`.
    fn app_ns(&self, media_ns: u64) -> i64 {
        // A block's media start lies before the start (P + B before its
        // due time): signed.
        let since = media_ns as f64 - T0 as f64;
        APP0 + (since * (1.0 + self.drift_ppm * 1e-6)).round() as i64
    }

    /// The media time of app clock reading `app_ns`.
    fn media_ns(&self, app_ns: f64) -> u64 {
        let since = ((app_ns - APP0 as f64) / (1.0 + self.drift_ppm * 1e-6)).round() as i64;
        T0.saturating_add_signed(since)
    }

    /// Keeps the stall just drawn, on the media clock (a writer that never
    /// stalls keeps none).
    fn record_stall(&mut self) {
        if self.writer == Writer::Stalling {
            let (start, len) = self.next_stall;
            let stall = (self.media_ns(start), self.media_ns(start + len));
            self.stalls.push(stall);
        }
    }

    /// The app thread's time for a tick due at `app_ns`: a tick inside a
    /// stall runs at the stall's end (with the others it held: a burst).
    fn stalled(&mut self, app_ns: f64) -> f64 {
        if self.writer == Writer::Ticks {
            return app_ns;
        }
        loop {
            let (start, len) = self.next_stall;
            if app_ns < start {
                return app_ns;
            }
            if app_ns < start + len {
                return start + len;
            }
            self.next_stall = (
                start
                    + self
                        .stall_rng
                        .between(STALL_EVERY_MIN_NS, STALL_EVERY_MAX_NS),
                self.stall_rng.between(STALL_MIN_NS, STALL_MAX_NS),
            );
            self.record_stall();
        }
    }

    /// The app's tick `k` in media time: nominal on the app's clock, plus
    /// its jitter, held by a stall.
    fn tick_at(&mut self, k: u64, rng: &mut Rng) -> u64 {
        let nominal = APP0 as f64 + k as f64 * CHUNK as f64 * 1e9 / f64::from(APP_RATE);
        let app = self.stalled(nominal + JITTER_NS * (2.0 * rng.unit() - 1.0));
        self.media_ns(app)
    }

    /// Runs the case; the checks are [`Soak::check`]'s.
    fn run(mut self, seed: u64) -> Self {
        let Path {
            mut pump,
            ends: mut p,
        } = path();
        let mut rng = Rng(seed);
        let mut ticker = Ticker::new();
        let clock = ManualClock::new(T0);
        let end = T0 + SOAK_NS;
        let mut wake_at = T0;
        let mut tick = 0;
        let mut tick_at = self.tick_at(tick, &mut rng);
        let mut second = T0 + SECOND;
        let mut faulted = !self.fault;
        let mut gaps = (0, 0);
        while wake_at.min(tick_at) < end {
            let at = wake_at.min(tick_at);
            if wake_at <= tick_at {
                clock.sleep_until(wake_at);
                let now = clock.now_ns().expect("a ready clock");
                let app = self.app_ns(now);
                let now = pump.read_clocks(&clock, || app).expect("a ready clock");
                let wake = pump.media(to_samples(now, DEVICE_RATE), Anchor::of(&clock));
                self.take_marks(&p);
                let due = ns_of(wake, DEVICE_RATE);
                wake_at = if !faulted && due >= T0 + FAULT_AT_NS {
                    faulted = true;
                    self.before_fault = Some(Counts::of(&p, due));
                    due + FAULT_LATE_NS
                } else {
                    due + rng.late_ns(self.wakes)
                };
            } else {
                ticker.tick(&mut p);
                tick += 1;
                tick_at = self.tick_at(tick, &mut rng);
            }
            let now = wake_at.min(tick_at);
            let counts = Counts::of(&p, at);
            if self.primed.is_none() && !p.tx_stats.priming() && !p.rx_stats.priming() {
                self.primed = Some(Counts::of(&p, now));
                gaps = (counts.tx_sum(), counts.rx_sum());
            }
            if self.primed.is_some() {
                if counts.tx_sum() != gaps.0 {
                    self.tx_gap_at.push(at);
                }
                if counts.rx_sum() != gaps.1 {
                    self.rx_gap_at.push(at);
                }
                gaps = (counts.tx_sum(), counts.rx_sum());
            }
            while now >= second {
                self.seconds.push(Counts::of(&p, second));
                second += SECOND;
            }
        }
        self.view = p.pump_stats.view();
        self
    }

    /// Checks each block's play time as it comes: on the app's clock at the
    /// block's media due time plus a constant (the device's RX path and the
    /// converter's delay), and each step one block (or the blocks a
    /// rebootstrap skipped) of the app's clock.
    fn take_marks(&mut self, p: &Ends) {
        let marks = std::mem::take(&mut *p.marks.lock().unwrap_or_else(PoisonError::into_inner));
        for (start, play_at) in marks {
            let start = start.expect("a media block has a start");
            let play_at = play_at.expect("a media block has a play time");
            self.marks += 1;
            // The app block's first sample on the media clock.
            let media = ns_of(start, APP_RATE);
            let offset = play_at - self.app_ns(media);
            let offset0 = *self.offset0.get_or_insert(offset);
            self.offset_err_max = self.offset_err_max.max((offset - offset0).abs());
            if let Some((last_start, last_at)) = self.last_mark {
                let frames = start - last_start;
                if frames != APP_BLOCK {
                    self.jumps.push(frames);
                }
                let want = self.app_ns(T0 + ns_of(frames, APP_RATE)) - APP0;
                self.step_err_max = self.step_err_max.max((play_at - last_at - want).abs());
            }
            self.last_mark = Some((start, play_at));
        }
    }

    /// The TX correction's median over the [`MEDIAN_SECONDS`] up to each
    /// second of the settled tail, with the second's time.
    fn tx_ppm_medians(&self) -> Vec<(u64, f64)> {
        let tail = T0 + SOAK_NS - SETTLED_NS;
        (MEDIAN_SECONDS..=self.seconds.len())
            .map(|end| &self.seconds[end - MEDIAN_SECONDS..end])
            .filter(|w| w[w.len() - 1].at_ns >= tail)
            .map(|w| {
                let mut ppm: Vec<f64> = w.iter().map(|c| c.tx_ppm).collect();
                (w[w.len() - 1].at_ns, median(&mut ppm))
            })
            .collect()
    }

    /// The pass criteria (#304's design and the main's decisions on it),
    /// hard:
    /// - after priming: no TX dropout or re-centre; an irregular writer's
    ///   gaps only at its stalls (one a stall at most: a stall past the
    ///   FIFO's depth is a gap no ratio can mend), none away from them;
    /// - no RX dropout, judged under [`Wakes::Short`] (the RX bridge under
    ///   the field's late wakes is dstack #307's, not judged here);
    /// - the TX correction's 30 s median within ±(drift + 2) ppm over the
    ///   last 5 min (a one-second spike passes, a ramp fails);
    /// - every play time on the app's clock within 1 µs of a constant
    ///   offset, every step one block within 1 µs (none off by 1 ms);
    /// - the fault case: one rebootstrap of 8 blocks, at most one gap per
    ///   side at it, then the same criteria from 30 s after it.
    fn check(&self) {
        let label = self.label();
        let rx_judged = self.wakes == Wakes::Short;
        let primed = self
            .primed
            .unwrap_or_else(|| panic!("{label}: never primed"));
        let last = *self.seconds.last().expect("seconds");
        let medians = self.tx_ppm_medians();
        let (lo, hi) = medians
            .iter()
            .fold((f64::MAX, f64::MIN), |(lo, hi), &(_, m)| {
                (lo.min(m), hi.max(m))
            });
        println!(
            "{label}: TX gaps {} ({} stalls), RX gaps {}, TX ppm 30 s median \
             {lo:.2} .. {hi:.2} over the last 5 min, last {last:?}",
            self.tx_gap_at.len(),
            self.stalls.len(),
            self.rx_gap_at.len(),
        );
        assert!(
            primed.at_ns - T0 < 2 * SECOND,
            "{label}: priming took {} ms",
            (primed.at_ns - T0) / MS
        );
        assert!(self.marks > 400_000, "{label}: {} blocks", self.marks);
        assert!(
            self.offset_err_max <= PLAY_AT_TOL_NS,
            "{label}: a play time {} ns off the app's clock",
            self.offset_err_max
        );
        assert!(
            self.step_err_max <= PLAY_AT_TOL_NS,
            "{label}: a play time step {} ns off its blocks",
            self.step_err_max
        );
        // The criteria hold from `from` (priming, or 30 s after the fault).
        let from = if self.fault {
            let v = self.view;
            assert_eq!(
                (v.rebootstraps, v.rebootstraps_late, v.missed),
                (1, 1, FAULT_SKIPPED),
                "{label}: {v:?}"
            );
            // The app's blocks skipped: the 8 device blocks' and the late one's.
            assert_eq!(
                self.jumps,
                [(FAULT_SKIPPED + 1) * APP_BLOCK],
                "{label}: the starts that jumped"
            );
            let before = self.before_fault.expect("the fault ran");
            let settle = self.at(before.at_ns + RESETTLE_NS);
            assert!(
                gaps_within(&self.tx_gap_at, before.at_ns, settle.at_ns) <= 1,
                "{label}: more than one TX gap at the fault: {:?}",
                self.gap_seconds(&self.tx_gap_at)
            );
            assert_eq!(
                gaps_within(&self.tx_gap_at, primed.at_ns, before.at_ns),
                0,
                "{label}: TX gaps before the fault: {:?}",
                self.gap_seconds(&self.tx_gap_at)
            );
            if rx_judged {
                assert!(
                    gaps_within(&self.rx_gap_at, before.at_ns, settle.at_ns) <= 1,
                    "{label}: more than one RX gap at the fault: {:?}",
                    self.gap_seconds(&self.rx_gap_at)
                );
                assert_eq!(
                    gaps_within(&self.rx_gap_at, primed.at_ns, before.at_ns),
                    0,
                    "{label}: RX gaps before the fault: {:?}",
                    self.gap_seconds(&self.rx_gap_at)
                );
            }
            settle
        } else {
            let v = self.view;
            assert_eq!((v.rebootstraps, v.missed), (0, 0), "{label}: {v:?}");
            assert!(self.jumps.is_empty(), "{label}: {:?}", self.jumps);
            primed
        };
        match self.writer {
            Writer::Ticks => assert_eq!(
                (
                    last.tx_underruns - from.tx_underruns,
                    last.tx_recentres - from.tx_recentres,
                ),
                (0, 0),
                "{label}: TX underruns, TX re-centres from {} s: {from:?} {last:?}",
                (from.at_ns - T0) / SECOND
            ),
            Writer::Stalling => self.check_stall_gaps(&label),
        }
        if rx_judged {
            assert_eq!(
                last.rx_underruns - from.rx_underruns,
                0,
                "{label}: RX underruns from {} s: {from:?} {last:?}",
                (from.at_ns - T0) / SECOND
            );
        }
        let band = self.drift_ppm.abs() + 2.0;
        assert!(!medians.is_empty(), "{label}: no settled seconds");
        for &(at, m) in &medians {
            assert!(
                m.abs() <= band,
                "{label}: tx_ppm's 30 s median {m:.2} at {} s, outside ±{band} ppm",
                (at - T0) / SECOND
            );
        }
    }

    /// An irregular writer's TX gaps: each inside a stall (or just after
    /// it), one a stall at most.
    fn check_stall_gaps(&self, label: &str) {
        let mut per_stall = vec![0u32; self.stalls.len()];
        for &at in &self.tx_gap_at {
            let stall = self
                .stalls
                .iter()
                .position(|&(start, end)| at >= start && at <= end + STALL_GAP_GRACE_NS)
                .unwrap_or_else(|| {
                    panic!(
                        "{label}: a TX gap at {:.3} s, away from every stall (gaps at {:?} s)",
                        (at - T0) as f64 / 1e9,
                        self.gap_seconds(&self.tx_gap_at)
                    )
                });
            per_stall[stall] += 1;
            assert!(
                per_stall[stall] <= 1,
                "{label}: more than one TX gap at the stall {:?} (gaps at {:?} s)",
                self.stalls[stall],
                self.gap_seconds(&self.tx_gap_at)
            );
        }
    }

    fn gap_seconds(&self, at: &[u64]) -> Vec<f64> {
        at.iter()
            .map(|&at| ((at - T0) as f64 / 1e6).round() / 1e3)
            .collect()
    }

    /// The counts of the first second at or after `at_ns`.
    fn at(&self, at_ns: u64) -> Counts {
        *self
            .seconds
            .iter()
            .find(|c| c.at_ns >= at_ns)
            .expect("a second after it")
    }
}

#[test]
fn gate_ten_minutes_with_the_app_clock_6_ppm_fast() {
    Soak::new(6.0, false, Wakes::Field, Writer::Ticks)
        .run(0x304_0001)
        .check();
}

#[test]
fn gate_ten_minutes_with_the_app_clock_6_ppm_slow() {
    Soak::new(-6.0, false, Wakes::Field, Writer::Ticks)
        .run(0x304_0002)
        .check();
}

#[test]
fn gate_ten_minutes_with_a_rebootstrap_app_fast() {
    Soak::new(6.0, true, Wakes::Field, Writer::Ticks)
        .run(0x304_0003)
        .check();
}

#[test]
fn gate_ten_minutes_with_a_rebootstrap_app_slow() {
    Soak::new(-6.0, true, Wakes::Field, Writer::Ticks)
        .run(0x304_0004)
        .check();
}

/// The irregular writer (dstack #303 design v2): the app's thread stalls
/// 10 to 25 ms every 2 to 6 s and catches up in a burst.
#[test]
fn gate_ten_minutes_of_an_irregular_writer_app_fast() {
    Soak::new(6.0, false, Wakes::Field, Writer::Stalling)
        .run(0x304_0005)
        .check();
}

#[test]
fn gate_ten_minutes_of_an_irregular_writer_app_slow() {
    Soak::new(-6.0, false, Wakes::Field, Writer::Stalling)
        .run(0x304_0006)
        .check();
}

/// The RX bridge judged strictly, with a rebootstrap, under wakes its
/// target covers (the field's 2 to 8 ms wakes are dstack #307's).
#[test]
fn gate_ten_minutes_of_the_rx_bridge_with_short_wakes_and_a_rebootstrap() {
    Soak::new(6.0, true, Wakes::Short, Writer::Ticks)
        .run(0x304_0007)
        .check();
}

/// The schedule's first block at the start (no catch-up) is what the
/// simulation's first wake runs: a guard that the harness drives the
/// pump the way its thread does.
#[test]
fn the_harness_s_first_wake_runs_the_first_block() {
    let p = path();
    let mut blocks = Blocks::bootstrap(to_samples(T0, DEVICE_RATE), p.ends.timing);
    let due = blocks.due();
    assert!(ns_of(due, DEVICE_RATE) >= T0);
    assert!(matches!(
        blocks.next(due),
        dstack::stream::Due::Run { late: false, .. }
    ));
}

/// The harness's irregular writer: ticks inside a stall run at its end, in
/// order, and the stalls drawn have their lengths and spacing.
#[test]
fn the_harness_s_stalls_hold_ticks_to_their_end() {
    let mut soak = Soak::new(0.0, false, Wakes::Field, Writer::Stalling);
    let mut rng = Rng(1);
    let mut last = 0;
    let mut held = 0;
    for k in 0..30_000 {
        let at = soak.tick_at(k, &mut rng);
        assert!(at >= last, "tick {k} before the one before it");
        if soak.stalls.iter().any(|&(_, end)| end == at) {
            held += 1;
        }
        last = at;
    }
    assert!(held > 20, "{held} ticks held by a stall");
    for w in soak.stalls.windows(2) {
        let (start, end) = w[0];
        assert!((10 * MS..=25 * MS).contains(&(end - start)), "{w:?}");
        assert!(
            (2 * SECOND..=6 * SECOND).contains(&(w[1].0 - start)),
            "{w:?}"
        );
    }
}

/// The band's median: a one-second spike leaves it, a ramp does not.
#[test]
fn the_median_ignores_a_spike_and_follows_a_ramp() {
    let mut spike = [6.0, 6.0, 12.8, 6.0, 6.0];
    assert_eq!(median(&mut spike), 6.0);
    let mut ramp = [6.0, -20.0, -40.0, -60.0];
    assert_eq!(median(&mut ramp), -30.0);
}

/// One minute of real time: the pump on its own thread on the box's
/// monotonic clock, the app on this thread at real ticks. Real wakes are
/// what the simulation cannot model; zero TX dropouts after priming, and
/// an RX gap only where the pump itself restarted (a runner's late thread).
/// Not in the release gate (no `gate_` name): on a shared runner it
/// measures the runner's load (the main's decision on #304).
#[cfg(any(target_os = "linux", windows))]
#[test]
fn one_minute_in_real_time() {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::{Duration, Instant};

    use dstack::clock::MonoClock;

    const REAL: Duration = Duration::from_secs(60);
    let Path { pump, ends: mut p } = path();
    let clock = Arc::new(MonoClock::new());
    clock.set_k(0);
    let stop = Arc::new(AtomicBool::new(false));
    let thread = dstack::stream::spawn(
        Arc::clone(&clock) as Arc<dyn PtpClock>,
        pump,
        Arc::clone(&stop),
    )
    .unwrap();
    let period = Duration::from_secs_f64(CHUNK as f64 / f64::from(APP_RATE));
    let mut ticker = Ticker::new();
    let start = Instant::now();
    let mut deadline = start;
    let mut primed: Option<(Counts, PumpView)> = None;
    while start.elapsed() < REAL {
        ticker.tick(&mut p);
        if primed.is_none() && !p.tx_stats.priming() && !p.rx_stats.priming() {
            primed = Some((Counts::of(&p, T0), p.pump_stats.view()));
        }
        deadline += period;
        std::thread::sleep(deadline.saturating_duration_since(Instant::now()));
    }
    stop.store(true, Ordering::SeqCst);
    let ended = Instant::now() + Duration::from_secs(5);
    while !thread.is_finished() {
        assert!(Instant::now() < ended, "the pump thread did not end");
        std::thread::sleep(Duration::from_millis(1));
    }
    thread.join().unwrap();
    let (primed, view0) = primed.expect("both bridges primed");
    let last = Counts::of(&p, T0);
    let view = p.pump_stats.view();
    let restarts = view.rebootstraps - view0.rebootstraps;
    assert_eq!(
        last.tx_underruns - primed.tx_underruns,
        0,
        "TX dropouts after priming: {primed:?} {last:?} {view:?}"
    );
    assert!(
        last.rx_underruns - primed.rx_underruns <= restarts,
        "RX dropouts past the pump's own restarts ({restarts}): {primed:?} {last:?} {view:?}"
    );
    assert!(view.blocks > 40_000, "{view:?}");
}

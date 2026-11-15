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
//! - **Real time** (one minute of wall time): `tests/soak_real_time.rs`,
//!   its own test binary, so these cases never load the box under it.
//!
//! The path itself is `soak/harness.rs`, shared by both.
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

#[path = "soak/harness.rs"]
mod harness;

use std::sync::PoisonError;

use dstack::clock::{Anchor, ManualClock, PtpClock, to_samples};
use dstack::media::tx::ns_of;
use dstack::stream::{Blocks, PumpView};

use harness::*;

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
/// OBS's audio pass with an output before the TX tap (dstack #303, field
/// 15.11.2026 on stream-snv: obs v0.4.39, dlink v0.6.137): OBS calls a
/// mix's outputs in turn on its audio thread, and an NDI output's audio
/// call took up to 10.1 ms (OBS's `out_cb_max_ms`; the pass up to 10.3 ms)
/// while the thread's own ticks stayed regular (gap at most 21.4 ms). The
/// TX write lands that late after its tick: this many ns usually, ...
const PASS_MIN_NS: f64 = 0.1e6;
const PASS_MAX_NS: f64 = 1e6;
/// ... and one tick in this many as late as the field's slow output call
/// (about 0.8 TX gaps a second at 46.9 ticks a second).
const PASS_SLOW_ONE_IN: f64 = 40.0;
const PASS_SLOW_MIN_NS: f64 = 7.5e6;
const PASS_SLOW_MAX_NS: f64 = 10.3e6;
/// The field's tick jitter under that pass (OBS's `wake_late_max_ms`).
const PASS_JITTER_NS: f64 = 0.4e6;
/// OBS's ticks on an idle box (obs's soak gate on dlink v0.6.140, CI run
/// 43733406564, 15.11.2026: `tx_push_gap_max_us` 21 342 to 21 424 in every
/// 10 s status but one): each side, ns. A writer this regular keeps its
/// write instants at one phase of the pump's 64-frame reads for minutes.
const STEADY_JITTER_NS: f64 = 0.05e6;
/// The TX correction's band is judged on the median of this many seconds
/// (the main's decision on #304: a one-second spike is allowed, a ramp is
/// not).
const MEDIAN_SECONDS: usize = 30;
/// An irregular writer's band over the drift, ppm (the main's decision on
/// #303, 14.11): at OBS's 22.67 ms target a stall over about 12 ms empties
/// the FIFO whatever the player does, and each such gap re-primes; the
/// level loop then drains what the re-prime leaves within its 20 ppm bound
/// around the rate (`bridge::player::LEVEL_BOUND_PPM`). A ramp past that
/// (today's -50 .. +47 ppm before #303) still fails.
const STALL_BAND_PPM: f64 = 20.0;
/// A TX tone break belongs to a counted event (an underrun, a re-centre or
/// a resync) at most this long before it: the event's silence or skip
/// reaches the output within the converter's delay and the app's block
/// (dstack #303).
const TONE_GRACE_NS: u64 = 20 * MS;

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
    /// drops out now and then (14 RX dropouts in 10 min on every version
    /// since v0.6.126, then under the floor; the floor is gone since
    /// v0.6.140, #303). That is dstack #307, an owner latency
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
    /// Regular ticks whose write lands late in OBS's audio pass (`PASS_*`):
    /// another output's call before the TX tap, up to 10.3 ms now and then.
    /// The FIFO never runs dry (the sawtooth's bottom is about 12 ms), so
    /// no gap is allowed.
    LatePass,
    /// Regular ticks with almost no jitter (`STEADY_JITTER_NS`): OBS's
    /// audio thread on an idle box (obs's soak gate on dlink v0.6.140,
    /// 15.11.2026: push gaps 21.34 to 21.42 ms). The FIFO never runs dry,
    /// so no gap is allowed.
    Steady,
}

/// The gaps in `at` (media times) within [from, to].
fn gaps_within(at: &[u64], from: u64, to: u64) -> usize {
    at.iter().filter(|&&t| t >= from && t <= to).count()
}

/// The breaks in `breaks` (media times, in order) that no counted event in
/// `counted` (media times, in order) accounts for: each break takes the
/// earliest event not yet taken within `grace` before it (or at it), one
/// event a break.
fn unaccounted(breaks: &[u64], counted: &[u64], grace: u64) -> Vec<u64> {
    let mut next = 0;
    let mut left = Vec::new();
    for &at in breaks {
        while next < counted.len() && counted[next] + grace < at {
            next += 1;
        }
        if next < counted.len() && counted[next] <= at {
            next += 1;
        } else {
            left.push(at);
        }
    }
    left
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
    /// From the TX bridge's first primed step on (settle time included,
    /// dstack #303): the media times of each TX tone break and of each
    /// counted TX event (an underrun, a re-centre or a resync), one entry
    /// a count.
    tx_primed: Option<Counts>,
    tone_break_at: Vec<u64>,
    tx_counted_at: Vec<u64>,
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
            tx_primed: None,
            tone_break_at: Vec::new(),
            tx_counted_at: Vec::new(),
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
        if self.writer == Writer::LatePass {
            let jitter = PASS_JITTER_NS * (2.0 * rng.unit() - 1.0);
            let pass = if rng.unit() < 1.0 / PASS_SLOW_ONE_IN {
                rng.between(PASS_SLOW_MIN_NS, PASS_SLOW_MAX_NS)
            } else {
                rng.between(PASS_MIN_NS, PASS_MAX_NS)
            };
            return self.media_ns(nominal + jitter + pass);
        }
        if self.writer == Writer::Steady {
            let jitter = STEADY_JITTER_NS * (2.0 * rng.unit() - 1.0);
            return self.media_ns(nominal + jitter);
        }
        let app = self.stalled(nominal + JITTER_NS * (2.0 * rng.unit() - 1.0));
        self.media_ns(app)
    }

    /// Runs the case; the checks are [`Soak::check`]'s.
    fn run(mut self, seed: u64) -> Self {
        let Path {
            mut pump,
            ends: mut p,
        } = simulated_path();
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
        let mut tx_last: Option<Counts> = None;
        while wake_at.min(tick_at) < end {
            let at = wake_at.min(tick_at);
            if wake_at <= tick_at {
                clock.sleep_until(wake_at);
                let now = clock.now_ns().expect("a ready clock");
                let app = self.app_ns(now);
                p.set_app_ns(app);
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
                p.set_app_ns(self.app_ns(tick_at));
                ticker.tick(&mut p);
                tick += 1;
                tick_at = self.tick_at(tick, &mut rng);
            }
            let now = wake_at.min(tick_at);
            let counts = Counts::of(&p, at);
            if self.tx_primed.is_none() && !p.tx_stats.priming() {
                self.tx_primed = Some(counts);
            }
            if let Some(last) = tx_last {
                let breaks = counts.tx_tone_gaps - last.tx_tone_gaps;
                let counted = counts.tx_events() - last.tx_events();
                self.tone_break_at
                    .extend(std::iter::repeat_n(at, breaks as usize));
                self.tx_counted_at
                    .extend(std::iter::repeat_n(at, counted as usize));
            }
            if self.tx_primed.is_some() {
                tx_last = Some(counts);
            }
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
    /// - from the TX bridge's first primed step (settle time and a fault's
    ///   30 s included): every TX tone break accounted for by its own
    ///   counted event (an underrun, a re-centre or a resync) at most
    ///   [`TONE_GRACE_NS`] before it, so with none counted, no break at all;
    /// - after priming: no TX dropout or re-centre; an irregular writer's
    ///   gaps only at its stalls (one a stall at most: a stall past the
    ///   FIFO's depth is a gap no ratio can mend), none away from them;
    /// - no RX dropout, judged under [`Wakes::Short`] (the RX bridge under
    ///   the field's late wakes is dstack #307's, not judged here);
    /// - the TX correction's 30 s median within ±(drift + 2) ppm over the
    ///   last 5 min (a one-second spike passes, a ramp fails); for an
    ///   irregular writer ±(drift + [`STALL_BAND_PPM`]);
    /// - every play time on the app's clock within 1 µs of a constant
    ///   offset, every step one block within 1 µs (none off by 1 ms);
    /// - the fault case: one rebootstrap of 8 blocks, at most one gap per
    ///   side at it, one TX resync (dstack #303), then the same criteria
    ///   from 30 s after it; every other case no resync.
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
            "{label}: TX gaps {} ({} stalls), TX tone breaks {} (events {}), RX gaps {}, \
             TX ppm 30 s median {lo:.2} .. {hi:.2} over the last 5 min, last {last:?}",
            self.tx_gap_at.len(),
            self.stalls.len(),
            self.tone_break_at.len(),
            self.tx_counted_at.len(),
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
        assert_eq!(
            last.tx_resyncs,
            u64::from(self.fault),
            "{label}: TX resyncs (one at the fault's skip, none else)"
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
        // Every discontinuity of the TX output is counted (dstack #303,
        // obs's v0.6.140 soak: its peer heard 2 tone gaps while dante-input
        // counted none), each by its own event (an underrun, a re-centre or
        // a resync) just before it, from the TX bridge's first primed step
        // on: the settle time and a fault's 30 s too, never only the judged
        // window (obs's 24 ms break 14 s into a stream, before lock).
        let tx_primed = self
            .tx_primed
            .unwrap_or_else(|| panic!("{label}: TX never primed"));
        let uncounted = unaccounted(&self.tone_break_at, &self.tx_counted_at, TONE_GRACE_NS);
        assert!(
            uncounted.is_empty(),
            "{label}: TX tone breaks no counted event accounts for at {:?} s \
             (breaks at {:?} s, events at {:?} s): {tx_primed:?} {last:?}",
            self.gap_seconds(&uncounted),
            self.gap_seconds(&self.tone_break_at),
            self.gap_seconds(&self.tx_counted_at),
        );
        match self.writer {
            Writer::Ticks | Writer::LatePass | Writer::Steady => assert_eq!(
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
        let band = self.drift_ppm.abs()
            + match self.writer {
                Writer::Ticks | Writer::LatePass | Writer::Steady => 2.0,
                Writer::Stalling => STALL_BAND_PPM,
            };
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

/// OBS's audio pass with a slow output before the TX tap (dstack #303,
/// stream-snv 15.11.2026: about 0.8 TX gaps a second from the start).
#[test]
fn gate_ten_minutes_of_a_late_audio_pass_app_fast() {
    Soak::new(6.0, false, Wakes::Field, Writer::LatePass)
        .run(0x304_0008)
        .check();
}

#[test]
fn gate_ten_minutes_of_a_late_audio_pass_app_slow() {
    Soak::new(-6.0, false, Wakes::Field, Writer::LatePass)
        .run(0x304_0009)
        .check();
}

/// OBS's audio thread on an idle box (dstack #303, obs's soak gate on dlink
/// v0.6.140, 15.11.2026: tx_ppm held 0.0 for 2 min under a 6 ppm skew,
/// then 15 to 39.7 ppm, and a last-minute median of 22.2 against ±8).
#[test]
fn gate_ten_minutes_of_a_steady_writer_app_fast() {
    Soak::new(6.0, false, Wakes::Field, Writer::Steady)
        .run(0x304_000a)
        .check();
}

#[test]
fn gate_ten_minutes_of_a_steady_writer_app_slow() {
    Soak::new(-6.0, false, Wakes::Field, Writer::Steady)
        .run(0x304_000b)
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

/// Each tone break takes one counted event at or up to the grace before
/// it; an event after the break, too early, or already taken leaves it
/// unaccounted (dstack #303).
#[test]
fn each_tone_break_takes_its_own_event_just_before_it() {
    let g = 20;
    assert_eq!(unaccounted(&[], &[], g), Vec::<u64>::new());
    assert_eq!(unaccounted(&[100], &[100], g), Vec::<u64>::new(), "same step");
    assert_eq!(unaccounted(&[100], &[80], g), Vec::<u64>::new(), "the grace");
    assert_eq!(unaccounted(&[100], &[79], g), [100], "too early");
    assert_eq!(unaccounted(&[100], &[101], g), [100], "after it");
    assert_eq!(unaccounted(&[100, 105], &[90], g), [105], "one event a break");
    assert_eq!(
        unaccounted(&[100, 105], &[90, 100], g),
        Vec::<u64>::new(),
        "two events, two breaks"
    );
    assert_eq!(
        unaccounted(&[10, 500], &[5, 1000], g),
        [500],
        "an event elsewhere covers nothing"
    );
    assert_eq!(unaccounted(&[], &[5], g), Vec::<u64>::new(), "events alone");
}

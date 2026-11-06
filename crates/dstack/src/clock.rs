// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The media clock (Gen 3 phase B, plan Task 1; design §5.2, §5.5): PTP
//! time on the box, read from dsync, the box's one PTP owner (a
//! follower). The device never opens UDP 319/320 and never sends PTP.
//!
//! dsync does not keep `CLOCK_REALTIME` at PTP time: it holds
//! `wall = PTP + D`, `D` a fleet date offset that steps (whole 200 ms a
//! night, or a step or slew every ~20 s in its micro mode). So:
//!
//! - PTP ns = `CLOCK_MONOTONIC` + k. `CLOCK_MONOTONIC` runs at the rate
//!   dsync's frequency word gives the kernel clock (the grandmaster's)
//!   and no step moves it.
//! - k = (`CLOCK_REALTIME` − D) − `CLOCK_MONOTONIC` ([`sample`]), with D
//!   from dsync's time server ([`dsyx`]) once a second. A D step moves
//!   the wall and D together, so k stays.
//! - A reading counts only while dsync is locked, no step is due within
//!   [`STEP_GUARD_NS`] and no slew runs (a slew moves the frequency word,
//!   so `CLOCK_MONOTONIC` leaves PTP time while it runs).
//! - [`Tracker`]: the first usable k anchors the clock; a k off by more than
//!   [`TOLERANCE_NS`] must hold for [`CONFIRM`] polls in a row before it
//!   replaces the old one (a grandmaster change, a re-acquisition): a
//!   re-anchor, counted by the clock's [`PtpClock::epoch`], after which the
//!   media threads re-align. A poll that cannot be used after the anchor is
//!   holdover: k stays.
//! - Media time = PTP ns × rate / 10⁹ ([`to_samples`], in `u128`); a
//!   packet's stamp is its seconds (mod 2³²) and the samples into that
//!   second ([`stamp`], [`from_stamp`]).
//!
//! dsync measures no path delay, so this clock trails the grandmaster
//! by the one-way delay (0.1–0.5 ms): the safe side for TX, and it shows in
//! the RX lateness. dsync stays the box's one PTP owner (the owner's
//! decision of 2026-10-26, #72); phase D measures this clock against the
//! grandmaster's from the received packets' own stamps (the packets'
//! judge, [`crate::media::rx::RxShared::last_min_lateness_ns`]).

pub mod dsyx;
pub mod escape;
pub mod steer;

use crate::device::TimingProfile;

#[cfg(any(target_os = "linux", windows))]
use std::sync::Arc;
#[cfg(any(target_os = "linux", windows))]
use std::sync::atomic::AtomicI32;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU32, AtomicU64, Ordering};

pub use dsyx::{DateState, Reading};
pub use escape::{
    ESCAPE_BLOCK_NS, ESCAPE_DEFER_MAX_NS, EscapeState, PRESEND_NS, SPIN_ESCAPE_MAX_MS,
    SPIN_ESCAPE_MIN_MS, TIMER_LATE_NS, pump_escape_line, pump_may_escape, spin_escape_line,
    spin_escape_of,
};

/// Two readings whose k differ by at most this agree (20 µs: far above
/// dsync's reply jitter, two clock reads microseconds apart, and below
/// anything a receiver's latency notices).
pub const TOLERANCE_NS: i64 = 20_000;
/// The polls in a row a different k must hold to replace the anchored one.
pub const CONFIRM: u32 = 3;
/// No reading counts within this of a date step's instant, before or after
/// (dsync applies a step in its next loop, and our reads of the wall
/// and of D may fall on either side of it).
pub const STEP_GUARD_NS: i64 = 2_000_000_000;

/// One reading as the tracker sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sample {
    /// A k the tracker may use.
    Usable { k_ns: i64 },
    /// dsync is not locked.
    NotLocked,
    /// dsync holds no date offset (the reply has no extension).
    NoDateState,
    /// A date step's instant is within [`STEP_GUARD_NS`].
    StepNear,
    /// A date slew runs.
    Slewing,
    /// dsync did not answer the poll in time.
    NoReply,
}

/// What a reading `r` gives, with the box's `CLOCK_REALTIME` and
/// `CLOCK_MONOTONIC` read right after it arrived (back to back).
pub fn sample(r: &Reading, wall_ns: i64, mono_ns: i64) -> Sample {
    if !r.locked {
        return Sample::NotLocked;
    }
    let Some(date) = r.date else {
        return Sample::NoDateState;
    };
    if date.slewing {
        return Sample::Slewing;
    }
    let ptp_now = wall_ns.wrapping_sub(date.d_ns);
    if date
        .step_at_ptp_ns
        .is_some_and(|at| at.abs_diff(ptp_now) <= STEP_GUARD_NS.unsigned_abs())
    {
        return Sample::StepNear;
    }
    Sample::Usable {
        k_ns: ptp_now.wrapping_sub(mono_ns),
    }
}

/// What one poll did to the clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    /// The first k: the clock is ready.
    Ready { k_ns: i64 },
    /// k stays: the poll agreed, or a different k is held (not yet
    /// [`CONFIRM`] polls), or holdover ended.
    Held,
    /// A different k held for [`CONFIRM`] polls and replaced the old one.
    ReAnchored { delta_ns: i64 },
    /// After the anchor, a poll that cannot be used: k stays.
    Holdover,
    /// Before the anchor, a poll that cannot be used.
    NotReady,
}

/// The decisions on k ([`Event`]); the poller applies them to the clock.
#[derive(Debug, Default, Clone)]
pub struct Tracker {
    k: Option<i64>,
    /// A different k and the polls in a row it held.
    candidate: Option<(i64, u32)>,
}

impl Tracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// The anchored k.
    pub fn k(&self) -> Option<i64> {
        self.k
    }

    /// One poll's sample: what it does to k.
    pub fn feed(&mut self, s: Sample) -> Event {
        let Sample::Usable { k_ns } = s else {
            self.candidate = None;
            return if self.k.is_some() {
                Event::Holdover
            } else {
                Event::NotReady
            };
        };
        let Some(k) = self.k else {
            self.k = Some(k_ns);
            return Event::Ready { k_ns };
        };
        if agree(k_ns, k) {
            self.candidate = None;
            return Event::Held;
        }
        let (first, n) = match self.candidate {
            Some((first, n)) if agree(k_ns, first) => (first, n + 1),
            _ => (k_ns, 1),
        };
        if n < CONFIRM {
            self.candidate = Some((first, n));
            return Event::Held;
        }
        self.k = Some(k_ns);
        self.candidate = None;
        Event::ReAnchored {
            delta_ns: k_ns.wrapping_sub(k),
        }
    }
}

/// Two k agree within [`TOLERANCE_NS`].
fn agree(a: i64, b: i64) -> bool {
    a.abs_diff(b) <= TOLERANCE_NS.unsigned_abs()
}

/// What the poller has done so far, for the control socket's status (plan
/// Task 7): its polls, the re-anchors, and the holdovers (a run of polls in
/// holdover is one), written by the poller only; and for a linked clock the
/// source in use and the follower's counts, written by the host's selector.
#[derive(Debug, Default)]
pub struct ClockStats {
    polls: AtomicU64,
    re_anchors: AtomicU32,
    holdovers: AtomicU32,
    in_holdover: AtomicBool,
    /// The TEST system clock, no poller (`host::ClockSource::System`).
    system: AtomicBool,
    /// The grandmaster's UUID as dsync's last reply named it (big-endian
    /// in the low 48 bits; 0 for none).
    grandmaster: AtomicU64,
    /// The master's UUID, as `grandmaster` (dsync 1.26 on).
    master: AtomicU64,
    /// A linked clock's source ([`linked::Source`] + 1; 0: not linked).
    linked: std::sync::atomic::AtomicU8,
    /// A linked clock's follower counts, as the selector read them last
    /// (`None`: no follower).
    follower: std::sync::Mutex<Option<FollowerCounts>>,
}

/// [`ClockStats`] read at one moment.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ClockCounts {
    pub polls: u64,
    pub re_anchors: u32,
    pub holdovers: u32,
    /// The last poll was in holdover.
    pub in_holdover: bool,
}

/// A linked clock's follower counts: dsync's own (its phase-lock windows in
/// `polls`, its re-anchors, holdovers), how many of its re-anchors the
/// media threads absorbed without a re-align (a step within
/// [`linked::HANDOVER_NS`]), and the state the selector decides on
/// (dstack #1, iemmixer 5.11.2026: after a grandmaster change the status
/// could not tell why the follower did not take over again).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FollowerCounts {
    pub counts: ClockCounts,
    pub absorbed: u32,
    /// dsync's lock gate: |e| within 50 µs for 3 windows.
    pub locked: bool,
    /// Locked, |pull| and the recent |e| p90 within 30 µs: only a settled
    /// follower takes over a ready service ([`linked::choose`]).
    pub settled: bool,
    /// The last window's error, ns (`None` before the first).
    pub offset_ns: Option<i64>,
    /// The recent windows' |e| p90, ns (`None` until there are enough).
    pub recent_abs_e_ns: Option<i64>,
    /// The phase, ns, still being retired along dsync's pull after a
    /// grandmaster change in the same time base (dsync#158); 0 when none.
    pub pull_ns: i64,
}

impl ClockStats {
    /// One poll's event.
    pub fn record(&self, ev: Event) {
        self.polls.fetch_add(1, Ordering::Relaxed);
        let holdover = ev == Event::Holdover;
        if self.in_holdover.swap(holdover, Ordering::Relaxed) != holdover && holdover {
            self.holdovers.fetch_add(1, Ordering::Relaxed);
        }
        if let Event::ReAnchored { .. } = ev {
            self.re_anchors.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The grandmaster dsync's last reply named (all zeros: none).
    pub fn set_grandmaster(&self, uuid: [u8; 6]) {
        self.grandmaster.store(uuid_word(uuid), Ordering::Relaxed);
    }

    /// The grandmaster's UUID; `None` before one was named.
    pub fn grandmaster(&self) -> Option<[u8; 6]> {
        word_uuid(self.grandmaster.load(Ordering::Relaxed))
    }

    /// The master dsync's last reply named (all zeros: none; dsync 1.26).
    pub fn set_master(&self, uuid: [u8; 6]) {
        self.master.store(uuid_word(uuid), Ordering::Relaxed);
    }

    /// The master's UUID; `None` before one was named.
    pub fn master(&self) -> Option<[u8; 6]> {
        word_uuid(self.master.load(Ordering::Relaxed))
    }

    /// The clock is the TEST system clock: nothing polls dsync.
    pub fn mark_system(&self) {
        self.system.store(true, Ordering::Relaxed);
    }

    /// Whether the clock is the TEST system clock.
    pub fn is_system(&self) -> bool {
        self.system.load(Ordering::Relaxed)
    }

    /// A linked clock uses `source` now (the host's selection, once a
    /// second).
    pub fn set_linked(&self, source: linked::Source) {
        self.linked.store(source as u8 + 1, Ordering::Relaxed);
    }

    /// The source a linked clock uses; `None` for a clock that is not
    /// linked (the dsync service only, or the system clock).
    pub fn linked(&self) -> Option<linked::Source> {
        match self.linked.load(Ordering::Relaxed) {
            0 => None,
            v => Some(linked::Source::from_u8(v - 1)),
        }
    }

    /// A linked clock's follower counts now (the host's selection, once a
    /// second; `None` when the follower did not start).
    pub fn set_follower(&self, counts: Option<FollowerCounts>) {
        *self
            .follower
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = counts;
    }

    /// The follower's counts as last set (its windows in `polls`); `None`
    /// without a follower.
    pub fn follower(&self) -> Option<FollowerCounts> {
        *self
            .follower
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// The service poller's counts.
    pub fn counts(&self) -> ClockCounts {
        ClockCounts {
            polls: self.polls.load(Ordering::Relaxed),
            re_anchors: self.re_anchors.load(Ordering::Relaxed),
            holdovers: self.holdovers.load(Ordering::Relaxed),
            in_holdover: self.in_holdover.load(Ordering::Relaxed),
        }
    }
}

/// One poll of dsync and what it did (the poller's step; the
/// `iem-dante-dev clock` lines are made of these).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Polled {
    /// dsync's reply; `None` when it did not answer in time.
    pub reading: Option<Reading>,
    /// What the reply gave the tracker.
    pub sample: Sample,
    /// What it did to k.
    pub event: Event,
    /// The anchored k after the poll; `None` before the clock is ready.
    pub k_ns: Option<i64>,
}

/// PTP time for the media threads.
pub trait PtpClock: Send + Sync {
    /// PTP ns now; `None` before the clock is ready.
    fn now_ns(&self) -> Option<u64>;
    /// Sleeps until PTP ns `ptp_ns` (at once when it is past). It may
    /// return early (a signal, or a clock that is not ready: then after at
    /// most [`NOT_READY_NAP_NS`]): callers read [`PtpClock::now_ns`] again.
    fn sleep_until(&self, ptp_ns: u64);
    /// The re-anchors so far: a change tells the media threads to re-align.
    fn epoch(&self) -> u32;
    /// The latest re-anchor whose time step the clock knows, with the epoch
    /// it began (dstack #1, 6.11.2026): on that epoch the media threads
    /// shift their schedules by the step instead of starting again
    /// ([`Anchor::step_from`]). `None`, the default: no re-anchor yet, or
    /// its step is unknown; the media threads then start again, counted.
    fn last_re_anchor(&self) -> Option<ReAnchor> {
        None
    }
}

/// A re-anchor whose time step the clock knows ([`PtpClock::last_re_anchor`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReAnchor {
    /// The epoch it began.
    pub epoch: u32,
    /// The clock's time just after it less just before it, ns.
    pub step_ns: i64,
}

/// What a media thread reads of the clock's time base with each time read:
/// the epoch and the latest known re-anchor ([`Anchor::of`]). A bare epoch
/// converts into one with no known step (the media threads start again on
/// a change, as before 0.6.48).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Anchor {
    pub epoch: u32,
    pub re_anchor: Option<ReAnchor>,
}

impl From<u32> for Anchor {
    fn from(epoch: u32) -> Anchor {
        Anchor {
            epoch,
            re_anchor: None,
        }
    }
}

impl Anchor {
    /// `clock`'s epoch and latest known re-anchor, read after its time.
    pub fn of(clock: &(impl PtpClock + ?Sized)) -> Anchor {
        Anchor {
            epoch: clock.epoch(),
            re_anchor: clock.last_re_anchor(),
        }
    }

    /// The time step from epoch `from` to this one, ns: known only when
    /// this epoch is the one after `from` and the clock's latest known
    /// re-anchor began it. Two re-anchors between two reads, or one whose
    /// step the clock does not know (a source switch of the linked clock),
    /// give `None`.
    pub fn step_from(&self, from: u32) -> Option<i64> {
        self.re_anchor
            .filter(|r| r.epoch == self.epoch && from.wrapping_add(1) == self.epoch)
            .map(|r| r.step_ns)
    }
}

/// How long [`PtpClock::sleep_until`] waits on a clock that is not ready.
pub const NOT_READY_NAP_NS: u64 = 10_000_000;

/// A clock for tests: ready from its start time, and time moves only in
/// [`PtpClock::sleep_until`] (to the time asked), [`ManualClock::advance`]
/// and [`ManualClock::re_anchor`].
#[derive(Debug)]
pub struct ManualClock {
    now: AtomicU64,
    ready: AtomicBool,
    epoch: AtomicU32,
    /// The latest re-anchor's step and the epoch it began (0: none).
    step_ns: AtomicI64,
    step_epoch: AtomicU32,
}

impl ManualClock {
    /// A ready clock at `start_ns`.
    pub fn new(start_ns: u64) -> Self {
        ManualClock {
            now: AtomicU64::new(start_ns),
            ready: AtomicBool::new(true),
            epoch: AtomicU32::new(0),
            step_ns: AtomicI64::new(0),
            step_epoch: AtomicU32::new(0),
        }
    }

    pub fn set_ready(&self, ready: bool) {
        self.ready.store(ready, Ordering::SeqCst);
    }

    /// Moves time on by `ns`.
    pub fn advance(&self, ns: u64) {
        self.now.fetch_add(ns, Ordering::SeqCst);
    }

    /// A re-anchor: time jumps by `delta_ns` and the epoch counts it; its
    /// step is known ([`PtpClock::last_re_anchor`]).
    pub fn re_anchor(&self, delta_ns: i64) {
        let epoch = self.epoch.load(Ordering::SeqCst).wrapping_add(1);
        self.step_ns.store(delta_ns, Ordering::SeqCst);
        self.step_epoch.store(epoch, Ordering::SeqCst);
        // Two's complement: adding the bits of a negative delta subtracts.
        self.now.fetch_add(delta_ns as u64, Ordering::SeqCst);
        self.epoch.store(epoch, Ordering::SeqCst);
    }
}

impl PtpClock for ManualClock {
    fn now_ns(&self) -> Option<u64> {
        self.ready
            .load(Ordering::SeqCst)
            .then(|| self.now.load(Ordering::SeqCst))
    }

    fn sleep_until(&self, ptp_ns: u64) {
        self.now.fetch_max(ptp_ns, Ordering::SeqCst);
    }

    fn epoch(&self) -> u32 {
        self.epoch.load(Ordering::SeqCst)
    }

    fn last_re_anchor(&self) -> Option<ReAnchor> {
        let epoch = self.step_epoch.load(Ordering::SeqCst);
        (epoch != 0).then(|| ReAnchor {
            epoch,
            step_ns: self.step_ns.load(Ordering::SeqCst),
        })
    }
}

/// How far a schedule on a grid of `grid` samples (a flow's fpp, the pump's
/// block; above 0) moves for a re-anchor of `step_ns` at `rate` (dstack #1,
/// 6.11.2026): the step in samples rounded to the nearest whole multiple of
/// the grid (a half rounds up).
///
/// - dsync's coordinated date step moves every device's time by the same
///   step (1.6 s is 153 600 samples at 96 kHz, a whole number of packets):
///   the receivers expect the stamps to go on at the old ones + the step,
///   which is what a whole-grid shift sends.
/// - A local correction of k (more than the tracker's 20 µs, a few samples)
///   rounds to 0: the stamps simply go on, and the clock's new time shows
///   as a lag or a wait of at most half the grid, which the schedule's own
///   checks absorb (a lag past the latency still starts it again).
/// - Starting again instead never lands nearer the clock (it starts on the
///   grid after now) and drops up to one grid step plus the lag: 2 packets
///   at a receiver in the field. So a known step is never refused for being
///   off the grid; only an unknown one starts the schedules again.
pub fn grid_shift(step_ns: i64, rate: u32, grid: u64) -> i64 {
    let grid = i128::from(grid);
    let m = nearest(i128::from(step_ns) * i128::from(rate), 1_000_000_000 * grid);
    // |step_ns| < 2^63 ns and rate < 10^9, so the shift fits an i64.
    (m * grid) as i64
}

/// `n / d` rounded to the nearest integer, a half up (`d` above 0).
fn nearest(n: i128, d: i128) -> i128 {
    let r = n.rem_euclid(d);
    n.div_euclid(d) + i128::from(r + r >= d)
}

/// A 6-byte UUID as the low bytes of a `u64` (big-endian).
fn uuid_word(uuid: [u8; 6]) -> u64 {
    let mut w = [0u8; 8];
    w[2..].copy_from_slice(&uuid);
    u64::from_be_bytes(w)
}

/// [`uuid_word`] back; `None` for all zeros (none named).
fn word_uuid(w: u64) -> Option<[u8; 6]> {
    let uuid: [u8; 6] = w.to_be_bytes()[2..].try_into().unwrap_or([0; 6]);
    (uuid != [0; 6]).then_some(uuid)
}

/// Samples at `rate` in `ptp_ns` (rounded down; `u128`, so years of a
/// grandmaster's uptime never overflow). A rate of 0 reads as 1.
pub fn to_samples(ptp_ns: u64, rate: u32) -> u64 {
    (u128::from(ptp_ns) * u128::from(rate.max(1)) / 1_000_000_000) as u64
}

/// A latency of `ns` at `rate` in whole samples, the nearest (a latency
/// is set in ns and stands for whole samples: 333 333 ns is 32 at 96 kHz,
/// where [`to_samples`] gives 31). A rate of 0 reads as 1.
pub fn latency_samples(ns: u64, rate: u32) -> u64 {
    ((u128::from(ns) * u128::from(rate.max(1)) + 500_000_000) / 1_000_000_000) as u64
}

/// A packet's stamp for sample index `samples`: its seconds (mod 2³²) and
/// the samples into that second. A rate of 0 reads as 1.
pub fn stamp(samples: u64, rate: u32) -> (u32, u32) {
    let rate = u64::from(rate.max(1));
    ((samples / rate) as u32, (samples % rate) as u32)
}

/// The sample index of the stamp (`secs`, `sub`) nearest `near` (the stamp
/// repeats every 2³² seconds; the later one on a tie; never below 0). A
/// rate of 0 reads as 1.
pub fn from_stamp(secs: u32, sub: u32, rate: u32, near: u64) -> u64 {
    let rate = i128::from(rate.max(1));
    let period = rate << 32;
    let base = i128::from(secs) * rate + i128::from(sub);
    let repeats = (i128::from(near) - base + period / 2).div_euclid(period);
    let index = base + repeats * period;
    (if index < 0 { index + period } else { index }) as u64
}

#[cfg(any(target_os = "linux", windows))]
pub mod linked;
#[cfg(any(target_os = "linux", windows))]
pub use os::{DSYNC, MonoClock, POLL, Poller, monotonic_ns, poll_dsync, realtime_ns};

/// How a media thread's wait ends on Windows (dstack #1 problem A).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitMode {
    /// The high-resolution waitable timer for all but the last spin tail
    /// (50 ms unless the device sets one), then a yielding spin (the
    /// low-latency profile).
    Spin,
    /// The high-resolution waitable timer for the whole wait, no spin
    /// (the normal profile).
    Timer,
}

/// The spin tail of the low-latency wait on Windows unless the device sets
/// one, ns: every wait shorter than this is spun ([`WaitMode::Spin`]).
pub const SPIN_TAIL_NS: u64 = 50_000_000;
/// The longest spin tail a device may set, ns.
pub const SPIN_TAIL_MAX_NS: u64 = 50_000_000;

/// The process's spin tail, ns ([`set_spin_tail_ns`]).
static SPIN_TAIL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(SPIN_TAIL_NS);

/// The spin tail for a device's `spin_tail_ns`: it, else
/// [`SPIN_TAIL_NS`], at most [`SPIN_TAIL_MAX_NS`].
pub fn spin_tail_of(configured: Option<u64>) -> u64 {
    configured.unwrap_or(SPIN_TAIL_NS).min(SPIN_TAIL_MAX_NS)
}

/// Sets the process's spin tail ([`spin_tail_of`] the device's).
pub fn set_spin_tail_ns(configured: Option<u64>) {
    SPIN_TAIL.store(
        spin_tail_of(configured),
        std::sync::atomic::Ordering::Relaxed,
    );
}

/// The spin tail of a wait in `mode` with the process's tail `tail_ns`:
/// none for the timer's wait.
pub fn spin_tail_for(mode: WaitMode, tail_ns: u64) -> u64 {
    match mode {
        WaitMode::Spin => tail_ns,
        WaitMode::Timer => 0,
    }
}

/// The process's bounded-escape period, ns; 0: off
/// ([`set_spin_escape_ms`]; process-wide, the last device set wins, as
/// the spin tail). Read by the Windows wait only.
static SPIN_ESCAPE: AtomicU64 = AtomicU64::new(0);

/// Sets the process's bounded-escape period ([`spin_escape_of`] the
/// device's). Only the Windows low-latency wait reads it.
pub fn set_spin_escape_ms(configured_ms: Option<u64>) {
    SPIN_ESCAPE.store(spin_escape_of(configured_ms), Ordering::Relaxed);
}

/// Whether the process runs the bounded escape (`spin_escape_ms` set).
pub fn escape_on() -> bool {
    SPIN_ESCAPE.load(Ordering::Relaxed) != 0
}

#[cfg(any(target_os = "linux", windows))]
pub use sys::{allow_escape, ask_escape, defer_escape};

/// The wait of a timing profile.
pub fn wait_mode_for(profile: TimingProfile) -> WaitMode {
    match profile {
        TimingProfile::LowLatency => WaitMode::Spin,
        TimingProfile::Normal => WaitMode::Timer,
    }
}

/// A timing profile set once ([`set_profile`] keeps the process's).
#[derive(Debug, Default)]
pub struct ProfileCell(std::sync::OnceLock<TimingProfile>);

impl ProfileCell {
    pub const fn new() -> Self {
        Self(std::sync::OnceLock::new())
    }

    /// Sets the profile, once: `false` when another one was set before.
    pub fn set(&self, profile: TimingProfile) -> bool {
        *self.0.get_or_init(|| profile) == profile
    }

    /// The profile set ([`TimingProfile::LowLatency`] until one is).
    pub fn get(&self) -> TimingProfile {
        self.0.get().copied().unwrap_or_default()
    }
}

static PROFILE: ProfileCell = ProfileCell::new();

/// Sets this process's timing profile, once (the device's config, before
/// its media threads start); `false` when another one was set before.
pub fn set_profile(profile: TimingProfile) -> bool {
    PROFILE.set(profile)
}

/// This process's timing profile ([`TimingProfile::LowLatency`] until set).
pub fn profile() -> TimingProfile {
    PROFILE.get()
}

/// Whether a media thread under `profile` asks for real-time priority:
/// low latency does, normal (an application that needs no lowest latency)
/// does not.
pub fn realtime_for(profile: TimingProfile) -> bool {
    profile == TimingProfile::LowLatency
}

/// The OS a media thread runs on, for [`thread_priority_for`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    /// Linux and the rest.
    Other,
}

impl Os {
    /// The OS this build runs on.
    pub const fn current() -> Os {
        if cfg!(windows) {
            Os::Windows
        } else {
            Os::Other
        }
    }
}

/// The priority a media thread (the pump, TX, RX) asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThreadPriority {
    /// Real-time priority: `SCHED_FIFO` on Linux, TIME_CRITICAL/HIGHEST
    /// on Windows (the low-latency profile).
    Realtime,
    /// Windows' THREAD_PRIORITY_HIGHEST within the process's normal
    /// class: no privilege, no process-wide change (dstack #90).
    Highest,
    /// The OS's normal priority.
    Normal,
}

/// The priority a media thread under `profile` asks for on `os`: low
/// latency asks real-time priority ([`realtime_for`]); normal asks
/// THREAD_PRIORITY_HIGHEST on Windows, where a busy video PC's threads
/// otherwise preempt dlink's pump for several ms (dstack #90:
/// songplayer's Dante output lost blocks), and normal priority elsewhere.
pub fn thread_priority_for(profile: TimingProfile, os: Os) -> ThreadPriority {
    match (profile, os) {
        (TimingProfile::LowLatency, _) => ThreadPriority::Realtime,
        (TimingProfile::Normal, Os::Windows) => ThreadPriority::Highest,
        (TimingProfile::Normal, Os::Other) => ThreadPriority::Normal,
    }
}

/// This process's wait mode ([`wait_mode_for`] its profile).
pub fn wait_mode() -> WaitMode {
    wait_mode_for(profile())
}

#[cfg(target_os = "linux")]
#[path = "clock/sys_linux.rs"]
mod sys;
#[cfg(windows)]
#[path = "clock/sys_windows.rs"]
mod sys;

/// The poller and the clock on the OS's wall and monotonic clocks
/// ([`sys`]): Linux's `CLOCK_REALTIME` and `CLOCK_MONOTONIC`; on Windows
/// the precise system time and QPC steered to its rate ([`steer`]), so a
/// date step moves neither k nor PTP time on either, see `sys_windows`.
#[cfg(any(target_os = "linux", windows))]
mod os {
    use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
    use std::time::Duration;

    use tokio::net::UdpSocket;
    use tokio::time::{Instant, MissedTickBehavior};
    use tracing::{debug, info, warn};

    use super::*;

    /// How often dsync is asked.
    pub const POLL: Duration = Duration::from_secs(1);
    /// dsync's time server on the box.
    pub const DSYNC: SocketAddr =
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, dsyx::PORT));
    /// How long one poll waits for its reply.
    pub const REPLY_TIMEOUT: Duration = Duration::from_millis(200);
    /// k before the clock is ready.
    const NOT_READY: i64 = i64::MIN;

    pub use super::sys::{monotonic_ns, realtime_ns};

    /// PTP time as `CLOCK_MONOTONIC` + k, k set by the poller.
    #[derive(Debug)]
    pub struct MonoClock {
        k_ns: AtomicI64,
        epoch: AtomicU32,
        /// The latest re-anchor's step (k after less k before: the clock's
        /// time is `CLOCK_MONOTONIC` + k, so that is its step exactly) and
        /// the epoch it began (0: none yet).
        step_ns: AtomicI64,
        step_epoch: AtomicU32,
    }

    impl Default for MonoClock {
        fn default() -> Self {
            Self::new()
        }
    }

    impl MonoClock {
        /// Not ready until the first k.
        pub fn new() -> Self {
            MonoClock {
                k_ns: AtomicI64::new(NOT_READY),
                epoch: AtomicU32::new(0),
                step_ns: AtomicI64::new(0),
                step_epoch: AtomicU32::new(0),
            }
        }

        /// The k in use.
        pub fn k_ns(&self) -> Option<i64> {
            let k = self.k_ns.load(Ordering::Acquire);
            (k != NOT_READY).then_some(k)
        }

        /// Sets k: the first makes the clock ready, a change re-anchors it
        /// (its step recorded with the epoch it begins, before the epoch
        /// moves, so a reader that sees the epoch sees its step).
        ///
        /// One writer (the poller); k moves before the epoch. The media
        /// threads read the epoch before the time ([`Anchor::of`] first),
        /// so a pass may see the new k with the old epoch (the jump starts
        /// its schedule again at the new time, and the next pass keeps it
        /// there: `media::tx::shift`), never the old k with the new epoch.
        /// Re-anchors are at least [`CONFIRM`] polls apart, so a step is
        /// never read under another's epoch.
        pub fn set_k(&self, k_ns: i64) {
            let old = self.k_ns.swap(k_ns, Ordering::AcqRel);
            if old != NOT_READY && old != k_ns {
                let epoch = self.epoch.load(Ordering::Acquire).wrapping_add(1);
                self.step_ns
                    .store(k_ns.wrapping_sub(old), Ordering::Relaxed);
                self.step_epoch.store(epoch, Ordering::Release);
                self.epoch.store(epoch, Ordering::Release);
            }
        }
    }

    impl PtpClock for MonoClock {
        fn now_ns(&self) -> Option<u64> {
            let k = self.k_ns()?;
            u64::try_from(i128::from(monotonic_ns()) + i128::from(k)).ok()
        }

        fn sleep_until(&self, ptp_ns: u64) {
            let Some(k) = self.k_ns() else {
                std::thread::sleep(Duration::from_nanos(NOT_READY_NAP_NS));
                return;
            };
            let target = i64::try_from(i128::from(ptp_ns) - i128::from(k)).unwrap_or(i64::MAX);
            super::sys::sleep_until_monotonic(target);
        }

        fn epoch(&self) -> u32 {
            self.epoch.load(Ordering::Acquire)
        }

        fn last_re_anchor(&self) -> Option<ReAnchor> {
            let epoch = self.step_epoch.load(Ordering::Acquire);
            (epoch != 0).then(|| ReAnchor {
                epoch,
                step_ns: self.step_ns.load(Ordering::Relaxed),
            })
        }
    }

    /// Polls dsync on the box (127.0.0.1, [`dsyx::PORT`]) once a second
    /// and keeps `clock`'s k, `freq` (dsync's frequency adjustment,
    /// ppb) and `stats` up to date; runs until the task ends.
    pub async fn poll_dsync(clock: Arc<MonoClock>, freq: Arc<AtomicI32>, stats: Arc<ClockStats>) {
        poll(DSYNC, clock, freq, stats).await
    }

    /// The poller, aimed at `target` (a stand-in server in the tests).
    /// Glue: every decision is [`sample`]'s and [`Tracker`]'s. Logs name
    /// the clock's state, k and deltas in ns, never an address.
    pub(crate) async fn poll(
        target: SocketAddr,
        clock: Arc<MonoClock>,
        freq: Arc<AtomicI32>,
        stats: Arc<ClockStats>,
    ) {
        let mut poller = loop {
            match Poller::connect(target).await {
                Ok(p) => break p,
                Err(e) => {
                    warn!(error = %e, "media clock: no socket to dsync, trying again");
                    tokio::time::sleep(POLL).await;
                }
            }
        };
        info!(
            port = target.port(),
            "media clock: polling dsync's time server once a second"
        );
        let mut tick = tokio::time::interval(POLL);
        tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut last = Event::Held;
        let mut waiting: Option<Sample> = None;
        loop {
            tick.tick().await;
            let p = poller.once(&clock, &freq).await;
            stats.record(p.event);
            if let Some(r) = &p.reading {
                stats.set_grandmaster(r.grandmaster);
                stats.set_master(r.master);
            }
            log_event(p.event, last, p.sample, &mut waiting, clock.epoch());
            last = p.event;
        }
    }

    /// One poll at a time of dsync's time server at a target: the
    /// socket, the request ids and the [`Tracker`]. The service's poller
    /// ([`poll_dsync`]) and `iem-dante-dev clock` both step it once a
    /// second.
    #[derive(Debug)]
    pub struct Poller {
        socket: UdpSocket,
        tracker: Tracker,
        id: u32,
        buf: [u8; dsyx::REPLY_MAX],
    }

    impl Poller {
        /// A socket on loopback, connected to `target` ([`DSYNC`] on
        /// the box): only the target's datagrams reach it.
        pub async fn connect(target: SocketAddr) -> std::io::Result<Self> {
            let socket = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).await?;
            socket.connect(target).await?;
            Ok(Poller {
                socket,
                tracker: Tracker::new(),
                // Any start; replies are matched by it.
                id: monotonic_ns() as u32,
                buf: [0u8; dsyx::REPLY_MAX],
            })
        }

        /// One request and its reply (at most `REPLY_TIMEOUT`): the
        /// reply's frequency adjustment goes to `freq`, its sample to the
        /// tracker, and a first or a replaced k to `clock`.
        pub async fn once(&mut self, clock: &MonoClock, freq: &AtomicI32) -> Polled {
            self.id = self.id.wrapping_add(1);
            let reply = ask(&self.socket, self.id, &mut self.buf).await;
            let s = match reply {
                Some((r, wall, mono)) => {
                    freq.store(r.freq_ppb, Ordering::Relaxed);
                    sample(&r, wall, mono)
                }
                None => Sample::NoReply,
            };
            let event = self.tracker.feed(s);
            let k_ns = self.tracker.k();
            if let (Event::Ready { .. } | Event::ReAnchored { .. }, Some(k)) = (event, k_ns) {
                clock.set_k(k);
            }
            Polled {
                reading: reply.map(|(r, _, _)| r),
                sample: s,
                event,
                k_ns,
            }
        }
    }

    /// One request and its reply within [`REPLY_TIMEOUT`], with the box's
    /// wall and monotonic clocks read the moment it arrived; a late reply to
    /// an earlier request is skipped.
    async fn ask(socket: &UdpSocket, id: u32, buf: &mut [u8]) -> Option<(Reading, i64, i64)> {
        if let Err(e) = socket.send(&dsyx::request(id)).await {
            debug!(error = %e, "media clock: the request to dsync was not sent");
            return None;
        }
        let deadline = Instant::now() + REPLY_TIMEOUT;
        loop {
            let n = match tokio::time::timeout_at(deadline, socket.recv(buf)).await {
                Ok(Ok(n)) => n,
                Ok(Err(e)) => {
                    debug!(error = %e, "media clock: no reply from dsync");
                    return None;
                }
                Err(_) => return None,
            };
            let wall = realtime_ns();
            let mono = super::sys::observe(wall);
            match dsyx::parse(&buf[..n], id) {
                Ok(r) => return Some((r, wall, mono)),
                Err(e) => debug!(error = %e, "media clock: a datagram skipped"),
            }
        }
    }

    /// One line per change of the clock's state.
    fn log_event(ev: Event, last: Event, s: Sample, waiting: &mut Option<Sample>, epoch: u32) {
        match ev {
            Event::Ready { k_ns } => info!(k_ns, "media clock ready: PTP time from dsync"),
            Event::ReAnchored { delta_ns } => warn!(
                delta_ns,
                epoch, "media clock re-anchored: a different k held for 3 polls"
            ),
            Event::Holdover if last != Event::Holdover => warn!(
                reason = ?s,
                "media clock in holdover: dsync gives no usable time, k kept"
            ),
            Event::Held if last == Event::Holdover => info!("media clock out of holdover"),
            Event::NotReady if *waiting != Some(s) => {
                info!(reason = ?s, "media clock not ready: waiting for dsync");
                *waiting = Some(s);
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod anchor_tests;

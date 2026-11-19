// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The engine's pump (Gen 3 phase C, plan
//! `docs/superpowers/plans/2026-10-26-gen3-phase-c-engine-on-linux.md` Task
//! 1; design §5.2): the engine's `Process` driven by the media clock
//! instead of a sound card's callback. It knows nothing about mixing.
//!
//! - Every B frames (the device's `block`: [`BLOCK`], 32, unless the
//!   config sets 16, one of [`BLOCKS`]), block k covers the input stamps
//!   [kB, kB + B) and runs once its last sample's playout time has passed:
//!   media now ≥ kB + B + P, P the device's latency in samples (the playout
//!   point the RX checks already read at). Its inputs are the RX rings at
//!   those stamps (an RX channel that is not routed to a flow is silence
//!   and reads nothing, so it counts no hole); a channel whose flow plays
//!   out later than P (its fpp, its transmitter's latency) is read that
//!   much earlier in stamps, [`crate::media::rx::RxShared::delay`], so only
//!   that source is later in the mix. Its outputs go into the
//!   engine's TX rings ([`crate::media::tx::TxShared::engine_ring`]) at
//!   kB + D, **D = B + P + W** ([`Timing::offset`]), W the device's
//!   `work_margin` ([`WORK_MARGIN`], 1 ms, unless the config sets it; at
//!   least B): RX media time plus a fixed processing offset.
//!   The TX thread sends them as [`crate::media::source::Source::Engine`]:
//!   the packet stamped kB + D leaves at kB + D + fpp + the TX margin
//!   (the device's `tx_margin_ns`, 250 µs by default), so a block
//!   that ends within W of its due time is always in time, whatever either
//!   side's fpp. The pump judges only its wake against W: a block that
//!   woke within W but ends later than that (its work, a preemption) is
//!   written after the TX thread sent its stamps as silence, counted in
//!   the TX lane's `holes` and as the pump's
//!   [`PumpView::writes_after_send`] (dstack #346).
//! - [`Blocks`] (pure) says what is due: a block, a wait, or a
//!   re-bootstrap when the pump is more than W behind (the skipped blocks'
//!   stamps go out as silence, counted as TX holes) or more than W and a
//!   block ahead (the clock went back: no schedule waits further, dstack
//!   #440). A start never catches up: the first block is the one whose
//!   input ends at the playout point.
//! - [`Pump`]: the buffers, allocated once; [`Pump::media`] runs every due
//!   block at a media time (the first media step and a new clock epoch
//!   re-bootstrap; the processor's `discontinuity` is called once before
//!   the next block); [`Pump::free`] runs one block on silent inputs with
//!   its outputs dropped, while the clock is not ready, so commands, meters
//!   and the engine's status keep flowing and nothing is sent.
//!   [`Pump::block`] is the real-time part (I7: no allocation, lock, log or
//!   system call but the clock read; `tests/hot_path.rs`, rtsan): `process`
//!   inside `catch_unwind`, a panic zeroes the block and the processor is
//!   never called again (the backends' crash model).
//! - [`PumpStats`]: what the engine's status and the control socket read,
//!   as [`dstack_audio_io::StreamStats`], the stream histograms
//!   ([`dstack_audio_io::hist::HistSnapshot`]) and a
//!   [`PumpView`]: each re-bootstrap with its [`Cause`], how late the
//!   pump woke ([`wake`], dstack #281), and where its lost time went
//!   ([`split`], dstack #354: the sleep's oversleep, the drain, the
//!   blocks' CPU time).
//! - [`app_offset`]: the media clock's offset from the app's clock that
//!   stamps each block's play time, filtered (dstack #295).
//! - [`watch::PumpWatch`]: once a second, from the daemon's tick (never
//!   the pump thread), one warning when re-bootstraps, skipped blocks or
//!   overruns grew in that second.
//! - The RX rings' writer in the normal profile (dstack #326): the pump
//!   claims it at [`Pump::new`] ([`crate::media::rx::RxShared::pump_intake`])
//!   and drains the flow sockets (non-blocking, every slot with a flow)
//!   just before each block reads the rings, and in each free block
//!   (counted unplaced). The pump reads by sample position, so its own
//!   lateness loses nothing, and no helper thread stands between a packet
//!   in its socket and the block that reads it. The drain is outside
//!   [`Pump::block`] (a socket read is a system call); its per-packet part
//!   carries the same rtsan attribute. The low-latency profile leaves the
//!   writes to the RX thread, as before.
//! - [`spawn`]: the `dstack-engine` thread (`SCHED_FIFO` [`ENGINE_PRIORITY`]
//!   when allowed): on the media clock while it is ready, paced on its own
//!   ([`free_deadline`], NullRt's rule) while it is not.
//!
//! A sample is a left-justified `i32` (full scale 2³¹, as the rings keep
//! them): [`to_f64`] scales by 2⁻³¹ exactly, [`quantize`] rounds at the
//! flow's bits and saturates at full scale.

use std::any::Any;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use dstack_audio_io::{Block, Process, panic_message};

use crate::clock::{PtpClock, to_samples};
use crate::media::ring::Ring;
use crate::media::rx::{Intake, RxShared};
use crate::media::tx::{Shift, TxShared, ns_of};
use app_offset::{OffsetFilter, Outcome};

/// The engine's default block, frames: the B = 32 of the RT contract, what
/// the device's `block` is when the config leaves it out.
pub const BLOCK: u64 = 32;
/// The blocks a device config may set, frames: 16 halves B (and lets W go
/// down to 16; #72, mic to in-ear under 1 ms), 32 is [`BLOCK`].
pub const BLOCKS: [u64; 5] = [16, BLOCK, 64, 128, 256];
/// W: how late a block may run and still be sent in time, samples (1 ms
/// at 96 kHz). A pump later than this re-bootstraps. The default of the
/// device's `work_margin` (#72 phase F sweeps it without a build), which
/// is never below the device's block.
pub const WORK_MARGIN: u64 = 96;
/// The largest `work_margin` a config may set, samples (about 43 ms at
/// 96 kHz): far beyond any useful margin, well inside the rings.
pub const MAX_WORK_MARGIN: u64 = 4096;
/// The pump thread's `SCHED_FIFO` priority when `RLIMIT_RTPRIO` allows it:
/// below the TX and RX threads' 45 and dsync's 50.
pub const ENGINE_PRIORITY: i32 = 40;
/// Free pacing more than this many blocks behind resynchronises instead
/// of bursting (NullRt's rule).
pub const FREE_RESYNC_BLOCKS: u32 = 8;

/// Full scale of a left-justified sample: 2³¹.
const FULL_SCALE: f64 = 2_147_483_648.0;

/// A left-justified sample as the engine's float: `s` · 2⁻³¹, exact.
pub fn to_f64(s: i32) -> f64 {
    f64::from(s) / FULL_SCALE
}

/// The engine's float as a left-justified sample of `bits` (1 to 32; others
/// read as the nearest): rounded at `bits`, saturating at full scale
/// (never wrapping), NaN as 0.
pub fn quantize(x: f64, bits: u8) -> i32 {
    let bits = bits.clamp(1, 32);
    let top = f64::from(1u32 << (bits - 1));
    // A float to int cast of NaN is 0; the clamp keeps ±∞ at the edges.
    let v = (x * top).round().clamp(-top, top - 1.0);
    ((v as i64) << (32 - bits)) as i32
}

/// Which device channels the engine's inputs and outputs are (indices from
/// 0): engine input i reads RX channel `rx[i]`, engine output j writes TX
/// channel `tx[j]`'s engine ring.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Map {
    pub rx: Vec<usize>,
    pub tx: Vec<usize>,
}

/// The pump's timing, samples. `block` is above 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// B: frames per block (the device's `block`, one of [`BLOCKS`]).
    pub block: u64,
    /// P: the playout point behind media now (the device's latency).
    pub playout: u64,
    /// W: the work margin (the device's `work_margin`).
    pub margin: u64,
}

impl Timing {
    /// The pump's timing for the device `cfg`: its `block`, its latency in
    /// whole samples ([`crate::clock::latency_samples`], as the RX reads
    /// its playout point), its `work_margin`. One place, so a host's pump
    /// cannot read a sample off the RX's playout point.
    pub fn of(cfg: &crate::device::DeviceConfig) -> Timing {
        Timing {
            block: cfg.block,
            playout: crate::clock::latency_samples(cfg.latency_ns, cfg.sample_rate),
            margin: cfg.work_margin,
        }
    }

    /// D = B + P + W: an output stamped T is the input stamped T − D.
    pub fn offset(&self) -> u64 {
        self.block + self.playout + self.margin
    }

    /// When the earliest block is due, B + P: block 0's (no block starts
    /// before media time 0), the floor of every bootstrap.
    fn first_due(&self) -> u64 {
        self.block + self.playout
    }
}

/// What [`Blocks::next`] says at a media time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Run the block starting at `start`; `late` when one block or more
    /// behind its due time.
    Run { start: u64, late: bool },
    /// Nothing due before media time `until`.
    Wait { until: u64 },
    /// More than the margin behind, or more than the margin and a block
    /// ahead (`cause`, [`Cause::Late`] or [`Cause::ClockBack`]): the schedule
    /// started again at now, `skipped` blocks never ran.
    Rebootstrap { skipped: u64, cause: Cause },
}

/// Why the pump's schedule started again (dstack #281): each re-bootstrap
/// counts in the total and in its cause's count ([`PumpView`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The pump woke more than its margin W behind a block ([`Blocks::next`]).
    Late,
    /// The media clock went back: the next block more than the margin and a
    /// block ahead ([`Blocks::next`], dstack #440).
    ClockBack,
    /// A clock re-anchor whose step is unknown or off the block grid
    /// ([`Blocks::re_anchor`]'s [`Shift::Restarted`]).
    Reanchor,
    /// The device latency moved (Dante Controller set it): the pump plays at
    /// the new playout point.
    Retime,
}

/// The pump's schedule (pure): the next block's start.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Blocks {
    start: u64,
    timing: Timing,
    /// Started again by [`Blocks::next`] as a clock gone back, and no pass's
    /// re-anchor point since ([`Blocks::re_anchor`], [`Blocks::unmark`]).
    back: bool,
}

impl Blocks {
    /// The schedule at media time `now`: the first start is the smallest
    /// multiple of the block whose input ends at or after `now − playout`,
    /// so its block is due at or after `now` (no catch-up at start).
    pub fn bootstrap(now: u64, t: Timing) -> Blocks {
        let start = now.saturating_sub(t.first_due()).div_ceil(t.block) * t.block;
        Blocks {
            start,
            timing: t,
            back: false,
        }
    }

    /// The next block's first input stamp.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// The same schedule judged against the margin `margin` (how far behind
    /// a block may run before [`Blocks::next`] starts again, and the room
    /// [`Blocks::re_anchor`] moves within): the TX pull's catch-up reach
    /// for its runs, W for a re-anchor (dstack #363).
    pub fn with_margin(self, margin: u64) -> Blocks {
        Blocks {
            timing: Timing {
                margin,
                ..self.timing
            },
            ..self
        }
    }

    /// When the next block is due: its last input sample's playout time.
    pub fn due(&self) -> u64 {
        self.start + self.timing.block + self.timing.playout
    }

    /// What is due at media time `now`; a run moves the schedule on by one
    /// block, a re-bootstrap starts it again at `now` (after which nothing
    /// is behind). No schedule waits more than the margin and a block (a
    /// bootstrap or a run leaves the next block under a block ahead, a
    /// re-anchor moves it up to W and a block past now), so a block due
    /// further ahead is the clock gone back (dstack #440: a step back read
    /// under the old epoch), started again at once instead of waited out.
    /// Near media time 0 the wait is judged from the earliest due time
    /// (B + P: no block starts before 0), so a start's own wait never is.
    /// Such a restart marks the schedule for the next pass's re-anchor
    /// ([`Blocks::re_anchor`]).
    pub fn next(&mut self, now: u64) -> Due {
        let due = self.due();
        if now < due {
            let from = now.max(self.timing.first_due());
            if due - from > self.timing.margin.saturating_add(self.timing.block) {
                let skipped = self.rebootstrap(now);
                self.back = true;
                return Due::Rebootstrap {
                    skipped,
                    cause: Cause::ClockBack,
                };
            }
            return Due::Wait { until: due };
        }
        let behind = now - due;
        if behind > self.timing.margin {
            return Due::Rebootstrap {
                skipped: self.rebootstrap(now),
                cause: Cause::Late,
            };
        }
        let start = self.start;
        self.start += self.timing.block;
        Due::Run {
            start,
            late: behind >= self.timing.block,
        }
    }

    /// A re-anchor of the clock at media time `now` (after it) with the
    /// device's `shift` ([`crate::media::tx::shift`]; `None`: the step is
    /// unknown): moved by it when it is a whole number of blocks and leaves
    /// the next block due at most W and a block past now (nothing skipped,
    /// nothing counted; a rest of the step off the grid is a lag or a wait
    /// of at most half a block, which [`Blocks::next`] judges as any other,
    /// W being at least the block); kept when it would leave it further (the
    /// step before read the new time with the old epoch and already started
    /// it again there); kept too when it moves back a schedule that the
    /// pass before started again as a clock gone back ([`Blocks::next`]:
    /// that pass read the new time of this step under the old epoch, so a
    /// move would leave it the step behind and re-bootstrap it late, dstack
    /// #440); else re-bootstrapped at `now`, and the blocks skipped (the
    /// [`Blocks::rebootstrap`] count) are returned with it. It spends the
    /// mark either way.
    pub fn re_anchor(&mut self, shift: Option<i64>, now: u64) -> (Shift, u64) {
        let back = std::mem::take(&mut self.back);
        let block = self.timing.block;
        let Some(by) = shift.filter(|by| by.rem_euclid(block as i64) == 0) else {
            return (Shift::Restarted, self.rebootstrap(now));
        };
        if back && by.is_negative() {
            return (Shift::Kept, 0);
        }
        let start = self.start.wrapping_add_signed(by);
        let due = start + block + self.timing.playout;
        if due > now.saturating_add(self.timing.margin).saturating_add(block) {
            return (Shift::Kept, 0);
        }
        self.start = start;
        (Shift::Moved, 0)
    }

    /// A pass's re-anchor point with no re-anchor: a clock-back mark from
    /// the pass before is spent, so it keeps a schedule through one pass's
    /// re-anchor only (dstack #440, ROZHODNUTÉ 19.11).
    pub fn unmark(&mut self) {
        self.back = false;
    }

    /// Starts the schedule again at media time `now` (a lag, a clock that
    /// went back, a new epoch whose step is unknown); returns the blocks between the old next
    /// start and the new one that never run (0 when the new one is not
    /// later).
    pub fn rebootstrap(&mut self, now: u64) -> u64 {
        let new = Blocks::bootstrap(now, self.timing);
        let skipped = new.start.saturating_sub(self.start) / self.timing.block;
        *self = new;
        skipped
    }
}

/// A duration in ns, saturating.
fn nanos(d: Duration) -> u64 {
    u64::try_from(d.as_nanos()).unwrap_or(u64::MAX)
}

/// The engine's pump (see the module): every buffer it needs, allocated in
/// [`Pump::new`].
pub struct Pump<P: Process> {
    processor: P,
    map: Map,
    timing: Timing,
    bits: u8,
    rx: Arc<RxShared>,
    tx: Arc<TxShared>,
    stats: Arc<PumpStats>,
    frames: usize,
    /// The engine's inputs and outputs, channel-major.
    input: Vec<f64>,
    output: Vec<f64>,
    /// One channel's samples of a block.
    samples: Vec<i32>,
    /// `None` until the first media step, and after free pacing.
    blocks: Option<Blocks>,
    /// The clock's epoch at the last media step.
    epoch: Option<u32>,
    /// The engine shift at that epoch: the outputs go into the engine rings
    /// at their position less it ([`crate::media::tx::EngineLine`]).
    engine_shift: i64,
    /// `Process::discontinuity` is due before the next block.
    discontinuity: bool,
    /// The processor panicked: never called again.
    faulted: bool,
    /// The last block's start, for the interval histogram.
    last_entry: Option<Instant>,
    /// The media clock less the app's, filtered ([`app_offset`], dstack
    /// #295): no offset before the first pair read (no block's play time).
    app: OffsetFilter,
    /// The last pair read ([`Pump::read_clocks`]) not yet given to the
    /// filter: its offset and app time. [`Pump::media`] gives it, after
    /// the wake's epoch (a re-anchor's step moves the offset).
    pair: Option<(i64, i64)>,
    /// The last block's play time, for the next one's deviation
    /// ([`play_at`], dstack #305).
    played: play_at::Track,
    /// The RX rings' writer while the pump is it (the normal profile,
    /// dstack #326): drained just before each block reads the rings.
    intake: Option<Intake>,
    /// Media now in ns as the last [`Pump::read_clocks`] read it, for the
    /// drain's packet judge (finer than a sample); taken by the next
    /// [`Pump::media`] at that time.
    read_ns: Option<u64>,
    /// How late the pump thread's last sleep returned, ns ([`Pump::slept`],
    /// dstack #370): the sleep's target is the next wake, so the next
    /// [`Pump::media`] takes it for that wake's record; `None` when
    /// unmeasured. Free pacing leaves it (its sleep is not measured): the
    /// next media step bootstraps, a wake that is never late, so nothing
    /// records it.
    overslept_ns: Option<u64>,
}

impl<P: Process> Pump<P> {
    /// A pump of `processor` over `map`'s channels with `timing`, its
    /// outputs quantized at `bits`, reading `rx`'s rings and writing `tx`'s
    /// engine rings, counting into `stats`. Every buffer is allocated here.
    pub fn new(
        processor: P,
        map: Map,
        timing: Timing,
        bits: u8,
        rx: Arc<RxShared>,
        tx: Arc<TxShared>,
        stats: Arc<PumpStats>,
    ) -> Self {
        let frames = usize::try_from(timing.block).unwrap_or(usize::MAX);
        // dstack #326: in the normal profile the pump writes the RX rings
        // itself, so no helper thread's scheduling stands between a packet
        // in its socket and the block that reads it.
        let intake = (tx.cfg.timing == crate::device::TimingProfile::Normal)
            .then(|| rx.pump_intake())
            .flatten();
        Pump {
            input: vec![0.0; map.rx.len() * frames],
            output: vec![0.0; map.tx.len() * frames],
            samples: vec![0; frames],
            processor,
            map,
            timing,
            bits,
            rx,
            tx,
            stats,
            frames,
            blocks: None,
            epoch: None,
            engine_shift: 0,
            discontinuity: false,
            faulted: false,
            last_entry: None,
            app: OffsetFilter::new(),
            pair: None,
            played: play_at::Track::default(),
            intake,
            read_ns: None,
            overslept_ns: None,
        }
    }

    /// Whether the pump writes the RX rings (dstack #326: the normal
    /// profile, unless the RX thread claimed them first).
    pub fn drains(&self) -> bool {
        self.intake.is_some()
    }

    /// Drains the RX flow sockets into the rings while the pump writes
    /// them, at PTP time `now_ns` (`None`: the clock is not ready); the
    /// drain's own time and datagrams recorded (dstack #354).
    fn drain(&mut self, now_ns: Option<u64>) {
        if let Some(intake) = &mut self.intake {
            let began = Instant::now();
            intake.drain_into_rings(now_ns);
            let datagrams = u64::try_from(intake.datagrams()).unwrap_or(u64::MAX);
            self.stats.drained(nanos(began.elapsed()), datagrams);
        }
    }

    /// The pump thread slept until media time `target_ns`, reading the
    /// media clock just before (`asked_ns`) and just after (`woke_ns`) on
    /// one epoch (`same_epoch`): recorded ([`PumpStats::slept`]) and kept
    /// for the next [`Pump::media`]'s wake, which that sleep woke for
    /// (dstack #370: each late wake's own oversleep).
    pub fn slept(
        &mut self,
        target_ns: u64,
        asked_ns: Option<u64>,
        woke_ns: Option<u64>,
        same_epoch: bool,
    ) {
        self.overslept_ns = self.stats.slept(target_ns, asked_ns, woke_ns, same_epoch);
    }

    /// Reads the app's clock (`app_ns`, [`crate::clock::app_monotonic_ns`]
    /// on the pump thread), media now on `clock` and the app's clock again,
    /// back to back, and keeps the pair ([`app_offset::pair_offset`]: media
    /// with the app reads' midpoint) for the next [`Pump::media`], which
    /// gives it to the blocks' play-time filter ([`play_at_ns`], dstack
    /// #295); returns media now, `None` while the clock is not ready (the
    /// app's clock is then read once, the pair dropped and the offset
    /// kept).
    pub fn read_clocks(
        &mut self,
        clock: &dyn PtpClock,
        mut app_ns: impl FnMut() -> i64,
    ) -> Option<u64> {
        let before = app_ns();
        let now = clock.now_ns()?;
        self.pair = Some(app_offset::pair_offset(before, now, app_ns()));
        self.read_ns = Some(now);
        Some(now)
    }

    /// The wake's pair (if one was read) through the play-time filter at
    /// the wake's epoch: a rejected pair counted, a step warned.
    fn filter_pair(&mut self) {
        let Some((raw, app_ns)) = self.pair.take() else {
            return;
        };
        match self.app.update(raw, app_ns) {
            Outcome::Rejected => {
                self.stats.play_at_rejected.fetch_add(1, Ordering::Relaxed);
            }
            Outcome::Stepped { by_ns } => {
                tracing::warn!(by_ns, "{}", app_offset::STEP_LINE);
            }
            Outcome::First | Outcome::Followed => {}
        }
    }

    /// Every block due at media sample time `now` with the clock at
    /// `anchor` (its epoch and latest known re-anchor; a bare epoch knows
    /// no step); returns the media sample time to wake at. The first media
    /// step (and the first after free pacing) bootstraps; a new epoch
    /// shifts the schedule by the re-anchor's step on the block grid when
    /// the clock knows it (no block lost, no `discontinuity`: the inputs
    /// and outputs go on, [`crate::media::tx::shift`]), else re-bootstraps
    /// (counted); a bootstrap or re-bootstrap calls the processor's
    /// `discontinuity` once before the next block.
    pub fn media(&mut self, now: u64, anchor: impl Into<crate::clock::Anchor>) -> u64 {
        let anchor = anchor.into();
        let rate = self.tx.cfg.sample_rate;
        // The sleep before this wake, for its record (dstack #370).
        let overslept_ns = self.overslept_ns.take();
        // The drain's time: the clock read of this wake when it is `now`,
        // else `now` itself.
        let drain_ns = self
            .read_ns
            .take()
            .filter(|&ns| to_samples(ns, rate) == now)
            .unwrap_or_else(|| ns_of(now, rate));
        let before = self.engine_shift;
        if self.epoch != Some(anchor.epoch) {
            self.engine_shift = self
                .tx
                .engine_line
                .shift_at(anchor, rate, self.timing.block);
            // A re-anchor moves the media clock, so the offset, by its
            // step (and the blocks' media start with it, and the play-time
            // check's last start by the shift; an unknown step starts it
            // afresh).
            if let Some(from) = self.epoch {
                match anchor.step_from(from) {
                    Some(step_ns) => {
                        self.app.shift(step_ns);
                        self.played.shift(self.engine_shift.wrapping_sub(before));
                    }
                    None => {
                        self.app.restart();
                        self.played = play_at::Track::default();
                    }
                }
            }
        }
        self.filter_pair();
        self.stats.media.store(true, Ordering::Relaxed);
        // The device latency the RX delays are planned against moved (Dante
        // Controller set it): play at it, the schedule started again.
        if let Some(playout) = self.rx.playout()
            && playout != self.timing.playout
        {
            self.timing.playout = playout;
            self.stats.retimed(self.timing);
            if self.blocks.take().is_some() {
                self.stats.rebootstrapped(0, Cause::Retime);
            }
        }
        let mut blocks = match self.blocks {
            Some(mut b) if self.epoch != Some(anchor.epoch) => {
                let step = self.epoch.and_then(|e| anchor.step_from(e));
                let shift = step.map(|_| self.engine_shift.wrapping_sub(before));
                let (outcome, skipped) = b.re_anchor(shift, now);
                if outcome == Shift::Restarted {
                    self.stats.rebootstrapped(skipped, Cause::Reanchor);
                }
                // The inputs jump when the senders stepped by other than
                // the shift (a step off the block grid): the processor
                // hears of it. A step the shift rounds to 0 is a local
                // correction, where the senders did not step.
                self.discontinuity |= outcome == Shift::Restarted
                    || step.is_some_and(|d| inputs_jump(d, rate, shift));
                tracing::info!(
                    epoch = anchor.epoch,
                    step_ns = ?step,
                    shift = ?shift,
                    engine_shift = self.engine_shift,
                    ?outcome,
                    skipped,
                    "{}",
                    crate::media::tx::shift::PUMP_LINE
                );
                b
            }
            Some(mut b) => {
                b.unmark();
                b
            }
            None => {
                self.discontinuity = true;
                Blocks::bootstrap(now, self.timing)
            }
        };
        self.epoch = Some(anchor.epoch);
        // The wake's lateness is its first answer's: how far behind the
        // oldest due block it woke (the blocks after it are a catch-up).
        let mut first = true;
        let wake = loop {
            let behind = now.saturating_sub(blocks.due());
            let due = blocks.next(now);
            if std::mem::take(&mut first) {
                self.stats.woke(behind, due, overslept_ns);
            }
            match due {
                Due::Run { start, late } => {
                    if late {
                        self.stats.late.fetch_add(1, Ordering::Relaxed);
                    }
                    self.drain(Some(drain_ns));
                    self.cpu_timed(|pump| pump.block(start));
                }
                Due::Wait { until } => break until,
                Due::Rebootstrap { skipped, cause } => {
                    self.stats.rebootstrapped(skipped, cause);
                    self.discontinuity = true;
                }
            }
        };
        self.blocks = Some(blocks);
        wake
    }

    /// One block while the clock is not ready: silent inputs, the RX rings
    /// not read, the outputs dropped (nothing reaches the engine rings);
    /// the next media step bootstraps.
    pub fn free(&mut self) {
        self.stats.media.store(false, Ordering::Relaxed);
        self.blocks = None;
        // Packets keep coming: read (counted unplaced), never left to fill
        // their sockets.
        self.drain(None);
        self.cpu_timed(|pump| {
            let entry = pump.enter();
            pump.input.fill(0.0);
            let fault = pump.run_processor(None, None);
            pump.leave(entry, fault);
        });
    }

    /// Runs `block` (one block) and records the thread's CPU time over it
    /// ([`split::thread_cpu_ns`], dstack #354): read outside
    /// [`Pump::block`], as on Linux it is a system call (no vDSO).
    fn cpu_timed(&mut self, block: impl FnOnce(&mut Self)) {
        let before = split::thread_cpu_ns();
        block(self);
        if let Some(cpu) = split::cpu_between(before, split::thread_cpu_ns()) {
            self.stats.record_cpu(cpu);
        }
    }

    /// The block whose inputs are stamped `start`…`start + B − 1` (the
    /// real-time part): each routed RX channel's ring read there (an
    /// unrouted one is silence and reads nothing), the processor, and each
    /// output quantized into its TX channel's engine ring at `start + D`
    /// (on the rings' line: less the engine shift).
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    pub fn block(&mut self, start: u64) {
        let entry = self.enter();
        let rings: &[Ring] = self.rx.rings();
        for (chan, &rx) in self.input.chunks_exact_mut(self.frames).zip(&self.map.rx) {
            match rings.get(rx).filter(|_| self.rx.routed(rx)) {
                Some(ring) => {
                    // A flow that needs a later playout (its fpp, its
                    // transmitter's latency) is read that much later.
                    ring.read_at(start.saturating_sub(self.rx.delay(rx)), &mut self.samples);
                    for (x, &s) in chan.iter_mut().zip(self.samples.iter()) {
                        *x = to_f64(s);
                    }
                }
                None => chan.fill(0.0),
            }
        }
        let rate = self.tx.cfg.sample_rate;
        let play_at = self
            .app
            .offset()
            .map(|offset| play_at_ns(start, self.timing, rate, offset));
        if let Some(dev) = self.played.next(start, play_at, rate) {
            self.stats.play_at_moved(dev);
        }
        let fault = self.run_processor(Some(start), play_at);
        let at =
            (start + self.timing.offset()).wrapping_add_signed(self.engine_shift.wrapping_neg());
        let mut after_send = 0;
        for (chan, &tx) in self.output.chunks_exact(self.frames).zip(&self.map.tx) {
            if let Some(ring) = self.tx.engine_ring(tx) {
                for (q, &y) in self.samples.iter_mut().zip(chan) {
                    *q = quantize(y, self.bits);
                }
                after_send += ring.put_at(at, &self.samples);
            }
        }
        if after_send > 0 {
            self.stats.wrote_after_send();
        }
        self.leave(entry, fault);
    }

    /// A block's start: the interval since the last one's.
    fn enter(&mut self) -> Instant {
        let now = Instant::now();
        if let Some(before) = self.last_entry.replace(now) {
            self.stats
                .hists
                .interval
                .record(nanos(now.saturating_duration_since(before)));
        }
        now
    }

    /// A block's end: its own time recorded, and a fault after it.
    fn leave(&mut self, entry: Instant, fault: Option<Box<dyn Any + Send>>) {
        let took = nanos(entry.elapsed());
        self.stats.record(took);
        if let Some(payload) = fault {
            self.stats.fault(panic_message(&*payload), took);
        }
    }

    /// The processor on the block's buffers (the outputs zeroed first, a
    /// pending discontinuity before it) inside `catch_unwind`. A panic
    /// zeroes the outputs and is returned; from then on the processor is
    /// never called again and the outputs stay zero.
    /// The block carries its media `start` ([`Block::start`]; `None` while
    /// free pacing) and its `play_at` ([`Block::play_at`]; `None` while
    /// free pacing or before the first pair read).
    fn run_processor(
        &mut self,
        start: Option<u64>,
        play_at: Option<i64>,
    ) -> Option<Box<dyn Any + Send>> {
        self.output.fill(0.0);
        if self.faulted {
            return None;
        }
        let discontinuity = std::mem::take(&mut self.discontinuity);
        let Pump {
            processor,
            input,
            output,
            frames,
            ..
        } = self;
        let caught = catch_unwind(AssertUnwindSafe(|| {
            if discontinuity {
                processor.discontinuity();
            }
            let mut block = Block::new(*frames, input, output);
            if let Some(at) = start {
                block = block.at(at);
            }
            if let Some(ns) = play_at {
                block = block.played_at(ns);
            }
            processor.process(&mut block);
        }));
        let payload = caught.err()?;
        self.output.fill(0.0);
        self.faulted = true;
        Some(payload)
    }
}

/// The play time of the block whose inputs start at `start` (samples at
/// `rate`) in the app's clock, ns (dstack #295): `start + P + B`, its
/// first sample's stamp plus the device's RX path P + (c) ((c) = B without
/// a converter; the block's due time), as a media time, less `offset_ns`
/// (the media clock less the app's, from the pairs read back to back,
/// filtered: [`Pump::read_clocks`], [`app_offset`]).
pub fn play_at_ns(start: u64, timing: Timing, rate: u32, offset_ns: i64) -> i64 {
    let media = ns_of(start + timing.playout + timing.block, rate);
    i64::try_from(media)
        .unwrap_or(i64::MAX)
        .saturating_sub(offset_ns)
}

/// Whether a pump moved by `shift` (`None`: not moved) reads its inputs
/// off by some samples after a re-anchor of `step_ns` at `rate`: the
/// senders' stamps moved by the step in whole samples, the pump by the
/// shift. A shift of 0 is a local correction (the senders did not step).
pub fn inputs_jump(step_ns: i64, rate: u32, shift: Option<i64>) -> bool {
    shift.is_some_and(|s| s != 0 && s != crate::clock::grid_shift(step_ns, rate, 1))
}

pub mod app_offset;
pub mod play_at;
pub mod split;
mod stats;
pub use stats::{PumpStats, PumpView, SecondSplit};
mod thread;
pub use thread::{free_deadline, spawn};
pub mod wake;
pub mod watch;

#[cfg(test)]
mod tests;

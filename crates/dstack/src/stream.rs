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
//!   side's fpp.
//! - [`Blocks`] (pure) says what is due: a block, a wait, or a
//!   re-bootstrap when the pump is more than W behind (the skipped blocks'
//!   stamps go out as silence, counted as TX holes) or more than
//!   [`DISCONTINUITY`] ahead (the clock went back). A start never catches
//!   up: the first block is the one whose input ends at the playout point.
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
//!   as [`StreamStats`], the stream histograms ([`HistSnapshot`]) and a
//!   [`PumpView`]: each re-bootstrap with its [`Cause`], and how late the
//!   pump woke ([`wake`], dstack #281).
//! - [`app_offset`]: the media clock's offset from the app's clock that
//!   stamps each block's play time, filtered (dstack #295).
//! - [`watch::PumpWatch`]: once a second, from the daemon's tick (never
//!   the pump thread), one warning when re-bootstraps, skipped blocks or
//!   overruns grew in that second.
//! - [`spawn`]: the `dstack-engine` thread (`SCHED_FIFO` [`ENGINE_PRIORITY`]
//!   when allowed): on the media clock while it is ready, paced on its own
//!   ([`free_deadline`], NullRt's rule) while it is not.
//!
//! A sample is a left-justified `i32` (full scale 2³¹, as the rings keep
//! them): [`to_f64`] scales by 2⁻³¹ exactly, [`quantize`] rounds at the
//! flow's bits and saturates at full scale.

use std::any::Any;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use dstack_audio_io::hist::{HistSnapshot, StreamHists};
use dstack_audio_io::telemetry::period_ns;
use dstack_audio_io::{Block, Process, StreamStats, panic_message};

use crate::clock::{PtpClock, to_samples};
use crate::media::repin::RePin;
use crate::media::ring::Ring;
use crate::media::rx::RxShared;
use crate::media::tx::{DISCONTINUITY, MediaThread, Shift, TxShared, ns_of, place, take_priority};
use app_offset::{OffsetFilter, Outcome};
use wake::WakeLateness;

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
}

/// What [`Blocks::next`] says at a media time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Due {
    /// Run the block starting at `start`; `late` when one block or more
    /// behind its due time.
    Run { start: u64, late: bool },
    /// Nothing due before media time `until`.
    Wait { until: u64 },
    /// More than the margin behind, or more than [`DISCONTINUITY`] ahead
    /// (`cause`, [`Cause::Late`] or [`Cause::ClockBack`]): the schedule
    /// started again at now, `skipped` blocks never ran.
    Rebootstrap { skipped: u64, cause: Cause },
}

/// Why the pump's schedule started again (dstack #281): each re-bootstrap
/// counts in the total and in its cause's count ([`PumpView`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cause {
    /// The pump woke more than its margin W behind a block ([`Blocks::next`]).
    Late,
    /// The media clock went back more than [`DISCONTINUITY`] ([`Blocks::next`]).
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
}

impl Blocks {
    /// The schedule at media time `now`: the first start is the smallest
    /// multiple of the block whose input ends at or after `now − playout`,
    /// so its block is due at or after `now` (no catch-up at start).
    pub fn bootstrap(now: u64, t: Timing) -> Blocks {
        let start = now.saturating_sub(t.playout + t.block).div_ceil(t.block) * t.block;
        Blocks { start, timing: t }
    }

    /// The next block's first input stamp.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// When the next block is due: its last input sample's playout time.
    pub fn due(&self) -> u64 {
        self.start + self.timing.block + self.timing.playout
    }

    /// What is due at media time `now`; a run moves the schedule on by one
    /// block, a re-bootstrap starts it again at `now` (after which nothing
    /// is behind).
    pub fn next(&mut self, now: u64) -> Due {
        let due = self.due();
        if now < due {
            if due - now > DISCONTINUITY {
                return Due::Rebootstrap {
                    skipped: self.rebootstrap(now),
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
    /// it again there); else re-bootstrapped at `now`, and the blocks
    /// skipped (the [`Blocks::rebootstrap`] count) are returned with it.
    pub fn re_anchor(&mut self, shift: Option<i64>, now: u64) -> (Shift, u64) {
        let block = self.timing.block;
        let Some(by) = shift.filter(|by| by.rem_euclid(block as i64) == 0) else {
            return (Shift::Restarted, self.rebootstrap(now));
        };
        let start = self.start.wrapping_add_signed(by);
        let due = start + block + self.timing.playout;
        if due > now.saturating_add(self.timing.margin).saturating_add(block) {
            return (Shift::Kept, 0);
        }
        self.start = start;
        (Shift::Moved, 0)
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

/// The pump's counts: written by its thread, read by the engine's control
/// thread and the device's control socket.
pub struct PumpStats {
    frames: u32,
    /// D and P, samples: the pump's timing, moved when the device latency
    /// changes ([`PumpStats::retimed`]).
    offset: AtomicU64,
    playout: AtomicU64,
    /// W, samples: fixed for the pump's life (a new latency moves P and D).
    margin: u64,
    /// One block's period, ns: a block longer than this is an overrun.
    period_ns: u64,
    /// The last step was on the media clock (not free pacing).
    media: AtomicBool,
    running: AtomicBool,
    faulted: AtomicBool,
    blocks: AtomicU64,
    late: AtomicU64,
    missed: AtomicU64,
    overruns: AtomicU64,
    /// The re-bootstraps by [`Cause`] (dstack #281); their sum is the
    /// total, so a reader's total and causes always agree.
    rebootstraps_late: AtomicU64,
    rebootstraps_clock_back: AtomicU64,
    rebootstraps_reanchor: AtomicU64,
    rebootstraps_retime: AtomicU64,
    /// The sample rate, Hz: a lateness in samples as ns.
    rate: u32,
    /// How late the pump woke ([`wake`], dstack #281).
    wakes: WakeLateness,
    max_process_ns: AtomicU64,
    /// The longest block since the pump watch's last look, ns
    /// ([`PumpStats::take_second_max_us`]; dstack #1: the watch's line
    /// reports that second's, not the longest since the start).
    second_max_ns: AtomicU64,
    /// The faulting block's own time; stored before `faulted`.
    fault_ns: AtomicU64,
    fault: Mutex<Option<String>>,
    hists: StreamHists,
    /// Pair reads the play-time filter rejected ([`app_offset`], dstack
    /// #295).
    play_at_rejected: AtomicU64,
}

/// The pump's numbers for the device's control socket (numbers only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PumpView {
    /// On the media clock (not free pacing).
    pub media: bool,
    /// D, samples.
    pub offset: u64,
    /// P, samples.
    pub playout: u64,
    pub blocks: u64,
    pub late: u64,
    /// Blocks skipped by re-bootstraps.
    pub missed: u64,
    /// Blocks longer than one period.
    pub overruns: u64,
    pub rebootstraps: u64,
    /// The longest block, whole µs.
    pub max_process_us: u64,
    /// The re-bootstraps by [`Cause`] (dstack #281): they add up to
    /// `rebootstraps`.
    pub rebootstraps_late: u64,
    pub rebootstraps_clock_back: u64,
    pub rebootstraps_reanchor: u64,
    pub rebootstraps_retime: u64,
    /// The latest wake since the start, whole µs: how far behind its due
    /// time the oldest block it found due was ([`wake`]).
    pub wake_behind_max_us: u64,
    /// The last whole minute's latest wake, whole µs (`None` before the
    /// pump watch took a minute).
    pub wake_behind_minute_max_us: Option<u64>,
    /// Pair reads of the media and the app's clock that the play-time
    /// filter rejected as torn ([`app_offset`], dstack #295).
    pub play_at_rejected: u64,
}

impl PumpStats {
    /// The counts of a pump with `timing` at `rate` Hz; the histograms are
    /// sized for one block at the rate (allocated here, before the thread).
    pub fn new(timing: Timing, rate: u32) -> Self {
        let frames = u32::try_from(timing.block).unwrap_or(u32::MAX);
        let period = period_ns(frames, f64::from(rate.max(1)));
        PumpStats {
            frames,
            offset: AtomicU64::new(timing.offset()),
            playout: AtomicU64::new(timing.playout),
            margin: timing.margin,
            period_ns: period,
            media: AtomicBool::new(false),
            running: AtomicBool::new(false),
            faulted: AtomicBool::new(false),
            blocks: AtomicU64::new(0),
            late: AtomicU64::new(0),
            missed: AtomicU64::new(0),
            overruns: AtomicU64::new(0),
            rebootstraps_late: AtomicU64::new(0),
            rebootstraps_clock_back: AtomicU64::new(0),
            rebootstraps_reanchor: AtomicU64::new(0),
            rebootstraps_retime: AtomicU64::new(0),
            rate,
            wakes: WakeLateness::new(),
            max_process_ns: AtomicU64::new(0),
            second_max_ns: AtomicU64::new(0),
            fault_ns: AtomicU64::new(0),
            fault: Mutex::new(None),
            hists: StreamHists::new(period),
            play_at_rejected: AtomicU64::new(0),
        }
    }

    /// One block's own time (the pump thread): counted, the longest kept
    /// (since the start and since the watch's last look), its histogram,
    /// an overrun when longer than one period.
    fn record(&self, took_ns: u64) {
        self.blocks.fetch_add(1, Ordering::Relaxed);
        self.max_process_ns.fetch_max(took_ns, Ordering::Relaxed);
        self.second_max_ns.fetch_max(took_ns, Ordering::Relaxed);
        self.hists.process.record(took_ns);
        if took_ns > self.period_ns {
            self.overruns.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// The longest block since the last call, whole µs, and a fresh start
    /// for the next (the pump watch, once a second).
    pub(crate) fn take_second_max_us(&self) -> u64 {
        self.second_max_ns.swap(0, Ordering::Relaxed) / 1_000
    }

    /// The pump plays at `timing` now (its W unchanged).
    fn retimed(&self, timing: Timing) {
        self.offset.store(timing.offset(), Ordering::Relaxed);
        self.playout.store(timing.playout, Ordering::Relaxed);
    }

    /// A re-bootstrap for `cause` that skipped `skipped` blocks: counted
    /// in its cause's count (the total is their sum).
    pub(crate) fn rebootstrapped(&self, skipped: u64, cause: Cause) {
        self.missed.fetch_add(skipped, Ordering::Relaxed);
        let by_cause = match cause {
            Cause::Late => &self.rebootstraps_late,
            Cause::ClockBack => &self.rebootstraps_clock_back,
            Cause::Reanchor => &self.rebootstraps_reanchor,
            Cause::Retime => &self.rebootstraps_retime,
        };
        by_cause.fetch_add(1, Ordering::Relaxed);
    }

    /// A wake on the media clock whose first due answer was `due`, `behind`
    /// samples after the oldest due block's time (the pump thread): a run
    /// or a re-bootstrap for lateness is recorded as the wake's lateness
    /// (late from one block behind, as [`Blocks::next`] counts it); a wait
    /// (nothing due yet) or a clock gone back is no lateness.
    fn woke(&self, behind: u64, due: Due) {
        let rebootstrap = match due {
            Due::Run { .. } => false,
            Due::Rebootstrap {
                cause: Cause::Late, ..
            } => true,
            Due::Wait { .. } | Due::Rebootstrap { .. } => return,
        };
        let late = behind >= u64::from(self.frames);
        self.wakes.record(
            ns_of(behind, self.rate),
            late,
            rebootstrap,
            wake::unix_ms_now,
        );
    }

    /// How late the pump woke: the buckets, the recent late wakes
    /// ([`WakeLateness`]).
    pub fn wakes(&self) -> &WakeLateness {
        &self.wakes
    }

    /// The processor panicked in a block of `took_ns`: its message and its
    /// time, then the flag (a reader that sees the fault sees both).
    fn fault(&self, message: String, took_ns: u64) {
        *self.fault.lock().unwrap_or_else(PoisonError::into_inner) = Some(message);
        self.fault_ns.store(took_ns.max(1), Ordering::Release);
        self.faulted.store(true, Ordering::Release);
    }

    /// The pump as a stream: frames B, callbacks the blocks, late, missed
    /// the skipped blocks, overruns, resets the re-bootstraps; no card, so
    /// no reopen.
    pub fn stream_stats(&self) -> StreamStats {
        let faulted = self.faulted.load(Ordering::Acquire);
        StreamStats {
            frames: self.frames,
            callbacks: self.blocks.load(Ordering::Relaxed),
            late: self.late.load(Ordering::Relaxed),
            missed: self.missed.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            resets: self.view().rebootstraps,
            faulted,
            running: self.running.load(Ordering::Acquire),
            max_process_ns: self.max_process_ns.load(Ordering::Relaxed),
            fault: self
                .fault
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
            last_reopen_us: 0,
            // Read after `faulted`, which is stored after it.
            fault_callback_ns: self.fault_ns.load(Ordering::Acquire),
        }
    }

    /// The stream histograms since the start: the interval between two
    /// blocks' starts and each block's own time.
    pub fn histograms(&self) -> HistSnapshot {
        self.hists.snapshot()
    }

    /// The pump's timing now: B and W (fixed) and P (moved with the
    /// device latency); one load, so B + P + W is always a D the pump had.
    pub fn timing(&self) -> Timing {
        Timing {
            block: u64::from(self.frames),
            playout: self.playout.load(Ordering::Relaxed),
            margin: self.margin,
        }
    }

    /// The numbers the device's control socket shows.
    pub fn view(&self) -> PumpView {
        let late = self.rebootstraps_late.load(Ordering::Relaxed);
        let clock_back = self.rebootstraps_clock_back.load(Ordering::Relaxed);
        let reanchor = self.rebootstraps_reanchor.load(Ordering::Relaxed);
        let retime = self.rebootstraps_retime.load(Ordering::Relaxed);
        PumpView {
            media: self.media.load(Ordering::Relaxed),
            offset: self.offset.load(Ordering::Relaxed),
            playout: self.playout.load(Ordering::Relaxed),
            blocks: self.blocks.load(Ordering::Relaxed),
            late: self.late.load(Ordering::Relaxed),
            missed: self.missed.load(Ordering::Relaxed),
            overruns: self.overruns.load(Ordering::Relaxed),
            rebootstraps: late + clock_back + reanchor + retime,
            max_process_us: self.max_process_ns.load(Ordering::Relaxed) / 1_000,
            rebootstraps_late: late,
            rebootstraps_clock_back: clock_back,
            rebootstraps_reanchor: reanchor,
            rebootstraps_retime: retime,
            wake_behind_max_us: self.wakes.max_us(),
            wake_behind_minute_max_us: self.wakes.minute_max_us(),
            play_at_rejected: self.play_at_rejected.load(Ordering::Relaxed),
        }
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
        }
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
        let before = self.engine_shift;
        if self.epoch != Some(anchor.epoch) {
            self.engine_shift = self
                .tx
                .engine_line
                .shift_at(anchor, rate, self.timing.block);
            // A re-anchor moves the media clock, so the offset, by its
            // step (and the blocks' media start with it).
            if let Some(from) = self.epoch {
                match anchor.step_from(from) {
                    Some(step_ns) => self.app.shift(step_ns),
                    None => self.app.restart(),
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
            Some(b) => b,
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
                self.stats.woke(behind, due);
            }
            match due {
                Due::Run { start, late } => {
                    if late {
                        self.stats.late.fetch_add(1, Ordering::Relaxed);
                    }
                    self.block(start);
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
        let entry = self.enter();
        self.input.fill(0.0);
        let fault = self.run_processor(None, None);
        self.leave(entry, fault);
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
        let fault = self.run_processor(Some(start), play_at);
        let at =
            (start + self.timing.offset()).wrapping_add_signed(self.engine_shift.wrapping_neg());
        for (chan, &tx) in self.output.chunks_exact(self.frames).zip(&self.map.tx) {
            if let Some(ring) = self.tx.engine_ring(tx) {
                for (q, &y) in self.samples.iter_mut().zip(chan) {
                    *q = quantize(y, self.bits);
                }
                ring.put_at(at, &self.samples);
            }
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

/// The deadline of the free block after one due at `deadline`, at `now`: a
/// period later; more than [`FREE_RESYNC_BLOCKS`] periods behind it, `now`
/// (no burst; NullRt's rule).
pub fn free_deadline(deadline: Instant, now: Instant, period: Duration) -> Instant {
    let next = deadline + period;
    if now > next + period * FREE_RESYNC_BLOCKS {
        now
    } else {
        next
    }
}

/// Starts the `dstack-engine` thread: `pump` on `clock` until `stop` is set
/// (within one block, or one free period, of it).
pub fn spawn<P: Process + 'static>(
    clock: Arc<dyn PtpClock>,
    pump: Pump<P>,
    stop: Arc<AtomicBool>,
) -> io::Result<JoinHandle<()>> {
    let stats = Arc::clone(&pump.stats);
    stats.running.store(true, Ordering::Release);
    let spawned = std::thread::Builder::new()
        .name("dstack-engine".to_string())
        .spawn(move || run(clock.as_ref(), pump, &stop));
    if spawned.is_err() {
        stats.running.store(false, Ordering::Release);
    }
    spawned
}

/// The pump thread's loop (glue: every decision is [`Pump`]'s, [`Blocks`]'
/// and [`free_deadline`]'s): on the media clock while it is ready, paced on
/// `Instant` deadlines a block apart while it is not.
fn run<P: Process>(clock: &dyn PtpClock, mut pump: Pump<P>, stop: &AtomicBool) {
    take_priority(MediaThread::Pump, ENGINE_PRIORITY, pump.tx.scheduling());
    place("engine pump", pump.tx.cfg.pump_cpu);
    let rate = pump.tx.cfg.sample_rate;
    // The bounded escape (dstack #1, Windows): only when W covers its block.
    let escapes = crate::clock::pump_may_escape(pump.timing.margin, pump.timing.block, rate);
    #[cfg(any(target_os = "linux", windows))]
    crate::clock::allow_escape(escapes);
    if let Some(line) = crate::clock::pump_escape_line(
        crate::clock::escape_on(),
        escapes,
        pump.timing.margin,
        pump.timing.block,
    ) {
        tracing::info!("{line}");
    }
    let period = Duration::from_nanos(pump.stats.period_ns);
    let mut deadline: Option<Instant> = None;
    let mut pin = RePin::new(pump.tx.cfg.pump_cpu);
    while !stop.load(Ordering::Acquire) {
        pin.keep(pump.tx.now_ns());
        // The epoch before the time (`crate::media::tx::shift`).
        let anchor = crate::clock::Anchor::of(clock);
        // The app's clock, media now and the app's clock again, back to
        // back (dstack #295).
        #[cfg(any(target_os = "linux", windows))]
        let now = pump.read_clocks(clock, crate::clock::app_monotonic_ns);
        #[cfg(not(any(target_os = "linux", windows)))]
        let now = clock.now_ns();
        if let Some(now_ns) = now {
            deadline = None;
            let wake = pump.media(to_samples(now_ns, rate), anchor);
            clock.sleep_until(ns_of(wake, rate));
        } else {
            pump.free();
            let now = Instant::now();
            let next = free_deadline(deadline.unwrap_or(now), now, period);
            deadline = Some(next);
            std::thread::sleep(next.saturating_duration_since(now));
        }
    }
    pump.stats.running.store(false, Ordering::Release);
}

pub mod app_offset;
pub mod wake;
pub mod watch;

#[cfg(test)]
mod tests;

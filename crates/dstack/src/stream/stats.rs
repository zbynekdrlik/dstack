// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The pump's counts ([`PumpStats`]) and the numbers its view gives the
//! engine's status and the device's control socket ([`PumpView`]): written
//! by the pump's thread, read by the control side. Moved out of the
//! pump's module unchanged (dstack #346), so the pump itself stays under
//! its size budget.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};

use dstack_audio_io::StreamStats;
use dstack_audio_io::hist::{HistSnapshot, StreamHists};
use dstack_audio_io::telemetry::period_ns;

use super::wake::{self, WakeLateness};
use super::{Cause, Due, Timing};
use crate::media::tx::ns_of;

/// The pump's counts: written by its thread, read by the engine's control
/// thread and the device's control socket.
pub struct PumpStats {
    pub(super) frames: u32,
    /// D and P, samples: the pump's timing, moved when the device latency
    /// changes ([`PumpStats::retimed`]).
    pub(super) offset: AtomicU64,
    pub(super) playout: AtomicU64,
    /// W, samples: fixed for the pump's life (a new latency moves P and D).
    pub(super) margin: u64,
    /// One block's period, ns: a block longer than this is an overrun.
    pub(super) period_ns: u64,
    /// The last step was on the media clock (not free pacing).
    pub(super) media: AtomicBool,
    pub(super) running: AtomicBool,
    pub(super) faulted: AtomicBool,
    pub(super) blocks: AtomicU64,
    pub(super) late: AtomicU64,
    pub(super) missed: AtomicU64,
    pub(super) overruns: AtomicU64,
    /// The re-bootstraps by [`Cause`] (dstack #281); their sum is the
    /// total, so a reader's total and causes always agree.
    pub(super) rebootstraps_late: AtomicU64,
    pub(super) rebootstraps_clock_back: AtomicU64,
    pub(super) rebootstraps_reanchor: AtomicU64,
    pub(super) rebootstraps_retime: AtomicU64,
    /// The sample rate, Hz: a lateness in samples as ns.
    pub(super) rate: u32,
    /// How late the pump woke ([`wake`], dstack #281).
    pub(super) wakes: WakeLateness,
    pub(super) max_process_ns: AtomicU64,
    /// The longest block since the pump watch's last look, ns
    /// ([`PumpStats::take_second_max_us`]; dstack #1: the watch's line
    /// reports that second's, not the longest since the start).
    pub(super) second_max_ns: AtomicU64,
    /// The faulting block's own time; stored before `faulted`.
    pub(super) fault_ns: AtomicU64,
    pub(super) fault: Mutex<Option<String>>,
    pub(super) hists: StreamHists,
    /// Pair reads the play-time filter rejected ([`app_offset`], dstack
    /// #295).
    pub(super) play_at_rejected: AtomicU64,
    /// Blocks whose play time moved over [`super::play_at::STEP_NS`] off their
    /// media time, and the largest deviation, ns (dstack #305).
    pub(super) play_at_steps: AtomicU64,
    pub(super) play_at_max_dev_ns: AtomicU64,
    /// Blocks with an output the TX thread had already sent as silence
    /// (dstack #346, [`PumpView::writes_after_send`]).
    pub(super) writes_after_send: AtomicU64,
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
    /// filter rejected as torn ([`super::app_offset`], dstack #295).
    pub play_at_rejected: u64,
    /// Blocks whose play time moved more than [`super::play_at::STEP_NS`]
    /// off the media time since the block before ([`super::play_at`],
    /// dstack #305).
    pub play_at_steps: u64,
    /// The largest such deviation since the start, whole µs.
    pub play_at_max_dev_us: u64,
    /// Blocks with an output the TX thread had already sent as silence
    /// (dstack #346): it read some of the block's stamps before the block
    /// wrote them ([`crate::media::ring::Ring::put_at`]), so they went out
    /// as its lane's `holes`, whatever the cause. Most often a block that woke within W
    /// (no re-bootstrap, no skipped block) but ended past W + fpp + the TX
    /// margin of its due time; also the first block after a lower device
    /// latency (a smaller D) or a re-anchor, landing on stamps already
    /// sent.
    pub writes_after_send: u64,
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
            play_at_steps: AtomicU64::new(0),
            play_at_max_dev_ns: AtomicU64::new(0),
            writes_after_send: AtomicU64::new(0),
        }
    }

    /// One block's own time (the pump thread): counted, the longest kept
    /// (since the start and since the watch's last look), its histogram,
    /// an overrun when longer than one period.
    pub(super) fn record(&self, took_ns: u64) {
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
    pub(super) fn retimed(&self, timing: Timing) {
        self.offset.store(timing.offset(), Ordering::Relaxed);
        self.playout.store(timing.playout, Ordering::Relaxed);
    }

    /// A re-bootstrap for `cause` that skipped `skipped` blocks: counted
    /// in its cause's count (the total is their sum).
    ///
    /// The cause is counted FIRST and `missed` is added with `Release`
    /// (dstack #332): a reader that loads `missed` with `Acquire` before
    /// the causes ([`PumpStats::view`]) never sees skipped blocks whose
    /// cause is not counted yet.
    pub(crate) fn rebootstrapped(&self, skipped: u64, cause: Cause) {
        let by_cause = match cause {
            Cause::Late => &self.rebootstraps_late,
            Cause::ClockBack => &self.rebootstraps_clock_back,
            Cause::Reanchor => &self.rebootstraps_reanchor,
            Cause::Retime => &self.rebootstraps_retime,
        };
        by_cause.fetch_add(1, Ordering::Relaxed);
        self.missed.fetch_add(skipped, Ordering::Release);
    }

    /// A wake on the media clock whose first due answer was `due`, `behind`
    /// samples after the oldest due block's time (the pump thread): a run
    /// or a re-bootstrap for lateness is recorded as the wake's lateness
    /// (late from one block behind, as [`Blocks::next`] counts it); a wait
    /// (nothing due yet) or a clock gone back is no lateness.
    pub(super) fn woke(&self, behind: u64, due: Due) {
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

    /// A block with an output the TX thread had already sent as silence
    /// (the pump thread; dstack #346, [`PumpView::writes_after_send`]).
    pub(crate) fn wrote_after_send(&self) {
        self.writes_after_send.fetch_add(1, Ordering::Relaxed);
    }

    /// How late the pump woke: the buckets, the recent late wakes
    /// ([`WakeLateness`]).
    pub fn wakes(&self) -> &WakeLateness {
        &self.wakes
    }

    /// The processor panicked in a block of `took_ns`: its message and its
    /// time, then the flag (a reader that sees the fault sees both).
    pub(super) fn fault(&self, message: String, took_ns: u64) {
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
            missed: self.missed.load(Ordering::Acquire),
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
        // `missed` first, with Acquire: every cause counted before those
        // blocks is visible below (dstack #332, [`PumpStats::rebootstrapped`]).
        let missed = self.missed.load(Ordering::Acquire);
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
            missed,
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
            play_at_steps: self.play_at_steps.load(Ordering::Relaxed),
            play_at_max_dev_us: self.play_at_max_dev_ns.load(Ordering::Relaxed) / 1_000,
            writes_after_send: self.writes_after_send.load(Ordering::Relaxed),
        }
    }
}

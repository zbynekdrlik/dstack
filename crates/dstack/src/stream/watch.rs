// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The pump watch (dstack #1, intercom 4.11.2026): once a second, when the
//! engine's pump re-bootstrapped, skipped blocks or overran in that second,
//! one warning line with the device, the counts that grew, that second's
//! longest block and the pump's timing (B, W, P, D). The pump only counts
//! ([`super::PumpStats`]); this runs on the daemon's tick, never on the
//! real-time pump thread, and never logs per event. A gap in an app's RX or
//! a TX hole in the same second as this line with `rebootstraps` or
//! `missed` grown is the pump falling more than W behind its block; with
//! only `overruns` grown, blocks took longer than their period (an
//! unoptimised converter in a debug build, dstack #1). A gap without the
//! line is the network or the RX/TX path.
//!
//! The line names the re-bootstraps' causes (`cause`, [`cause_word`]:
//! `late`, `clock_back`, `reanchor`, `retime`, joined by `+`; `none` when
//! only overruns grew) and that second's latest wake (`behind_max_us`,
//! [`super::wake`], dstack #281). Every [`MINUTE_NS`] the watch also ends
//! the pump's wake minute ([`super::wake::WakeLateness::take_minute`]),
//! which the status shows, and logs one info line ([`REJECTED_LINE`]) when
//! the play-time filter rejected torn clock pairs in that minute
//! (`play_at_rejected`, [`super::app_offset`], dstack #295).

use tracing::{info, warn};

use super::app_offset::REJECTED_LINE;
use super::{PumpStats, PumpView};
use crate::media::late::{EVERY_NS, MINUTE_NS};

/// The text of the pump watch's warning: the line a consumer greps for.
pub const PUMP_WARNING: &str = "engine pump lost or overran blocks this second";

/// What grew in the pump's counts since the last look.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Grew {
    /// Times the schedule started again (more than W behind, a clock that
    /// went back, a new epoch, a new device latency).
    pub rebootstraps: u64,
    /// Blocks those re-bootstraps skipped (lost on both sides).
    pub missed: u64,
    /// Blocks that took longer than one period.
    pub overruns: u64,
    /// The re-bootstraps by cause ([`super::Cause`], dstack #281).
    pub late: u64,
    pub clock_back: u64,
    pub reanchor: u64,
    pub retime: u64,
}

/// The causes of the re-bootstraps that grew, as the warning names them:
/// `late`, `clock_back`, `reanchor`, `retime` in that order, joined by `+`;
/// `none` when none grew (only overruns).
pub fn cause_word(grew: &Grew) -> String {
    let words: Vec<&str> = [
        (grew.late, "late"),
        (grew.clock_back, "clock_back"),
        (grew.reanchor, "reanchor"),
        (grew.retime, "retime"),
    ]
    .into_iter()
    .filter(|&(n, _)| n > 0)
    .map(|(_, word)| word)
    .collect();
    if words.is_empty() {
        "none".to_string()
    } else {
        words.join("+")
    }
}

/// The watch's memory: the counts at the last look.
#[derive(Debug, Default)]
pub struct PumpWatch {
    seen: Grew,
    next_ns: Option<u64>,
    /// When the pump's wake minute ends next (`None`: the next look starts
    /// one).
    minute_ns: Option<u64>,
    /// The address of the engine's counts at the last watch: another
    /// engine (a later attach) is learnt afresh.
    engine: usize,
    /// The engine's `play_at_rejected` when the minute started.
    rejected: u64,
}

impl PumpWatch {
    pub const fn new() -> Self {
        PumpWatch {
            seen: Grew {
                rebootstraps: 0,
                missed: 0,
                overruns: 0,
                late: 0,
                clock_back: 0,
                reanchor: 0,
                retime: 0,
            },
            next_ns: None,
            minute_ns: None,
            engine: 0,
            rejected: 0,
        }
    }

    /// At `now_ns`, a look once [`EVERY_NS`] has passed since the last (the
    /// first only learns the counts): what grew in `view` since then;
    /// `None` between looks and when nothing grew. Counts that went down (a
    /// new engine attached) count as no growth and are learnt.
    pub fn look(&mut self, now_ns: u64, view: &PumpView) -> Option<Grew> {
        self.looked(now_ns, view).flatten()
    }

    /// [`PumpWatch::look`], told apart: `None` between looks, `Some` at a
    /// look (with what grew, if anything reportable did).
    fn looked(&mut self, now_ns: u64, view: &PumpView) -> Option<Option<Grew>> {
        if self.next_ns.is_some_and(|next| now_ns < next) {
            return None;
        }
        let first = self.next_ns.is_none();
        self.next_ns = Some(now_ns.saturating_add(EVERY_NS));
        let now = Grew {
            rebootstraps: view.rebootstraps,
            missed: view.missed,
            overruns: view.overruns,
            late: view.rebootstraps_late,
            clock_back: view.rebootstraps_clock_back,
            reanchor: view.rebootstraps_reanchor,
            retime: view.rebootstraps_retime,
        };
        let seen = std::mem::replace(&mut self.seen, now);
        let grew = Grew {
            rebootstraps: now.rebootstraps.saturating_sub(seen.rebootstraps),
            missed: now.missed.saturating_sub(seen.missed),
            overruns: now.overruns.saturating_sub(seen.overruns),
            late: now.late.saturating_sub(seen.late),
            clock_back: now.clock_back.saturating_sub(seen.clock_back),
            reanchor: now.reanchor.saturating_sub(seen.reanchor),
            retime: now.retime.saturating_sub(seen.retime),
        };
        let any = grew.rebootstraps > 0 || grew.missed > 0 || grew.overruns > 0;
        Some((any && !first).then_some(grew))
    }

    /// Whether the pump's wake minute ends at this look at `now_ns`: once
    /// [`MINUTE_NS`] has passed since the minute started (the first look
    /// starts one).
    fn minute_ends(&mut self, now_ns: u64) -> bool {
        let ends = self.minute_ns.is_some_and(|end| now_ns >= end);
        if ends || self.minute_ns.is_none() {
            self.minute_ns = Some(now_ns.saturating_add(MINUTE_NS));
        }
        ends
    }

    /// [`PumpWatch::look`] on the engine `stats` of the device `device`
    /// (`None` before an engine is attached: nothing), a growth logged as
    /// one warning ([`PUMP_WARNING`]) whose `max_process_us` is that
    /// second's longest block and `behind_max_us` that second's latest
    /// wake (both taken at every look, a quiet one too; the largest since
    /// the start stay in the pump's view), with the causes of the
    /// re-bootstraps that grew (`cause`, [`cause_word`]). Every
    /// [`MINUTE_NS`] a look ends the pump's wake minute, with one info line
    /// ([`REJECTED_LINE`]) when play-time pairs were rejected in it. Counts from
    /// another engine than the last watch's (a later attach) are only
    /// learnt, as a first look.
    pub fn watch(&mut self, now_ns: u64, device: &str, stats: Option<&PumpStats>) {
        let Some(stats) = stats else { return };
        let engine = std::ptr::from_ref(stats).addr();
        if engine != self.engine {
            self.engine = engine;
            self.next_ns = None;
            self.minute_ns = None;
        }
        let view = stats.view();
        let Some(grew) = self.looked(now_ns, &view) else {
            return;
        };
        // Every look starts the next second's longest block and latest
        // wake afresh.
        let second_max_us = stats.take_second_max_us();
        let behind_max_us = stats.wakes().take_second_max_us();
        let first_minute = self.minute_ns.is_none();
        if self.minute_ends(now_ns) {
            stats.wakes().take_minute();
            let rejected = view.play_at_rejected.saturating_sub(self.rejected);
            if rejected > 0 {
                info!(
                    device,
                    rejected,
                    total = view.play_at_rejected,
                    "{REJECTED_LINE}"
                );
            }
            self.rejected = view.play_at_rejected;
        } else if first_minute {
            self.rejected = view.play_at_rejected;
        }
        let Some(grew) = grew else {
            return;
        };
        let timing = stats.timing();
        warn!(
            device,
            rebootstraps = grew.rebootstraps,
            missed = grew.missed,
            overruns = grew.overruns,
            max_process_us = second_max_us,
            media = view.media,
            block = timing.block,
            margin = timing.margin,
            playout = timing.playout,
            offset = timing.offset(),
            cause = %cause_word(&grew),
            behind_max_us,
            "{PUMP_WARNING}"
        );
    }
}

#[cfg(test)]
mod tests;

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

use tracing::warn;

use super::{PumpStats, PumpView};
use crate::media::late::EVERY_NS;

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
}

/// The watch's memory: the counts at the last look.
#[derive(Debug, Default)]
pub struct PumpWatch {
    seen: Grew,
    next_ns: Option<u64>,
    /// The address of the engine's counts at the last watch: another
    /// engine (a later attach) is learnt afresh.
    engine: usize,
}

impl PumpWatch {
    pub const fn new() -> Self {
        PumpWatch {
            seen: Grew {
                rebootstraps: 0,
                missed: 0,
                overruns: 0,
            },
            next_ns: None,
            engine: 0,
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
        let grew = Grew {
            rebootstraps: view.rebootstraps.saturating_sub(self.seen.rebootstraps),
            missed: view.missed.saturating_sub(self.seen.missed),
            overruns: view.overruns.saturating_sub(self.seen.overruns),
        };
        self.seen = Grew {
            rebootstraps: view.rebootstraps,
            missed: view.missed,
            overruns: view.overruns,
        };
        let any = grew.rebootstraps > 0 || grew.missed > 0 || grew.overruns > 0;
        Some((any && !first).then_some(grew))
    }

    /// [`PumpWatch::look`] on the engine `stats` of the device `device`
    /// (`None` before an engine is attached: nothing), a growth logged as
    /// one warning ([`PUMP_WARNING`]) whose `max_process_us` is that
    /// second's longest block (taken at every look, a quiet one too; the
    /// longest since the start stays in the pump's view). Counts from
    /// another engine than the last watch's (a later attach) are only
    /// learnt, as a first look.
    pub fn watch(&mut self, now_ns: u64, device: &str, stats: Option<&PumpStats>) {
        let Some(stats) = stats else { return };
        let engine = std::ptr::from_ref(stats).addr();
        if engine != self.engine {
            self.engine = engine;
            self.next_ns = None;
        }
        let view = stats.view();
        let Some(grew) = self.looked(now_ns, &view) else {
            return;
        };
        // Every look starts the next second's longest block afresh.
        let second_max_us = stats.take_second_max_us();
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
            "{PUMP_WARNING}"
        );
    }
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The pump's stall watch (dstack #440): on the daemon's tick, one warning
//! when the pump runs, the media clock is ready and its blocks did not rise
//! for more than [`STALL_BLOCKS`] blocks' time, once per stall; one info
//! line when they rise again. On iemmixer-snv (16.11.2026) the pump slept
//! three days after a re-anchor with every count at 0: `health` read
//! `pump_stalled`, and nothing was logged. A pump not running (stopped,
//! or not started) or a clock not ready (the pump paces itself then, and
//! its blocks still rise) is never a stall; the time is counted from the
//! last look that saw blocks rise, or saw the pump not live.

use tracing::{info, warn};

use super::super::PumpStats;

/// The warning: the line a consumer greps for.
pub const STALL_WARNING: &str = "engine pump stalled: no block ran while the clock is ready";
/// The info line when blocks rise again after a stall.
pub const RESUMED_LINE: &str = "engine pump runs blocks again after a stall";
/// Blocks' time without a rise that is a stall.
pub const STALL_BLOCKS: u64 = 3;

/// What a look found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stall {
    /// No block rose for `for_ns` (more than [`STALL_BLOCKS`] blocks' time)
    /// while live, at `blocks`.
    Stalled { for_ns: u64, blocks: u64 },
    /// Blocks rose again after a stall of at least `for_ns`.
    Resumed { for_ns: u64 },
}

/// The watch's memory.
#[derive(Debug, Default)]
pub struct StallWatch {
    /// The blocks at the last look (`None`: the next look learns).
    blocks: Option<u64>,
    /// The last look that saw them rise, or the pump not live.
    rose_ns: u64,
    /// A stall was warned and blocks have not risen since.
    stalled: bool,
    /// The address of the engine's counts at the last look.
    engine: usize,
}

impl StallWatch {
    pub const fn new() -> Self {
        StallWatch {
            blocks: None,
            rose_ns: 0,
            stalled: false,
            engine: 0,
        }
    }

    /// A look at `now_ns` at the pump's `blocks`, `live` when the pump runs
    /// and the clock is ready, a block lasting `period_ns`. Pure.
    pub fn look(&mut self, now_ns: u64, blocks: u64, live: bool, period_ns: u64) -> Option<Stall> {
        let Some(seen) = self.blocks.replace(blocks) else {
            self.rose_ns = now_ns;
            return None;
        };
        let since = now_ns.saturating_sub(self.rose_ns);
        if blocks != seen {
            self.rose_ns = now_ns;
            let was = std::mem::replace(&mut self.stalled, false);
            return was.then_some(Stall::Resumed { for_ns: since });
        }
        if !live {
            self.rose_ns = now_ns;
            return None;
        }
        if self.stalled || since <= period_ns.saturating_mul(STALL_BLOCKS) {
            return None;
        }
        self.stalled = true;
        Some(Stall::Stalled {
            for_ns: since,
            blocks,
        })
    }

    /// [`StallWatch::look`] on the engine `stats` of the device `device`
    /// (`None` before an engine is attached: nothing, and the next look
    /// learns), the clock `ready`: a stall as one warning
    /// ([`STALL_WARNING`]), blocks rising again after it as one info line
    /// ([`RESUMED_LINE`]). Another engine than the last look's (a later
    /// attach) is learnt afresh.
    pub fn watch(&mut self, now_ns: u64, device: &str, stats: Option<&PumpStats>, ready: bool) {
        let Some(stats) = stats else {
            self.blocks = None;
            return;
        };
        let engine = std::ptr::from_ref(stats).addr();
        if engine != self.engine {
            self.engine = engine;
            self.blocks = None;
            self.stalled = false;
        }
        let live = stats.running.load(std::sync::atomic::Ordering::Acquire) && ready;
        let view = stats.view();
        match self.look(now_ns, view.blocks, live, stats.period_ns) {
            Some(Stall::Stalled { for_ns, blocks }) => warn!(
                device,
                blocks,
                stalled_ms = for_ns / 1_000_000,
                block_us = stats.period_ns / 1_000,
                media = view.media,
                late = view.late,
                missed = view.missed,
                rebootstraps = view.rebootstraps,
                "{STALL_WARNING}"
            ),
            Some(Stall::Resumed { for_ns }) => info!(
                device,
                stalled_ms = for_ns / 1_000_000,
                blocks = view.blocks,
                "{RESUMED_LINE}"
            ),
            None => {}
        }
    }
}

#[cfg(test)]
mod tests;

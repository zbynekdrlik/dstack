// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! The blocks' play times checked against their media times (dstack #305,
//! obs#57): from one block to the next, `play_at` ([`super::play_at_ns`],
//! the app's clock) moves by the media time between the blocks' starts, give
//! or take the rate gap between the media clock and the app's (about 6 ppm:
//! 16 ns a 2.67 ms block) and the offset filter's slew
//! ([`super::app_offset`]). A move off that by more than [`STEP_NS`] is a
//! play-time step an app's buffer would hear; the pump counts them
//! (`play_at_steps`) and keeps the largest deviation since the start
//! (`play_at_max_dev_us`), so a consumer gates on dlink's own numbers.

use std::sync::atomic::Ordering;

use super::{PumpStats, ns_of};

/// A block whose play time moved more than this off its media time since
/// the block before is a step, ns (1 ms).
pub const STEP_NS: u64 = 1_000_000;

/// How far the play time moved off the media time from the block at media
/// sample `before.0` (played at `before.1`, app ns) to the block at `now.0`
/// (played at `now.1`), ns at `rate`; saturating.
pub fn deviation_ns(before: (u64, i64), now: (u64, i64), rate: u32) -> u64 {
    let played = i128::from(now.1) - i128::from(before.1);
    let media = i128::from(ns_of(now.0, rate)) - i128::from(ns_of(before.0, rate));
    u64::try_from((played - media).unsigned_abs()).unwrap_or(u64::MAX)
}

/// The pump's last block with a play time: each next one's deviation from
/// it ([`deviation_ns`]).
#[derive(Debug, Default, Clone, Copy)]
pub struct Track {
    last: Option<(u64, i64)>,
}

impl Track {
    /// The block at media sample `start`, played at `at` (app ns; `None`:
    /// no play time, free pacing or no pair read yet, which forgets the
    /// last block): its deviation from the last block with one, `None` for
    /// the first.
    pub fn next(&mut self, start: u64, at: Option<i64>, rate: u32) -> Option<u64> {
        let Some(at) = at else {
            self.last = None;
            return None;
        };
        let before = self.last.replace((start, at))?;
        Some(deviation_ns(before, (start, at), rate))
    }
}

impl PumpStats {
    /// One block's play-time deviation, ns ([`Track`]): a step over
    /// [`STEP_NS`] counted, the largest kept.
    pub(crate) fn play_at_moved(&self, dev_ns: u64) {
        if dev_ns > STEP_NS {
            self.play_at_steps.fetch_add(1, Ordering::Relaxed);
        }
        self.play_at_max_dev_ns.fetch_max(dev_ns, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests;

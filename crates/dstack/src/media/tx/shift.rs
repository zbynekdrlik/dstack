// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! TX and the engine's pump across a clock re-anchor (dstack #1, 6.11.2026:
//! dsync's coordinated date step re-anchored the clock, every flow started
//! again on the grid after now, a receiver saw 2 packets missing, and the
//! pump re-bootstrapped).
//!
//! - When the clock knows the step ([`Anchor::step_from`]), the device
//!   takes ONE shift from it: the step rounded to whole engine blocks
//!   ([`clock::grid_shift`] on the device's `block`; the [`EngineLine`]
//!   decides it once an epoch). Every TX flow and the pump move their
//!   schedules by that same shift, so the stamps go on with no packet left
//!   out: at the old ones + the step for a coordinated step (what every
//!   receiver expects, since it stepped too), unchanged for a step under
//!   half a block (a local correction of k). A flow whose fpp does not
//!   divide the shift (fpp 64 on block 32, fpp 60) starts again instead,
//!   counted, so its stamps stay on its fpp grid. An unknown step starts
//!   every schedule again, counted, as before.
//! - The samples: the pump writes block k's outputs into the engine rings
//!   at `kB + D` and TX reads them at the packet's stamp. Were both only to
//!   shift, TX would read positions the pump never wrote (the samples in
//!   flight, about W + B + fpp, sit at the old positions) and send silence.
//!   So both index the engine rings on one line of positions that no step
//!   moves: the position less the engine shift S, the sum of the shifts.
//!   One shift for both keeps that line continuous for every flow, and the
//!   work margin between the pump's writes and TX's reads unchanged.
//! - A pass may read the clock's new time with its old epoch (the clock
//!   moves k, then the epoch; the media threads read the epoch first, so
//!   never the other way round): the jump starts the schedule again at the
//!   new time on that pass, and the shift on the next pass would put it a
//!   whole step ahead. A shift that lands more than the latency past now
//!   is therefore kept out ([`Shift::Kept`]); the schedule stays where the
//!   restart put it.
//! - The counter and the sine are functions of the stamp they name, and an
//!   echo reads an RX ring where the senders' own stamps put the samples:
//!   those stay at the stamp.

use std::sync::{Mutex, PoisonError};

use tracing::info;

use super::{Source, Transmitter, clock};
use crate::clock::Anchor;

/// The engine rings' line of positions: the engine shift S (samples) at
/// the clock epoch it was last moved to. The first of the TX thread and
/// the pump to see an epoch moves it, by the step on the block grid when
/// the step from the line's epoch is known (else by nothing: the schedules
/// start again anyway); the other reads it. A thread that starts later (a
/// pump after a re-anchor) reads the same S, and each thread's shift is
/// the change of S it saw, so the two always shift alike. Locked once a
/// re-anchor (never on a send), held for a few loads and stores.
#[derive(Debug, Default)]
pub struct EngineLine(Mutex<Option<(u32, i64)>>);

impl EngineLine {
    /// S at the clock's `anchor`, the shift on a grid of `block` frames at
    /// `rate` (moving the line when this is the first look at that epoch).
    /// A read of an epoch older than the line's (a thread a re-anchor
    /// behind) gets the line's S and moves nothing.
    pub fn shift_at(&self, anchor: Anchor, rate: u32, block: u64) -> i64 {
        let mut line = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let shift = match *line {
            None => 0,
            Some((epoch, shift)) => {
                // This epoch, or an older one: as the line has it.
                if (anchor.epoch.wrapping_sub(epoch) as i32) < 1 {
                    return shift;
                }
                let step = anchor
                    .step_from(epoch)
                    .map_or(0, |d| clock::grid_shift(d, rate, block));
                shift.wrapping_add(step)
            }
        };
        *line = Some((anchor.epoch, shift));
        shift
    }
}

/// What a re-anchor did to one schedule (a flow's or the pump's).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shift {
    /// Moved by the shift: nothing lost.
    Moved,
    /// Left where it was: the shift would have put it more than its
    /// latency past now (a pass that read the new time with the old epoch
    /// already started it again at the new time).
    Kept,
    /// The shift is off its grid (or unknown): started again, counted.
    Restarted,
}

/// The position a TX channel's `source` is read at for the samples stamped
/// `stamp`: the engine's ring on its line (the stamp less the engine shift
/// `engine_shift`), any other source at the stamp itself.
pub fn source_position(source: &Source, stamp: u64, engine_shift: i64) -> u64 {
    match source {
        Source::Engine => stamp.wrapping_add_signed(engine_shift.wrapping_neg()),
        _ => stamp,
    }
}

impl Transmitter {
    /// A pass at media time `now` sees the clock at `anchor`, an epoch
    /// other than the last pass's: the engine shift follows the line, and
    /// when the step from the last pass's epoch is known every flow moves
    /// by the change of S ([`super::Schedule::re_anchor`]); else every flow
    /// starts again (counted). Logged: a re-anchor is rare.
    pub(super) fn re_anchored(&mut self, now: u64, anchor: Anchor) {
        let before = self.engine_shift;
        self.engine_shift =
            self.shared
                .engine_line
                .shift_at(anchor, self.rate, self.shared.cfg.block);
        let Some(old) = self.epoch else {
            return;
        };
        let step = anchor.step_from(old);
        let shift = step.map(|_| self.engine_shift.wrapping_sub(before));
        let mut counts = [0u32; 3];
        for schedule in self.lanes.iter_mut().filter_map(|l| l.schedule.as_mut()) {
            counts[schedule.re_anchor(shift, now) as usize] += 1;
        }
        let [moved, kept, restarted] = counts;
        info!(
            epoch = anchor.epoch,
            step_ns = ?step,
            shift = ?shift,
            engine_shift = self.engine_shift,
            moved,
            kept,
            restarted,
            "TX flows across a clock re-anchor"
        );
    }
}

#[cfg(test)]
mod tests;

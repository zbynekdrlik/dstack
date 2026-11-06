// SPDX-License-Identifier: MIT OR Apache-2.0
// Copyright (C) 2026 dstack contributors.

//! TX and the engine's pump across a clock re-anchor (dstack #1, 6.11.2026:
//! dsync's coordinated date step re-anchored the clock, every flow started
//! again on the grid after now, a receiver saw 2 packets missing, and the
//! pump re-bootstrapped).
//!
//! - When the clock knows the step ([`Anchor::step_from`]), every schedule
//!   moves by it rounded to its own grid ([`clock::grid_shift`]): a flow by
//!   whole packets, the pump by whole blocks. The stamps go on with no
//!   packet left out, at the old ones + the step for a coordinated step
//!   (what every receiver expects, since it stepped too), and unchanged for
//!   a step under half a packet (a local correction of k). Only an unknown
//!   step starts the schedules again, counted, as before.
//! - The samples: the pump writes block k's outputs into the engine rings
//!   at `kB + D` and TX reads them at the packet's stamp. Were both only to
//!   shift, TX would read positions the pump never wrote (the samples in
//!   flight, about W + B + fpp, sit at the old positions) and send silence.
//!   So both index the engine rings on one line of positions that no step
//!   moves: the position less the engine shift S, the sum of the steps on
//!   the block grid ([`EngineLine`]). The pump's block after the step
//!   writes where its block before would have gone on, and TX's packet
//!   after the step reads where its packet before would have gone on, in
//!   whichever order the two threads see the new epoch.
//! - The counter and the sine are functions of the stamp they name, and an
//!   echo reads an RX ring where the senders' own stamps put the samples:
//!   those stay at the stamp.

use std::sync::atomic::{AtomicU64, Ordering};

use super::{Source, Transmitter, clock};
use crate::clock::Anchor;

/// The engine rings' line of positions: the engine shift S (samples) at
/// the clock epoch it was last moved to, one word. The first of the TX
/// thread and the pump to see an epoch moves it, by the step on the block
/// grid when the step from the line's epoch is known (else by nothing: the
/// schedules start again anyway); the other reads it. A thread that starts
/// later (a pump after a re-anchor) reads the same S, so the two always
/// agree, which is all the line needs: S itself is no time.
#[derive(Debug)]
pub struct EngineLine(AtomicU64);

/// No epoch seen yet. A real word equal to it (epoch u32::MAX with S = -1)
/// only restarts the line at S = 0 once.
const UNSET: u64 = u64::MAX;

impl Default for EngineLine {
    fn default() -> Self {
        EngineLine(AtomicU64::new(UNSET))
    }
}

/// The epoch and S in one word (S as i32: ±2^31 samples, over 6 hours of
/// steps at 96 kHz; past that it wraps, the same for both readers).
fn pack(epoch: u32, shift: i32) -> u64 {
    let [e0, e1, e2, e3] = epoch.to_be_bytes();
    let [s0, s1, s2, s3] = shift.to_be_bytes();
    u64::from_be_bytes([e0, e1, e2, e3, s0, s1, s2, s3])
}

fn unpack(w: u64) -> (u32, i32) {
    let [e0, e1, e2, e3, s0, s1, s2, s3] = w.to_be_bytes();
    (
        u32::from_be_bytes([e0, e1, e2, e3]),
        i32::from_be_bytes([s0, s1, s2, s3]),
    )
}

impl EngineLine {
    /// S at the clock's `anchor` for a pump of `block` frames at `rate`
    /// (moving the line when this is the first look at that epoch). A read
    /// of an epoch older than the line's (a thread a re-anchor behind) gets
    /// the line's S and moves nothing.
    pub fn shift_at(&self, anchor: Anchor, rate: u32, block: u64) -> i64 {
        let mut word = self.0.load(Ordering::Acquire);
        loop {
            let next = if word == UNSET {
                0
            } else {
                let (epoch, shift) = unpack(word);
                // This epoch, or an older one: as the line has it.
                if (anchor.epoch.wrapping_sub(epoch) as i32) < 1 {
                    return i64::from(shift);
                }
                let step = anchor
                    .step_from(epoch)
                    .map_or(0, |d| clock::grid_shift(d, rate, block));
                shift.wrapping_add(step as i32)
            };
            match self.0.compare_exchange_weak(
                word,
                pack(anchor.epoch, next),
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return i64::from(next),
                Err(seen) => word = seen,
            }
        }
    }
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
    /// other than the last pass's: every flow shifts by the step on its
    /// fpp grid when the step from the last pass's epoch is known, else
    /// starts again (counted); the engine shift follows the line.
    pub(super) fn re_anchored(&mut self, now: u64, anchor: Anchor) {
        if let Some(old) = self.epoch {
            let step = anchor.step_from(old);
            for schedule in self.lanes.iter_mut().filter_map(|l| l.schedule.as_mut()) {
                match step {
                    Some(d) => schedule.shift(clock::grid_shift(d, self.rate, schedule.fpp)),
                    None => schedule.realign(now),
                }
            }
        }
        self.engine_shift =
            self.shared
                .engine_line
                .shift_at(anchor, self.rate, self.shared.cfg.block);
    }
}

#[cfg(test)]
mod tests;

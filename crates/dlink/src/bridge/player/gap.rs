// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output's own gap (dstack #307): what an output that writes the FIFO
//! itself and asks for the room it has ([`super::Player::fill_pulled`])
//! drops after its callback was held past its buffer.
//!
//! A WASAPI shared-mode engine reads the client's buffer of B frames one
//! period p at each of its ticks (on its own clock; a tick that finds less
//! plays silence for the rest), and the event that wakes the callback is
//! set at the tick. A callback held past what the buffer held comes back
//! to a buffer the ticks emptied (cpal's room, B less the padding, is then
//! all of B: a "dry" call), and the card played silence meanwhile. That
//! silence is (k + 1) p - B for the k ticks that found the buffer short,
//! so it can only be one of `p - B mod p` (taken mod p) plus whole periods.
//! A call bigger than the usual one but short of B found frames left: the
//! card played no silence.
//!
//! The pull before each call reads the whole time since the pull before,
//! so over the calls from a gap on the running sum of the excess
//! (`pulled - call`) is the silence plus the latest call's lateness after
//! its tick, less the lateness of the call before the gap (where that pull
//! started). So:
//!
//! - At a dry call, what surely was silence is dropped at once: the sum
//!   past one period (a lateness is under a period), which keeps the depth
//!   under the controller's excess bound, whose re-centre mid-call ran the
//!   rest of the big call dry.
//! - The next [`SETTLE_CALLS`] calls add their excess; the lowest running
//!   sum since the last dry call (the least lateness) is taken to the
//!   silence it can be ([`snap`]), with the latenesses' noise allowed from
//!   a quarter period under to three quarters over (a held PC's wakes after
//!   a hold run late), and what is not dropped yet is dropped at the end.
//!   A pull that wrote nothing (its lock held,
//!   [`crate::bridge::SharedPull::pull`]) is left out of the lowest sum: the
//!   next pull reads its time. A dry call while settling (two holds) counts
//!   its own silence in and starts the settling again.
//!
//! Not seen: a callback held after it read its room but before its pull (a
//! call of the usual size with a large excess, which is also what a pull
//! skipped on the call before looks like): that excess stays with the
//! controller. An engine period that is not a whole number of the
//! network's frames (470, 470, 471...) has no repeated usual call
//! ([`super::usual_call`]) and keeps the first call as the usual one.

/// The calls after a dry call whose excess settles its silence.
pub const SETTLE_CALLS: u32 = 4;

/// What a gap left open (all input frames; [`GapState::default`]: none).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GapState {
    /// The excess since the gap opened, running.
    pub sum: i64,
    /// The lowest running sum since the last dry call.
    pub least: i64,
    /// Calls left to settle (0: no gap open).
    pub left: u32,
    /// Dry calls in this gap.
    pub dry: u32,
    /// Frames this gap dropped so far.
    pub dropped: usize,
}

/// What [`output_gap`] decides for one call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Gap {
    /// Input frames to drop before the call.
    pub drop: usize,
    /// A gap that dropped frames was settled with this call (count it).
    pub ended: bool,
    /// What is left open for the next call.
    pub state: GapState,
}

/// The silence of `dry` dry calls on a buffer of `buffer` frames and a
/// period of `period` that `estimate` stands for: `dry` times the least
/// silence one can be (`period - buffer mod period`, mod period) plus the
/// whole periods the estimate holds past that, rounded down unless within
/// a quarter period of the next.
pub fn snap(estimate: i64, dry: u32, buffer: usize, period: usize) -> i64 {
    let p = period as i64;
    let base = i64::from(dry) * ((p - buffer as i64 % p) % p);
    base + ((estimate - base + p / 4).div_euclid(p)).max(0) * p
}

/// One call's decision (module doc): its pull wrote `pulled` frames, it
/// takes `call`, the output's usual call (its engine period) is `usual`
/// and its buffer `buffer` (the largest call), all input frames, with
/// `state` left by the call before.
pub fn output_gap(state: GapState, pulled: usize, call: usize, usual: usize, buffer: usize) -> Gap {
    let none = Gap {
        drop: 0,
        ended: false,
        state: GapState::default(),
    };
    let dry = usual > 0 && call == buffer && buffer > usual;
    if !dry && state.left == 0 {
        return none;
    }
    let p = usual as i64;
    let sum = state.sum + pulled as i64 - call as i64;
    if dry {
        let drop = (sum - p - state.dropped as i64).max(0);
        return Gap {
            drop: drop as usize,
            ended: false,
            state: GapState {
                sum,
                least: sum,
                left: SETTLE_CALLS,
                dry: state.dry + 1,
                dropped: state.dropped + drop as usize,
            },
        };
    }
    let least = match pulled {
        0 => state.least,
        _ => state.least.min(sum),
    };
    if state.left > 1 {
        return Gap {
            drop: 0,
            ended: false,
            state: GapState {
                sum,
                least,
                left: state.left - 1,
                ..state
            },
        };
    }
    let silence = snap(least, state.dry, buffer, usual);
    let drop = (silence - state.dropped as i64).max(0) as usize;
    Gap {
        drop,
        ended: state.dropped + drop > 0,
        state: GapState::default(),
    }
}

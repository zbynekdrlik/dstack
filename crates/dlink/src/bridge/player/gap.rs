// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output's own gap (dstack #307): what an output that writes the FIFO
//! itself and asks for the room it has ([`super::Player::fill_pulled`])
//! drops after its callback was held past its buffer.
//!
//! A WASAPI shared-mode engine reads the client's buffer B one period p at
//! each of its ticks (on its own clock; a tick that finds less plays
//! silence for the rest), and the event that wakes the callback is set at
//! the tick. A callback held H past its event's tick comes back to a
//! buffer the ticks emptied: the card played silence, and the call asks
//! for the room, B less what is left (all of B once it ran dry). The pull
//! before it reads the whole time since the last pull. So the call's
//! excess, `pulled - call`, is the silence plus the call's phase off the
//! engine's grid, less the lateness of the call before after its own tick
//! (where that pull started). The phase is no loss: the next calls, back
//! on the grid, read it less. Over the calls from the big one on, the
//! running sum of the excess is the silence plus the latest call's
//! lateness less that first lateness, and lateness is never negative.
//!
//! Hence two steps. At the big call (bigger than the usual one), what is
//! surely silence is dropped at once: the excess past one period (the
//! phase is under a period), which keeps the depth under the controller's
//! excess bound, whose re-centre mid-call ran the rest of the big call
//! dry. The rest stays owed while the next [`SETTLE_CALLS`] calls add
//! their excess; the lowest running sum among them (the least lateness)
//! is the silence left, dropped at the end unless it is under a quarter
//! period (the latenesses' noise, no gap). A big call while owed carries
//! the sum on (two holds back to back). A pull that wrote nothing (its
//! lock held, [`crate::bridge::SharedPull::pull`]) is left out of the
//! lowest sum: the next pull reads its time.
//!
//! Neither the silence nor the buffer need be whole periods (Windows'
//! shared buffer is often 2 periods and a fraction: 1056 frames at 48 kHz
//! for a 480-frame period). Not seen: a callback held after it read its
//! room but before its pull (a call of the usual size with a large excess,
//! which also is what a pull skipped on the call before looks like); that
//! excess stays to the controller.

/// The calls after a big call whose excess settles what it owes.
pub const SETTLE_CALLS: u32 = 4;

/// What a gap left open (all input frames; [`GapState::default`]: none).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GapState {
    /// The excess owed since the big call, running.
    pub sum: i64,
    /// The lowest running sum so far.
    pub least: i64,
    /// Calls left to settle (0: no gap open).
    pub left: u32,
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

/// One call's decision (module doc): its pull wrote `pulled` frames, it
/// takes `call`, the output's usual call (its engine period) is `usual`,
/// all input frames, with `state` left by the call before.
pub fn output_gap(state: GapState, pulled: usize, call: usize, usual: usize) -> Gap {
    let none = Gap {
        drop: 0,
        ended: false,
        state: GapState::default(),
    };
    if usual == 0 {
        return none;
    }
    let period = usual as i64;
    let excess = pulled as i64 - call as i64;
    let open = state.left > 0;
    if call > usual {
        let total = if open { state.sum } else { 0 } + excess;
        let drop = (total - period).max(0);
        let left = total - drop;
        return Gap {
            drop: drop as usize,
            ended: false,
            state: GapState {
                sum: left,
                least: left,
                left: SETTLE_CALLS,
                dropped: if open { state.dropped } else { 0 } + drop as usize,
            },
        };
    }
    if !open {
        return none;
    }
    let sum = state.sum + excess;
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
                dropped: state.dropped,
            },
        };
    }
    let drop = if least * 4 >= period {
        least as usize
    } else {
        0
    };
    let dropped = state.dropped + drop;
    Gap {
        drop,
        ended: dropped > 0,
        state: GapState::default(),
    }
}

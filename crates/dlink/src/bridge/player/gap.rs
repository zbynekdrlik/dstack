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
//! started). Both latenesses are measured against the least of the recent
//! ones ([`Lateness`]: the running excess of the calls before the gap), so
//! the estimate's noise is the difference of two floors, near 0. So:
//!
//! - At a dry call, what surely was silence is dropped at once: the
//!   silence the running sum (plus the lateness of the call before) holds,
//!   rounded down ([`snap`] with no slack: the call's phase off the grid is
//!   under a period, so this is the silence or a period under it, never
//!   over). That keeps the depth under the controller's excess bound,
//!   whose re-centre mid-call ran the rest of the big call dry; on a buffer
//!   of up to two periods and a fraction, so the big call itself fits.
//! - The next [`SETTLE_CALLS`] calls add their excess; the lowest running
//!   sum since the last dry call (the least lateness after the gap), plus
//!   the lateness of the call before the gap, is taken to the silence it
//!   can be ([`snap`]: the noise allowed from a quarter period under to
//!   three quarters over, mostly late wakes after a hold; none at all
//!   below a quarter period under the least silence), and what is not
//!   dropped yet is dropped at the end.
//!   A pull that wrote nothing (its lock held,
//!   [`crate::bridge::SharedPull::pull`]) is left out of the lowest sum and
//!   of the recent latenesses: the next pull reads its time. A dry call
//!   while settling (two holds) counts its own silence in and starts the
//!   settling again.
//!
//! The estimate is off by the difference between the least lateness after
//! the gap and the least before it; past three quarters of a period over
//! (7.5 ms at a 10 ms period: every call after the hold that late) or a
//! quarter under, the drop is a period off. Not seen: a callback held after it
//! read its room but before its pull (a call of the usual size with a
//! large excess, which is also what a pull skipped on the call before
//! looks like): that excess stays with the controller. An engine period
//! that is not a whole number of the network's frames (470, 470, 471...)
//! has no repeated usual call ([`super::usual_call`]) and keeps the first
//! call as the usual one. An output whose calls follow the hardware
//! position continuously (ALSA through PipeWire) has no dry call: dlisten
//! uses this on Windows only.

/// The calls after a dry call whose excess settles its silence.
pub const SETTLE_CALLS: u32 = 4;

/// The recent calls whose least lateness is the floor the call before a
/// gap is measured against ([`Lateness`]).
pub const RECENT_CALLS: usize = 4;

/// The lateness of an output's calls after their ticks, relative: the
/// running excess of its calls (each call's pull reads the time since
/// the last, so the running sum moves by each call's lateness change) and
/// its last [`RECENT_CALLS`] values. Fixed size, no allocation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Lateness {
    run: i64,
    recent: [i64; RECENT_CALLS],
    next: usize,
    /// The recent values hold this run's calls ([`Lateness::forget`]).
    known: bool,
}

impl Lateness {
    /// A call outside a gap whose pull wrote `pulled` and that took
    /// `call`. A pull that wrote nothing moves the sum but is no lateness
    /// (the next pull reads its time).
    pub fn add(&mut self, pulled: usize, call: usize) {
        self.run += pulled as i64 - call as i64;
        if pulled > 0 {
            if !self.known {
                self.forget();
            }
            self.recent[self.next] = self.run;
            self.next = (self.next + 1) % RECENT_CALLS;
        }
    }

    /// Starts the recent values afresh at the running sum now: at the
    /// first call, and after a gap (whose calls carry its silence, so the
    /// running sum no longer lines up with the values before it). Until
    /// new calls come, the lateness reads 0 over the floor (under, never
    /// over: the safe side).
    pub fn forget(&mut self) {
        self.recent = [self.run; RECENT_CALLS];
        self.known = true;
    }

    /// The lateness of the last call over the least of the recent ones:
    /// negative after a pull that wrote nothing (the next pull pays its
    /// call), which the estimate needs as it is.
    pub fn over_floor(&self) -> i64 {
        let least = self.recent.iter().copied().min().unwrap_or(self.run);
        self.run - least
    }
}

/// What a gap left open (all input frames; [`GapState::default`]: none).
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct GapState {
    /// The excess since the gap opened, running.
    pub sum: i64,
    /// The lowest running sum since the last dry call.
    pub least: i64,
    /// The lateness of the call before the gap over its floor
    /// ([`Lateness::over_floor`]).
    pub before: i64,
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
/// whole periods the estimate holds past it, rounded down unless within
/// `slack` of the next; none when the estimate is more than `slack` under
/// that least (no dry call played less). The settling's slack is a quarter
/// period (its noise is mostly late wakes after the hold: over); the dry
/// call's is 0 (its phase off the grid is over by up to a period).
pub fn snap(estimate: i64, dry: u32, buffer: usize, period: usize, slack: i64) -> i64 {
    let p = period as i64;
    let base = i64::from(dry) * ((p - buffer as i64 % p) % p);
    match (estimate - base + slack).div_euclid(p) {
        k if k < 0 => 0,
        k => base + k * p,
    }
}

/// One call's decision (module doc): its pull wrote `pulled` frames, it
/// takes `call`, the output's usual call (its engine period) is `usual`
/// and its buffer `buffer` (the largest call), all input frames, the call
/// before was `before` late over its floor ([`Lateness::over_floor`]),
/// with `state` left by the call before.
pub fn output_gap(
    state: GapState,
    pulled: usize,
    call: usize,
    usual: usize,
    buffer: usize,
    before: i64,
) -> Gap {
    let none = Gap {
        drop: 0,
        ended: false,
        state: GapState::default(),
    };
    let dry = usual > 0 && call == buffer && buffer > usual;
    if !dry && state.left == 0 {
        return none;
    }
    let sum = state.sum + pulled as i64 - call as i64;
    if dry {
        let before = if state.left == 0 {
            before
        } else {
            state.before
        };
        let surely = snap(sum + before, state.dry + 1, buffer, usual, 0);
        let drop = (surely - state.dropped as i64).max(0);
        return Gap {
            drop: drop as usize,
            ended: false,
            state: GapState {
                sum,
                least: sum,
                before,
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
    let silence = snap(
        least + state.before,
        state.dry,
        buffer,
        usual,
        usual as i64 / 4,
    );
    let drop = (silence - state.dropped as i64).max(0) as usize;
    Gap {
        drop,
        ended: state.dropped + drop > 0,
        state: GapState::default(),
    }
}

//! A steered clock that never steps, for Windows (dstack #1, songplayer,
//! 1.11.2026). Linux has one in `CLOCK_MONOTONIC`: it runs at the rate
//! dantesync steers and no date step moves it. Windows has no such clock:
//! QPC is not steered, and the system time is steered but steps. Until
//! now, our "monotonic" clock there was the system time, so dantesync's
//! nightly date step moved PTP time with it. At 02:00:11Z the wall stepped
//! +1.6 s; the TX schedule started again at once (lag 153609 samples), the
//! engine pump skipped 1200 blocks, and the clock re-anchored back only
//! 4.2 s later.
//!
//! [`Steer`] builds one from QPC and the wall: a [`Model`] maps the QPC
//! reading to ns at the wall's rate, and the clock poller updates it from
//! each poll's (QPC, wall) pair. A wall jump larger than drift could explain
//! ([`STEP_MIN_NS`] plus [`DRIFT_PPB`] of the time since the last poll) is a
//! date step: it goes into an offset and the clock does not move. Smaller
//! differences are slewed out over the next [`SLEW_NS`], within [`MAX_PPB`].

/// A wall jump over this (plus drift) is a step.
pub const STEP_MIN_NS: i64 = 1_000_000;
/// The rate difference drift may explain between polls, ppb (100 ppm).
pub const DRIFT_PPB: i64 = 100_000;
/// The steepest slew, ppb (1000 ppm).
pub const MAX_PPB: i64 = 1_000_000;
/// The time a slew takes the error out in: the poll.
pub const SLEW_NS: i64 = 1_000_000_000;

/// The clock at QPC time `q` (ns): `base_m` at `base_q`, then QPC's
/// time scaled by 1 + `ppb`·10⁻⁹.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Model {
    pub base_q: i64,
    pub base_m: i64,
    pub ppb: i64,
}

impl Model {
    /// The clock at QPC time `q`.
    pub fn at(&self, q: i64) -> i64 {
        let dq = i128::from(q) - i128::from(self.base_q);
        let scaled = dq + dq * i128::from(self.ppb) / 1_000_000_000;
        (i128::from(self.base_m) + scaled) as i64
    }

    /// The model as three words (a seqlock cell).
    pub fn words(&self) -> [u64; 3] {
        [self.base_q as u64, self.base_m as u64, self.ppb as u64]
    }

    /// The model three words hold.
    pub fn from_words(w: [u64; 3]) -> Model {
        Model {
            base_q: w[0] as i64,
            base_m: w[1] as i64,
            ppb: w[2] as i64,
        }
    }
}

/// What one update did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Update {
    /// The first pair: the clock starts at the wall.
    First,
    /// The wall jumped by `jump_ns` (a date step): kept in the offset, the
    /// clock did not move.
    Step { jump_ns: i64 },
    /// The clock was `error_ns` behind the wall less the offset; it runs
    /// at `ppb` until the next update.
    Slew { error_ns: i64, ppb: i64 },
}

/// The model and what it learned from the wall.
#[derive(Debug, Default, Clone)]
pub struct Steer {
    model: Option<Model>,
    /// The date steps so far: the clock follows the wall less this.
    offset: i64,
    /// The last update's QPC time and wall less the offset.
    last: (i64, i64),
}

impl Steer {
    pub const fn new() -> Self {
        Steer {
            model: None,
            offset: 0,
            last: (0, 0),
        }
    }

    /// The model in use; `None` before the first update.
    pub fn model(&self) -> Option<Model> {
        self.model
    }

    /// The date steps kept so far, ns.
    pub fn offset(&self) -> i64 {
        self.offset
    }

    /// One pair from the poller: QPC time `q` and the wall `wall`, read
    /// back to back.
    pub fn update(&mut self, q: i64, wall: i64) -> Update {
        let Some(model) = self.model else {
            self.model = Some(Model {
                base_q: q,
                base_m: wall,
                ppb: 0,
            });
            self.last = (q, wall);
            return Update::First;
        };
        let (q0, a0) = self.last;
        let dq = q.saturating_sub(q0).max(1);
        let m = model.at(q);
        let error = wall - self.offset - m;
        let allowed =
            i128::from(STEP_MIN_NS) + i128::from(dq) * i128::from(DRIFT_PPB) / 1_000_000_000;
        let (ppb, update) = if i128::from(error.unsigned_abs()) > allowed {
            // A date step: the wall less the offset meets the clock again.
            self.offset += error;
            (model.ppb, Update::Step { jump_ns: error })
        } else {
            // The wall's rate since the last update, plus the error taken
            // out over the next poll.
            let a = wall - self.offset;
            let rate = i128::from(a - a0 - dq) * 1_000_000_000 / i128::from(dq);
            let pull = i128::from(error) * 1_000_000_000 / i128::from(SLEW_NS);
            let ppb = (rate + pull).clamp(-i128::from(MAX_PPB), i128::from(MAX_PPB)) as i64;
            (
                ppb,
                Update::Slew {
                    error_ns: error,
                    ppb,
                },
            )
        };
        self.model = Some(Model {
            base_q: q,
            base_m: m,
            ppb,
        });
        self.last = (q, wall - self.offset);
        update
    }
}

#[cfg(test)]
mod tests;

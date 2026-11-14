// SPDX-License-Identifier: MIT OR Apache-2.0

//! The FIFO's producer as the player sees it (dstack #303): when a chunked
//! writer (OBS's 1024-frame ticks) writes its next chunk on its own
//! schedule, so a re-prime can start at the depth whose MEAN over the
//! writer's sawtooth is the target, whatever the moment of the writer's
//! cycle it ends at.
//!
//! The schedule is a line from frames written to time: the writer's
//! frames come at its rate, so the write that brings the count to `n`
//! is due at `t0 + n / rate`. A write on time (within [`TOLERANCE`] of a
//! chunk's period of the line) pulls `t0` by [`GAIN`] of its error, which
//! averages the writer's jitter (±2 ms on a 21.3 ms tick) and follows a
//! drifting writer clock. A writer that stalls (an app thread held 10 to
//! 25 ms) and then writes the chunks it owes at once is late on the line,
//! so it moves nothing; and since the line counts frames, the chunks the
//! burst delivered are not expected again: the next write is due where
//! the schedule says the chunk after them comes.
//!
//! Pure: times are seconds on the player's clock, counts are the FIFO's
//! arrivals, both given by the caller.

/// How much of its error a write on time moves the schedule.
pub const GAIN: f64 = 0.1;
/// The share of a chunk's period within which a write is on time.
pub const TOLERANCE: f64 = 0.25;

/// The writer's schedule (module doc).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Writer {
    /// The writer's chunk, frames, and its rate, frames a second.
    chunk: f64,
    rate: f64,
    /// When the write that brought the count to 0 was due, s; `None`
    /// until the first write.
    t0: Option<f64>,
    /// The count at the last write seen.
    last: u64,
}

impl Writer {
    /// A writer of `chunk` frames a write at `rate` frames a second.
    pub fn new(chunk: usize, rate: u32) -> Self {
        Writer {
            chunk: chunk as f64,
            rate: f64::from(rate),
            t0: None,
            last: 0,
        }
    }

    /// One chunk's period, s.
    pub fn period_s(&self) -> f64 {
        self.chunk / self.rate
    }

    /// When the write that brings the count to `written` is due, s, once a
    /// write was seen.
    pub fn due_s(&self, written: u64) -> Option<f64> {
        Some(self.t0? + written as f64 / self.rate)
    }

    /// A write seen at `t_s` that brought the count to `written`. A count
    /// that rose by more than a chunk since the last write seen is a
    /// catch-up burst: even its last chunk, a little late on the line,
    /// moves nothing.
    pub fn wrote(&mut self, t_s: f64, written: u64) {
        let burst = written.saturating_sub(self.last) as f64 > self.chunk;
        self.last = written;
        match self.due_s(written) {
            None => self.t0 = Some(t_s - written as f64 / self.rate),
            Some(due) => {
                let err = t_s - due;
                if !burst
                    && err.abs() <= TOLERANCE * self.period_s()
                    && let Some(t0) = self.t0.as_mut()
                {
                    *t0 += GAIN * err;
                }
                // Else late (a stall's catch-up) or early: the schedule
                // stays.
            }
        }
    }

    /// The time from `t_s` until the next chunk is due, `written` frames
    /// written so far, s, within [0, period]: 0 when it is overdue;
    /// `None` before the first write.
    pub fn until_next(&self, t_s: f64, written: u64) -> Option<f64> {
        let due = self.due_s(written + self.chunk as u64)?;
        Some((due - t_s).clamp(0.0, self.period_s()))
    }
}

#[cfg(test)]
mod tests;

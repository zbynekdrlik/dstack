// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output side (dstack #139): the output callback pulls frames at the
//! output device's own rate and clock from the [`Fifo`] the dlink pump
//! fills at the network's.
//!
//! - **Conversion:** `dstack-resample`'s drifting [`Converter`] (the sinc
//!   stage, equal rates too) gives [`BLOCK`] output frames a call from the
//!   input frames it asks for. A callback of any size is served from that
//!   block, so the converter's call size never follows the driver's.
//! - **Steering:** [`dstack_resample::asrc::Controller`] (SongPlayer's laws) keeps the
//!   FIFO's depth at [`TARGET_S`] by leaving the converter's ratio a few ppm
//!   off nominal. The controller was written for the mirror topology (the
//!   converter writes the buffer, the other clock reads it): here the
//!   network writes and the converter reads. So the rate point is fed
//!   mirrored, `other = 2·now − produced`, with `now` the output's clock
//!   (frames made / its rate) and `produced` the network's (frames arrived
//!   / its rate): a network fast by +d ppm fits as −d ppm, less output per
//!   input, which is the change that drains it.
//! - **Priming:** at the start and after an underrun the output plays
//!   silence until the FIFO holds the target (plus the cushion below), then
//!   starts from there; an excess at that moment is skipped.
//! - **Underruns:** a block whose input is not there plays silence and
//!   counts in [`Stats`]. Each underrun raises the cushion the next priming
//!   fills by [`CUSHION_STEP_S`], up to [`CUSHION_MAX_S`]: a PC whose pump
//!   comes late gets more margin, and the controller keeps that excess as
//!   its cushion (`with_cushion_max_s`, `played_silence`) instead of braking
//!   it away at once. Every [`CUSHION_DECAY_S`] with no dropout it steps
//!   down again.
//! - **Re-centre:** a depth the controller cannot steer back (under
//!   [`FLOOR_S`] or more than [`HARD_EXCESS_S`] off) skips an excess; a
//!   deficit is a dropout like an underrun (counted, refilled with a
//!   larger cushion): the audio breaks either way.
//!
//! [`Player::fill_with`] allocates nothing, takes no lock and makes no
//! system call: it runs on the audio driver's thread.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use dstack_resample::asrc::{Basis, Controller, Step};
use dstack_resample::{ConvertError, Converter, Fixed};

use crate::fifo::Fifo;
use crate::map;

/// The FIFO depth the controller keeps, s: the MVP's latency (the issue's
/// design: about 40 ms), room for a late pump block and a large driver
/// callback.
pub const TARGET_S: f64 = 0.040;
/// A depth under this re-primes, s.
pub const FLOOR_S: f64 = 0.005;
/// A depth more than this off the target re-centres, s (over the target
/// plus the largest cushion).
pub const HARD_EXCESS_S: f64 = 0.060;
/// The cushion each underrun adds to the next priming, s.
pub const CUSHION_STEP_S: f64 = 0.010;
/// The most cushion steps: the largest cushion is this many
/// [`CUSHION_STEP_S`].
pub const CUSHION_STEPS: u32 = 4;
/// The largest cushion, s.
pub const CUSHION_MAX_S: f64 = 0.040;
/// After this long playing with no dropout, the cushion steps down one
/// [`CUSHION_STEP_S`], s: a PC whose pump was late once does not keep the
/// largest latency for good.
pub const CUSHION_DECAY_S: f64 = 60.0;
/// The FIFO's size, s of the network's rate: the target, the cushion and
/// the excess with room to spare.
pub const FIFO_S: f64 = 0.5;
/// The converter's output frames a call.
pub const BLOCK: usize = 128;

const _: () = assert!(CUSHION_STEPS as f64 * CUSHION_STEP_S == CUSHION_MAX_S);
const _: () = assert!(TARGET_S + CUSHION_MAX_S < TARGET_S + HARD_EXCESS_S);
const _: () = assert!(TARGET_S + HARD_EXCESS_S < FIFO_S);

/// The FIFO for `channels` channels at the network's `net_rate`
/// ([`FIFO_S`]).
pub fn fifo_for(channels: usize, net_rate: u32) -> Fifo {
    Fifo::new(channels, frames_of(FIFO_S, net_rate))
}

/// `seconds` in whole frames at `rate`, rounded.
fn frames_of(seconds: f64, rate: u32) -> usize {
    (seconds * f64::from(rate)).round() as usize
}

/// The player's counts, read by the status thread. One `Stats` outlives
/// the players of a run: a player made for another output (the tray's
/// output menu) goes on counting from the last one's counts.
#[derive(Debug, Default)]
pub struct Stats {
    /// The current output's rate, Hz.
    out_rate: AtomicU32,
    underruns: AtomicU64,
    played: AtomicU64,
    recentres: AtomicU64,
    /// The converter's correction, ppm (`f64` bits).
    ppm: AtomicU64,
    priming: AtomicBool,
}

impl Stats {
    /// Blocks played as silence because their input was not there.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }

    /// Output frames handed to the driver.
    pub fn played(&self) -> u64 {
        self.played.load(Ordering::Relaxed)
    }

    /// Depths the controller could not steer back (an excess skipped, or a
    /// deficit refilled, which also counts as an underrun).
    pub fn recentres(&self) -> u64 {
        self.recentres.load(Ordering::Relaxed)
    }

    /// The converter's correction, ppm.
    pub fn ppm(&self) -> f64 {
        f64::from_bits(self.ppm.load(Ordering::Relaxed))
    }

    /// The current output's rate, Hz.
    pub fn out_rate(&self) -> u32 {
        self.out_rate.load(Ordering::Relaxed)
    }

    /// Whether the output waits for the FIFO to fill (silence).
    pub fn priming(&self) -> bool {
        self.priming.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Silence until the FIFO holds this many frames.
    Priming(usize),
    Running,
}

/// The output side (module doc).
pub struct Player {
    fifo: Arc<Fifo>,
    conv: Converter,
    ctl: Controller,
    net_rate: u32,
    out_rate: u32,
    /// The converter's input, channel-major.
    input: Vec<f64>,
    /// The converter's last output block, channel-major.
    block: Vec<f64>,
    /// Frames of `block` already handed out.
    pos: usize,
    /// The last callback's frames, in input frames.
    callback_in: usize,
    state: State,
    /// The cushion the next priming fills, in [`CUSHION_STEP_S`].
    steps: u32,
    /// The output frame (`made`) of the last dropout or cushion step down.
    calm_since: u64,
    /// Output frames made (blocks, silence too): the output's clock.
    made: u64,
    /// Input frames converted.
    consumed: u64,
    underruns: u64,
    played: u64,
    /// The re-centres of the players before this one.
    recentres_before: u64,
    stats: Arc<Stats>,
}

/// The controller at our target, its calm zone the default
/// ([`dstack_resample::asrc::CALM_ZONE_MS`]): SongPlayer widens it to half a callback
/// because it reads the depth once a callback, so the read's phase in the
/// sawtooth wanders. We read it after every block, several times a
/// callback, and the controller's 1 s window mean averages the sawtooth
/// out; a wide zone would only leave a larger offset to the slow level
/// loop.
fn controller() -> Controller {
    Controller::new(TARGET_S, FLOOR_S, HARD_EXCESS_S, Basis::Arrival)
        .with_cushion_max_s(CUSHION_MAX_S)
}

impl Player {
    /// A player reading `fifo` (the network's `net_rate`) for an output at
    /// `out_rate`, counting on in `stats` (from its counts so far).
    pub fn new(
        fifo: Arc<Fifo>,
        net_rate: u32,
        out_rate: u32,
        stats: Arc<Stats>,
    ) -> Result<Self, ConvertError> {
        let channels = fifo.channels();
        let conv = Converter::drifting(net_rate, out_rate, channels, BLOCK, Fixed::Output)?;
        let input = vec![0.0; conv.input_frames_max() * channels];
        stats.priming.store(true, Ordering::Relaxed);
        stats.out_rate.store(out_rate, Ordering::Relaxed);
        Ok(Player {
            fifo,
            conv,
            ctl: controller(),
            net_rate,
            out_rate,
            input,
            block: vec![0.0; BLOCK * channels],
            pos: BLOCK,
            callback_in: 0,
            state: State::Priming(frames_of(TARGET_S, net_rate)),
            steps: 0,
            calm_since: 0,
            made: 0,
            consumed: 0,
            underruns: stats.underruns(),
            played: stats.played(),
            recentres_before: stats.recentres(),
            stats,
        })
    }

    /// The counts, for the status thread.
    pub fn stats(&self) -> Arc<Stats> {
        Arc::clone(&self.stats)
    }

    /// Input frames converted so far.
    pub fn consumed(&self) -> u64 {
        self.consumed
    }

    /// Fills an interleaved output buffer of `outputs` channels, each
    /// sample made from an `f64` by `to` (the driver's sample format).
    pub fn fill_with<T>(&mut self, out: &mut [T], outputs: usize, to: impl Fn(f64) -> T) {
        let Some(frames) = out.len().checked_div(outputs) else {
            return;
        };
        self.callback_in = frames_of(frames as f64 / f64::from(self.out_rate), self.net_rate);
        let inputs = self.fifo.channels();
        for frame in out.chunks_exact_mut(outputs) {
            if self.pos == BLOCK {
                self.next_block();
            }
            for (o, slot) in frame.iter_mut().enumerate() {
                let src = map::source(o, outputs, inputs);
                *slot = to(map::sample(src, &self.block, BLOCK, self.pos));
            }
            self.pos += 1;
        }
        self.played += frames as u64;
        self.publish();
    }

    fn publish(&self) {
        let s = &self.stats;
        s.underruns.store(self.underruns, Ordering::Relaxed);
        s.played.store(self.played, Ordering::Relaxed);
        s.recentres.store(
            self.recentres_before + self.ctl.recentres(),
            Ordering::Relaxed,
        );
        s.ppm.store(self.ctl.ppm().to_bits(), Ordering::Relaxed);
        s.priming
            .store(matches!(self.state, State::Priming(_)), Ordering::Relaxed);
    }

    /// The next [`BLOCK`] output frames into `block`.
    fn next_block(&mut self) {
        self.pos = 0;
        self.made += BLOCK as u64;
        if let State::Priming(level) = self.state {
            // The controller reads the depth after each block, so over a
            // callback it reads half a callback (and a block) under the
            // depth at the callback's start: start that much over the
            // level, so its reads begin at the level, not a callback under.
            let level = level + (self.callback_in + self.conv.input_frames_next()) / 2;
            let held = self.fifo.len();
            if held < level {
                self.block.fill(0.0);
                return;
            }
            // Start from the level: an excess (audio that came while the
            // output was not running yet) is skipped, not drained.
            self.fifo.skip(held - level);
            self.conv.reset();
            self.state = State::Running;
        }
        let channels = self.fifo.channels();
        let need = self.conv.input_frames_next();
        if !self
            .fifo
            .pop_planar(&mut self.input[..need * channels], need)
        {
            self.underrun();
            return;
        }
        if self
            .conv
            .process(&self.input, need, &mut self.block, BLOCK)
            .is_err()
        {
            // Never with the sizes the converter gave; silence, not a panic
            // on the driver's thread.
            self.block.fill(0.0);
        }
        self.consumed += need as u64;
        self.calm_down();
        self.steer();
    }

    /// The block's input was not there: silence, counted, and the next
    /// priming fills more (module doc).
    fn underrun(&mut self) {
        self.block.fill(0.0);
        self.dropout();
    }

    /// The audio breaks (an underrun, or a depth under the floor that only
    /// a refill mends): counted as an underrun, and the next priming fills
    /// a larger cushion (module doc).
    fn dropout(&mut self) {
        self.underruns += 1;
        self.steps = (self.steps + 1).min(CUSHION_STEPS);
        self.calm_since = self.made;
        let extra_s = self.extra_s();
        // The controller keeps the extra as its cushion: restart drops the
        // old one and the silence count, the first count is the start and
        // the second adds the extra.
        self.ctl.restart();
        self.ctl.played_silence(0.0);
        self.ctl.played_silence(extra_s);
        self.state = State::Priming(frames_of(TARGET_S + extra_s, self.net_rate));
    }

    /// The cushion the next priming fills, s.
    fn extra_s(&self) -> f64 {
        f64::from(self.steps) * CUSHION_STEP_S
    }

    /// One step down of the cushion after [`CUSHION_DECAY_S`] with no
    /// dropout.
    fn calm_down(&mut self) {
        let calm = frames_of(CUSHION_DECAY_S, self.out_rate) as u64;
        if self.steps > 0 && self.made - self.calm_since >= calm {
            self.steps -= 1;
            self.calm_since = self.made;
        }
    }

    /// One observation after a converted block, and its answer.
    fn steer(&mut self) {
        let now_s = self.made as f64 / f64::from(self.out_rate);
        let net = f64::from(self.net_rate);
        let depth_s = self.fifo.len() as f64 / net;
        let produced_s = self.fifo.arrived() as f64 / net;
        let ppm = match self.ctl.observe(now_s, depth_s, 2.0 * now_s - produced_s) {
            Step::Hold(ppm) => ppm,
            Step::Recentre { by_s, ppm } | Step::Prime { by_s, ppm } => {
                // A re-centre is never at 0 (it is past the floor or the
                // excess): the sign alone tells a deficit from an excess.
                if by_s.is_sign_negative() {
                    self.fifo.skip(frames_of(-by_s, self.net_rate));
                    // The audio jumps: the filter's history is no longer
                    // its past.
                    self.conv.reset();
                } else {
                    self.dropout();
                }
                ppm
            }
        };
        // Within the converter's room by the controller's own bound.
        let _ = self.conv.set_ppm(ppm);
    }
}

#[cfg(test)]
mod tests;

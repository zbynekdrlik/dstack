// SPDX-License-Identifier: MIT OR Apache-2.0

//! The output side (dstack #139): the output callback pulls frames at the
//! output device's own rate and clock from the [`Fifo`] the dlink pump
//! fills at the network's.
//!
//! - **Conversion:** `dstack-resample`'s drifting [`Converter`] (the sinc
//!   stage, equal rates too) gives [`BLOCK`] output frames a call from the
//!   input frames it asks for. A callback of any size is served from that
//!   block, so the converter's call size never follows the driver's.
//! - **Steering:** [`dstack_resample::asrc::Controller`] (SongPlayer's
//!   laws) keeps the FIFO's depth at its target, the device's
//!   [`AppBuffer`] (dstack #247: the app's chunk and one pump block, or
//!   the value the app set), by leaving the converter's ratio a few ppm
//!   off nominal. The controller was written for the mirror topology (the
//!   converter writes the buffer, the other clock reads it): here the
//!   network writes and the converter reads. So the rate point is fed
//!   mirrored, `other = 2·now − produced`, with `now` the output's clock
//!   (frames made / its rate) and `produced` the network's (frames arrived
//!   / its rate): a network fast by +d ppm fits as −d ppm, less output per
//!   input, which is the change that drains it.
//! - **Priming:** at the start and after an underrun the output plays
//!   silence until the FIFO holds the target and half the wider swing (the
//!   output's callback or the app's chunk; in dlisten the chunk is the
//!   output's callback, so it counts once), then starts from there; an
//!   excess at that moment is skipped. A chunked producer (its chunk wider
//!   than the output's callback: OBS's 1024-frame ticks) is followed on its
//!   own schedule (`player::writer`, dstack #303): a re-prime that ends mid-cycle
//!   (after a stalled writer's catch-up burst) starts at the depth whose
//!   mean over the writer's sawtooth is the target, not at the top of a
//!   sawtooth that the next tick lifts by a whole chunk more. Before that
//!   change every such re-prime left the mean up to a chunk over the
//!   target, and the controller's braking drained it at up to -70 ppm.
//! - **Resync:** a caller that names each block's start
//!   ([`Player::fill_at`]) and skips blocks (the pump's rebootstrap, #302)
//!   re-primes once to the target (the excess the producer wrote meanwhile
//!   is skipped), counted in [`Stats::resyncs`], and the output's clock
//!   counts the skipped time, so the rate fit sees no step. Before, the
//!   excess was drained by the ratio: -200 ppm for about a minute.
//! - **Underruns:** a block whose input is not there plays silence, counts
//!   in [`Stats`] and primes again to the same target. The target never
//!   grows after a dropout (dstack #247: the automatic cushion of up to
//!   +40 ms is gone): the latency stays the stated term, and a PC that
//!   needs more sets a larger [`AppBuffer`].
//! - **Re-centre:** a depth the controller cannot steer back (under
//!   [`floor_s`] or more than [`hard_excess_s`] over the target) skips the
//!   excess back to the target; a deficit is a dropout like an underrun
//!   (counted, refilled to the target): the audio breaks either way.
//!
//! [`Player::fill_with`] allocates nothing, takes no lock and makes no
//! system call: it runs on the audio driver's thread.
//!
//! The text above is dlisten's direction. The player only knows the FIFO's
//! producer (named "the network" here) and its own caller (named "the
//! output"): dsend (dstack #164) runs it the other way round, the
//! endpoint's loopback capture writing the FIFO at the endpoint's rate and
//! the dlink pump calling [`Player::fill_with`] at the network's.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use dstack_resample::asrc::{Basis, Controller, Step};
use dstack_resample::{ConvertError, Converter, Fixed};

use super::buffer::{AppBuffer, MAX_APP_BUFFER_NS};
use super::fifo::Fifo;
use super::map;

mod writer;

use writer::Writer;

/// The highest floor, s: a depth under the floor re-primes.
pub const FLOOR_S: f64 = 0.005;
/// The FIFO's size, s of the network's rate: the largest target and its
/// excess ([`hard_excess_s`]) with room to spare.
pub const FIFO_S: f64 = 0.5;
/// The converter's output frames a call.
pub const BLOCK: usize = 128;
/// How far the offset and level loops may move the ratio from the locked
/// rate fit, ppm (dstack #303, the main's decision of 14.11): a depth
/// offset a re-prime leaves drains slowly instead of ramping.
pub const LEVEL_BOUND_PPM: f64 = 20.0;

const _: () = assert!(3.0 * (MAX_APP_BUFFER_NS as f64 / 1e9) < FIFO_S);

/// The floor for a target of `target_s`, s: [`FLOOR_S`], at most a quarter
/// of the target. A chunked producer's depth swings half a chunk under the
/// target (dsend: 10 ms packets around 11.3 ms reach 6.3 ms), so a 5 ms
/// floor would already re-prime (a dropout) for a packet 1.4 ms late; the
/// FIFO itself still serves the block until it is empty.
pub fn floor_s(target_s: f64) -> f64 {
    FLOOR_S.min(target_s / 4.0)
}

/// How far over the target the depth may go before the excess is skipped
/// back to the target, s: twice the target, and at least twice the app's
/// chunk `chunk_s` (a target set under the chunk still primes half a
/// chunk over it and then takes a whole chunk at once), at most twice
/// [`MAX_APP_BUFFER_NS`] (so the FIFO holds it). A burst of late chunks
/// is skipped; the swing of a chunked producer never is. A deficit never
/// reaches it: under the target the floor decides.
pub fn hard_excess_s(target_s: f64, chunk_s: f64) -> f64 {
    (2.0 * target_s)
        .max(2.0 * chunk_s)
        .min(2.0 * (MAX_APP_BUFFER_NS as f64 / 1e9))
}

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
    /// The FIFO's target, ns ([`AppBuffer::target_ns`]).
    target_ns: AtomicU64,
    underruns: AtomicU64,
    played: AtomicU64,
    recentres: AtomicU64,
    resyncs: AtomicU64,
    /// The converter's correction, ppm (`f64` bits).
    ppm: AtomicU64,
    priming: AtomicBool,
    /// The player sums every channel to both speakers (`--mono`).
    mono: AtomicBool,
}

impl Stats {
    /// Whether the player sums every channel to both speakers (`--mono`).
    pub fn mono(&self) -> bool {
        self.mono.load(Ordering::Relaxed)
    }

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

    /// Re-primes after the caller skipped blocks ([`Player::fill_at`]: a
    /// block that did not start where the last one ended, the pump's
    /// rebootstrap); an app publishes it as `tx_resyncs` (dstack #303).
    pub fn resyncs(&self) -> u64 {
        self.resyncs.load(Ordering::Relaxed)
    }

    /// The converter's correction, ppm.
    pub fn ppm(&self) -> f64 {
        f64::from_bits(self.ppm.load(Ordering::Relaxed))
    }

    /// The FIFO's target, ms (the device's [`AppBuffer`]).
    pub fn target_ms(&self) -> f64 {
        self.target_ns.load(Ordering::Relaxed) as f64 / 1e6
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
    /// The app's chunk ([`AppBuffer::chunk_ns`]), in input frames.
    chunk_in: usize,
    state: State,
    /// The FIFO's target, s ([`AppBuffer::target_s`]).
    target_s: f64,
    /// Output frames made (blocks, silence too): the output's clock.
    made: u64,
    /// Input frames converted.
    consumed: u64,
    underruns: u64,
    played: u64,
    /// The re-centres of the players before this one.
    recentres_before: u64,
    resyncs: u64,
    /// The producer's schedule ([`writer`]), followed when its chunk is
    /// wider than the output's callback.
    writer: Writer,
    /// The FIFO's arrivals at the last look (a change is a write).
    arrived: u64,
    /// Output frames of the caller's blocks that never came (a skip,
    /// [`Player::fill_at`]): the output's clock counts them.
    skipped: u64,
    /// Where the caller's next block starts, if it names its starts; a
    /// block without one after it leaves [`Next::Interrupted`].
    next_start: Option<Next>,
    /// The output's position at the frame being made, frames (played,
    /// within the current call).
    at: u64,
    /// `--mono`: every channel summed to both speakers ([`map::source_mono`]).
    mono: bool,
    stats: Arc<Stats>,
}

/// The controller at `buffer`'s target, its floor and excess from it
/// ([`floor_s`], [`hard_excess_s`]) and no cushion (dstack #247), its
/// calm zone the default ([`dstack_resample::asrc::CALM_ZONE_MS`]):
/// SongPlayer widens it to half a callback
/// because it reads the depth once a callback, so the read's phase in the
/// sawtooth wanders. We read it after every block, several times a
/// callback, and the controller's 1 s window mean averages the sawtooth
/// out; a wide zone would only leave a larger offset to the slow level
/// loop. Once the rate fit is locked, the offset and level loops move the
/// ratio at most [`LEVEL_BOUND_PPM`] from the fitted rate (dstack #303).
fn controller(buffer: &AppBuffer) -> Controller {
    let target_s = buffer.target_s();
    Controller::new(
        target_s,
        floor_s(target_s),
        hard_excess_s(target_s, buffer.chunk_s()),
        Basis::Arrival,
    )
    .with_level_bound_ppm(LEVEL_BOUND_PPM)
}

impl Player {
    /// A player reading `fifo` (the network's `net_rate`) for an output at
    /// `out_rate`, keeping the FIFO at `buffer`'s target (dstack #247: the
    /// app's chunk and one pump block, or the value it set), counting on in
    /// `stats` (from its counts so far).
    pub fn new(
        fifo: Arc<Fifo>,
        net_rate: u32,
        out_rate: u32,
        buffer: AppBuffer,
        stats: Arc<Stats>,
    ) -> Result<Self, ConvertError> {
        let channels = fifo.channels();
        let conv = Converter::drifting(net_rate, out_rate, channels, BLOCK, Fixed::Output)?;
        let input = vec![0.0; conv.input_frames_max() * channels];
        let arrived = fifo.arrived();
        let chunk_in = frames_of(buffer.chunk_ns() as f64 / 1e9, net_rate);
        stats.priming.store(true, Ordering::Relaxed);
        stats.out_rate.store(out_rate, Ordering::Relaxed);
        let target_s = buffer.target_s();
        stats
            .target_ns
            .store(buffer.clamped_ns(), Ordering::Relaxed);
        Ok(Player {
            fifo,
            conv,
            ctl: controller(&buffer),
            net_rate,
            out_rate,
            input,
            block: vec![0.0; BLOCK * channels],
            pos: BLOCK,
            callback_in: 0,
            chunk_in,
            state: State::Priming(frames_of(target_s, net_rate)),
            target_s,
            made: 0,
            consumed: 0,
            underruns: stats.underruns(),
            played: stats.played(),
            recentres_before: stats.recentres(),
            resyncs: stats.resyncs(),
            writer: Writer::new(chunk_in, net_rate),
            arrived,
            skipped: 0,
            next_start: None,
            at: 0,
            mono: false,
            stats,
        })
    }

    /// Under `--mono`: every channel summed to both speakers.
    pub fn with_mono(mut self, mono: bool) -> Self {
        self.mono = mono;
        self.stats.mono.store(mono, Ordering::Relaxed);
        self
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
        self.fill_at(None, out, outputs, to);
    }

    /// [`Player::fill_with`] for a block that starts at `start` on the
    /// caller's clock, output frames (a dlink [`crate::Process`]: its
    /// [`crate::Block::start`]). A block that does not start where the last
    /// one ended (the pump skipped blocks, #302) resyncs the player: one
    /// re-prime to the target, counted in [`Stats::resyncs`] (module doc).
    /// So does the first start after blocks with none (free pacing: the
    /// device had no media clock) that followed one with a start.
    /// A caller that fills one block in pieces passes each piece's own
    /// start (the block's start + the frames before it), or every piece
    /// after the first resyncs.
    pub fn fill_at<T>(
        &mut self,
        start: Option<u64>,
        out: &mut [T],
        outputs: usize,
        to: impl Fn(f64) -> T,
    ) {
        let Some(frames) = out.len().checked_div(outputs) else {
            return;
        };
        match (start, self.next_start) {
            (Some(start), Some(Next::At(expected))) if start != expected => {
                self.resync(start.saturating_sub(expected));
            }
            // Back on the starts after blocks with none: its frames were
            // played, so the re-prime skips no time.
            (Some(_), Some(Next::Interrupted)) => self.resync(0),
            _ => {}
        }
        self.next_start = match start {
            Some(s) => Some(Next::At(s.saturating_add(frames as u64))),
            None => self.next_start.map(|_| Next::Interrupted),
        };
        self.callback_in = frames_of(frames as f64 / f64::from(self.out_rate), self.net_rate);
        self.look(self.played);
        let inputs = self.fifo.channels();
        for (f, frame) in out.chunks_exact_mut(outputs).enumerate() {
            if self.pos == BLOCK {
                self.at = self.played + f as u64;
                self.next_block();
            }
            for (o, slot) in frame.iter_mut().enumerate() {
                let src = if self.mono {
                    map::source_mono(o, outputs, inputs)
                } else {
                    map::source(o, outputs, inputs)
                };
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
        s.resyncs.store(self.resyncs, Ordering::Relaxed);
        s.ppm.store(self.ctl.ppm().to_bits(), Ordering::Relaxed);
        s.priming
            .store(matches!(self.state, State::Priming(_)), Ordering::Relaxed);
    }

    /// A write the producer made since the last look, seen at the output's
    /// frame `frames`, goes to its schedule ([`Writer::wrote`]).
    fn look(&mut self, frames: u64) {
        let arrived = self.fifo.arrived();
        if arrived != self.arrived {
            self.arrived = arrived;
            let now_s = self.clock_s(frames);
            self.writer.wrote(now_s, arrived);
        }
    }

    /// The output's clock at its frame `frames` (played), s: the caller's
    /// skipped blocks counted ([`Player::fill_at`]).
    fn clock_s(&self, frames: u64) -> f64 {
        (frames + self.skipped) as f64 / f64::from(self.out_rate)
    }

    /// The caller skipped `frames` of its blocks (or its schedule started
    /// again: 0): one re-prime to the target, counted; the output's clock
    /// counts the skipped time, so the rate fit and the writer's schedule
    /// see none of it. Not an underrun: the producer wrote meanwhile, so
    /// the re-prime only skips what came while the output was not called.
    fn resync(&mut self, frames: u64) {
        self.skipped += frames;
        self.resyncs += 1;
        self.ctl.restart();
        self.state = State::Priming(frames_of(self.target_s, self.net_rate));
    }

    /// The depth to start from, input frames, when the priming ends now
    /// with the producer writing `chunk` frames a cycle and its next write
    /// `until_next` from now, frames, the target `target` and the
    /// converter's call `need`: the controller's reads (one after each
    /// call) then average the target over the writer's cycle, as when the
    /// priming ends right after a write (`until_next` = `chunk`: the
    /// target and half of both). A perfectly regular writer whose chunk is
    /// whole calls keeps the reads at one phase against its writes, which
    /// can leave their mean up to a call off; a real writer's jitter (OBS:
    /// ±2 ms) spreads the phase, and rounding the distance to whole calls
    /// measured worse in the soak (dstack #303), so it is taken as it is.
    fn phased_level(target: usize, chunk: usize, until_next: usize, need: usize) -> usize {
        (target + until_next + need / 2).saturating_sub(chunk / 2)
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
            // A chunked producer (dsend's 10 ms packets, OBS's 1024-frame
            // ticks) makes the same sawtooth from the other side: the
            // priming ends just after a chunk landed, and the depth falls a
            // whole chunk before the next one (dstack #247: at a target of
            // a chunk and a block, that start alone took the depth under
            // the floor). The wider of the two swings is the one to centre.
            let swing = self.callback_in.max(self.chunk_in);
            let need = self.conv.input_frames_next();
            // A chunked producer on its own schedule: the depth whose mean
            // is the target at this moment of its cycle (module doc).
            let now_s = self.clock_s(self.at);
            // The depth first, then the schedule: a write since the call's
            // start (the producer's own thread) is in both, and one landing
            // between the two reads is in the schedule only, so the start
            // errs a chunk high (drained by the bounded level loop), never a
            // chunk low (a dropout).
            let held = self.fifo.len();
            self.look(self.at);
            let level = match self.writer.until_next(now_s, self.arrived) {
                Some(until_next) if self.chunk_in > self.callback_in => Self::phased_level(
                    level,
                    self.chunk_in,
                    frames_of(until_next, self.net_rate),
                    need,
                ),
                _ => level + (swing + need) / 2,
            };
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
        self.steer();
    }

    /// The block's input was not there: silence, counted, and primed again
    /// to the target (module doc).
    fn underrun(&mut self) {
        self.block.fill(0.0);
        self.dropout();
    }

    /// The audio breaks (an underrun, or a depth under the floor that only
    /// a refill mends): counted as an underrun, and the output primes to
    /// the same target again, never more (dstack #247).
    fn dropout(&mut self) {
        self.underruns += 1;
        self.ctl.restart();
        self.state = State::Priming(frames_of(self.target_s, self.net_rate));
    }

    /// The FIFO's target, s.
    pub fn target_s(&self) -> f64 {
        self.target_s
    }

    /// One observation after a converted block, and its answer.
    fn steer(&mut self) {
        let now_s = (self.made + self.skipped) as f64 / f64::from(self.out_rate);
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

/// Where the caller's next block starts ([`Player::fill_at`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Next {
    /// At this output frame of the caller's clock.
    At(u64),
    /// Blocks without a start came after one with a start: the next start
    /// resyncs.
    Interrupted,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod resync_tests;

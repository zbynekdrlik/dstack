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
//!   output's callback or the producer's chunk, [`Player::new`]; a
//!   pump-fed player (dlisten, dstack-asio to ASIO) has the pump's block
//!   for its chunk, so the callback is the swing. The callback is the
//!   frames of one fill call: a caller that fills one callback in pieces
//!   primes for a piece, so dstack-asio fills the driver's whole buffer at
//!   once, dstack #317), then starts from there; an
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
//! - **The producer's line** (dstack #303): a caller that names its block
//!   starts ([`Player::fill_at`], a dlink [`crate::Process`]) is steered on
//!   the depth against the line through the producer's writes, not on the
//!   FIFO's depth sampled after each converter call: the frames at the
//!   last write ([`Fifo::last_write`], stamped on the FIFO's clock) plus
//!   the time since it at the nominal rate, at most one mean write ahead,
//!   less half the mean write (the sawtooth's mean). A chunked writer
//!   (OBS's 1024 frames) and the converter's calls (128 frames) are on
//!   commensurate grids: with a steady writer, which samples fall after a
//!   write stays fixed until the write instant drifts across a call, every
//!   ~444 s at 6 ppm, so the sampled mean and rate point held still, then
//!   stepped by a call's share (2.67 ms), and the braking pinned the level
//!   bound. Writer jitter dithered it away, which hid it from a jittering
//!   test writer. The line moves continuously at any jitter, zero too.
//!   [`Player::fill_with`] (dlisten's output callback) keeps the sampled
//!   depth and reads no clock.
//! - **Underruns:** a block whose input is not there plays silence, counts
//!   in [`Stats`] and primes again to the same target. The target never
//!   grows after a dropout (dstack #247: the automatic cushion of up to
//!   +40 ms is gone): the latency stays the stated term, and a PC that
//!   needs more sets a larger [`AppBuffer`].
//! - **Re-centre:** a depth more than [`hard_excess_s`] over the target
//!   (a burst the controller cannot steer back) skips the excess back to
//!   the target. A deficit has no floor (dstack #303): it is a dropout only
//!   when the FIFO cannot serve the converter's next call (an underrun).
//!   A writer late inside its app's pass (OBS's NDI output ahead of
//!   dlink's TX tap, ~10 ms) took the depth under the old floor
//!   (min(5 ms, target/4)) while the FIFO still held audio and the write
//!   was on its way, and the floor's re-prime made a whole target of
//!   silence out of it.
//!
//! - **The output's own gap** (dstack #307): an output that writes the
//!   FIFO itself just before and asks for the room it has (dlisten's
//!   WASAPI callback, [`Player::fill_pulled`]) comes back from a hold past
//!   its buffer with a pull of the whole time held and a call of the whole
//!   buffer; the time the card played silence of its own ([`output_gap`])
//!   is dropped and counted apart
//!   ([`Stats::output_gaps`]), never left to the excess bound (whose
//!   re-centre mid-call ran the rest of the big call dry).
//!
//! [`Player::fill_with`] allocates nothing, takes no lock and makes no
//! system call: it runs on the audio driver's thread. [`Player::fill_at`]
//! with a start reads the FIFO's clock once a converter call (the app's
//! clock: vDSO `clock_gettime` on Linux, QPC on Windows), nothing more.
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
use super::fifo::{Fifo, Stamp};
use super::map;

mod gap;
mod writer;

pub use gap::{Gap, GapState, Lateness, RECENT_CALLS, SETTLE_CALLS, output_gap, snap};

use writer::Writer;

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

/// How far over the target the depth may go before the excess is skipped
/// back to the target, s: twice the target, and at least twice the app's
/// chunk `chunk_s` (a target set under the chunk still primes half a
/// chunk over it and then takes a whole chunk at once), at most twice
/// [`MAX_APP_BUFFER_NS`] (so the FIFO holds it). A burst of late chunks
/// is skipped; the swing of a chunked producer never is. A deficit never
/// reaches it: under the target only an underrun breaks the audio.
pub fn hard_excess_s(target_s: f64, chunk_s: f64) -> f64 {
    (2.0 * target_s)
        .max(2.0 * chunk_s)
        .min(2.0 * (MAX_APP_BUFFER_NS as f64 / 1e9))
}

/// The output's usual call after a call of `call` frames, the call before
/// it `last` and the usual one `usual` (0: none yet): the smallest call
/// that came twice in a row. A stream's first call is often the whole
/// buffer, a call of the whole buffer after a hold is no usual one, and a
/// stray small call (a wake that found little room) does not repeat, so it
/// cannot make every period look big ([`output_gap`]).
pub fn usual_call(usual: usize, last: usize, call: usize) -> usize {
    if usual == 0 {
        call
    } else if call == last {
        usual.min(call)
    } else {
        usual
    }
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

/// `conv`'s delay at `out_rate` into `stats` ([`Stats::converter_delay_ns`]).
fn publish_delay(stats: &Stats, conv: &Converter, out_rate: u32) {
    let ns = (conv.delay_frames() as f64 * 1e9 / f64::from(out_rate)).round() as u64;
    stats.converter_delay_ns.store(ns, Ordering::Relaxed);
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
    /// The current player's converter delay, ns ([`Stats::converter_delay_ns`]).
    converter_delay_ns: AtomicU64,
    underruns: AtomicU64,
    played: AtomicU64,
    recentres: AtomicU64,
    resyncs: AtomicU64,
    convert_errors: AtomicU64,
    output_gaps: AtomicU64,
    output_lost_frames: AtomicU64,
    /// The converter's correction, ppm (`f64` bits).
    ppm: AtomicU64,
    /// The rate fit's last locked rate, ppm (`f64` bits; with `locked`).
    locked_ppm: AtomicU64,
    /// A player on these counts locked its rate fit once.
    locked: AtomicBool,
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

    /// Blocks played as silence because the converter refused them (never
    /// with the sizes it gave the player; counted so it is never silent,
    /// dstack #303); an app publishes it as `convert_errors`.
    pub fn convert_errors(&self) -> u64 {
        self.convert_errors.load(Ordering::Relaxed)
    }

    /// The converter's correction, ppm.
    pub fn ppm(&self) -> f64 {
        f64::from_bits(self.ppm.load(Ordering::Relaxed))
    }

    /// The rate the ASRC's fit last locked on, ppm: `None` until a player
    /// on these counts locked (60 s of rate points), then the latest
    /// locked value, kept across a re-prime and the next player (another
    /// open). An app keeps it for the next run's [`Player::with_seed_ppm`]
    /// (dstack #382).
    pub fn locked_ppm(&self) -> Option<f64> {
        self.locked
            .load(Ordering::Acquire)
            .then(|| f64::from_bits(self.locked_ppm.load(Ordering::Relaxed)))
    }

    /// The FIFO's target, ms (the device's [`AppBuffer`]).
    pub fn target_ms(&self) -> f64 {
        self.target_ns.load(Ordering::Relaxed) as f64 / 1e6
    }

    /// The current player's converter delay, ns: how far behind its input
    /// the sinc's output runs, at the output's rate (dstack #382; 1333333
    /// at 96 <-> 96 kHz with the default 256 taps, 250000 with 48,
    /// [`Player::with_taps`]). Part of the bridge's latency beside the
    /// FIFO's target.
    pub fn converter_delay_ns(&self) -> u64 {
        self.converter_delay_ns.load(Ordering::Relaxed)
    }

    /// The current output's rate, Hz.
    pub fn out_rate(&self) -> u32 {
        self.out_rate.load(Ordering::Relaxed)
    }

    /// Whether the output waits for the FIFO to fill (silence).
    pub fn priming(&self) -> bool {
        self.priming.load(Ordering::Relaxed)
    }

    /// Gaps of the output itself: holds of its callback past its buffer
    /// whose audio, due while the output played silence of its own, was
    /// dropped ([`Player::fill_pulled`], dstack #307), counted once each
    /// gap is settled. Not the bridge's dropouts: an app publishes it as
    /// `output_gaps`.
    pub fn output_gaps(&self) -> u64 {
        self.output_gaps.load(Ordering::Relaxed)
    }

    /// Input frames dropped for the output's gaps ([`Stats::output_gaps`]):
    /// the time the output played silence of its own, to within a
    /// converter call.
    pub fn output_lost_frames(&self) -> u64 {
        self.output_lost_frames.load(Ordering::Relaxed)
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
    /// The producer's chunk ([`Player::new`]'s `producer_chunk_ns`), in
    /// input frames.
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
    convert_errors: u64,
    output_gaps: u64,
    output_lost_frames: u64,
    /// The output's usual call, input frames ([`output_gap`]; 0: none
    /// yet).
    usual_call_in: usize,
    /// The output's last call, input frames ([`usual_call`]).
    last_call_in: usize,
    /// The output's largest call, input frames: its whole buffer, the call
    /// that follows a gap ([`output_gap`]).
    buffer_call_in: usize,
    /// The lateness of the output's calls outside a gap ([`Lateness`]).
    late: Lateness,
    /// The output's gap left open ([`output_gap`]).
    gap: GapState,
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
    /// The current call named its start: steer on the producer's line.
    on_line: bool,
    /// The writer's schedule is on the FIFO's clock ([`Player::look`]).
    writer_on_line: bool,
    /// The last stamp read ([`Fifo::last_write`]).
    stamp: Option<Stamp>,
    stats: Arc<Stats>,
}

/// The controller at `buffer`'s target, no floor (dstack #303: a deficit
/// is a dropout only when the FIFO cannot serve a call), its excess from
/// the target ([`hard_excess_s`]) and no cushion (dstack #247), its
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
        0.0,
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
    ///
    /// `producer_chunk_ns` is what the FIFO's producer writes at once, ns
    /// (see [`super::chunk_ns`]), on its own and apart from the target's
    /// terms (dstack #317): an app that writes its tick (OBS's TX: its
    /// 1024 frames, [`AppBuffer::chunk_ns`]), a capture its packet (dsend),
    /// an ASIO driver its buffer (dstack-asio from ASIO), and the dlink pump
    /// its block ([`AppBuffer::block_ns`]: dlisten, dstack-asio to ASIO).
    /// A producer whose chunk is wider than the output's call is followed on
    /// its own schedule (module doc); taking the target's chunk for a
    /// pump-fed player followed a schedule the pump does not keep, and
    /// after a rebootstrap re-primed under one callback's take on every
    /// callback (dasio's ASIO bridge, 15.11.2026).
    pub fn new(
        fifo: Arc<Fifo>,
        net_rate: u32,
        out_rate: u32,
        buffer: AppBuffer,
        producer_chunk_ns: u64,
        stats: Arc<Stats>,
    ) -> Result<Self, ConvertError> {
        let channels = fifo.channels();
        let conv = Converter::drifting(net_rate, out_rate, channels, BLOCK, Fixed::Output)?;
        let input = vec![0.0; conv.input_frames_max() * channels];
        let arrived = fifo.arrived();
        let chunk_in = frames_of(producer_chunk_ns as f64 / 1e9, net_rate);
        stats.priming.store(true, Ordering::Relaxed);
        stats.out_rate.store(out_rate, Ordering::Relaxed);
        publish_delay(&stats, &conv, out_rate);
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
            convert_errors: stats.convert_errors(),
            output_gaps: stats.output_gaps(),
            output_lost_frames: stats.output_lost_frames(),
            usual_call_in: 0,
            last_call_in: 0,
            buffer_call_in: 0,
            late: Lateness::default(),
            gap: GapState::default(),
            writer: Writer::new(chunk_in, net_rate),
            arrived,
            skipped: 0,
            next_start: None,
            at: 0,
            mono: false,
            on_line: false,
            writer_on_line: false,
            stamp: None,
            stats,
        })
    }

    /// Starts the ASRC from a known rate (dstack #382): `ppm` is the last
    /// run's [`Stats::locked_ppm`] for the same producer and caller (the
    /// same two clocks). It is the controller's rate until its own fit
    /// locks (60 s), and the correction starts at it, so a known drift
    /// costs no depth in the first minute (7.5 ppm drifted about 0.45 ms
    /// before the lock, dasio#120). Held within the controller's bound
    /// (`asrc::MAX_PPM`). Call it right after [`Player::new`].
    pub fn with_seed_ppm(mut self, ppm: f64) -> Self {
        self.ctl.seed_ppm(ppm);
        self
    }

    /// The converter with a sinc of `taps` instead of
    /// [`dstack_resample::TAPS`] (dstack #382,
    /// [`dstack_resample::Converter::drifting_with_taps`]: a shorter filter
    /// delays less and keeps less of the band; at equal rates 48 taps
    /// still keep 20 Hz to 20 kHz flat). Call it right after
    /// [`Player::new`], before any fill. Refused (the player is dropped)
    /// for a length the converter refuses.
    pub fn with_taps(mut self, taps: usize) -> Result<Self, ConvertError> {
        let channels = self.fifo.channels();
        let conv = Converter::drifting_with_taps(
            self.net_rate,
            self.out_rate,
            channels,
            BLOCK,
            Fixed::Output,
            taps,
        )?;
        self.input = vec![0.0; conv.input_frames_max() * channels];
        publish_delay(&self.stats, &conv, self.out_rate);
        self.conv = conv;
        Ok(self)
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

    /// [`Player::fill_with`] for an output that writes the FIFO itself
    /// just before (its RX pull, dstack #307: `pulled` the frames the pull
    /// wrote, [`super::RxPull::pull`]) and asks for the room it has (a
    /// fill-the-room driver: WASAPI shared mode, where cpal asks for the
    /// buffer's frames less its padding).
    ///
    /// Such an output's callback held past its buffer comes back to a card
    /// that played silence meanwhile: its pull reads the whole time held
    /// and the call asks for the whole buffer, so the time the card played
    /// silence stays in the FIFO. Up to v0.6.170
    /// that excess went past the controller's bound, which re-centred to
    /// the target after the call's first converter block and left the rest
    /// of the big call to run the FIFO dry (an underrun), or, under the
    /// bound, stayed as latency (mbc's dlisten, 17.11.2026). Now that
    /// silence is dropped ([`output_gap`]: what surely was silence before
    /// the big call, the rest settled over the next calls, the call's
    /// phase off the engine's grid left), counted as the output's own gap
    /// ([`Stats::output_gaps`]), and the output's clock counts the time (as
    /// a resync's skip), so the rate fit sees no step: no re-centre, no
    /// underrun, the latency the same. Only a dry call (the whole buffer,
    /// bigger than the usual one) opens a gap: a fixed-size callback (an
    /// ASIO switch) never makes one,
    /// which is why it is not this function's caller.
    pub fn fill_pulled<T>(
        &mut self,
        pulled: usize,
        out: &mut [T],
        outputs: usize,
        to: impl Fn(f64) -> T,
    ) {
        if let Some(frames) = out.len().checked_div(outputs) {
            let call = frames_of(frames as f64 / f64::from(self.out_rate), self.net_rate);
            let usual = self.usual_call_in;
            self.buffer_call_in = self.buffer_call_in.max(call);
            let gap = output_gap(
                self.gap,
                pulled,
                call,
                usual,
                self.buffer_call_in,
                self.late.over_floor(),
            );
            // A call outside a gap is the floor's; a gap's calls carry its
            // silence, so the floor starts afresh after one.
            match (self.gap.left, gap.state.left) {
                (0, 0) => self.late.add(pulled, call),
                (_, 0) => self.late.forget(),
                _ => {}
            }
            self.usual_call_in = usual_call(usual, self.last_call_in, call);
            self.last_call_in = call;
            if self.state == State::Running {
                self.gap = gap.state;
                // The converter keeps its history: the audio jumps as the
                // card's did, and a reset would add a step from silence (its
                // filter's ring) to the card's gap. The output's clock counts
                // the time in its own frames, converted from the whole loss
                // so far so the rounding of each drop does not add up. (No
                // test of `drop > 0` first: skipping 0 is a no-op, and its
                // `>=` mutant could not be told apart.)
                let dropped = self.fifo.skip(gap.drop) as u64;
                let out_of = |lost: u64| {
                    frames_of(lost as f64 / f64::from(self.net_rate), self.out_rate) as u64
                };
                let lost = self.output_lost_frames + dropped;
                self.skipped += out_of(lost) - out_of(self.output_lost_frames);
                self.output_lost_frames = lost;
                self.output_gaps += u64::from(gap.ended);
            } else {
                // Priming (an underrun after the gap's first drop, too):
                // the priming starts from its own level; a gap that
                // dropped frames still counts. (The lateness floor of a gap
                // cut short is not started afresh here: the priming's calls
                // refill it before the output plays.)
                self.output_gaps += u64::from(self.gap.dropped > 0);
                self.gap = GapState::default();
            }
        }
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
        self.on_line = start.is_some();
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
        s.convert_errors
            .store(self.convert_errors, Ordering::Relaxed);
        s.output_gaps.store(self.output_gaps, Ordering::Relaxed);
        s.output_lost_frames
            .store(self.output_lost_frames, Ordering::Relaxed);
        s.ppm.store(self.ctl.ppm().to_bits(), Ordering::Relaxed);
        if self.ctl.locked() {
            s.locked_ppm
                .store(self.ctl.rate_ppm().to_bits(), Ordering::Relaxed);
            s.locked.store(true, Ordering::Release);
        }
        s.priming
            .store(matches!(self.state, State::Priming(_)), Ordering::Relaxed);
    }

    /// A write the producer made since the last look goes to its schedule
    /// ([`Writer::wrote`]): on the producer's line at its stamp (the
    /// FIFO's clock, dstack #303), else seen at the output's frame
    /// `frames` (the output's clock). A change of clock starts the
    /// schedule again.
    fn look(&mut self, frames: u64) {
        let fresh = self.on_line.then(|| self.fifo.last_write()).flatten();
        if fresh.is_some() {
            self.stamp = fresh;
        }
        // On the line once a write was stamped; a stamp read that met a
        // write in progress on every try changes nothing.
        let on_line = self.on_line && self.stamp.is_some();
        if on_line != self.writer_on_line {
            self.writer = Writer::new(self.chunk_in, self.net_rate);
            self.writer_on_line = on_line;
        }
        let (at_s, arrived) = match (on_line, fresh) {
            (true, Some(stamp)) => (stamp.at_ns as f64 / 1e9, stamp.arrived),
            (true, None) => return,
            (false, _) => (self.clock_s(frames), self.fifo.arrived()),
        };
        if arrived != self.arrived {
            self.arrived = arrived;
            self.writer.wrote(at_s, arrived);
        }
    }

    /// The schedule's clock now, s ([`Player::look`]): the FIFO's on the
    /// producer's line, else the output's at its frame `frames`.
    fn schedule_s(&self, frames: u64) -> f64 {
        if self.writer_on_line {
            self.fifo.now_ns() as f64 / 1e9
        } else {
            self.clock_s(frames)
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

    /// [`Player::phased_level`], never under one output call's take: the
    /// output's call `call` and the converter's `need`, input frames
    /// (dstack #317). A producer modelled wider than it writes (dasio's
    /// to-ASIO player took the ASIO buffer for the pump's block), or a
    /// schedule it no longer keeps, re-primed under one call's take, so
    /// every call ran dry and re-primed there again, for minutes. The
    /// floor is the call, not the target: a sawtooth's bottom is under the
    /// target by design (half a chunk), and a floor at the target would put
    /// the mean up to half a chunk over it again (dstack #303).
    fn phased_start(
        target: usize,
        chunk: usize,
        until_next: usize,
        need: usize,
        call: usize,
    ) -> usize {
        Self::phased_level(target, chunk, until_next, need).max(call + need)
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
            // a chunk and a block, that start alone took the depth down to
            // a dropout). The wider of the two swings is the one to centre.
            let swing = self.callback_in.max(self.chunk_in);
            let need = self.conv.input_frames_next();
            // A chunked producer on its own schedule: the depth whose mean
            // is the target at this moment of its cycle (module doc).
            // The depth first, then the schedule: a write since the call's
            // start (the producer's own thread) is in both, and one landing
            // between the two reads is in the schedule only, so the start
            // errs a chunk high (drained by the bounded level loop), never a
            // chunk low (a dropout).
            let held = self.fifo.len();
            self.look(self.at);
            let now_s = self.schedule_s(self.at);
            let level = match self.writer.until_next(now_s, self.arrived) {
                Some(until_next) if self.chunk_in > self.callback_in => Self::phased_start(
                    level,
                    self.chunk_in,
                    frames_of(until_next, self.net_rate),
                    need,
                    self.callback_in,
                ),
                _ => level + (swing + need) / 2,
            } + if self.writer_on_line {
                // On the producer's line the first observation comes
                // after this call's take and the line holds through the
                // call: half a call more, a whole call over the level in
                // all, where the sampled reads average half a call
                // (dstack #303).
                need / 2
            } else {
                0
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
            // on the driver's thread, and counted (dstack #303).
            self.block.fill(0.0);
            self.convert_errors += 1;
        }
        self.consumed += need as u64;
        self.steer();
    }

    /// The block's input was not there, the one dropout (dstack #303: no
    /// floor): silence, counted as an underrun, and the output primes to
    /// the same target again, never more (dstack #247).
    fn underrun(&mut self) {
        self.block.fill(0.0);
        self.underruns += 1;
        self.ctl.restart();
        self.state = State::Priming(frames_of(self.target_s, self.net_rate));
    }

    /// The FIFO's target, s.
    pub fn target_s(&self) -> f64 {
        self.target_s
    }

    /// The depth and the producer's count, frames: on the producer's line
    /// for a caller that names its starts and once a write was stamped
    /// (module doc), else the FIFO's own.
    /// `None` when the stamp read met a write in progress on every try
    /// after one was stamped: that one observation is skipped, never taken
    /// in the other measure (the sampled depth is up to half a write off
    /// the line).
    fn observed(&mut self) -> Option<(f64, f64)> {
        let sampled = (self.fifo.len() as f64, self.fifo.arrived() as f64);
        if !self.on_line {
            return Some(sampled);
        }
        match self.fifo.last_write() {
            Some(stamp) => {
                self.stamp = Some(stamp);
                Some(producer_line(
                    stamp,
                    self.fifo.now_ns(),
                    f64::from(self.net_rate),
                    self.fifo.read_frames(),
                ))
            }
            None if self.stamp.is_some() => None,
            // No write yet.
            None => Some(sampled),
        }
    }

    /// One observation after a converted block, and its answer.
    fn steer(&mut self) {
        let now_s = (self.made + self.skipped) as f64 / f64::from(self.out_rate);
        let net = f64::from(self.net_rate);
        let Some((depth, produced)) = self.observed() else {
            return;
        };
        let depth_s = depth / net;
        let produced_s = produced / net;
        let ppm = match self.ctl.observe(now_s, depth_s, 2.0 * now_s - produced_s) {
            Step::Hold(ppm) => ppm,
            Step::Recentre { by_s, ppm } | Step::Prime { by_s, ppm } => {
                // Only an excess re-centres (a floor of 0: no depth is under
                // it, and a deficit is at most the target, under the
                // excess bound), so `by_s` is the excess to skip, negative.
                // `Prime` never comes: the controller is made without
                // `with_priming` (the player primes itself, `State::Priming`).
                self.fifo.skip(frames_of(-by_s, self.net_rate));
                // The audio jumps: the filter's history is no longer its
                // past.
                self.conv.reset();
                ppm
            }
        };
        // Within the converter's room by the controller's own bound.
        let _ = self.conv.set_ppm(ppm);
    }
}

/// The depth and the producer's count on the producer's line at `now_ns`
/// (module doc), frames: `stamp` the last write, `rate` the producer's
/// nominal rate, `read` the reader's position. The time since the write
/// counts from 0 (a write stamped after `now_ns` was read) to one mean
/// write's period: a late write holds the line where it was due, so the
/// depth falls with the reads until it comes, and a stopped writer never
/// shows an excess. The mean write stands for the producer's cycle: one
/// push a cycle (OBS's tick, dsend's capture packet, an ASIO callback). A
/// producer that pushed one cycle in several pieces back to back would
/// halve it, and the line would saturate halfway through the cycle.
fn producer_line(stamp: Stamp, now_ns: i64, rate: f64, read: u64) -> (f64, f64) {
    let mean = stamp.mean_write();
    let elapsed_s = (now_ns.saturating_sub(stamp.at_ns) as f64 / 1e9).clamp(0.0, mean / rate);
    let ahead = elapsed_s * rate - mean / 2.0;
    (
        stamp.written as f64 - read as f64 + ahead,
        stamp.arrived as f64 + ahead,
    )
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

#[cfg(test)]
mod line_tests;

#[cfg(test)]
mod gap_tests;

// SPDX-License-Identifier: MIT OR Apache-2.0

//! The two sides of a Dante <-> ASIO bridge (dstack #243, the owner's
//! ROZHODNUTÉ of 13.11.2026: a dasio instance kind "bridge to" a local ASIO
//! driver, e.g. a SoundGrid virtual soundcard, whose clock is not Dante's).
//!
//! The clock crossing is `dlink::bridge`'s, one per direction: a lock-free
//! [`Fifo`] written on the producer's clock and a [`Player`] (dstack-
//! resample's drifting sinc, steered by the ASRC controller) pulling it on
//! the consumer's, held at the [`AppBuffer`] target (dstack #247): the ASIO
//! buffer + one pump block by default, a configured `app_buffer_ns`
//! verbatim; never grown after a dropout (counted in its [`Stats`]).
//!
//! - **Dante -> ASIO:** the driver's buffer switch runs [`AsioSide`], whose
//!   player plays the to-ASIO FIFO at the driver's clock (dlisten's
//!   direction). Pulled ([`Bridge::pull_to_asio`], dstack #307, the
//!   owner's ROZHODNUTÉ of 16.11.2026: no latency term), the switch itself
//!   first drains the flow sockets and reads the RX rings by position
//!   (media now less P, `dlink::bridge::RxPull`) into the FIFO in one
//!   push, so a late pump never empties it: the thread with the deadline
//!   does the work. Otherwise the pump runs [`DanteSide`], which writes
//!   each RX block into the FIFO at the network's clock, and a pump that
//!   wakes late (2 to 20 ms on a loaded PC) lets it run dry before it
//!   catches up (`tests/to_asio_pull.rs`: 13 underruns in 8 s under 31
//!   stalls of 10 to 20 ms, 0 pulled). The latency is the same either way
//!   (the same P and target); pulled, the wait from a sample's due time to
//!   its playing loses the half pump block it waited in the ring.
//! - **ASIO -> Dante:** [`AsioSide`] writes the driver's inputs into the
//!   from-ASIO FIFO at the driver's clock; [`DanteSide`]'s player pulls them
//!   for TX at the network's (dsend's direction).
//!
//! The pump's side lives as long as the device; the driver's side is made
//! anew at every open ([`Bridge::asio_side`], from the rate and buffer the
//! open read), and so is the from-ASIO player, handed to the running
//! `DanteSide` through `dlink::bridge`'s [`Switch`] (dsend's handover) and
//! given back the same way to be freed off the pump's thread. Until the
//! first open the pump's side plays a player on an empty FIFO (priming:
//! silence), so `from_asio_stats` reads priming at a 1 ms target until the
//! driver's first open. The switch keeps one handover in flight: while the
//! pump does not run (the Dante device restarting), a second driver open is
//! refused and the runner retries it after its backoff. Neither `process` allocates, and
//! neither waits on a lock (the switch's bounded channel never blocks; the
//! pull's lock is only tried). Neither makes a system call but a clock read
//! (the FIFO's stamp, the block age) and, pulled, the pull's non-blocking
//! socket reads on the driver's side.
//!
//! A pulled bridge (dstack #307): the app claims the RX rings' writer for
//! itself (`RxShared::app_intake`, before the pump is made, its map with
//! no RX channels; `dlink::Link::start_pulling` does both), makes the
//! `RxPull` on [`Bridge::to_asio_fifo`] and hands it over with
//! [`Bridge::pull_to_asio`] before the driver's first open, and calls
//! [`Bridge::keep_pulled`] from its own loop about every 250 ms: while no
//! driver runs, that drain keeps the flow heard (else it reads silent and
//! is closed and asked again).
//!
//! The from-ASIO side's status figures ([`BlockAge`], dstack #325): the
//! TX packets' age and the smallest receiver latency it serves, from the
//! TX side's own measure, and a warning when a receiver counts our packets
//! late ([`BlockAge::look`] from the status loop); and, as a diagnostic,
//! its blocks' modelled age at the pump's take (the driver's side records
//! each hand-over, the pump's side its take; [`Bridge::from_asio_age`]).

use std::sync::{Arc, Mutex, OnceLock, TryLockError};

use dlink::bridge::pull::{IdleWatch, Look, PullStats};
use dlink::bridge::{
    AppBuffer, Clock, Fifo, Player, RxPull, Stats, Switch, Taker, chunk_ns, fifo_for, switch,
};
use dstack_audio_io::{Block, Process};

use crate::device::Opened;

mod age;
pub use age::{AgeReport, BlockAge, NETWORK_ALLOWANCE_NS};

/// Frames a side converts at once at least (its interleaved scratch): the
/// pump's side a whole pump block, the driver's side its whole buffer
/// ([`Bridge::asio_side`]), when wider.
const CHUNK: usize = 256;

/// What a bridge carries (a dasio bridge instance's settings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Spec {
    /// The Dante device's rate, Hz.
    pub dante_rate: u32,
    /// The pump's block, frames at `dante_rate`.
    pub block_frames: u32,
    /// Channels from Dante to the driver (RX -> its outputs).
    pub to_asio: usize,
    /// Channels from the driver to Dante (its inputs -> TX).
    pub from_asio: usize,
    /// The FIFO target set in the config, ns (`None`: the ASIO buffer + one
    /// pump block).
    pub app_buffer_ns: Option<u64>,
}

/// The bridge: its FIFOs, players' counts and the pump's side (module doc).
pub struct Bridge {
    spec: Spec,
    to_asio: Option<Arc<Fifo>>,
    to_stats: Arc<Stats>,
    from_stats: Arc<Stats>,
    switch: Option<Switch<Player>>,
    dante: Option<DanteSide>,
    age: Arc<BlockAge>,
    /// The to-ASIO RX pull, once the app hands it over
    /// ([`Bridge::pull_to_asio`]): shared with the pump's side and every
    /// driver side.
    pull: Arc<OnceLock<Mutex<RxPull>>>,
    /// The pull's counts and the idle watch over them
    /// ([`Bridge::keep_pulled`]).
    pulled: Option<(Arc<PullStats>, IdleWatch)>,
}

/// What the FIFO's producer writes at once for the to-ASIO player (dstack
/// #317, #307), ns: the driver's buffer when the driver side pulls the
/// rings (one push a buffer switch), else the pump's block (the pump
/// writes it, a block at a time).
pub fn to_asio_chunk_ns(buffer: AppBuffer, pulled: bool) -> u64 {
    if pulled {
        buffer.chunk_ns()
    } else {
        buffer.block_ns()
    }
}

impl Bridge {
    /// A bridge for `spec`; an error only for a rate the converter refuses.
    pub fn new(spec: Spec) -> Result<Self, String> {
        Self::with_clock(spec, Arc::new(dstack::clock::app_monotonic_ns))
    }

    /// [`Bridge::new`] with the from-ASIO block age ([`BlockAge`]) on
    /// `clock` (a simulation's time; the app's clock in [`Bridge::new`]).
    pub fn with_clock(spec: Spec, clock: Clock) -> Result<Self, String> {
        let age = Arc::new(BlockAge::new(clock));
        let to_asio = (spec.to_asio > 0).then(|| Arc::new(fifo_for(spec.to_asio, spec.dante_rate)));
        let from_stats = Arc::new(Stats::default());
        let (from_asio, switch) = if spec.from_asio > 0 {
            // Until the driver's first open: a FIFO nobody writes, so the
            // player primes (silence) and counts no underrun.
            let idle = Arc::new(fifo_for(spec.from_asio, spec.dante_rate));
            let buffer = AppBuffer::new(0, chunk_ns(u64::from(spec.block_frames), spec.dante_rate))
                .set(Some(dlink::bridge::MIN_APP_BUFFER_NS));
            let first = Player::new(
                idle,
                spec.dante_rate,
                spec.dante_rate,
                buffer,
                // Nobody writes it.
                0,
                Arc::clone(&from_stats),
            )
            .map_err(|e| e.to_string())?;
            let (taker, switch) = switch();
            (Some((first, taker)), Some(switch))
        } else {
            (None, None)
        };
        let pull = Arc::new(OnceLock::new());
        let dante = DanteSide {
            to_asio: to_asio.clone(),
            pull: Arc::clone(&pull),
            from_asio,
            outputs: spec.from_asio,
            // A pump block wider than a piece would prime the from-ASIO
            // player for a piece (dstack #317): one call a block.
            scratch: vec![0.0; CHUNK.max(spec.block_frames as usize) * spec.from_asio],
            age: Arc::clone(&age),
        };
        Ok(Bridge {
            spec,
            to_asio,
            to_stats: Arc::new(Stats::default()),
            from_stats,
            switch,
            dante: Some(dante),
            age,
            pull,
            pulled: None,
        })
    }

    /// The Dante -> ASIO side pulled by the driver's buffer switch (dstack
    /// #307; module doc): `pull` reads the device's RX rings by position
    /// into this bridge's to-ASIO FIFO ([`Bridge::to_asio_fifo`]). Make it
    /// with `dlink::Link::start_pulling` (its FIFO this one), or with
    /// `RxPull::new` over `RxShared::app_intake`, claimed before the pump
    /// (`stream::Pump::new`) is made, with a pump map of no RX channels.
    /// From here on the pump's side writes nothing into the to-ASIO FIFO
    /// and each driver side pulls before it plays. Give it before the
    /// driver's first open ([`Bridge::asio_side`]: the player's producer
    /// is the driver's buffer then), and call [`Bridge::keep_pulled`]
    /// while the bridge runs.
    ///
    /// Refused for a bridge with no to-ASIO channels, a pull of another
    /// FIFO, or a second pull.
    pub fn pull_to_asio(&mut self, pull: RxPull) -> Result<(), String> {
        let Some(fifo) = &self.to_asio else {
            return Err("the bridge has no channel to ASIO to pull".to_string());
        };
        if !Arc::ptr_eq(fifo, pull.fifo()) {
            return Err("the pull writes another FIFO than the bridge's to-ASIO one".to_string());
        }
        let stats = pull.stats();
        self.pull
            .set(Mutex::new(pull))
            .map_err(|_| "the bridge pulls already".to_string())?;
        self.pulled = Some((stats, IdleWatch::default()));
        Ok(())
    }

    /// The look at the to-ASIO pull ([`Bridge::pull_to_asio`]), from the
    /// app's own loop on a steady period (about every 250 ms; never the
    /// pump or the driver's callback). While no driver side pulls (no
    /// driver open: closed, the reopen backoff, a stalled driver), it
    /// drains the flow sockets itself (`RxPull::drain_idle`), so the flow
    /// stays heard and the device keeps it; a buffer switch that pulls
    /// again takes over at once. Only under the lock, and only while no
    /// switch pulled since the look. A lock a panic poisoned is cleared
    /// here (the switches pull on through it). `None` without a pull;
    /// else the look, for the app's log (the change both ways, the
    /// restarts' rise).
    pub fn keep_pulled(&mut self) -> Option<Look> {
        let pull = self.pull.get()?;
        let (stats, watch) = self.pulled.as_mut()?;
        if pull.is_poisoned() {
            pull.clear_poison();
        }
        let pulls = stats.pulls();
        let look = watch.look(pulls, stats.restarts());
        if look.idle
            && let Ok(mut p) = pull.try_lock()
            && stats.pulls() == pulls
        {
            p.drain_idle();
        }
        Some(look)
    }

    /// The to-ASIO pull's counts ([`Bridge::pull_to_asio`]; `None`
    /// without one).
    pub fn to_asio_pull_stats(&self) -> Option<Arc<PullStats>> {
        self.pulled.as_ref().map(|(s, _)| Arc::clone(s))
    }

    pub fn spec(&self) -> Spec {
        self.spec
    }

    /// The pump's side, once (the device's `Process`).
    pub fn dante_side(&mut self) -> Option<DanteSide> {
        self.dante.take()
    }

    /// The FIFO target for a driver opened as `opened` (dstack #247): its
    /// buffer + one pump block, or the configured value.
    pub fn app_buffer(&self, opened: &Opened) -> AppBuffer {
        let rate = opened.rate.round() as u32;
        AppBuffer::new(
            chunk_ns(u64::from(opened.buffer_frames), rate),
            chunk_ns(u64::from(self.spec.block_frames), self.spec.dante_rate),
        )
        .set(self.spec.app_buffer_ns)
    }

    /// The driver's side for a driver opened as `opened` (its rate, buffer
    /// and channels), and the from-ASIO player handed to the pump's side.
    /// Refused when the open's channels are not the spec's counts.
    pub fn asio_side(&mut self, opened: &Opened) -> Result<AsioSide, String> {
        if (opened.outputs.len(), opened.inputs.len()) != (self.spec.to_asio, self.spec.from_asio) {
            return Err(format!(
                "the driver's channels ({} out, {} in) are not the bridge's ({} to ASIO, {} from \
                 ASIO)",
                opened.outputs.len(),
                opened.inputs.len(),
                self.spec.to_asio,
                self.spec.from_asio
            ));
        }
        let buffer = self.app_buffer(opened);
        if let Some(problem) = buffer.problem() {
            return Err(problem);
        }
        let rate = opened.rate.round() as u32;
        let dante = self.spec.dante_rate;
        let chunk = to_asio_chunk_ns(buffer, self.pull.get().is_some());
        let player = match &self.to_asio {
            Some(fifo) => Some(
                Player::new(
                    Arc::clone(fifo),
                    dante,
                    rate,
                    buffer,
                    chunk,
                    Arc::clone(&self.to_stats),
                )
                .map_err(|e| e.to_string())?,
            ),
            None => None,
        };
        let mut from_asio = None;
        if let Some(switch) = &mut self.switch {
            let fifo = Arc::new(fifo_for(self.spec.from_asio, rate));
            let p = Player::new(
                Arc::clone(&fifo),
                rate,
                dante,
                buffer,
                // The driver writes the from-ASIO FIFO, its buffer at once.
                buffer.chunk_ns(),
                Arc::clone(&self.from_stats),
            )
            .map_err(|e| e.to_string())?;
            switch.send(p)?;
            from_asio = Some(fifo);
        }
        // The whole buffer in one call: the player primes for the take of
        // one callback, and pieces of it would count half a buffer when the
        // driver takes the whole one back to back (dstack #317).
        let frames = CHUNK.max(opened.buffer_frames as usize);
        Ok(AsioSide {
            pull: Arc::clone(&self.pull),
            player,
            outputs: self.spec.to_asio,
            age: from_asio.is_some().then(|| Arc::clone(&self.age)),
            rate,
            from_asio,
            scratch: vec![0.0; frames * self.spec.to_asio],
        })
    }

    /// Frees the players the pump's side gave back; returns how many.
    pub fn collect(&mut self) -> usize {
        self.switch.as_mut().map_or(0, Switch::collect)
    }

    /// The Dante -> ASIO player's counts (depth target, ppm, underruns).
    pub fn to_asio_stats(&self) -> Arc<Stats> {
        Arc::clone(&self.to_stats)
    }

    /// The ASIO -> Dante player's counts.
    pub fn from_asio_stats(&self) -> Arc<Stats> {
        Arc::clone(&self.from_stats)
    }

    /// The from-ASIO figures (dstack #325): [`BlockAge::report`] for the
    /// status, [`BlockAge::look`] from its loop.
    pub fn from_asio_age(&self) -> Arc<BlockAge> {
        Arc::clone(&self.age)
    }

    /// The to-ASIO FIFO (its depth and overflows for a status line).
    pub fn to_asio_fifo(&self) -> Option<Arc<Fifo>> {
        self.to_asio.clone()
    }
}

/// The player's output for `frames` frames into the block's first
/// `outputs` channels, through `scratch` (interleaved, `outputs` > 0): in
/// pieces of the frames `scratch` holds.
fn play(player: &mut Player, block: &mut Block<'_>, outputs: usize, scratch: &mut [f64]) {
    let frames = block.frames();
    let piece = scratch.len() / outputs;
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(piece);
        let buf = &mut scratch[..n * outputs];
        // Each piece's own start: a block the engine skipped resyncs the
        // player once (dstack #303), a piece of the same block never.
        let start = block.start().map(|s| s + done as u64);
        player.fill_at(start, buf, outputs, |x| x);
        for c in 0..outputs {
            // A channel the block lacks is skipped.
            for (f, slot) in block.output(c).iter_mut().skip(done).take(n).enumerate() {
                *slot = buf[f * outputs + c];
            }
        }
        done += n;
    }
}

/// The block's inputs into `fifo` (a channel the block lacks: silence).
fn feed(fifo: &Fifo, block: &Block<'_>) {
    fifo.push_with(block.frames(), |f, c| {
        block.input(c).get(f).copied().unwrap_or(0.0)
    });
}

/// The pump's side: RX into the to-ASIO FIFO, the from-ASIO player into TX.
pub struct DanteSide {
    to_asio: Option<Arc<Fifo>>,
    pull: Arc<OnceLock<Mutex<RxPull>>>,
    /// The from-ASIO player and its handover's end.
    from_asio: Option<(Player, Taker<Player>)>,
    outputs: usize,
    scratch: Vec<f64>,
    age: Arc<BlockAge>,
}

impl Process for DanteSide {
    fn process(&mut self, block: &mut Block<'_>) {
        // Pulled, the driver's side writes the to-ASIO FIFO (dstack #307);
        // a pump with no RX inputs (a pulling app's) has nothing for it.
        if let Some(fifo) = &self.to_asio
            && self.pull.get().is_none()
            && block.inputs() > 0
        {
            feed(fifo, block);
        }
        match &mut self.from_asio {
            Some((player, taker)) => {
                taker.take(player);
                // The pump's take of the driver's blocks (dstack #325).
                self.age.taken();
                play(player, block, self.outputs, &mut self.scratch);
            }
            None => block.zero_outputs(),
        }
    }
}

/// The driver's side: the to-ASIO player into its outputs, its inputs into
/// the from-ASIO FIFO.
pub struct AsioSide {
    /// The to-ASIO pull, once the app gave it (pulled before each play).
    pull: Arc<OnceLock<Mutex<RxPull>>>,
    player: Option<Player>,
    outputs: usize,
    from_asio: Option<Arc<Fifo>>,
    /// The from-ASIO block age (with `from_asio`) and the driver's rate.
    age: Option<Arc<BlockAge>>,
    rate: u32,
    scratch: Vec<f64>,
}

impl AsioSide {
    /// The to-ASIO pull's turn (dstack #307): drain the flow sockets and
    /// read the rings into the to-ASIO FIFO, up to media now less P. Never
    /// waits: only an earlier run's last switch or the app's idle drain
    /// ([`Bridge::keep_pulled`]) can hold the lock, and this switch then
    /// plays what the FIFO holds. A lock a panicking pull poisoned still
    /// pulls (its state is read positions). Its system calls are the
    /// pull's: the clock read and the sockets' non-blocking reads.
    fn pull_rx(&self) {
        let Some(pull) = self.pull.get() else {
            return;
        };
        match pull.try_lock() {
            Ok(mut p) => {
                p.pull();
            }
            Err(TryLockError::Poisoned(e)) => {
                e.into_inner().pull();
            }
            Err(TryLockError::WouldBlock) => {}
        }
    }

    /// The rest of the buffer switch, after the pull: the driver's inputs
    /// into the from-ASIO FIFO, the hand-over's age, the to-ASIO player
    /// into the driver's outputs. No allocation, lock or blocking call
    /// (rtsan's `nonblocking`, `tests/rtsan.rs`); the FIFO's stamp and the
    /// age read the app's clock.
    #[cfg_attr(iem_rtsan, sanitize(realtime = "nonblocking"))]
    fn serve(&mut self, block: &mut Block<'_>) {
        if let Some(fifo) = &self.from_asio {
            feed(fifo, block);
        }
        if let Some(age) = &self.age {
            age.handed(block.frames(), self.rate);
        }
        match &mut self.player {
            Some(p) => play(p, block, self.outputs, &mut self.scratch),
            None => block.zero_outputs(),
        }
    }
}

impl Process for AsioSide {
    fn process(&mut self, block: &mut Block<'_>) {
        self.pull_rx();
        self.serve(block);
    }
}

#[cfg(test)]
mod tests;

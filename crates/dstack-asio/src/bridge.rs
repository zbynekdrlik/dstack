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
//! - **Dante -> ASIO:** the pump runs [`DanteSide`], which writes each RX
//!   block into the to-ASIO FIFO at the network's clock; the driver's buffer
//!   switch runs [`AsioSide`], whose player pulls it at the driver's clock
//!   (dlisten's direction).
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
//! refused and the runner retries it after its backoff. Neither `process` allocates or makes a system call, and
//! neither waits on a lock (the switch's bounded channel never blocks).

use std::sync::Arc;

use dlink::bridge::{AppBuffer, Fifo, Player, Stats, Switch, Taker, chunk_ns, fifo_for, switch};
use dstack_audio_io::{Block, Process};

use crate::device::Opened;

/// Frames a side converts at once (its interleaved scratch).
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
}

impl Bridge {
    /// A bridge for `spec`; an error only for a rate the converter refuses.
    pub fn new(spec: Spec) -> Result<Self, String> {
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
                Arc::clone(&from_stats),
            )
            .map_err(|e| e.to_string())?;
            let (taker, switch) = switch();
            (Some((first, taker)), Some(switch))
        } else {
            (None, None)
        };
        let dante = DanteSide {
            to_asio: to_asio.clone(),
            from_asio,
            outputs: spec.from_asio,
            scratch: vec![0.0; CHUNK * spec.from_asio],
        };
        Ok(Bridge {
            spec,
            to_asio,
            to_stats: Arc::new(Stats::default()),
            from_stats,
            switch,
            dante: Some(dante),
        })
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
        let player = match &self.to_asio {
            Some(fifo) => Some(
                Player::new(
                    Arc::clone(fifo),
                    dante,
                    rate,
                    buffer,
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
                Arc::clone(&self.from_stats),
            )
            .map_err(|e| e.to_string())?;
            switch.send(p)?;
            from_asio = Some(fifo);
        }
        Ok(AsioSide {
            player,
            outputs: self.spec.to_asio,
            from_asio,
            scratch: vec![0.0; CHUNK * self.spec.to_asio],
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

    /// The to-ASIO FIFO (its depth and overflows for a status line).
    pub fn to_asio_fifo(&self) -> Option<Arc<Fifo>> {
        self.to_asio.clone()
    }
}

/// The player's output for `frames` frames into the block's first
/// `outputs` channels, through `scratch` (interleaved, [`CHUNK`] frames).
fn play(player: &mut Player, block: &mut Block<'_>, outputs: usize, scratch: &mut [f64]) {
    let frames = block.frames();
    let mut done = 0;
    while done < frames {
        let n = (frames - done).min(CHUNK);
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
    /// The from-ASIO player and its handover's end.
    from_asio: Option<(Player, Taker<Player>)>,
    outputs: usize,
    scratch: Vec<f64>,
}

impl Process for DanteSide {
    fn process(&mut self, block: &mut Block<'_>) {
        if let Some(fifo) = &self.to_asio {
            feed(fifo, block);
        }
        match &mut self.from_asio {
            Some((player, taker)) => {
                taker.take(player);
                play(player, block, self.outputs, &mut self.scratch);
            }
            None => block.zero_outputs(),
        }
    }
}

/// The driver's side: the to-ASIO player into its outputs, its inputs into
/// the from-ASIO FIFO.
pub struct AsioSide {
    player: Option<Player>,
    outputs: usize,
    from_asio: Option<Arc<Fifo>>,
    scratch: Vec<f64>,
}

impl Process for AsioSide {
    fn process(&mut self, block: &mut Block<'_>) {
        if let Some(fifo) = &self.from_asio {
            feed(fifo, block);
        }
        match &mut self.player {
            Some(p) => play(p, block, self.outputs, &mut self.scratch),
            None => block.zero_outputs(),
        }
    }
}

#[cfg(test)]
mod tests;

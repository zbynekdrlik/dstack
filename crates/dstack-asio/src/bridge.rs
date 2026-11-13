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
//! `DanteSide` through a bounded channel and given back the same way to be
//! freed off the pump's thread ([`Bridge::collect`]). Neither `process`
//! allocates, locks or makes a system call.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};

use dlink::bridge::{AppBuffer, Fifo, Player, Stats, buffer, fifo_for};
use dstack_audio_io::{Block, Process};

use crate::device::Opened;

/// Frames a side converts at once (its interleaved scratch).
const CHUNK: usize = 256;
/// Players in flight between the bridge and the pump's side, each way.
const HANDOVERS: usize = 4;

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
    handover: SyncSender<Player>,
    retired: Receiver<Player>,
    dante: Option<DanteSide>,
}

impl Bridge {
    pub fn new(spec: Spec) -> Self {
        let (handover, incoming) = sync_channel(HANDOVERS);
        let (retire, retired) = sync_channel(HANDOVERS);
        let to_asio = (spec.to_asio > 0).then(|| Arc::new(fifo_for(spec.to_asio, spec.dante_rate)));
        let dante = DanteSide {
            to_asio: to_asio.clone(),
            player: None,
            outputs: spec.from_asio,
            incoming,
            retire,
            scratch: vec![0.0; CHUNK * spec.from_asio],
        };
        Bridge {
            spec,
            to_asio,
            to_stats: Arc::new(Stats::default()),
            from_stats: Arc::new(Stats::default()),
            handover,
            retired,
            dante: Some(dante),
        }
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
            buffer::chunk_ns(u64::from(opened.buffer_frames), rate),
            buffer::chunk_ns(u64::from(self.spec.block_frames), self.spec.dante_rate),
        )
        .set(self.spec.app_buffer_ns)
    }

    /// The driver's side for a driver opened as `opened` (its rate and
    /// buffer), and the from-ASIO player handed to the pump's side. Frees
    /// the players the pump gave back first.
    pub fn asio_side(&self, opened: &Opened) -> Result<AsioSide, String> {
        self.collect();
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
        let from_asio =
            (self.spec.from_asio > 0).then(|| Arc::new(fifo_for(self.spec.from_asio, rate)));
        if let Some(fifo) = &from_asio {
            let p = Player::new(
                Arc::clone(fifo),
                rate,
                dante,
                buffer,
                Arc::clone(&self.from_stats),
            )
            .map_err(|e| e.to_string())?;
            self.handover.try_send(p).map_err(|e| match e {
                TrySendError::Full(_) => {
                    "the pump has not taken the last players (is the Dante device running?)"
                        .to_string()
                }
                TrySendError::Disconnected(_) => "the pump's side is gone".to_string(),
            })?;
        }
        Ok(AsioSide {
            player,
            outputs: self.spec.to_asio,
            from_asio,
            scratch: vec![0.0; CHUNK * self.spec.to_asio],
        })
    }

    /// Frees the players the pump's side gave back; returns how many.
    pub fn collect(&self) -> usize {
        let mut n = 0;
        while self.retired.try_recv().is_ok() {
            n += 1;
        }
        n
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
        player.fill_with(buf, outputs, |x| x);
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
    player: Option<Player>,
    outputs: usize,
    incoming: Receiver<Player>,
    retire: SyncSender<Player>,
    scratch: Vec<f64>,
}

impl DanteSide {
    /// Takes the player of the driver's latest open, if one came; the one it
    /// replaces goes back to be freed.
    fn take_next(&mut self) {
        match self.incoming.try_recv() {
            Ok(next) => {
                if let Some(old) = self.player.replace(next) {
                    // At most HANDOVERS players are ever in flight, so the
                    // channel has room; a full one keeps (leaks) nothing:
                    // the player drops here.
                    let _ = self.retire.try_send(old);
                }
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
    }
}

impl Process for DanteSide {
    fn process(&mut self, block: &mut Block<'_>) {
        self.take_next();
        if let Some(fifo) = &self.to_asio {
            feed(fifo, block);
        }
        match &mut self.player {
            Some(p) => play(p, block, self.outputs, &mut self.scratch),
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

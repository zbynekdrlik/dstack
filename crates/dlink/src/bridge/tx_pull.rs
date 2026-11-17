// SPDX-License-Identifier: MIT OR Apache-2.0

//! The TX side of a bridge pulled by the device's TX thread (dstack #363,
//! the TX twin of #307's [`super::RxPull`]; the design of #346, approved
//! 17.11.2026).
//!
//! An app whose TX audio runs on its own clock (dsend's capture, OBS's
//! mix) writes it into a [`super::Fifo`]; [`TxPull`] takes it out through
//! the [`Player`] (the ASRC steered on the FIFO's depth at the device's
//! app buffer) into the device's TX block, each piece at its own start
//! ([`Player::fill_at`]: a block that does not follow on resyncs the
//! player once). [`crate::Link::start_tx_pulling`] hands it to the device,
//! which runs it on the TX thread: the TX thread fills each block at the
//! pump's own due time and stamp just before it sends, so a late pump no
//! longer leaves sent stamps empty. A replacement player (another output,
//! another rate) comes through a [`Switch`] ([`TxPull::switched`]).
//!
//! [`TxPull`]'s `process` allocates nothing, takes no lock and makes no
//! system call: its scratch is sized at the start, the player's buffers
//! are its own, and the switch's `try_recv` takes no lock.

use dstack_audio_io::{Block, Process};

use super::{Player, Switch, Taker, switch};

/// The most frames one player call fills; a longer block takes several.
pub const CHUNK: usize = 1024;

/// Whether a pull of `outputs` channels fits the TX channels `channels`
/// of a device of `tx` TX channels: the reason when not.
pub fn check(channels: &[usize], tx: usize, outputs: usize) -> Result<(), String> {
    if channels.len() != outputs {
        return Err(format!(
            "the TX pull fills {outputs} channels for {} TX channels",
            channels.len()
        ));
    }
    match channels.iter().find(|&&c| c >= tx) {
        Some(c) => Err(format!(
            "the app's TX channel {} is past the device's {tx} TX channels",
            c + 1
        )),
        None => Ok(()),
    }
}

/// A processor with nothing to do: the pump's processor of an app whose
/// only audio is the pulled TX (dsend), [`crate::Link::start_tx_pulling`]
/// with no RX channels.
#[derive(Debug, Default, Clone, Copy)]
pub struct Idle;

impl Process for Idle {
    fn process(&mut self, _block: &mut Block<'_>) {}
}

/// The TX side of a bridge (module doc): the [`Player`] of the app's FIFO
/// filling `outputs` TX channels, its player replaceable through a
/// [`Switch`].
pub struct TxPull {
    player: Player,
    taker: Option<Taker<Player>>,
    outputs: usize,
    /// One player call's frames, interleaved.
    scratch: Vec<f64>,
}

impl std::fmt::Debug for TxPull {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TxPull")
            .field("outputs", &self.outputs)
            .field("switched", &self.taker.is_some())
            .finish_non_exhaustive()
    }
}

impl TxPull {
    /// `player` filling `outputs` TX channels (the player maps its FIFO's
    /// channels on them: L/R from the first two, a mono FIFO on both). The
    /// player's output rate is the app's (`crate::Config::sample_rate`):
    /// a device that converts runs the pull behind its TX converter.
    pub fn new(player: Player, outputs: usize) -> TxPull {
        TxPull {
            player,
            taker: None,
            outputs,
            scratch: vec![0.0; CHUNK * outputs],
        }
    }

    /// [`TxPull::new`] playing `first`, and the [`Switch`] that hands it a
    /// replacement: the TX thread takes it at its next block and gives
    /// the one it replaces back to be freed on the switch's thread
    /// ([`Switch::collect`]), never its own.
    pub fn switched(first: Player, outputs: usize) -> (TxPull, Switch<Player>) {
        let (taker, switch) = switch();
        let mut pull = TxPull::new(first, outputs);
        pull.taker = Some(taker);
        (pull, switch)
    }

    /// The TX channels it fills.
    pub fn outputs(&self) -> usize {
        self.outputs
    }

    /// The player it plays now (the last one handed over and taken).
    pub fn player(&self) -> &Player {
        &self.player
    }
}

impl Process for TxPull {
    fn process(&mut self, block: &mut Block<'_>) {
        if let Some(taker) = &self.taker {
            taker.take(&mut self.player);
        }
        let frames = block.frames();
        let outputs = self.outputs;
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(CHUNK);
            let buf = &mut self.scratch[..n * outputs];
            // Each piece's own start: a block that does not follow on
            // resyncs the player once (dstack #303), a piece of the same
            // block never.
            let start = block.start().map(|s| s + done as u64);
            self.player.fill_at(start, buf, outputs, |x| x);
            for c in 0..outputs.min(block.outputs()) {
                let out = block.output(c);
                for (f, slot) in out.iter_mut().skip(done).take(n).enumerate() {
                    *slot = buf[f * outputs + c];
                }
            }
            done += n;
        }
    }
}

#[cfg(test)]
mod tests;

// SPDX-License-Identifier: MIT OR Apache-2.0

//! The dlink side (dstack #164): the device's processor, run by its pump
//! every block, fills the TX channels (L, R) from the captured output's
//! FIFO through the [`Player`] (the converter steered on the FIFO's depth,
//! [`dstack_endpoint::player`]). The pump calls it on the network's clock,
//! so the player's "output" is the network here and its "network" (the
//! FIFO's producer) the capture.
//!
//! Another output chosen in dtray's menu brings another rate and channel
//! count, so another FIFO and player. The main thread makes them and hands
//! the player over through a [`Switch`]: a bounded channel of one, which
//! the pump reads with `try_recv` (no allocation, no lock); the player it
//! replaces goes back through a second one, so it is freed on the main
//! thread ([`Switch::collect`]), never in the pump.
//!
//! [`Source::process`] allocates nothing, takes no lock and makes no system
//! call (the pump's rules, I7).

use std::sync::mpsc::{Receiver, SyncSender, TryRecvError, TrySendError, sync_channel};

use dlink::{Block, Process};
use dstack_endpoint::player::Player;

/// dsend's TX channels: L and R.
pub const TX_CHANNELS: usize = 2;
/// The TX channels' names.
pub const TX_NAMES: [&str; TX_CHANNELS] = ["L", "R"];
/// The most frames one player call fills; a longer block takes several.
pub const CHUNK: usize = 1024;
/// [`Switch::send`]'s error while the pump has not taken the last player.
pub const NOT_TAKEN: &str = "the Dante device's pump has not taken the last output yet";
/// [`Switch::send`]'s error once the device (its pump) is gone.
pub const GONE: &str = "the Dante device is gone";

/// The device's processor (module doc).
pub struct Source {
    player: Player,
    incoming: Receiver<Player>,
    retired: SyncSender<Player>,
    /// One player call's frames, interleaved.
    scratch: Vec<f64>,
}

/// The main thread's end: hands a new player to the pump and frees the
/// ones it replaced.
pub struct Switch {
    outgoing: SyncSender<Player>,
    retired: Receiver<Player>,
    /// Players handed over whose predecessor was not freed yet: each
    /// handover retires exactly one player, so at most this many wait in
    /// `retired` (kept within its room, [`RETIRED`]).
    pending: usize,
}

/// The room of the channel back to the main thread: [`Switch::send`]
/// refuses a handover that could leave more players than this to be freed,
/// so the pump's `try_send` back always has room and never frees one.
pub const RETIRED: usize = 2;

/// The processor playing `first`, and its [`Switch`].
pub fn source(first: Player) -> (Source, Switch) {
    let (outgoing, incoming) = sync_channel(1);
    let (back, retired) = sync_channel(RETIRED);
    (
        Source {
            player: first,
            incoming,
            retired: back,
            scratch: vec![0.0; CHUNK * TX_CHANNELS],
        },
        Switch {
            outgoing,
            retired,
            pending: 0,
        },
    )
}

impl Switch {
    /// Hands `next` to the pump, which plays it from its next block. The
    /// players it replaced are freed first. An error, and `next` freed
    /// here, when the pump has not taken the last one yet or the players it
    /// replaced still wait to be freed ([`NOT_TAKEN`]: it does not run), or
    /// it is gone ([`GONE`]).
    pub fn send(&mut self, next: Player) -> Result<(), &'static str> {
        self.collect();
        if self.pending >= RETIRED {
            return Err(NOT_TAKEN);
        }
        self.outgoing.try_send(next).map_err(|e| match e {
            TrySendError::Full(_) => NOT_TAKEN,
            TrySendError::Disconnected(_) => GONE,
        })?;
        self.pending += 1;
        Ok(())
    }

    /// Frees the players the pump replaced; returns how many.
    pub fn collect(&mut self) -> usize {
        let mut n = 0;
        while self.retired.try_recv().is_ok() {
            n += 1;
        }
        self.pending = self.pending.saturating_sub(n);
        n
    }
}

impl Source {
    /// Takes a player the main thread handed over, if there is one.
    fn take_next(&mut self) {
        match self.incoming.try_recv() {
            Ok(next) => {
                let old = std::mem::replace(&mut self.player, next);
                // Back to the main thread to be freed. `send` keeps at most
                // [`RETIRED`] handovers unfreed, so the channel has room.
                let _ = self.retired.try_send(old);
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {}
        }
    }
}

impl Process for Source {
    fn process(&mut self, block: &mut Block<'_>) {
        self.take_next();
        let frames = block.frames();
        let mut done = 0;
        while done < frames {
            let n = (frames - done).min(CHUNK);
            let buf = &mut self.scratch[..n * TX_CHANNELS];
            self.player.fill_with(buf, TX_CHANNELS, |x| x);
            for c in 0..TX_CHANNELS {
                // A channel the block lacks (fewer TX mapped) is skipped.
                let out = block.output(c);
                for (f, slot) in out.iter_mut().skip(done).take(n).enumerate() {
                    *slot = buf[f * TX_CHANNELS + c];
                }
            }
            done += n;
        }
    }
}

#[cfg(test)]
mod tests;
